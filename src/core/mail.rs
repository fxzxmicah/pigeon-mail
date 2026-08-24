use crate::integration::backend::{EdsAccountBinding, SharedMailBackend};
use crate::model::account::MailAccountId;
use crate::model::mail::{
    ConversationId, ConversationSummary, DraftMessage, FolderId, MailFolder, MailboxMode,
    MessageDetail, StoredMessageRef,
};

#[derive(Clone)]
pub struct MailService {
    backend: SharedMailBackend,
}

impl MailService {
    pub fn new(backend: SharedMailBackend) -> Self {
        Self { backend }
    }

    pub(crate) fn backend(&self) -> SharedMailBackend {
        self.backend.clone()
    }

    pub fn eds_binding(&self, account_id: &MailAccountId) -> Option<EdsAccountBinding> {
        self.backend.eds_binding(account_id)
    }

    pub async fn activate_account(
        &self,
        account_id: &MailAccountId,
    ) -> anyhow::Result<MailboxMode> {
        self.backend.activate_account(account_id).await
    }

    pub async fn list_folders(
        &self,
        account_id: &MailAccountId,
    ) -> anyhow::Result<Vec<MailFolder>> {
        self.backend.list_folders(account_id).await
    }

    pub async fn list_conversations(
        &self,
        account_id: &MailAccountId,
        folder_id: &FolderId,
        offset: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<ConversationSummary>> {
        self.backend
            .list_conversations(account_id, folder_id, offset, limit)
            .await
    }

    pub async fn search(
        &self,
        account_id: &MailAccountId,
        query: &str,
    ) -> anyhow::Result<Vec<ConversationSummary>> {
        self.backend.search(account_id, query).await
    }

    pub async fn refresh_account(&self, account_id: &MailAccountId) -> anyhow::Result<()> {
        self.backend.refresh(account_id).await
    }

    pub async fn message_detail(
        &self,
        account_id: &MailAccountId,
        conversation_id: &ConversationId,
    ) -> anyhow::Result<Option<MessageDetail>> {
        self.backend
            .get_message_detail(account_id, conversation_id)
            .await
    }

    pub async fn cached_message_detail(
        &self,
        account_id: &MailAccountId,
        conversation_id: &ConversationId,
    ) -> anyhow::Result<Option<MessageDetail>> {
        self.backend
            .get_cached_message_detail(account_id, conversation_id)
            .await
    }

    pub async fn open_attachment(
        &self,
        account_id: &MailAccountId,
        conversation_id: &ConversationId,
        attachment_uri: &str,
    ) -> anyhow::Result<Option<String>> {
        self.backend
            .open_attachment(account_id, conversation_id, attachment_uri)
            .await
    }

    pub async fn set_starred(
        &self,
        account_id: &MailAccountId,
        conversation_id: &ConversationId,
        starred: bool,
    ) -> anyhow::Result<()> {
        self.backend
            .set_starred(account_id, conversation_id, starred)
            .await
    }

    pub async fn set_read(
        &self,
        account_id: &MailAccountId,
        conversation_id: &ConversationId,
        read: bool,
    ) -> anyhow::Result<()> {
        self.backend
            .set_read(account_id, conversation_id, read)
            .await
    }

    pub async fn move_to_folder(
        &self,
        account_id: &MailAccountId,
        conversation_id: &ConversationId,
        folder_id: &FolderId,
    ) -> anyhow::Result<()> {
        self.backend
            .move_to_folder(account_id, conversation_id, folder_id)
            .await
    }

    pub async fn save_draft(
        &self,
        draft: &DraftMessage,
    ) -> anyhow::Result<Option<StoredMessageRef>> {
        self.backend.save_draft(draft).await
    }

    pub async fn send_draft(&self, draft: &DraftMessage) -> anyhow::Result<bool> {
        self.backend.send_draft(draft).await
    }
}
