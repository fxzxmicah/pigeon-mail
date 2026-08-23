use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::model::account::MailAccountId;
use crate::model::mail::{ConversationId, ConversationSummary, FolderId};

const PENDING_MAIL_ACTIONS_FILE: &str = "pending-mail-actions.json";
const PENDING_MESSAGE_FLAGS_FILE: &str = "pending-message-flags.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PendingMove {
    pub(crate) account_id: MailAccountId,
    pub(crate) destination_folder_id: FolderId,
    pub(crate) summary: ConversationSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PendingMessageFlags {
    pub(crate) account_id: MailAccountId,
    pub(crate) conversation_id: ConversationId,
    pub(crate) read: Option<bool>,
    pub(crate) starred: Option<bool>,
}

#[derive(Clone)]
pub(crate) struct PendingMailActionStore {
    path: PathBuf,
    moves: Arc<Mutex<Vec<PendingMove>>>,
    moves_available: bool,
    flags_path: PathBuf,
    flags: Arc<Mutex<Vec<PendingMessageFlags>>>,
    flags_available: bool,
}

impl PendingMailActionStore {
    pub(crate) fn open_default() -> Self {
        #[cfg(test)]
        let path = std::env::temp_dir()
            .join(format!(
                "pending-mail-action-test-{}",
                glib::uuid_string_random()
            ))
            .join(PENDING_MAIL_ACTIONS_FILE);
        #[cfg(not(test))]
        let path = crate::config::data_file(PENDING_MAIL_ACTIONS_FILE);
        Self::open(path)
    }

    pub(crate) fn open(path: PathBuf) -> Self {
        let (moves, moves_available) = match crate::integration::json::load_vec(&path) {
            Ok(moves) => (moves, true),
            Err(error) => {
                crate::logging::report_failure("pending-move-journal-load", &error);
                (Vec::new(), false)
            }
        };
        let flags_path = path.with_file_name(PENDING_MESSAGE_FLAGS_FILE);
        let (flags, flags_available) = match crate::integration::json::load_vec(&flags_path) {
            Ok(flags) => (flags, true),
            Err(error) => {
                crate::logging::report_failure("pending-flag-journal-load", &error);
                (Vec::new(), false)
            }
        };
        Self {
            path,
            moves: Arc::new(Mutex::new(moves)),
            moves_available,
            flags_path,
            flags: Arc::new(Mutex::new(flags)),
            flags_available,
        }
    }

    pub(crate) fn for_account(&self, account_id: &MailAccountId) -> Vec<PendingMove> {
        self.moves
            .lock()
            .expect("pending move lock poisoned")
            .iter()
            .filter(|pending| pending.account_id == *account_id)
            .cloned()
            .collect()
    }

    pub(crate) fn queue_move(&self, pending: PendingMove) -> anyhow::Result<()> {
        self.update(|moves| upsert_move(moves, pending))
    }

    pub(crate) fn flags_for_account(&self, account_id: &MailAccountId) -> Vec<PendingMessageFlags> {
        self.flags
            .lock()
            .expect("pending flag lock poisoned")
            .iter()
            .filter(|pending| pending.account_id == *account_id)
            .cloned()
            .collect()
    }

    pub(crate) fn queue_read(
        &self,
        account_id: MailAccountId,
        conversation_id: ConversationId,
        read: bool,
    ) -> anyhow::Result<()> {
        self.update_flags(|flags| {
            upsert_flags(flags, account_id, conversation_id, Some(read), None)
        })
    }

    pub(crate) fn queue_starred(
        &self,
        account_id: MailAccountId,
        conversation_id: ConversationId,
        starred: bool,
    ) -> anyhow::Result<()> {
        self.update_flags(|flags| {
            upsert_flags(flags, account_id, conversation_id, None, Some(starred))
        })
    }

    pub(crate) fn remove_completed_flags(
        &self,
        account_id: &MailAccountId,
        completed: &HashSet<ConversationId>,
    ) -> anyhow::Result<()> {
        self.update_flags(|flags| {
            flags.retain(|pending| {
                pending.account_id != *account_id || !completed.contains(&pending.conversation_id)
            });
        })
    }

