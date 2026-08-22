use crate::model::account::{AliasId, MailAccount, MailAccountId, SendingIdentity};
use crate::model::mail::{
    ConversationId, ConversationSummary, FolderId, FolderKind, MailFolder, MessageDetail, MessageId,
};

const STUB_ACCOUNT_ID: &str = "local-stub";

pub(crate) fn stub_account() -> MailAccount {
    MailAccount {
        id: MailAccountId(STUB_ACCOUNT_ID.into()),
        display_name: "Pigeon Mail Stub".into(),
        primary_address: "welcome@pigeon.invalid".into(),
        aliases: vec![SendingIdentity::with_id(
            AliasId("local-stub-primary".into()),
            "welcome@pigeon.invalid".into(),
            "Pigeon Mail".into(),
            None,
            "<p>Pigeon Mail</p>".into(),
            "Pigeon Mail".into(),
            true,
        )],
    }
}

pub(crate) struct StubMailboxStore {
    messages: Vec<ConversationRecord>,
}

#[derive(Clone)]
struct ConversationRecord {
    folder_id: FolderId,
    summary: ConversationSummary,
    detail: MessageDetail,
}

impl StubMailboxStore {
    pub(crate) fn seeded() -> Self {
        let account = stub_account();
        let recipient = account
            .default_identity()
            .expect("stub account must have a sending identity")
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
            .filter(|record| record.folder_id.0 == folder_id)
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
            .filter(|record| record.folder_id == *folder_id)
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
            "Connect Pigeon to your mail",
            vec!["Pigeon Mail".into()],
            1,
            1,
            true,
            "Open GNOME Settings → Online Accounts and add an account with Mail enabled.",
            "Pigeon Mail Stub <stub@pigeon.invalid>",
            vec![recipient.into()],
            "Welcome",
            "<p><b>This is a stub mailbox.</b></p><p>Open GNOME Settings → Online Accounts, add your provider account, and make sure Mail is enabled. If an account is already present, the EDS mail source is currently unavailable.</p>",
            "This is a stub mailbox.\n\nOpen GNOME Settings → Online Accounts, add your provider account, and make sure Mail is enabled. If an account is already present, the EDS mail source is currently unavailable.",
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
        folder_id: FolderId(folder_id.into()),
        summary: ConversationSummary {
            id: ConversationId(conversation_id.into()),
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
    use super::StubMailboxStore;
    use crate::model::mail::FolderId;

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
}
