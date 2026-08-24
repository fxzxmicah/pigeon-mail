use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::anyhow;
use futures::future::BoxFuture;

use super::{EdsAccountBinding, MailBackend, SharedMailBackend, select_mail_backend};
use crate::integration::journal::PendingMailActionStore;
use crate::model::account::MailAccountId;
use crate::model::mail::{
    ConversationId, ConversationSummary, DraftMessage, FolderId, MailFolder, MailboxMode,
    MessageDetail, StoredMessageRef,
};

#[derive(Clone)]
struct ActivatedBackend {
    backend: SharedMailBackend,
    mode: MailboxMode,
}

type BackendSlot = Arc<Mutex<Option<ActivatedBackend>>>;

struct BackendPool {
    accounts: Mutex<HashMap<MailAccountId, BackendSlot>>,
    available_bindings: Mutex<HashMap<MailAccountId, EdsAccountBinding>>,
    pending_mail_actions: PendingMailActionStore,
}

impl BackendPool {
    fn new() -> Self {
        Self::with_pending_actions(PendingMailActionStore::open_default())
    }

    fn with_pending_actions(pending_mail_actions: PendingMailActionStore) -> Self {
        Self {
            accounts: Mutex::new(HashMap::new()),
            available_bindings: Mutex::new(HashMap::new()),
            pending_mail_actions,
        }
    }

    fn backend(&self, account_id: &MailAccountId) -> anyhow::Result<SharedMailBackend> {
        let slot = self
            .accounts
            .lock()
            .map_err(|_| anyhow!("account backend pool lock poisoned"))?
            .get(account_id)
            .cloned()
            .ok_or_else(|| anyhow!("mail account '{}' is not active", account_id.0))?;
        let active = slot
            .lock()
            .map_err(|_| anyhow!("account backend slot lock poisoned"))?;
        active
            .as_ref()
            .map(|entry| entry.backend.clone())
            .ok_or_else(|| anyhow!("mail account '{}' is still activating", account_id.0))
    }

    fn activated_backend(
        &self,
        account_id: &MailAccountId,
    ) -> anyhow::Result<Option<ActivatedBackend>> {
        let slot = self
            .accounts
            .lock()
            .map_err(|_| anyhow!("account backend pool lock poisoned"))?
            .get(account_id)
            .cloned();
        let Some(slot) = slot else {
            return Ok(None);
        };
        let activated = slot
            .lock()
            .map_err(|_| anyhow!("account backend slot lock poisoned"))?
            .clone();
        Ok(activated)
    }
}

pub fn lazy_mail_backend() -> SharedMailBackend {
    Arc::new(BackendPool::new()) as SharedMailBackend
}

