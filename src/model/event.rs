use std::sync::atomic::{AtomicU64, Ordering};

use crate::model::account::MailAccountId;
use crate::model::mail::{
    AttachmentOperation, ConversationSummary, FolderId, MailFolder, MailboxMode, MessageDetail,
    StoredMessageRef, WriteOutcome,
};

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RequestId(u64);

impl RequestId {
    pub fn next() -> Self {
        Self(NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed))
    }

    #[cfg(test)]
    pub(crate) fn wrapping_add(self, value: u64) -> Self {
        Self(self.0.wrapping_add(value))
    }
}

#[cfg(test)]
impl From<u64> for RequestId {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone)]
pub struct AccountMailboxLoad {
    pub mode: MailboxMode,
    pub content: MailboxContentSnapshot,
    pub failure: Option<RefreshFailureKind>,
}

#[derive(Debug, Clone)]
pub struct MailboxContentSnapshot {
    pub folders: Vec<MailFolder>,
    pub selected_folder_id: Option<FolderId>,
    pub conversations: Vec<ConversationSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshFailureKind {
    Connectivity,
    Authentication,
    Storage,
    Backend,
}

#[derive(Debug, Clone)]
pub enum MailEvent {
    AccountActivated {
        request_id: RequestId,
        load: AccountMailboxLoad,
    },
    AccountRefreshCompleted {
        account_id: MailAccountId,
        failure: Option<RefreshFailureKind>,
    },
    MailboxReloaded {
        request_id: RequestId,
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
        request_id: RequestId,
        result: Result<Vec<ConversationSummary>, String>,
    },
    SearchCompleted {
        request_id: RequestId,
        result: Result<Vec<ConversationSummary>, String>,
    },
    MessageDetailLoaded {
        request_id: RequestId,
        result: Result<Option<MessageDetail>, String>,
    },
    MessageActionCompleted {
        request_id: RequestId,
        result: Result<WriteOutcome, String>,
    },
    AccountIdentitiesSaveCompleted {
        request_id: RequestId,
        result: Result<(), String>,
    },
    PendingWorkChanged,
    DraftSaveCompleted {
        result: Result<Option<StoredMessageRef>, String>,
    },
    SendCompleted {
        result: Result<(), String>,
    },
    AttachmentPrepared {
        operation: AttachmentOperation,
        display_name: String,
        result: Result<String, String>,
    },
}
