use anyhow::{anyhow, ensure};
use crate::integration::backend::{
    BackendChangeCallback, BackendChangeMonitor, MailBackend, RefreshOutcome, SharedMailBackend,
};
use crate::model::account::{MailAccount, MailAccountId, SendingIdentity};
use crate::model::mail::{
    ConversationId, ConversationSummary, FolderId, FolderKind, MailFolder, MessageDetail,
    MessageId, PreparedMessage, StoredMessageRef, WriteOutcome, sort_and_deduplicate_conversations,
};

const STUB_ACCOUNT_ID: &str = "local-stub";

pub(crate) fn is_stub_account_id(account_id: &MailAccountId) -> bool {
    account_id.0 == STUB_ACCOUNT_ID
}

pub(crate) fn stub_account_id() -> MailAccountId {
    MailAccountId(STUB_ACCOUNT_ID.into())
}

pub(crate) fn stub_account() -> MailAccount {
    MailAccount::new(
        stub_account_id(),
        "Pigeon Mail Stub".into(),
        SendingIdentity::new(
            "welcome@pigeon.invalid".into(),
            "Pigeon Mail".into(),
            None,
            crate::model::account::Signature {
                html: "<p>Pigeon Mail</p>".into(),
                text: "Pigeon Mail".into(),
            },
        ),
    )
}

pub(crate) fn mail_backend() -> SharedMailBackend {
    std::sync::Arc::new(StubBackend::new(stub_account_id())) as SharedMailBackend
}

#[cfg(test)]
pub(crate) fn test_mail_backend(account_id: MailAccountId) -> SharedMailBackend {
    std::sync::Arc::new(StubBackend::new(account_id)) as SharedMailBackend
}

pub(crate) struct StubMailboxStore {
    messages: Vec<ConversationRecord>,
}

struct StubBackend {
    account_id: MailAccountId,
    store: StubMailboxStore,
}

#[derive(Clone)]
struct ConversationRecord {
    summary: ConversationSummary,
    detail: MessageDetail,
}

impl StubMailboxStore {
    pub(crate) fn seeded() -> Self {
        let account = stub_account();
        let recipient = account
            .default_identity()
            .mailbox();
        Self {
            messages: stub_records(&recipient),
        }
    }

    fn find_record(&self, conversation_id: &str) -> Option<&ConversationRecord> {
        self.messages
            .iter()
            .find(|record| record.summary.id.0 == conversation_id)
    }

    fn unread_count(&self, folder_id: &str) -> u32 {
        self.messages
            .iter()
            .filter(|record| record.summary.folder_id.0 == folder_id)
            .map(|record| record.summary.unread_count)
            .sum()
    }

    pub(crate) fn folders(&self) -> Vec<MailFolder> {
        standard_folders()
            .into_iter()
            .map(|(id, name, kind)| MailFolder {
                id: FolderId(id.into()),
                name: name.into(),
                unread_count: self.unread_count(id),
                kind,
            })
            .collect()
    }

    pub(crate) fn conversations(&self, folder_id: &FolderId) -> Vec<ConversationSummary> {
        self.messages
            .iter()
            .filter(|record| record.summary.folder_id == *folder_id)
            .map(|record| record.summary.clone())
            .collect()
    }

    pub(crate) fn message_detail(&self, conversation_id: &str) -> Option<MessageDetail> {
        self.find_record(conversation_id)
            .map(|record| record.detail.clone())
    }

    pub(crate) fn search(&self, query: &str) -> Vec<ConversationSummary> {
        let query = query.to_lowercase();
        self.messages
            .iter()
            .filter(|record| {
                format!(
                    "{} {} {} {}",
                    record.summary.subject,
                    record.summary.preview,
                    record.summary.participants.join(" "),
                    record.detail.from
                )
                .to_lowercase()
                .contains(&query)
            })
            .map(|record| record.summary.clone())
            .collect()
    }
}

impl StubBackend {
    pub(crate) fn new(account_id: MailAccountId) -> Self {
        Self {
            account_id,
            store: StubMailboxStore::seeded(),
        }
    }

    fn conversation_exists(&self, conversation_id: &ConversationId) -> bool {
        self.store.message_detail(&conversation_id.0).is_some()
    }
}

impl MailBackend for StubBackend {
    fn account_id(&self) -> &MailAccountId {
        &self.account_id
    }

    fn unresolved_message_action_count(&self) -> usize {
        0
    }

