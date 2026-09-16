use std::sync::Arc;

use crate::integration::backend::{
    BackendChangeMonitor, RefreshOutcome, SharedMailBackend, SharedMailBackendRouter,
};
use crate::model::account::{MailAccount, MailAccountId};
use crate::model::event::{AccountMailboxLoad, MailboxContentSnapshot};
use crate::model::mail::{
    ConversationId, ConversationSummary, FolderId, MailFolder, MailboxMode, MessageAction,
    MessageDetail, PreparedMessage, StoredMessageRef, WriteOutcome,
};

#[derive(Clone)]
pub struct MailService {
    router: SharedMailBackendRouter,
}

#[derive(Clone)]
pub(crate) struct AccountMailService {
    backend: SharedMailBackend,
}

impl MailService {
    pub fn new(router: SharedMailBackendRouter) -> Self {
        Self { router }
    }

    pub(crate) fn same_router(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.router, &other.router)
    }

    pub(crate) fn lease_account(
        &self,
        account_id: &MailAccountId,
    ) -> Option<AccountMailService> {
        self.router
            .lease_account_backend(account_id)
            .map(AccountMailService::new)
    }

    pub(crate) fn unresolved_message_action_count(&self) -> usize {
        self.router.unresolved_message_action_count()
    }

    pub(crate) fn activate_account_service(
        &self,
        account_id: &MailAccountId,
    ) -> anyhow::Result<AccountMailService> {
        self.activate_account(account_id)
            .map(|(service, _)| service)
    }

    pub(crate) fn save_account_identities(
        &self,
        accounts: &[MailAccount],
    ) -> anyhow::Result<()> {
        crate::integration::account::save_identities(accounts)
    }

    pub fn activate_mailbox(
        &self,
        account_id: &MailAccountId,
        conversation_limit: usize,
    ) -> anyhow::Result<AccountMailboxLoad> {
        let (service, mode) = self.activate_account(account_id)?;
        Ok(service.load_mailbox(mode, conversation_limit))
    }

    fn activate_account(
        &self,
        account_id: &MailAccountId,
    ) -> anyhow::Result<(AccountMailService, MailboxMode)> {
        let activated = self.router.activate_account(account_id)?;
        Ok((AccountMailService::new(activated.backend), activated.mode))
    }
}

impl AccountMailService {
    pub(crate) fn new(backend: SharedMailBackend) -> Self {
        Self { backend }
    }

    pub(crate) fn account_id(&self) -> &MailAccountId {
        self.backend.account_id()
    }

    pub(crate) fn unresolved_message_action_count(&self) -> usize {
        self.backend.unresolved_message_action_count()
    }

    pub(crate) fn same_backend(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.backend, &other.backend)
    }

    pub(crate) fn open_change_monitor(
        &self,
        callback: impl Fn() + Send + Sync + 'static,
    ) -> anyhow::Result<BackendChangeMonitor> {
        self.backend.open_change_monitor(Arc::new(callback))
    }

    pub fn list_folders(&self) -> anyhow::Result<Vec<MailFolder>> {
        self.backend.list_folders()
    }

    pub fn list_conversations(
        &self,
        folder_id: &FolderId,
        offset: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<ConversationSummary>> {
        self.backend.list_conversations(folder_id, offset, limit)
    }

    pub(crate) fn load_mailbox(
        &self,
        mode: MailboxMode,
        conversation_limit: usize,
    ) -> AccountMailboxLoad {
        match self.load_mailbox_content(None, conversation_limit) {
            Ok(content) => AccountMailboxLoad {
                mode,
                content,
                failure: None,
            },
            Err(error) => {
                let failure = crate::failure::classify_failure(&error);
                crate::logging::report_failure("mailbox-cache-load", &error);
                AccountMailboxLoad {
                    mode,
                    content: MailboxContentSnapshot {
                        folders: Vec::new(),
                        selected_folder_id: None,
                        conversations: Vec::new(),
                    },
                    failure: Some(failure),
                }
            }
        }
    }

    pub(crate) fn load_mailbox_content(
        &self,
        selected_folder_id: Option<FolderId>,
        conversation_limit: usize,
    ) -> anyhow::Result<MailboxContentSnapshot> {
        let folders = self.list_folders()?;
        let selected_folder_id = selected_folder_id
            .filter(|selected| folders.iter().any(|folder| folder.id == *selected))
            .or_else(|| folders.first().map(|folder| folder.id.clone()));
        let conversations = if let Some(folder_id) = selected_folder_id.as_ref() {
            self.list_conversations(folder_id, 0, conversation_limit)?
        } else {
            Vec::new()
        };
        Ok(MailboxContentSnapshot {
            folders,
            selected_folder_id,
            conversations,
        })
    }

    pub fn search(&self, query: &str) -> anyhow::Result<Vec<ConversationSummary>> {
        self.backend.search(query)
    }

    pub(crate) fn refresh(&self) -> anyhow::Result<RefreshOutcome> {
        self.backend.refresh()
    }

    pub(crate) fn fill_message_cache(
        &self,
        conversation_id: &ConversationId,
    ) -> anyhow::Result<bool> {
        self.backend.fill_message_cache(conversation_id)
    }

    pub fn cached_message_detail(
        &self,
        conversation_id: &ConversationId,
    ) -> anyhow::Result<Option<MessageDetail>> {
        self.backend.get_cached_message_detail(conversation_id)
    }

    pub(crate) fn materialize_attachment(
        &self,
        conversation_id: &ConversationId,
        attachment_token: &str,
    ) -> anyhow::Result<Option<String>> {
        self.backend
            .materialize_attachment(conversation_id, attachment_token)
    }

    pub(crate) fn apply_message_action(
        &self,
        conversation_id: &ConversationId,
        action: &MessageAction,
    ) -> anyhow::Result<WriteOutcome> {
        match action {
            MessageAction::SetStarred(starred) => {
                self.backend.set_starred(conversation_id, *starred)
            }
            MessageAction::SetRead(read) => {
                self.backend.set_read(conversation_id, *read)
            }
            MessageAction::MoveTo(folder_id) => {
                self.backend.move_to_folder(conversation_id, folder_id)
            }
        }
    }

    pub fn save_draft(
        &self,
        message: &PreparedMessage,
    ) -> anyhow::Result<Option<StoredMessageRef>> {
        self.backend.save_draft(message)
    }

    pub fn queue_delivery(
        &self,
        message: &PreparedMessage,
    ) -> anyhow::Result<WriteOutcome> {
        self.backend.queue_delivery(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailbox_content_validates_selection_against_its_folder_snapshot() {
        let service = AccountMailService::new(crate::integration::stub::mail_backend());

        let selected = service
            .load_mailbox_content(Some(FolderId("sent".into())), 50)
            .unwrap();
        assert_eq!(selected.selected_folder_id, Some(FolderId("sent".into())));
        assert!(selected.conversations.is_empty());

        let fallback = service
            .load_mailbox_content(Some(FolderId("missing".into())), 50)
            .unwrap();
        assert_eq!(
            fallback.selected_folder_id,
            fallback.folders.first().map(|folder| folder.id.clone())
        );
        assert!(fallback
            .conversations
            .iter()
            .all(|conversation| {
                Some(&conversation.folder_id) == fallback.selected_folder_id.as_ref()
            }));
    }
}
