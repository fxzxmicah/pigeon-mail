use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use anyhow::anyhow;
use super::actions::MessageActionQueue;
use super::{
    ActivatedBackend, MailBackendRouter, SharedMailBackend, SharedMailBackendRouter,
    build_live_account_backend,
};
use crate::integration::account::EdsAccountBinding;
use crate::model::account::MailAccountId;

struct BackendPool {
    routes: Mutex<BackendRoutes>,
    routing_updates: Mutex<()>,
    stub: SharedMailBackend,
    action_queue: MessageActionQueue,
}

#[derive(Default)]
struct BackendRoutes {
    catalog: HashMap<MailAccountId, EdsAccountBinding>,
    materialized: HashMap<MailAccountId, SharedMailBackend>,
}

impl BackendPool {
    fn new() -> Self {
        Self::with_action_queue(MessageActionQueue::new())
    }

    fn with_action_queue(action_queue: MessageActionQueue) -> Self {
        Self {
            routes: Mutex::new(BackendRoutes::default()),
            routing_updates: Mutex::new(()),
            stub: crate::integration::stub::mail_backend(),
            action_queue,
        }
    }

    #[cfg(test)]
    fn catalog_binding(&self, account_id: &MailAccountId) -> Option<EdsAccountBinding> {
        self.routes
            .lock()
            .expect("account routes lock poisoned")
            .catalog
            .get(account_id)
            .cloned()
    }
}

pub(crate) fn mail_backend_router() -> SharedMailBackendRouter {
    Arc::new(BackendPool::new()) as SharedMailBackendRouter
}

impl MailBackendRouter for BackendPool {
    fn unresolved_message_action_count(&self) -> usize {
        self.action_queue.len()
    }

    fn activate_account(&self, account_id: &MailAccountId) -> anyhow::Result<ActivatedBackend> {
        if crate::integration::stub::is_stub_account_id(account_id) {
            return Ok(ActivatedBackend {
                backend: self.stub.clone(),
                mode: crate::model::mail::MailboxMode::NoAccount,
            });
        }
        let _routing_update = self
            .routing_updates
            .lock()
            .expect("account routing update lock poisoned");
        let binding = {
            let routes = self
                .routes
                .lock()
                .expect("account routes lock poisoned");
            if let Some(backend) = routes.materialized.get(account_id) {
                return Ok(ActivatedBackend {
                    backend: backend.clone(),
                    mode: crate::model::mail::MailboxMode::Live,
                });
            }
            routes
                .catalog
                .get(account_id)
                .cloned()
                .ok_or_else(|| anyhow!("mail account '{}' is unavailable", account_id.0))?
        };
        let backend = build_live_account_backend(binding, self.action_queue.clone())?;
        self.routes
            .lock()
            .expect("account routes lock poisoned")
            .materialized
            .insert(account_id.clone(), backend.clone());
        Ok(ActivatedBackend {
            backend,
            mode: crate::model::mail::MailboxMode::Live,
        })
    }

    fn update_binding_catalog(
        &self,
        bindings: &[EdsAccountBinding],
    ) -> Vec<MailAccountId> {
        let _routing_update = self
            .routing_updates
            .lock()
            .expect("account routing update lock poisoned");
        let catalog: HashMap<_, _> = bindings
            .iter()
            .cloned()
            .map(|binding| (binding.account_id.clone(), binding))
            .collect();
        let mut routes = self
            .routes
            .lock()
            .expect("account routes lock poisoned");
        let changed_routes = routes
            .catalog
            .keys()
            .chain(catalog.keys())
            .filter(|account_id| routes.catalog.get(*account_id) != catalog.get(*account_id))
            .cloned()
            .collect::<HashSet<_>>();
        routes
            .materialized
            .retain(|account_id, _| !changed_routes.contains(account_id));
        routes.catalog = catalog;
        let mut changed_routes = changed_routes.into_iter().collect::<Vec<_>>();
        changed_routes.sort_by(|left, right| left.0.cmp(&right.0));
        changed_routes
    }