    pub(crate) fn remove_completed(
        &self,
        account_id: &MailAccountId,
        completed: &HashSet<ConversationId>,
    ) -> anyhow::Result<()> {
        self.update(|moves| {
            moves.retain(|pending| {
                pending.account_id != *account_id || !completed.contains(&pending.summary.id)
            });
        })
    }

    pub(crate) fn discard_missing_remote_sources(
        &self,
        account_id: &MailAccountId,
        remote_ids: &HashSet<ConversationId>,
    ) -> anyhow::Result<()> {
        self.update(|moves| {
            moves.retain(|pending| {
                pending.account_id != *account_id || remote_ids.contains(&pending.summary.id)
            });
        })
    }

    pub(crate) fn apply_move_overlay(
        &self,
        account_id: &MailAccountId,
        conversations: &mut HashMap<String, Vec<ConversationSummary>>,
    ) {
        apply_move_overlay(account_id, &self.for_account(account_id), conversations);
    }

    pub(crate) fn apply_flag_overlay(
        &self,
        account_id: &MailAccountId,
        conversations: &mut HashMap<String, Vec<ConversationSummary>>,
    ) {
        apply_flag_overlay(
            account_id,
            &self.flags_for_account(account_id),
            conversations,
        );
    }

    fn update(&self, update: impl FnOnce(&mut Vec<PendingMove>)) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.moves_available,
            "pending move local cache journal is unavailable"
        );
        let mut moves = self.moves.lock().expect("pending move lock poisoned");
        let mut next = moves.clone();
        update(&mut next);
        crate::integration::json::save_slice(&self.path, &next)?;
        *moves = next;
        Ok(())
    }

    fn update_flags(
        &self,
        update: impl FnOnce(&mut Vec<PendingMessageFlags>),
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.flags_available,
            "pending flag local cache journal is unavailable"
        );
        let mut flags = self.flags.lock().expect("pending flag lock poisoned");
        let mut next = flags.clone();
        update(&mut next);
        crate::integration::json::save_slice(&self.flags_path, &next)?;
        *flags = next;
        Ok(())
    }
}

fn apply_flag_overlay(
    account_id: &MailAccountId,
    pending: &[PendingMessageFlags],
    conversations: &mut HashMap<String, Vec<ConversationSummary>>,
) {
    for summaries in conversations.values_mut() {
        for summary in summaries {
            let Some(flags) = pending.iter().find(|flags| {
                flags.account_id == *account_id && flags.conversation_id == summary.id
            }) else {
                continue;
            };
            if let Some(read) = flags.read {
                summary.unread_count = if read { 0 } else { 1 };
            }
            if let Some(starred) = flags.starred {
                summary.starred = starred;
            }
        }
    }
}

fn upsert_flags(
    flags: &mut Vec<PendingMessageFlags>,
    account_id: MailAccountId,
    conversation_id: ConversationId,
    read: Option<bool>,
    starred: Option<bool>,
) {
    if let Some(existing) = flags.iter_mut().find(|pending| {
        pending.account_id == account_id && pending.conversation_id == conversation_id
    }) {
        if read.is_some() {
            existing.read = read;
        }
        if starred.is_some() {
            existing.starred = starred;
        }
    } else {
        flags.push(PendingMessageFlags {
            account_id,
            conversation_id,
            read,
            starred,
        });
    }
}

fn upsert_move(moves: &mut Vec<PendingMove>, pending: PendingMove) {
    if let Some(existing) = moves.iter_mut().find(|existing| {
        existing.account_id == pending.account_id && existing.summary.id == pending.summary.id
    }) {
        *existing = pending;
    } else {
        moves.push(pending);
    }
}

