use crate::model::account::MailAccountId;
use crate::model::mail::{
    ConversationId, ConversationSummary, FolderId, MailFolder, MailboxMode, MessageDetail,
    StoredMessageRef,
};

#[derive(Debug, Clone)]
pub struct AccountMailboxSnapshot {
    pub mode: MailboxMode,
    pub folders: Vec<MailFolder>,
    pub conversations: Vec<ConversationSummary>,
}

#[derive(Debug, Clone)]
pub struct MailboxContentSnapshot {
    pub folders: Vec<MailFolder>,
    pub selected_folder_id: Option<FolderId>,
    pub conversations: Vec<ConversationSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentDisposition {
    Open,
    SaveAs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageAction {
    SetStarred(bool),
    SetRead(bool),
    MoveTo(FolderId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshFailureKind {
    Connectivity,
    Authentication,
    Storage,
    Backend,
}

impl RefreshFailureKind {
    pub fn status_message(self) -> &'static str {
        match self {
            Self::Connectivity | Self::Authentication | Self::Backend => "Local/Cache",
            Self::Storage => "Cache unavailable",
        }
    }
}

#[derive(Debug, Clone)]
pub enum CacheEvent {
    AccountActivated {
        request_id: u64,
        account_id: MailAccountId,
        result: Result<AccountMailboxSnapshot, String>,
    },
    AccountRefreshCompleted {
        account_id: MailAccountId,
        failure: Option<RefreshFailureKind>,
    },
    RemoteAccountChanged {
        account_id: MailAccountId,
    },
    MailboxReloaded {
        request_id: u64,
        account_id: MailAccountId,
        result: Result<MailboxContentSnapshot, String>,
    },
    NewMailAvailable {
        account_id: MailAccountId,
        folder_id: FolderId,
        folder_name: String,
        count: usize,
    },
    MailboxCacheChanged {
        account_id: MailAccountId,
    },
    ThreadPageLoaded {
        request_id: u64,
        account_id: MailAccountId,
        folder_id: FolderId,
        offset: usize,
        result: Result<Vec<ConversationSummary>, String>,
    },
    SearchCompleted {
        request_id: u64,
        account_id: MailAccountId,
        query: String,
        result: Result<Vec<ConversationSummary>, String>,
    },
    MessageDetailLoaded {
        request_id: u64,
        account_id: MailAccountId,
        conversation_id: ConversationId,
        result: Result<Option<MessageDetail>, String>,
    },
    MessageActionCompleted {
        account_id: MailAccountId,
        conversation_id: ConversationId,
        action: MessageAction,
        result: Result<(), String>,
    },
    DraftSaveCompleted {
        result: Result<Option<StoredMessageRef>, String>,
    },
    SendCompleted {
        result: Result<(), String>,
    },
    AttachmentPrepared {
        disposition: AttachmentDisposition,
        display_name: String,
        result: Result<String, String>,
    },
}

impl CacheEvent {
    pub fn account_activated(
        request_id: u64,
        account_id: MailAccountId,
        result: Result<AccountMailboxSnapshot, String>,
    ) -> Self {
        Self::AccountActivated {
            request_id,
            account_id,
            result,
        }
    }

    pub fn account_refresh(account_id: MailAccountId, failure: Option<RefreshFailureKind>) -> Self {
        Self::AccountRefreshCompleted {
            account_id,
            failure,
        }
    }

    pub fn remote_changed(account_id: MailAccountId) -> Self {
        Self::RemoteAccountChanged { account_id }
    }

    pub fn mailbox_reloaded(
        request_id: u64,
        account_id: MailAccountId,
        result: Result<MailboxContentSnapshot, String>,
    ) -> Self {
        Self::MailboxReloaded {
            request_id,
            account_id,
            result,
        }
    }

    pub fn mailbox_changed(account_id: MailAccountId) -> Self {
        Self::MailboxCacheChanged { account_id }
    }

    pub fn new_mail(
        account_id: MailAccountId,
        folder_id: FolderId,
        folder_name: String,
        count: usize,
    ) -> Self {
        Self::NewMailAvailable {
            account_id,
            folder_id,
            folder_name,
            count,
        }
    }

    pub fn message_detail(
        request_id: u64,
        account_id: MailAccountId,
        conversation_id: ConversationId,
        result: Result<Option<MessageDetail>, String>,
    ) -> Self {
        Self::MessageDetailLoaded {
            request_id,
            account_id,
            conversation_id,
            result,
        }
    }

    pub fn thread_page(
        request_id: u64,
        account_id: MailAccountId,
        folder_id: FolderId,
        offset: usize,
        result: Result<Vec<ConversationSummary>, String>,
    ) -> Self {
        Self::ThreadPageLoaded {
            request_id,
            account_id,
            folder_id,
            offset,
            result,
        }
    }

    pub fn search(
        request_id: u64,
        account_id: MailAccountId,
        query: String,
        result: Result<Vec<ConversationSummary>, String>,
    ) -> Self {
        Self::SearchCompleted {
            request_id,
            account_id,
            query,
            result,
        }
    }

    pub fn attachment_prepared(
        disposition: AttachmentDisposition,
        display_name: String,
        result: Result<String, String>,
    ) -> Self {
        Self::AttachmentPrepared {
            disposition,
            display_name,
            result,
        }
    }

    pub fn message_action(
        account_id: MailAccountId,
        conversation_id: ConversationId,
        action: MessageAction,
        result: Result<(), String>,
    ) -> Self {
        Self::MessageActionCompleted {
            account_id,
            conversation_id,
            action,
            result,
        }
    }

    pub fn draft_saved(result: Result<Option<StoredMessageRef>, String>) -> Self {
        Self::DraftSaveCompleted { result }
    }

    pub fn send_completed(result: Result<(), String>) -> Self {
        Self::SendCompleted { result }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_statuses_describe_state_without_prescribing_a_fix() {
        assert_eq!(
            RefreshFailureKind::Connectivity.status_message(),
            "Local/Cache"
        );
        assert_eq!(
            RefreshFailureKind::Authentication.status_message(),
            "Local/Cache"
        );
        assert_eq!(
            RefreshFailureKind::Storage.status_message(),
            "Cache unavailable"
        );
        assert_eq!(RefreshFailureKind::Backend.status_message(), "Local/Cache");
    }

    #[test]
    fn constructors_create_the_expected_scoped_events() {
        let account_id = MailAccountId("account-1".into());
        let conversation_id = ConversationId("conversation-1".into());
        let folder_id = FolderId("inbox".into());

        assert!(matches!(
            CacheEvent::account_activated(
                3,
                account_id.clone(),
                Ok(AccountMailboxSnapshot {
                    mode: MailboxMode::Live,
                    folders: Vec::new(),
                    conversations: Vec::new(),
                }),
            ),
            CacheEvent::AccountActivated {
                request_id: 3,
                account_id,
                result: Ok(AccountMailboxSnapshot {
                    mode: MailboxMode::Live,
                    folders,
                    conversations,
                }),
            } if account_id.0 == "account-1"
                && folders.is_empty()
                && conversations.is_empty()
        ));

        assert!(matches!(
            CacheEvent::account_refresh(account_id.clone(), None),
            CacheEvent::AccountRefreshCompleted { failure: None, .. }
        ));
        assert!(matches!(
            CacheEvent::remote_changed(account_id.clone()),
            CacheEvent::RemoteAccountChanged { account_id }
                if account_id.0 == "account-1"
        ));
        assert!(matches!(
            CacheEvent::mailbox_reloaded(
                4,
                account_id.clone(),
                Ok(MailboxContentSnapshot {
                    folders: Vec::new(),
                    selected_folder_id: None,
                    conversations: Vec::new(),
                }),
            ),
            CacheEvent::MailboxReloaded {
                request_id: 4,
                account_id,
                result: Ok(MailboxContentSnapshot {
                    folders,
                    selected_folder_id: None,
                    conversations,
                }),
            } if account_id.0 == "account-1"
                && folders.is_empty()
                && conversations.is_empty()
        ));
        assert!(matches!(
            CacheEvent::account_refresh(
                account_id.clone(),
                Some(RefreshFailureKind::Authentication),
            ),
            CacheEvent::AccountRefreshCompleted {
                failure: Some(RefreshFailureKind::Authentication),
                ..
            }
        ));
        assert!(matches!(
            CacheEvent::search(
                8,
                MailAccountId("account-1".into()),
                "needle".into(),
                Ok(Vec::new()),
            ),
            CacheEvent::SearchCompleted {
                request_id: 8,
                account_id,
                query,
                result: Ok(results),
            } if account_id.0 == "account-1" && query == "needle" && results.is_empty()
        ));
        assert!(matches!(
            CacheEvent::mailbox_changed(account_id.clone()),
            CacheEvent::MailboxCacheChanged { .. }
        ));
        assert!(matches!(
            CacheEvent::new_mail(
                account_id.clone(),
                FolderId("receipts".into()),
                "Receipts".into(),
                2,
            ),
            CacheEvent::NewMailAvailable {
                account_id,
                folder_id,
                folder_name,
                count: 2,
            } if account_id.0 == "account-1"
                && folder_id.0 == "receipts"
                && folder_name == "Receipts"
        ));
        assert!(matches!(
            CacheEvent::thread_page(7, account_id.clone(), folder_id, 50, Ok(Vec::new())),
            CacheEvent::ThreadPageLoaded {
                request_id: 7,
                account_id,
                folder_id,
                offset: 50,
                result: Ok(conversations),
            } if account_id.0 == "account-1"
                && folder_id.0 == "inbox"
                && conversations.is_empty()
        ));
        assert!(matches!(
            CacheEvent::message_detail(9, account_id, conversation_id, Ok(None)),
            CacheEvent::MessageDetailLoaded {
                request_id: 9,
                account_id,
                conversation_id,
                result: Ok(None),
            } if account_id.0 == "account-1" && conversation_id.0 == "conversation-1"
        ));
        assert!(matches!(
            CacheEvent::message_action(
                MailAccountId("account-1".into()),
                ConversationId("conversation-1".into()),
                MessageAction::SetRead(true),
                Ok(()),
            ),
            CacheEvent::MessageActionCompleted {
                action: MessageAction::SetRead(true),
                result: Ok(()),
                ..
            }
        ));
        assert!(matches!(
            CacheEvent::draft_saved(Err("Draft not saved.".into())),
            CacheEvent::DraftSaveCompleted { result: Err(error) }
                if error == "Draft not saved."
        ));
        assert!(matches!(
            CacheEvent::send_completed(Err("Message not sent.".into())),
            CacheEvent::SendCompleted { result: Err(error) }
                if error == "Message not sent."
        ));
        assert!(matches!(
            CacheEvent::attachment_prepared(
                AttachmentDisposition::SaveAs,
                "report.pdf".into(),
                Ok("file:///tmp/report.pdf".into()),
            ),
            CacheEvent::AttachmentPrepared {
                disposition: AttachmentDisposition::SaveAs,
                display_name,
                result: Ok(uri),
            } if display_name == "report.pdf" && uri == "file:///tmp/report.pdf"
        ));
    }
}