    fn lease_account_backend(&self, account_id: &MailAccountId) -> Option<SharedMailBackend> {
        if crate::integration::stub::is_stub_account_id(account_id) {
            return Some(self.stub.clone());
        }
        self.routes
            .lock()
            .expect("account routes lock poisoned")
            .materialized
            .get(account_id)
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::super::actions::MessageActionQueue;
    use super::BackendPool;
    use crate::integration::account::EdsAccountBinding;
    use crate::integration::backend::{
        AccountBackend, MailBackendRouter, SharedMailBackend,
    };
    use crate::model::account::MailAccountId;
    use crate::model::mail::{ConversationId, DraftMessage, WriteOutcome};

    #[test]
    fn unknown_accounts_are_not_materialized_as_stub_backends() {
        let unknown = MailAccountId("unknown-account".into());
        let pool = BackendPool::with_action_queue(test_action_queue());

        assert!(pool.activate_account(&unknown).is_err());
        assert!(pool.routes.lock().unwrap().materialized.is_empty());
    }

    #[test]
    fn stub_operations_use_normal_routing_without_materialization() {
        let pool = BackendPool::with_action_queue(test_action_queue());
        let account_id = crate::integration::stub::stub_account_id();
        let conversation_id = ConversationId("stub-account".into());
        let mut draft = DraftMessage::empty(
            account_id.clone(),
            "Stub Sender <stub@example.invalid>".into(),
        );

        let activated = pool.activate_account(&account_id).unwrap();
        assert!(Arc::ptr_eq(&activated.backend, &pool.stub));
        assert_eq!(
            activated.mode,
            crate::model::mail::MailboxMode::NoAccount
        );
        assert!(activated.backend.open_change_monitor(Arc::new(|| {})).is_ok());
        activated.backend.refresh().unwrap();

        let folders = activated.backend.list_folders().unwrap();
        assert!(!folders.is_empty());
        let conversations = activated.backend.list_conversations(
            &folders[0].id,
            0,
            crate::model::mail::CONVERSATION_PAGE_SIZE,
        )
        .unwrap();
        assert!(!conversations.is_empty());

        assert_eq!(
            activated.backend.set_read(&conversation_id, true).unwrap(),
            WriteOutcome::Unchanged
        );
        assert!(activated
            .backend
            .queue_delivery(&draft.clone().into_prepared().unwrap())
            .is_err());
        draft.to.push("recipient@example.invalid".into());
        let prepared = draft.into_prepared().unwrap();
        assert!(
            activated.backend.save_draft(&prepared).unwrap().is_none()
        );
        assert_eq!(
            activated.backend.queue_delivery(&prepared).unwrap(),
            WriteOutcome::Unchanged
        );
        assert!(pool.routes.lock().unwrap().materialized.is_empty());
    }

    #[test]
    fn catalog_binding_is_available_without_mailbox_activation() {
        let pool = BackendPool::with_action_queue(test_action_queue());
        let account_id = MailAccountId("settings-target".into());
        let initial = binding(&account_id.0);
        assert_eq!(
            pool.update_binding_catalog(std::slice::from_ref(&initial)),
            [account_id.clone()]
        );
        assert!(
            pool.update_binding_catalog(std::slice::from_ref(&initial))
                .is_empty()
        );

        let mut changed = initial;
        changed.route_fingerprint.mailbox = 1;
        assert_eq!(
            pool.update_binding_catalog(std::slice::from_ref(&changed)),
            [account_id.clone()]
        );

        let resolved = pool
            .catalog_binding(&account_id)
            .expect("settings writes should resolve an inactive catalog account");

        assert_eq!(resolved, changed);
        assert!(pool.routes.lock().unwrap().materialized.is_empty());
    }

    #[test]
    fn failed_activation_does_not_install_an_unavailable_backend() {
        let pool = BackendPool::with_action_queue(test_action_queue());
        let account_id = MailAccountId("unavailable-account".into());
        pool.update_binding_catalog(&[binding(&account_id.0)]);

        assert!(pool.activate_account(&account_id).is_err());
        assert!(!pool
            .routes
            .lock()
            .unwrap()
            .materialized
            .contains_key(&account_id));
        assert!(pool.activate_account(&account_id).is_err());
    }

    #[test]
    fn catalog_diff_preserves_unchanged_routes_and_retires_removed_routes() {
        let retained = MailAccountId("retained-account".into());
        let removed = MailAccountId("removed-account".into());
        let unmaterialized = MailAccountId("unmaterialized-account".into());
        let pool = BackendPool::with_action_queue(test_action_queue());
        let retained_backend = install_activated_backend(&pool, retained.clone());
        let removed_backend = install_activated_backend(&pool, removed.clone());
        let changed = pool.update_binding_catalog(&[
            binding("retained-account"),
            binding("removed-account"),
            binding("unmaterialized-account"),
        ]);
        assert_eq!(changed, [unmaterialized.clone()]);
        assert!(!Arc::ptr_eq(&retained_backend, &removed_backend));
        let retained_lease = pool.activate_account(&retained).unwrap();
        assert!(Arc::ptr_eq(&retained_backend, &retained_lease.backend));
        assert!(Arc::ptr_eq(
            &retained_backend,
            &pool.lease_account_backend(&retained).unwrap()
        ));
        assert!(pool.lease_account_backend(&unmaterialized).is_none());
        assert_eq!(pool.routes.lock().unwrap().materialized.len(), 2);
        assert!(
            pool.routes
                .lock()
                .unwrap()
                .catalog
                .contains_key(&unmaterialized)
        );

        let removed_lease = pool.activate_account(&removed).unwrap();
        assert!(Arc::ptr_eq(&removed_backend, &removed_lease.backend));
        assert!(Arc::ptr_eq(
            &removed_backend,
            &pool.lease_account_backend(&removed).unwrap()
        ));

        let changed = pool.update_binding_catalog(&[binding("retained-account")]);
        assert_eq!(changed, [removed.clone(), unmaterialized]);
        assert!(pool.activate_account(&removed).is_err());
        assert!(Arc::ptr_eq(&removed_backend, &removed_lease.backend));
        assert!(Arc::ptr_eq(
            &retained_backend,
            &pool.lease_account_backend(&retained).unwrap()
        ));
        assert_eq!(pool.routes.lock().unwrap().materialized.len(), 1);
    }

    #[test]
    fn leased_backend_survives_future_route_invalidation() {
        let account_id = MailAccountId("draining-account".into());
        let pool = BackendPool::with_action_queue(test_action_queue());
        let backend = install_activated_backend(&pool, account_id.clone());
        let lease = pool
            .lease_account_backend(&account_id)
            .expect("a materialized account should be leasable");
        assert!(Arc::ptr_eq(&backend, &lease));

        pool.update_binding_catalog(&[]);

        assert!(pool.lease_account_backend(&account_id).is_none());
        assert!(Arc::ptr_eq(&backend, &lease));
    }

    #[test]
    fn changed_route_retires_the_previous_backend_before_lazy_reactivation() {
        let account_id = MailAccountId("changed-account".into());
        let pool = BackendPool::with_action_queue(test_action_queue());
        let _previous_lease = install_activated_backend(&pool, account_id.clone());
        let mut changed_binding = binding(&account_id.0);
        changed_binding.transport_backend_name = "changed".into();

        assert_eq!(
            pool.update_binding_catalog(std::slice::from_ref(&changed_binding)),
            [account_id.clone()]
        );
        assert!(pool.lease_account_backend(&account_id).is_none());
        assert_eq!(pool.catalog_binding(&account_id), Some(changed_binding));
        assert!(pool.activate_account(&account_id).is_err());
    }

    fn test_action_queue() -> MessageActionQueue {
        MessageActionQueue::new()
    }

    fn install_activated_backend(
        pool: &BackendPool,
        account_id: MailAccountId,
    ) -> SharedMailBackend {
        let backend =
            Arc::new(AccountBackend::for_test(binding(&account_id.0))) as SharedMailBackend;
        let mut routes = pool.routes.lock().unwrap();
        routes.catalog.insert(account_id.clone(), binding(&account_id.0));
        routes.materialized.insert(account_id, backend.clone());
        backend
    }

    fn binding(id: &str) -> EdsAccountBinding {
        EdsAccountBinding {
            account_id: MailAccountId(id.into()),
            account_uid: format!("{id}-source"),
            account_parent_uid: format!("{id}-collection"),
            account_backend_name: "test".into(),
            account_auth_method: None,
            identity_uid: format!("{id}-identity"),
            transport_uid: format!("{id}-transport"),
            transport_backend_name: "test".into(),
            transport_auth_method: None,
            route_fingerprint: crate::integration::account::RouteFingerprint {
                account: 0,
                transport: 0,
                mailbox: 0,
            },
        }
    }
}