impl MailBackend for BackendPool {
    fn activate_account(
        &self,
        account_id: &MailAccountId,
    ) -> BoxFuture<'_, anyhow::Result<MailboxMode>> {
        let account_id = account_id.clone();
        Box::pin(async move {
            if let Some(active) = self.activated_backend(&account_id)? {
                return Ok(active.mode);
            }
            let binding = self
                .available_bindings
                .lock()
                .map_err(|_| anyhow!("available account binding lock poisoned"))?
                .get(&account_id)
                .cloned()
                .ok_or_else(|| anyhow!("mail account '{}' is unavailable", account_id.0))?;
            let slot = {
                let mut accounts = self
                    .accounts
                    .lock()
                    .map_err(|_| anyhow!("account backend pool lock poisoned"))?;
                accounts
                    .entry(account_id.clone())
                    .or_insert_with(|| Arc::new(Mutex::new(None)))
                    .clone()
            };
            let mut active = slot
                .lock()
                .map_err(|_| anyhow!("account backend slot lock poisoned"))?;
            if let Some(active) = active.as_ref() {
                return Ok(active.mode);
            }

            let selection = select_mail_backend(binding, self.pending_mail_actions.clone());
            let mode = selection.mode;
            *active = Some(ActivatedBackend {
                backend: selection.backend,
                mode,
            });
            Ok(mode)
        })
    }

    fn update_binding_catalog(
        &self,
        bindings: &[EdsAccountBinding],
        invalidated_accounts: &[MailAccountId],
    ) {
        let catalog = bindings
            .iter()
            .cloned()
            .map(|binding| (binding.account_id.clone(), binding))
            .collect();
        let mut available_bindings = self
            .available_bindings
            .lock()
            .expect("available account binding lock poisoned");
        let mut accounts = self
            .accounts
            .lock()
            .expect("account backend pool lock poisoned");
        *available_bindings = catalog;
        for account_id in invalidated_accounts {
            accounts.remove(account_id);
        }
    }

    fn eds_binding(&self, account_id: &MailAccountId) -> Option<EdsAccountBinding> {
        let slot = self
            .accounts
            .lock()
            .expect("account backend pool lock poisoned")
            .get(account_id)?
            .clone();
        let backend = slot
            .lock()
            .expect("account backend slot lock poisoned")
            .as_ref()?
            .backend
            .clone();
        backend.eds_binding(account_id)
    }

    fn list_folders(
        &self,
        account_id: &MailAccountId,
    ) -> BoxFuture<'_, anyhow::Result<Vec<MailFolder>>> {
        let backend = self.backend(account_id);
        let account_id = account_id.clone();
        Box::pin(async move { backend?.list_folders(&account_id).await })
    }

    fn list_conversations(
        &self,
        account_id: &MailAccountId,
        folder_id: &FolderId,
        offset: usize,
        limit: usize,
    ) -> BoxFuture<'_, anyhow::Result<Vec<ConversationSummary>>> {
        let backend = self.backend(account_id);
        let account_id = account_id.clone();
        let folder_id = folder_id.clone();
        Box::pin(async move {
            backend?
                .list_conversations(&account_id, &folder_id, offset, limit)
                .await
        })
    }

    fn get_message_detail(
        &self,
        account_id: &MailAccountId,
        conversation_id: &ConversationId,
    ) -> BoxFuture<'_, anyhow::Result<Option<MessageDetail>>> {
        let backend = self.backend(account_id);
        let account_id = account_id.clone();
        let conversation_id = conversation_id.clone();
        Box::pin(async move {
            backend?
                .get_message_detail(&account_id, &conversation_id)
                .await
        })
    }

    fn get_cached_message_detail(
        &self,
        account_id: &MailAccountId,
        conversation_id: &ConversationId,
    ) -> BoxFuture<'_, anyhow::Result<Option<MessageDetail>>> {
        let backend = self.backend(account_id);
        let account_id = account_id.clone();
        let conversation_id = conversation_id.clone();
        Box::pin(async move {
            backend?
                .get_cached_message_detail(&account_id, &conversation_id)
                .await
        })
    }

    fn open_attachment(
        &self,
        account_id: &MailAccountId,
        conversation_id: &ConversationId,
        attachment_uri: &str,
    ) -> BoxFuture<'_, anyhow::Result<Option<String>>> {
        let backend = self.backend(account_id);
        let account_id = account_id.clone();
        let conversation_id = conversation_id.clone();
        let attachment_uri = attachment_uri.to_string();
        Box::pin(async move {
            backend?
                .open_attachment(&account_id, &conversation_id, &attachment_uri)
                .await
        })
    }

    fn search(
        &self,
        account_id: &MailAccountId,
        query: &str,
    ) -> BoxFuture<'_, anyhow::Result<Vec<ConversationSummary>>> {
        let backend = self.backend(account_id);
        let account_id = account_id.clone();
        let query = query.to_string();
        Box::pin(async move { backend?.search(&account_id, &query).await })
    }

    fn refresh(&self, account_id: &MailAccountId) -> BoxFuture<'_, anyhow::Result<()>> {
        let backend = self.backend(account_id);
        let account_id = account_id.clone();
        Box::pin(async move { backend?.refresh(&account_id).await })
    }

    fn set_starred(
        &self,
        account_id: &MailAccountId,
        conversation_id: &ConversationId,
        starred: bool,
    ) -> BoxFuture<'_, anyhow::Result<()>> {
        let backend = self.backend(account_id);
        let account_id = account_id.clone();
        let conversation_id = conversation_id.clone();
        Box::pin(async move {
            backend?
                .set_starred(&account_id, &conversation_id, starred)
                .await
        })
    }

    fn set_read(
        &self,
        account_id: &MailAccountId,
        conversation_id: &ConversationId,
        read: bool,
    ) -> BoxFuture<'_, anyhow::Result<()>> {
        let backend = self.backend(account_id);
        let account_id = account_id.clone();
        let conversation_id = conversation_id.clone();
        Box::pin(async move { backend?.set_read(&account_id, &conversation_id, read).await })
    }

    fn move_to_folder(
        &self,
        account_id: &MailAccountId,
        conversation_id: &ConversationId,
        folder_id: &FolderId,
    ) -> BoxFuture<'_, anyhow::Result<()>> {
        let backend = self.backend(account_id);
        let account_id = account_id.clone();
        let conversation_id = conversation_id.clone();
        let folder_id = folder_id.clone();
        Box::pin(async move {
            backend?
                .move_to_folder(&account_id, &conversation_id, &folder_id)
                .await
        })
    }

    fn save_draft(
        &self,
        draft: &DraftMessage,
    ) -> BoxFuture<'_, anyhow::Result<Option<StoredMessageRef>>> {
        let backend = self.backend(&draft.account_id);
        let draft = draft.clone();
        Box::pin(async move { backend?.save_draft(&draft).await })
    }

    fn send_draft(&self, draft: &DraftMessage) -> BoxFuture<'_, anyhow::Result<bool>> {
        let backend = self.backend(&draft.account_id);
        let draft = draft.clone();
        Box::pin(async move { backend?.send_draft(&draft).await })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{ActivatedBackend, BackendPool};
    use crate::integration::backend::{EdsAccountBinding, MailBackend, stub_backend};
    use crate::integration::journal::PendingMailActionStore;
    use crate::model::account::MailAccountId;
    use crate::model::mail::MailboxMode;

    #[test]
    fn unknown_accounts_are_not_materialized_as_stub_backends() {
        let unknown = MailAccountId("unknown-account".into());
        let pool = BackendPool::with_pending_actions(test_pending_actions());

        assert!(futures::executor::block_on(pool.activate_account(&unknown)).is_err());
        assert!(pool.accounts.lock().unwrap().is_empty());
    }

    #[test]
    fn catalog_changes_preserve_half_active_backends_until_explicit_invalidation() {
        let active = MailAccountId("active-account".into());
        let half_active = MailAccountId("half-active-account".into());
        let inactive = MailAccountId("inactive-account".into());
        let pool = BackendPool::with_pending_actions(test_pending_actions());
        let active_backend = install_activated_stub(&pool, active.clone());
        let half_active_backend = install_activated_stub(&pool, half_active.clone());
        pool.update_binding_catalog(&[binding("inactive-account")], &[]);
        assert!(!Arc::ptr_eq(&active_backend, &half_active_backend));
        assert_eq!(
            futures::executor::block_on(pool.activate_account(&active)).unwrap(),
            MailboxMode::StubUnavailable
        );
        assert!(Arc::ptr_eq(
            &active_backend,
            &pool.backend(&active).unwrap()
        ));
        assert!(futures::executor::block_on(pool.list_folders(&inactive)).is_err());
        assert_eq!(pool.accounts.lock().unwrap().len(), 2);
        assert!(
            pool.available_bindings
                .lock()
                .unwrap()
                .contains_key(&inactive)
        );

        pool.update_binding_catalog(&[], &[]);
        assert_eq!(
            futures::executor::block_on(pool.activate_account(&half_active)).unwrap(),
            MailboxMode::StubUnavailable
        );
        assert!(Arc::ptr_eq(
            &half_active_backend,
            &pool.backend(&half_active).unwrap()
        ));

        pool.update_binding_catalog(&[], std::slice::from_ref(&half_active));
        assert!(futures::executor::block_on(pool.activate_account(&half_active)).is_err());
        assert!(Arc::ptr_eq(
            &active_backend,
            &pool.backend(&active).unwrap()
        ));
        assert_eq!(pool.accounts.lock().unwrap().len(), 1);
    }

    fn test_pending_actions() -> PendingMailActionStore {
        PendingMailActionStore::open(
            std::env::temp_dir()
                .join(format!(
                    "mail-backend-pool-test-{}",
                    glib::uuid_string_random()
                ))
                .join("pending-mail-actions.json"),
        )
    }

    fn install_activated_stub(
        pool: &BackendPool,
        account_id: MailAccountId,
    ) -> crate::integration::backend::SharedMailBackend {
        let backend = stub_backend();
        pool.accounts.lock().unwrap().insert(
            account_id,
            Arc::new(Mutex::new(Some(ActivatedBackend {
                backend: backend.clone(),
                mode: MailboxMode::StubUnavailable,
            }))),
        );
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
            drafts_folder: None,
            sent_folder: None,
        }
    }
}
