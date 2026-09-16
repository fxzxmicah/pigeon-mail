use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use crate::core::mail::AccountMailService;
use crate::model::account::MailAccountId;
use crate::model::mail::{ConversationId, ConversationSummary, FolderId};

type ConversationBaseline = HashMap<ConversationId, i64>;
type AccountBaseline = HashMap<FolderId, ConversationBaseline>;

pub(super) struct NotificationTracker {
    baselines: Mutex<HashMap<MailAccountId, AccountBaseline>>,
}

pub(super) struct FolderSnapshot {
    folder_id: FolderId,
    folder_name: String,
    conversations: Option<Vec<ConversationSummary>>,
}

pub(super) struct NotificationSnapshot {
    folders: Vec<FolderSnapshot>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct FolderNotification {
    pub(super) folder_id: FolderId,
    pub(super) folder_name: String,
    pub(super) count: usize,
}

impl NotificationTracker {
    pub(super) fn new() -> Self {
        Self {
            baselines: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn observe(
        &self,
        account_id: &MailAccountId,
        snapshot: NotificationSnapshot,
    ) -> Vec<FolderNotification> {
        let mut baselines = self
            .baselines
            .lock()
            .expect("notification baseline lock poisoned");
        let current = baselines.entry(account_id.clone()).or_default();
        let present_folders = snapshot
            .folders
            .iter()
            .map(|folder| folder.folder_id.clone())
            .collect::<HashSet<_>>();
        current.retain(|folder_id, _| present_folders.contains(folder_id));
        let mut notifications = Vec::new();

        for folder in snapshot.folders {
            let Some(conversations) = folder.conversations else {
                continue;
            };
            let next_baseline = conversations
                .iter()
                .map(|summary| (summary.id.clone(), summary.last_updated_unix_ms))
                .collect::<ConversationBaseline>();
            if let Some(previous) = current.get(&folder.folder_id) {
                let count = conversations
                    .iter()
                    .filter(|summary| {
                        summary.unread_count > 0
                            && previous
                                .get(&summary.id)
                                .is_none_or(|known| summary.last_updated_unix_ms > *known)
                    })
                    .count();
                if count > 0 {
                    notifications.push(FolderNotification {
                        folder_id: folder.folder_id.clone(),
                        folder_name: folder.folder_name,
                        count,
                    });
                }
            }
            current.insert(folder.folder_id, next_baseline);
        }

        notifications
    }
}

pub(super) fn load_notification_snapshot(
    service: &AccountMailService,
) -> anyhow::Result<NotificationSnapshot> {
    let folders = service.list_folders()?;
    let mut snapshots = Vec::with_capacity(folders.len());
    for folder in folders {
        let conversations = match service.list_conversations(&folder.id, 0, 256) {
            Ok(conversations) => Some(conversations),
            Err(error) => {
                crate::logging::report_deferred("new-mail-folder-snapshot", &error);
                None
            }
        };
        snapshots.push(FolderSnapshot {
            folder_id: folder.id,
            folder_name: folder.name,
            conversations,
        });
    }
    Ok(NotificationSnapshot { folders: snapshots })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(id: &str, updated: i64, unread: u32) -> ConversationSummary {
        ConversationSummary {
            id: ConversationId(id.into()),
            folder_id: FolderId("inbox".into()),
            subject: String::new(),
            participants: Vec::new(),
            message_count: 1,
            unread_count: unread,
            attachment_count: 0,
            starred: false,
            last_updated_unix_ms: updated,
            preview: String::new(),
        }
    }

    fn folder(
        id: &str,
        name: &str,
        conversations: Vec<ConversationSummary>,
    ) -> FolderSnapshot {
        FolderSnapshot {
            folder_id: FolderId(id.into()),
            folder_name: name.into(),
            conversations: Some(conversations),
        }
    }

    fn snapshot(folders: Vec<FolderSnapshot>) -> NotificationSnapshot {
        NotificationSnapshot { folders }
    }

    #[test]
    fn first_observation_seeds_then_reports_new_unread_conversations_per_folder() {
        let tracker = NotificationTracker::new();
        let account_id = MailAccountId("account-1".into());

        assert!(tracker
            .observe(
                &account_id,
                snapshot(vec![
                    folder(
                        "inbox",
                        "Inbox",
                        vec![summary("shared", 10, 1), summary("flag-only", 10, 0)],
                    ),
                    folder("custom", "Receipts", vec![summary("shared", 10, 1)]),
                    folder("archive", "Archive", Vec::new()),
                ]),
            )
            .is_empty());

        assert_eq!(
            tracker.observe(
                &account_id,
                snapshot(vec![
                    folder(
                        "inbox",
                        "Inbox",
                        vec![
                            summary("shared", 10, 0),
                            summary("flag-only", 10, 1),
                            summary("read", 20, 0),
                        ],
                    ),
                    folder("custom", "Receipts", vec![summary("shared", 20, 1)]),
                    folder(
                        "archive",
                        "Archive",
                        vec![summary("new", 30, 1), summary("second", 30, 1)],
                    ),
                ]),
            ),
            vec![
                FolderNotification {
                    folder_id: FolderId("custom".into()),
                    folder_name: "Receipts".into(),
                    count: 1,
                },
                FolderNotification {
                    folder_id: FolderId("archive".into()),
                    folder_name: "Archive".into(),
                    count: 2,
                },
            ]
        );
    }

    #[test]
    fn observations_are_scoped_per_account_and_forget_absent_folders() {
        let tracker = NotificationTracker::new();

        assert!(tracker
            .observe(
                &MailAccountId("account-1".into()),
                snapshot(vec![folder(
                    "custom",
                    "Custom",
                    vec![summary("shared", 10, 1)],
                )]),
            )
            .is_empty());
        assert!(tracker
            .observe(
                &MailAccountId("account-2".into()),
                snapshot(vec![folder(
                    "custom",
                    "Custom",
                    vec![summary("shared", 20, 1)],
                )]),
            )
            .is_empty());
        assert!(tracker
            .observe(&MailAccountId("account-1".into()), snapshot(Vec::new()))
            .is_empty());
        assert!(tracker
            .observe(
                &MailAccountId("account-1".into()),
                snapshot(vec![folder(
                    "custom",
                    "Custom",
                    vec![summary("shared", 30, 1)],
                )]),
            )
            .is_empty());
        assert_eq!(
            tracker.observe(
                &MailAccountId("account-2".into()),
                snapshot(vec![folder(
                    "custom",
                    "Custom",
                    vec![summary("shared", 30, 1)],
                )]),
            ),
            vec![FolderNotification {
                folder_id: FolderId("custom".into()),
                folder_name: "Custom".into(),
                count: 1,
            }]
        );
    }

    #[test]
    fn failed_folder_read_preserves_its_last_successful_baseline() {
        let tracker = NotificationTracker::new();
        let account_id = MailAccountId("account-1".into());

        assert!(tracker
            .observe(
                &account_id,
                snapshot(vec![folder(
                    "custom",
                    "Custom",
                    vec![summary("known", 10, 1)],
                )]),
            )
            .is_empty());
        assert!(tracker
            .observe(
                &account_id,
                NotificationSnapshot {
                    folders: vec![FolderSnapshot {
                        folder_id: FolderId("custom".into()),
                        folder_name: "Custom".into(),
                        conversations: None,
                    }],
                },
            )
            .is_empty());
        assert_eq!(
            tracker.observe(
                &account_id,
                snapshot(vec![folder(
                    "custom",
                    "Custom",
                    vec![summary("known", 20, 1)],
                )]),
            ),
            vec![FolderNotification {
                folder_id: FolderId("custom".into()),
                folder_name: "Custom".into(),
                count: 1,
            }]
        );
    }
}
