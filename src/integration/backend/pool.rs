use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::anyhow;
use futures::future::BoxFuture;

use super::{EdsAccountBinding, MailBackend, SharedMailBackend, select_mail_backend};
use crate::integration::journal::PendingMailActionStore;
use crate::model::account::{MailAccount, MailAccountId};
use crate::model::mail::{
    ConversationId, ConversationSummary, DraftMessage, FolderId, MailFolder, MailboxMode,
    MessageDetail,
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
}

pub fn lazy_mail_backend() -> SharedMailBackend {
    Arc::new(BackendPool::new()) as SharedMailBackend
}

impl MailBackend for BackendPool {
    fn activate_account(
        &self,
        account: &MailAccount,
    ) -> BoxFuture<'_, anyhow::Result<MailboxMode>> {
        let account = account.clone();
        Box::pin(async move {
            let slot = {
                let mut accounts = self
                    .accounts
                    .lock()
                    .map_err(|_| anyhow!("account backend pool lock poisoned"))?;
                accounts
                    .entry(account.id.clone())
                    .or_insert_with(|| Arc::new(Mutex::new(None)))
                    .clone()
            };
            let mut active = slot
                .lock()
                .map_err(|_| anyhow!("account backend slot lock poisoned"))?;
            if let Some(active) = active.as_ref() {
                return Ok(active.mode);
            }

            let binding = self
                .available_bindings
                .lock()
                .map_err(|_| anyhow!("available account binding lock poisoned"))?
                .get(&account.id)
                .cloned();
            let selection = select_mail_backend(
                std::slice::from_ref(&account),
                binding.into_iter().collect(),
                self.pending_mail_actions.clone(),
            );
            let mode = selection.mode;
            *active = Some(ActivatedBackend {
                backend: selection.backend,
                mode,
            });
            Ok(mode)
        })
    }

    fn invalidate_account(&self, account_id: &MailAccountId) {
        self.accounts
            .lock()
            .expect("account backend pool lock poisoned")
            .remove(account_id);
    }

    fn replace_available_bindings(&self, bindings: &[EdsAccountBinding]) {
        *self
            .available_bindings
            .lock()
            .expect("available account binding lock poisoned") = bindings
            .iter()
            .cloned()
            .map(|binding| (MailAccountId(binding.account_id.clone()), binding))
            .collect();
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
        account_id: &MailAccountId,
        draft: &DraftMessage,
    ) -> BoxFuture<'_, anyhow::Result<Option<MessageDetail>>> {
        let backend = self.backend(account_id);
        let account_id = account_id.clone();
        let draft = draft.clone();
        Box::pin(async move { backend?.save_draft(&account_id, &draft).await })
    }

    fn send_draft(
        &self,
        account_id: &MailAccountId,
        draft: &DraftMessage,
    ) -> BoxFuture<'_, anyhow::Result<Option<MessageDetail>>> {
        let backend = self.backend(account_id);
        let account_id = account_id.clone();
        let draft = draft.clone();
        Box::pin(async move { backend?.send_draft(&account_id, &draft).await })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::BackendPool;
    use crate::integration::backend::{EdsAccountBinding, MailBackend};
    use crate::integration::journal::PendingMailActionStore;
    use crate::model::account::{MailAccount, MailAccountId};
    use crate::model::mail::MailboxMode;

    #[test]
    fn reuses_activated_accounts_and_updates_only_the_inactive_binding_catalog() {
        let active = account("active-account");
        let half_active = account("half-active-account");
        let inactive = account("inactive-account");
        let pending_actions = PendingMailActionStore::open(
            std::env::temp_dir()
                .join(format!(
                    "mail-backend-pool-test-{}",
                    glib::uuid_string_random()
                ))
                .join("pending-mail-actions.json"),
        );
        let pool = BackendPool::with_pending_actions(pending_actions);
        pool.replace_available_bindings(&[
            binding("active-account"),
            binding("half-active-account"),
        ]);

        assert_eq!(
            futures::executor::block_on(pool.activate_account(&active)).unwrap(),
            MailboxMode::Live
        );
        assert_eq!(
            futures::executor::block_on(pool.activate_account(&half_active)).unwrap(),
            MailboxMode::Live
        );
        let active_backend = pool.backend(&active.id).unwrap();
        let half_active_backend = pool.backend(&half_active.id).unwrap();
        assert!(!Arc::ptr_eq(&active_backend, &half_active_backend));
        assert_eq!(
            futures::executor::block_on(pool.activate_account(&active)).unwrap(),
            MailboxMode::Live
        );
        assert!(Arc::ptr_eq(
            &active_backend,
            &pool.backend(&active.id).unwrap()
        ));
        assert!(futures::executor::block_on(pool.list_folders(&inactive.id)).is_err());
        assert_eq!(pool.accounts.lock().unwrap().len(), 2);

        pool.replace_available_bindings(&[]);
        assert_eq!(
            futures::executor::block_on(pool.activate_account(&half_active)).unwrap(),
            MailboxMode::Live
        );
        assert!(Arc::ptr_eq(
            &half_active_backend,
            &pool.backend(&half_active.id).unwrap()
        ));

        pool.invalidate_account(&half_active.id);
        assert_eq!(
            futures::executor::block_on(pool.activate_account(&half_active)).unwrap(),
            MailboxMode::StubUnavailable
        );
        assert!(Arc::ptr_eq(
            &active_backend,
            &pool.backend(&active.id).unwrap()
        ));
        assert_eq!(pool.accounts.lock().unwrap().len(), 2);
    }

    fn account(id: &str) -> MailAccount {
        MailAccount {
            id: MailAccountId(id.into()),
            display_name: format!("{id} display"),
            primary_address: format!("{id}@example.com"),
            aliases: Vec::new(),
        }
    }

    fn binding(id: &str) -> EdsAccountBinding {
        EdsAccountBinding {
            account_id: id.into(),
            account_label: format!("{id} display"),
            account_uid: Some(format!("{id}-source")),
            account_parent_uid: None,
            account_backend_name: Some("test".into()),
            account_auth_method: None,
            identity_uid: None,
            identity_name: None,
            identity_reply_to: None,
            identity_aliases: None,
            transport_uid: Some(format!("{id}-transport")),
            transport_backend_name: Some("test".into()),
            transport_auth_method: None,
            drafts_folder: None,
            sent_folder: None,
        }
    }
}