    fn open_change_monitor(
        &self,
        _callback: BackendChangeCallback,
    ) -> anyhow::Result<BackendChangeMonitor> {
        Ok(Box::new(()))
    }

    fn list_folders(&self) -> anyhow::Result<Vec<MailFolder>> {
        Ok(self.store.folders())
    }

    fn list_conversations(
        &self,
        folder_id: &FolderId,
        offset: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<ConversationSummary>> {
        let conversations = self.store.conversations(folder_id);
        Ok(super::backend::slice_conversations(&conversations, offset, limit))
    }

    fn fill_message_cache(&self, conversation_id: &ConversationId) -> anyhow::Result<bool> {
        Ok(self.conversation_exists(conversation_id))
    }

    fn get_cached_message_detail(
        &self,
        conversation_id: &ConversationId,
    ) -> anyhow::Result<Option<MessageDetail>> {
        Ok(self.store.message_detail(&conversation_id.0))
    }

    fn materialize_attachment(
        &self,
        _conversation_id: &ConversationId,
        _attachment_token: &str,
    ) -> anyhow::Result<Option<String>> {
        Ok(None)
    }

    fn search(&self, query: &str) -> anyhow::Result<Vec<ConversationSummary>> {
        let mut matches = self.store.search(query);
        sort_and_deduplicate_conversations(&mut matches);
        Ok(matches)
    }

    fn refresh(&self) -> anyhow::Result<RefreshOutcome> {
        Ok(RefreshOutcome::default())
    }

    fn set_starred(
        &self,
        conversation_id: &ConversationId,
        _starred: bool,
    ) -> anyhow::Result<WriteOutcome> {
        ensure_stub_conversation(self, conversation_id)?;
        Ok(WriteOutcome::Unchanged)
    }

    fn set_read(
        &self,
        conversation_id: &ConversationId,
        _read: bool,
    ) -> anyhow::Result<WriteOutcome> {
        ensure_stub_conversation(self, conversation_id)?;
        Ok(WriteOutcome::Unchanged)
    }

    fn move_to_folder(
        &self,
        conversation_id: &ConversationId,
        folder_id: &FolderId,
    ) -> anyhow::Result<WriteOutcome> {
        ensure_stub_conversation(self, conversation_id)?;
        ensure!(
            self.store
                .folders()
                .iter()
                .any(|folder| folder.id == *folder_id),
            "stub folder '{}' is unavailable",
            folder_id.0
        );
        Ok(WriteOutcome::Unchanged)
    }

    fn save_draft(
        &self,
        _message: &PreparedMessage,
    ) -> anyhow::Result<Option<StoredMessageRef>> {
        Ok(None)
    }

    fn queue_delivery(&self, message: &PreparedMessage) -> anyhow::Result<WriteOutcome> {
        if message.has_recipient() {
            Ok(WriteOutcome::Unchanged)
        } else {
            Err(anyhow!("message has no recipients"))
        }
    }
}

fn ensure_stub_conversation(
    backend: &StubBackend,
    conversation_id: &ConversationId,
) -> anyhow::Result<()> {
    ensure!(
        backend.conversation_exists(conversation_id),
        "stub conversation '{}' is unavailable",
        conversation_id.0
    );
    Ok(())
}

fn standard_folders() -> Vec<(&'static str, &'static str, FolderKind)> {
    vec![
        ("inbox", "Inbox", FolderKind::Inbox),
        ("drafts", "Drafts", FolderKind::Drafts),
        ("sent", "Sent", FolderKind::Sent),
        ("archive", "Archive", FolderKind::Archive),
        ("trash", "Trash", FolderKind::Trash),
    ]
}

fn stub_records(recipient: &str) -> Vec<ConversationRecord> {
    vec![
        seeded_record(
            "inbox",
            "stub-account",
            "stub-account-message",
            "Mail account unavailable",
            vec!["Pigeon Mail".into()],
            1,
            1,
            true,
            "No usable mail account is currently available.",
            "Pigeon Mail Stub <stub@pigeon.invalid>",
            vec![recipient.into()],
            "Welcome",
            "<p><b>No usable mail account is currently available.</b></p><p>Account discovery or the mail service may be unavailable.</p>",
            "No usable mail account is currently available.\n\nAccount discovery or the mail service may be unavailable.",
        ),
        seeded_record(
            "inbox",
            "stub-navigation",
            "stub-navigation-message",
            "Explore the three-pane mailbox",
            vec!["Pigeon Mail".into()],
            1,
            0,
            false,
            "Use the folder list, message list, and reading pane with the mouse or keyboard.",
            "Pigeon Mail Stub <stub@pigeon.invalid>",
            vec![recipient.into()],
            "Welcome",
            "<p>Use ↑ and ↓ inside a list, and ← and → to move between the folder list, message list, and reading pane. Activating a message opens it; focus and selection remain useful visual indicators.</p>",
            "Use Up and Down inside a list, and Left and Right to move between the folder list, message list, and reading pane. Activating a message opens it; focus and selection remain useful visual indicators.",
        ),
    ]
}

fn seeded_record(
    folder_id: &str,
    conversation_id: &str,
    message_id: &str,
    subject: &str,
    participants: Vec<String>,
    message_count: u32,
    unread_count: u32,
    starred: bool,
    preview: &str,
    from: &str,
    to: Vec<String>,
    date_label: &str,
    body_html: &str,
    body_text: &str,
) -> ConversationRecord {
    ConversationRecord {
        summary: ConversationSummary {
            id: ConversationId(conversation_id.into()),
            folder_id: FolderId(folder_id.into()),
            subject: subject.into(),
            participants,
            message_count,
            unread_count,
            attachment_count: 0,
            starred,
            last_updated_unix_ms: 0,
            preview: preview.into(),
        },
        detail: MessageDetail {
            message_id: MessageId(message_id.into()),
            conversation_id: ConversationId(conversation_id.into()),
            subject: subject.into(),
            from: from.into(),
            to,
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: None,
            date_label: date_label.into(),
            starred,
            unread: unread_count > 0,
            attachments: Vec::new(),
            body: crate::model::mail::MessageBody::from_parts(body_html.into(), body_text.into()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{StubBackend, StubMailboxStore, stub_account_id};
    use crate::integration::backend::MailBackend;
    use crate::model::mail::{ConversationId, DraftMessage, FolderId, WriteOutcome};

    #[test]
    fn seeded_stub_is_read_only_and_searchable() {
        let store = StubMailboxStore::seeded();
        let detail = store
            .message_detail("stub-account")
            .expect("seeded stub message should remain available");
        assert!(detail.unread);
        assert_eq!(store.conversations(&FolderId("inbox".into())).len(), 2);
        assert!(store.conversations(&FolderId("sent".into())).is_empty());
        assert_eq!(store.search("three-pane").len(), 1);
        assert!(store.search("not present").is_empty());
    }

    #[test]
    fn backend_writes_validate_then_leave_the_stub_unchanged() {
        let backend = StubBackend::new(stub_account_id());
        let known = ConversationId("stub-account".into());
        let unknown = ConversationId("absent-message".into());
        let before = backend.store.conversations(&FolderId("inbox".into()));

        assert!(backend.fill_message_cache(&known).unwrap());
        assert!(!backend.fill_message_cache(&unknown).unwrap());
        assert!(backend.get_cached_message_detail(&unknown)
            .unwrap()
            .is_none());
        assert!(backend
            .materialize_attachment(&known, "absent-attachment")
            .unwrap()
            .is_none());
        assert_eq!(
            backend.set_read(&known, true).unwrap(),
            WriteOutcome::Unchanged
        );
        assert_eq!(
            backend
                .move_to_folder(&known, &FolderId("trash".into()))
                .unwrap(),
            WriteOutcome::Unchanged
        );
        assert!(backend.set_read(&unknown, true).is_err());
        assert!(
            backend
                .move_to_folder(&known, &FolderId("absent-folder".into()))
                .is_err()
        );

        let mut draft = DraftMessage::empty(
            stub_account_id(),
            "Sender <sender@example.invalid>".into(),
        );
        assert!(backend
            .queue_delivery(&draft.clone().into_prepared().unwrap())
            .is_err());
        draft.to.push("recipient@example.invalid".into());
        let prepared = draft.into_prepared().unwrap();
        assert!(backend.save_draft(&prepared).unwrap().is_none());
        assert_eq!(
            backend.queue_delivery(&prepared).unwrap(),
            WriteOutcome::Unchanged
        );

        assert_eq!(backend.store.conversations(&FolderId("inbox".into())), before);
        assert!(backend.store.conversations(&FolderId("drafts".into())).is_empty());
        assert!(backend.store.conversations(&FolderId("sent".into())).is_empty());
    }
}