fn apply_move_overlay(
    account_id: &MailAccountId,
    pending_moves: &[PendingMove],
    conversations: &mut HashMap<String, Vec<ConversationSummary>>,
) {
    for pending in pending_moves
        .iter()
        .filter(|pending| pending.account_id == *account_id)
    {
        for summaries in conversations.values_mut() {
            summaries.retain(|summary| summary.id != pending.summary.id);
        }
        let Some(destination) = conversations.get_mut(&pending.destination_folder_id.0) else {
            continue;
        };
        destination.push(pending.summary.clone());
        destination.sort_by(|left, right| {
            right
                .last_updated_unix_ms
                .cmp(&left.last_updated_unix_ms)
                .then_with(|| left.subject.cmp(&right.subject))
        });
        destination.dedup_by(|left, right| left.id == right.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(id: &str, subject: &str, unread_count: u32) -> ConversationSummary {
        ConversationSummary {
            id: ConversationId(id.into()),
            subject: subject.into(),
            participants: vec!["Sender".into()],
            message_count: 1,
            unread_count,
            attachment_count: 0,
            starred: false,
            last_updated_unix_ms: 10,
            preview: "preview".into(),
        }
    }

    fn pending(account: &str, id: &str, destination: &str) -> PendingMove {
        PendingMove {
            account_id: MailAccountId(account.into()),
            destination_folder_id: FolderId(destination.into()),
            summary: summary(id, id, 1),
        }
    }

    #[test]
    fn overlay_moves_one_cached_summary_to_destination() {
        let account = MailAccountId("account-1".into());
        let mut cache = HashMap::from([
            ("inbox".into(), vec![summary("m1", "one", 1)]),
            ("archive".into(), vec![summary("m2", "two", 0)]),
        ]);

        apply_move_overlay(
            &account,
            &[pending("account-1", "m1", "archive")],
            &mut cache,
        );

        assert!(cache["inbox"].is_empty());
        let archived = &cache["archive"];
        assert_eq!(archived.len(), 2);
        assert_eq!(archived.iter().filter(|item| item.id.0 == "m1").count(), 1);
    }

    #[test]
    fn overlay_is_idempotent_and_ignores_other_accounts() {
        let account = MailAccountId("account-1".into());
        let mut cache = HashMap::from([
            ("inbox".into(), vec![summary("m1", "one", 1)]),
            ("archive".into(), Vec::new()),
        ]);
        let moves = [
            pending("account-1", "m1", "archive"),
            pending("account-2", "m2", "archive"),
        ];

        apply_move_overlay(&account, &moves, &mut cache);
        apply_move_overlay(&account, &moves, &mut cache);

        assert_eq!(cache["archive"].len(), 1);
        assert_eq!(cache["archive"][0].id.0, "m1");
    }

    #[test]
    fn overlay_does_not_make_an_unloaded_destination_look_complete() {
        let account = MailAccountId("account-1".into());
        let mut cache = HashMap::from([("inbox".into(), vec![summary("m1", "one", 1)])]);

        apply_move_overlay(
            &account,
            &[pending("account-1", "m1", "archive")],
            &mut cache,
        );

        assert!(cache["inbox"].is_empty());
        assert!(!cache.contains_key("archive"));
    }

    #[test]
    fn newer_move_replaces_the_destination() {
        let mut moves = vec![pending("account-1", "m1", "archive")];
        upsert_move(&mut moves, pending("account-1", "m1", "trash"));

        assert_eq!(moves.len(), 1);
        assert_eq!(moves[0].destination_folder_id.0, "trash");
    }

    #[test]
    fn json_round_trip_preserves_overlay_data() {
        let moves = vec![pending("account-1", "folder\u{1f}uid", "archive")];
        let encoded = serde_json::to_string(&moves).unwrap();
        let decoded: Vec<PendingMove> = serde_json::from_str(&encoded).unwrap();

        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].summary.id.0, "folder\u{1f}uid");
        assert_eq!(decoded[0].summary.unread_count, 1);
    }

    #[test]
    fn remote_reconciliation_is_scoped_and_keeps_still_pending_sources() {
        let mut moves = vec![
            pending("account-1", "still-remote", "archive"),
            pending("account-1", "already-moved", "trash"),
            pending("account-2", "not-refreshed", "archive"),
        ];
        let remote_ids = HashSet::from([ConversationId("still-remote".into())]);

        moves.retain(|pending| {
            pending.account_id != MailAccountId("account-1".into())
                || remote_ids.contains(&pending.summary.id)
        });

        assert_eq!(moves.len(), 2);
        assert!(
            moves
                .iter()
                .any(|pending| pending.summary.id.0 == "still-remote")
        );
        assert!(
            moves
                .iter()
                .any(|pending| pending.account_id.0 == "account-2")
        );
    }

    #[test]
    fn newer_flag_values_merge_without_losing_the_other_flag() {
        let account = MailAccountId("account-1".into());
        let conversation = ConversationId("inbox\u{1f}uid".into());
        let mut flags = Vec::new();

        upsert_flags(
            &mut flags,
            account.clone(),
            conversation.clone(),
            Some(true),
            None,
        );
        upsert_flags(
            &mut flags,
            account.clone(),
            conversation.clone(),
            None,
            Some(true),
        );
        upsert_flags(&mut flags, account, conversation, Some(false), None);

        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].read, Some(false));
        assert_eq!(flags[0].starred, Some(true));
    }

    #[test]
    fn pending_flags_json_round_trip_preserves_both_intents() {
        let pending = vec![PendingMessageFlags {
            account_id: MailAccountId("account-1".into()),
            conversation_id: ConversationId("inbox\u{1f}uid".into()),
            read: Some(false),
            starred: Some(true),
        }];

        let encoded = serde_json::to_string(&pending).unwrap();
        let decoded: Vec<PendingMessageFlags> = serde_json::from_str(&encoded).unwrap();

        assert_eq!(decoded, pending);
    }

    #[test]
    fn pending_flags_overlay_remote_summaries_and_is_account_scoped() {
        let account = MailAccountId("account-1".into());
        let id = ConversationId("inbox\u{1f}uid".into());
        let pending = PendingMessageFlags {
            account_id: account.clone(),
            conversation_id: id.clone(),
            read: Some(true),
            starred: Some(true),
        };
        let other = PendingMessageFlags {
            account_id: MailAccountId("account-2".into()),
            conversation_id: id.clone(),
            read: Some(false),
            starred: Some(false),
        };
        let mut conversations = HashMap::from([("inbox".into(), vec![summary(&id.0, "one", 1)])]);

        apply_flag_overlay(&account, &[other, pending], &mut conversations);

        assert_eq!(conversations["inbox"][0].unread_count, 0);
        assert!(conversations["inbox"][0].starred);
    }

    #[test]
    fn unreadable_journals_reject_updates_without_overwriting_source_data() {
        let directory = std::env::temp_dir().join(format!(
            "pending-mail-action-test-{}",
            glib::uuid_string_random()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let moves_path = directory.join(PENDING_MAIL_ACTIONS_FILE);
        let flags_path = directory.join(PENDING_MESSAGE_FLAGS_FILE);
        std::fs::write(&moves_path, "not valid move JSON").unwrap();
        std::fs::write(&flags_path, "not valid flag JSON").unwrap();

        let store = PendingMailActionStore::open(moves_path.clone());

        assert!(
            store
                .queue_move(pending("account-1", "m1", "archive"))
                .is_err()
        );
        assert!(
            store
                .queue_read(
                    MailAccountId("account-1".into()),
                    ConversationId("m1".into()),
                    true,
                )
                .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(&moves_path).unwrap(),
            "not valid move JSON"
        );
        assert_eq!(
            std::fs::read_to_string(&flags_path).unwrap(),
            "not valid flag JSON"
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn cloned_stores_serialize_concurrent_account_updates_without_losing_intent() {
        let directory = std::env::temp_dir().join(format!(
            "pending-mail-action-test-{}",
            glib::uuid_string_random()
        ));
        let moves_path = directory.join(PENDING_MAIL_ACTIONS_FILE);
        let store = PendingMailActionStore::open(moves_path.clone());
        let workers = (0..8)
            .map(|index| {
                let store = store.clone();
                std::thread::spawn(move || {
                    store.queue_move(pending(
                        &format!("account-{index}"),
                        &format!("message-{index}"),
                        if index % 2 == 0 { "archive" } else { "trash" },
                    ))
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().unwrap().unwrap();
        }

        let persisted: Vec<PendingMove> = crate::integration::json::load_vec(&moves_path).unwrap();
        assert_eq!(persisted.len(), 8);
        for index in 0..8 {
            assert!(
                persisted
                    .iter()
                    .any(|move_| move_.account_id.0 == format!("account-{index}"))
            );
        }

        std::fs::remove_dir_all(directory).unwrap();
    }
}
