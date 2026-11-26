use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use anyhow::anyhow;

use super::{actions::MessageActionQueue, slice_conversations};
use crate::integration::account::EdsAccountBinding;
use crate::integration::local::{LocalFolder, LocalMailbox};
use crate::model::mail::{ConversationId, ConversationSummary, FolderId, MailFolder};

#[derive(Clone, Default)]
pub(super) struct MailboxCache {
    state: Arc<Mutex<MailboxCacheState>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RemotePublication {
    Published,
    Stale,
}

pub(super) enum ConversationLoad {
    Cached(Vec<ConversationSummary>),
    Required { revision: u64 },
}

pub(super) enum ConversationPublication {
    Published(Vec<ConversationSummary>),
    Stale,
}

#[derive(Default)]
struct MailboxCacheState {
    folders: Option<Vec<MailFolder>>,
    conversations: HashMap<FolderId, Vec<ConversationSummary>>,
    revision: u64,
}

impl MailboxCache {
    pub(super) fn folders(&self) -> Option<Vec<MailFolder>> {
        self.state
            .lock()
            .expect("mailbox cache lock poisoned")
            .folders
            .clone()
    }

    pub(super) fn install_folders_if_absent(
        &self,
        folders: Vec<MailFolder>,
    ) -> Vec<MailFolder> {
        let mut cache = self.state.lock().expect("mailbox cache lock poisoned");
        cache.folders.get_or_insert(folders).clone()
    }

    #[cfg(test)]
    pub(super) fn conversations(
        &self,
        folder_id: &FolderId,
    ) -> Option<Vec<ConversationSummary>> {
        self.state
            .lock()
            .expect("mailbox cache lock poisoned")
            .conversations
            .get(folder_id)
            .cloned()
    }

    pub(super) fn begin_conversation_load(
        &self,
        folder_id: &FolderId,
        offset: usize,
        limit: usize,
    ) -> ConversationLoad {
        let cache = self.state.lock().expect("mailbox cache lock poisoned");
        match cache.conversations.get(folder_id) {
            Some(conversations) => {
                ConversationLoad::Cached(slice_conversations(conversations, offset, limit))
            }
            None => {
                ConversationLoad::Required {
                    revision: cache.revision,
                }
            }
        }
    }

    pub(super) fn publish_conversations(
        &self,
        route: &EdsAccountBinding,
        folder_id: &FolderId,
        expected_revision: u64,
        conversations: Vec<ConversationSummary>,
        actions: &MessageActionQueue,
        page: (usize, usize),
    ) -> ConversationPublication {
        let mut cache = self.state.lock().expect("mailbox cache lock poisoned");
        if cache.revision != expected_revision {
            return ConversationPublication::Stale;
        }
        cache
            .conversations
            .entry(folder_id.clone())
            .or_insert(conversations);
        actions.apply_to_projection(route, &mut cache.conversations);
        debug_assert!(conversation_projection_is_consistent(&cache.conversations));
        refresh_folder_counts(&mut cache);
        cache
            .conversations
            .get(folder_id)
            .map(|rows| slice_conversations(rows, page.0, page.1))
            .map(ConversationPublication::Published)
            .expect("published conversation folder remains cached")
    }

    pub(super) fn matching_conversations(
        &self,
        predicate: impl Fn(&ConversationSummary) -> bool,
    ) -> Vec<ConversationSummary> {
        self.state
            .lock()
            .expect("mailbox cache lock poisoned")
            .conversations
            .values()
            .flatten()
            .filter(|summary| predicate(summary))
            .cloned()
            .collect()
    }

    pub(super) fn apply_action_projection(
        &self,
        route: &EdsAccountBinding,
        actions: &MessageActionQueue,
    ) {
        let mut cache = self.state.lock().expect("mailbox cache lock poisoned");
        if !actions.apply_to_projection(route, &mut cache.conversations) {
            return;
        }
        debug_assert!(conversation_projection_is_consistent(&cache.conversations));
        record_projection_change(&mut cache);
    }

    pub(super) fn record_message_change(
        &self,
        conversation_id: &ConversationId,
        update: impl Fn(&mut ConversationSummary),
    ) {
        let mut cache = self.state.lock().expect("mailbox cache lock poisoned");
        debug_assert!(conversation_projection_is_consistent(&cache.conversations));
        let summary = cache
            .conversations
            .values_mut()
            .find_map(|summaries| {
                summaries
                    .iter_mut()
                    .find(|summary| summary.id == *conversation_id)
            });
        if let Some(summary) = summary {
            update(summary);
        }
        record_projection_change(&mut cache);
    }

    pub(super) fn revision(&self) -> u64 {
        self.state
            .lock()
            .expect("mailbox cache lock poisoned")
            .revision
    }

    pub(super) fn publish_remote(
        &self,
        route: &EdsAccountBinding,
        expected_revision: u64,
        folders: Vec<MailFolder>,
        mut conversations: HashMap<FolderId, Vec<ConversationSummary>>,
        actions: &MessageActionQueue,
    ) -> RemotePublication {
        debug_assert!(conversation_projection_is_consistent(&conversations));
        let mut cache = self.state.lock().expect("mailbox cache lock poisoned");
        if cache.revision != expected_revision {
            return RemotePublication::Stale;
        }
        actions.apply_to_projection(route, &mut conversations);
        debug_assert!(conversation_projection_is_consistent(&conversations));
        cache.folders = Some(folders);
        cache.conversations = conversations;
        record_projection_change(&mut cache);
        RemotePublication::Published
    }

    pub(super) fn publish_local(
        &self,
        mailbox: &LocalMailbox<'_>,
        expected_revision: u64,
        projections: Vec<(LocalFolder, FolderId, Vec<ConversationSummary>)>,
        actions: &MessageActionQueue,
    ) -> bool {
        let mut cache = self.state.lock().expect("mailbox cache lock poisoned");
        if cache.revision != expected_revision {
            return false;
        }
        let MailboxCacheState {
            folders,
            conversations,
            ..
        } = &mut *cache;
        let folders = folders.as_mut().expect("folder projection has been loaded");
        for (role, folder_id, rows) in projections {
            if role == LocalFolder::Outbox
                && folder_id == mailbox.folder_id(role)
                && rows.is_empty()
            {
                folders.retain(|folder| folder.id != folder_id);
                conversations.remove(&folder_id);
            } else {
                mailbox.ensure_projection_folder(folders, role);
                conversations.insert(folder_id, rows);
            }
        }
        actions.apply_to_projection(mailbox.binding(), conversations);
        debug_assert!(conversation_projection_is_consistent(conversations));
        record_projection_change(&mut cache);
        true
    }

    pub(super) fn resolve_move(
        &self,
        conversation_id: &ConversationId,
        folder_id: &FolderId,
    ) -> anyhow::Result<(FolderId, ConversationSummary)> {
        let cache = self.state.lock().expect("mailbox cache lock poisoned");
        debug_assert!(conversation_projection_is_consistent(&cache.conversations));
        let folders = cache
            .folders
            .as_deref()
            .ok_or_else(|| anyhow!("cannot resolve a move before folders are cached"))?;
        resolve_move_request(
            folders,
            &cache.conversations,
            conversation_id,
            folder_id,
        )
    }
}

fn record_projection_change(cache: &mut MailboxCacheState) {
    cache.revision = cache.revision.wrapping_add(1);
    refresh_folder_counts(cache);
}

fn conversation_projection_is_consistent(
    conversations: &HashMap<FolderId, Vec<ConversationSummary>>,
) -> bool {
    let mut ids = HashSet::new();
    conversations.iter().all(|(folder_id, summaries)| {
        summaries
            .iter()
            .all(|summary| summary.folder_id == *folder_id && ids.insert(&summary.id))
    })
}

fn refresh_folder_counts(cache: &mut MailboxCacheState) {
    let unread_counts = cache
        .conversations
        .iter()
        .map(|(folder_id, summaries)| {
            (
                folder_id.clone(),
                summaries.iter().map(|summary| summary.unread_count).sum(),
            )
        })
        .collect::<HashMap<_, _>>();
    let Some(folders) = cache.folders.as_mut() else {
        return;
    };
    for folder in folders {
        if let Some(unread_count) = unread_counts.get(&folder.id) {
            folder.unread_count = *unread_count;
        }
    }
}

pub(super) fn resolve_move_request(
    folders: &[MailFolder],
    conversations: &HashMap<FolderId, Vec<ConversationSummary>>,
    conversation_id: &ConversationId,
    folder_id: &FolderId,
) -> anyhow::Result<(FolderId, ConversationSummary)> {
    let destination_folder_id = folders
        .iter()
        .find(|folder| folder.id == *folder_id)
        .map(|folder| folder.id.clone())
        .ok_or_else(|| anyhow!("could not resolve destination folder '{}'", folder_id.0))?;
    let summary = conversations
        .values()
        .flatten()
        .find(|conversation| conversation.id == *conversation_id)
        .cloned()
        .ok_or_else(|| {
            anyhow!(
                "could not locate cached conversation '{}' before moving it",
                conversation_id.0
            )
        })?;
    Ok((destination_folder_id, summary))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use super::super::{AccountBackend, MailBackend};
    use crate::integration::account::RouteFingerprint;
    use crate::model::account::MailAccountId;
    use crate::model::mail::FolderKind;

    fn route() -> EdsAccountBinding {
        EdsAccountBinding {
            account_id: MailAccountId("account".into()),
            account_uid: "account-source".into(),
            account_parent_uid: "collection-source".into(),
            account_backend_name: "imapx".into(),
            account_auth_method: None,
            identity_uid: "identity-source".into(),
            transport_uid: "transport-source".into(),
            transport_backend_name: "smtp".into(),
            transport_auth_method: None,
            route_fingerprint: RouteFingerprint {
                account: 0,
                transport: 0,
                mailbox: 0,
            },
        }
    }

    fn summary(folder: &str, id: &str, unread_count: u32) -> ConversationSummary {
        ConversationSummary {
            id: ConversationId(format!("{folder}\u{1f}{id}")),
            folder_id: FolderId(folder.into()),
            subject: id.into(),
            participants: Vec::new(),
            message_count: 1,
            unread_count,
            attachment_count: 0,
            starred: false,
            last_updated_unix_ms: 0,
            preview: String::new(),
        }
    }

    #[test]
    fn late_remote_snapshot_cannot_restore_a_locally_cleared_unread_count() {
        let cache = MailboxCache::default();
        let binding = route();
        let actions = MessageActionQueue::new();
        let folder_id = FolderId("inbox".into());
        let folders = vec![MailFolder {
            id: folder_id.clone(),
            name: "Inbox".into(),
            unread_count: 1,
            kind: FolderKind::Inbox,
        }];
        let remote_rows = HashMap::from([(folder_id.clone(), vec![summary("inbox", "message", 1)])]);
        assert_eq!(
            cache.publish_remote(&binding, cache.revision(), folders.clone(), remote_rows.clone(), &actions),
            RemotePublication::Published,
        );
        let remote_revision = cache.revision();

        cache.record_message_change(&ConversationId("inbox\u{1f}message".into()), |summary| {
            summary.unread_count = 0
        });

        assert_eq!(
            cache.publish_remote(&binding, remote_revision, folders, remote_rows, &actions),
            RemotePublication::Stale,
        );
        assert_eq!(cache.folders().unwrap()[0].unread_count, 0);
        assert_eq!(cache.conversations(&folder_id).unwrap()[0].unread_count, 0);
    }

    #[test]
    fn an_unloaded_message_change_still_invalidates_an_older_snapshot() {
        let cache = MailboxCache::default();
        let revision = cache.revision();

        cache.record_message_change(&ConversationId("drafts\u{1f}unloaded".into()), |_| {
            panic!("an unloaded summary must not be synthesized")
        });

        assert_eq!(cache.revision(), revision.wrapping_add(1));
        assert_eq!(
            cache.publish_remote(
                &route(),
                revision,
                Vec::new(),
                HashMap::new(),
                &MessageActionQueue::new(),
            ),
            RemotePublication::Stale
        );
    }

    #[test]
    fn local_change_prevents_an_older_lazy_folder_read_from_publishing() {
        let cache = MailboxCache::default();
        let folder_id = FolderId("drafts".into());
        let revision = match cache.begin_conversation_load(&folder_id, 0, 0) {
            ConversationLoad::Required { revision } => {
                revision
            }
            ConversationLoad::Cached(_) => {
                panic!("an empty cache must require a load")
            }
        };
        cache.record_message_change(&ConversationId("drafts\u{1f}new".into()), |_| {});

        assert!(matches!(
            cache.publish_conversations(
                &route(),
                &folder_id,
                revision,
                vec![summary("drafts", "old", 1)],
                &MessageActionQueue::new(),
                (0, 0),
            ),
            ConversationPublication::Stale
        ));
        assert!(cache.conversations(&folder_id).is_none());
    }

    #[test]
    fn a_cached_page_returns_only_the_requested_rows() {
        let cache = MailboxCache::default();
        let folder_id = FolderId("inbox".into());
        let rows = vec![
            summary("inbox", "first", 1),
            summary("inbox", "second", 0),
        ];
        cache
            .state
            .lock()
            .unwrap()
            .conversations
            .insert(folder_id.clone(), rows.clone());
        assert!(matches!(
            cache.begin_conversation_load(&folder_id, 1, 1),
            ConversationLoad::Cached(page) if page.as_slice() == &rows[1..]
        ));
        assert!(matches!(
            cache.begin_conversation_load(&folder_id, 2, 1),
            ConversationLoad::Cached(page) if page.is_empty()
        ));
    }

    #[test]
    fn local_projection_publishes_replacement_and_count_together() {
        let cache = MailboxCache::default();
        let folder_id = FolderId("drafts".into());
        cache.install_folders_if_absent(vec![MailFolder {
            id: folder_id.clone(),
            name: "Drafts".into(),
            unread_count: 1,
            kind: FolderKind::Drafts,
        }]);
        cache.state.lock().unwrap().conversations.insert(
            folder_id,
            vec![summary("drafts", "old", 1)],
        );

        let binding = route();
        let mailbox = LocalMailbox::new(&binding);
        let folder_id = FolderId("drafts".into());
        assert!(cache.publish_local(
            &mailbox,
            cache.revision(),
            vec![(LocalFolder::Drafts, folder_id.clone(), Vec::new())],
            &MessageActionQueue::new(),
        ));
        assert!(cache.conversations(&folder_id).unwrap().is_empty());
        assert_eq!(cache.folders().unwrap()[0].unread_count, 0);
        assert_eq!(cache.revision(), 1);
    }

    #[test]
    fn local_delivery_projection_moves_rows_and_rejects_older_publications() {
        let cache = MailboxCache::default();
        let binding = route();
        let mailbox = LocalMailbox::new(&binding);
        cache.install_folders_if_absent(vec![MailFolder {
            id: FolderId("drafts".into()),
            name: "Drafts".into(),
            unread_count: 1,
            kind: FolderKind::Drafts,
        }]);
        cache.state.lock().unwrap().conversations.insert(
            FolderId("drafts".into()), vec![summary("drafts", "old", 1)],
        );
        let outbox = mailbox.folder_id(LocalFolder::Outbox);
        let drafts = FolderId("drafts".into());
        let revision = cache.revision();
        let actions = MessageActionQueue::new();
        assert!(cache.publish_local(
            &mailbox,
            revision,
            vec![
                (LocalFolder::Drafts, drafts.clone(), Vec::new()),
                (LocalFolder::Outbox, outbox.clone(), vec![summary(&outbox.0, "queued", 0)]),
            ],
            &actions,
        ));
        assert!(cache.conversations(&drafts).unwrap().is_empty());
        assert_eq!(cache.folders().unwrap().len(), 2);
        assert_eq!(cache.conversations(&outbox).unwrap().len(), 1);
        assert!(!cache.publish_local(
            &mailbox,
            revision,
            vec![(LocalFolder::Outbox, outbox.clone(), Vec::new())],
            &actions,
        ));

        let sent = mailbox.folder_id(LocalFolder::Sent);
        assert!(cache.publish_local(
            &mailbox,
            cache.revision(),
            vec![
                (LocalFolder::Outbox, outbox.clone(), Vec::new()),
                (LocalFolder::Sent, sent.clone(), vec![summary(&sent.0, "sent", 0)]),
            ],
            &actions,
        ));
        assert!(cache.folders().unwrap().iter().all(|folder| folder.id != outbox));
        assert!(cache.conversations(&outbox).is_none());
        assert_eq!(cache.conversations(&sent).unwrap().len(), 1);
    }

    #[test]
    fn smtp_delivery_is_visible_even_when_the_imap_store_cannot_open() {
        let binding = route();
        let mailbox = LocalMailbox::new(&binding);
        let backend = AccountBackend::for_test(binding.clone());
        let cache = &backend.cached_mailbox;
        let outbox = mailbox.folder_id(LocalFolder::Outbox);
        let sent = mailbox.folder_id(LocalFolder::Sent);
        let delivered = summary(&sent.0, "delivered", 0);
        cache.install_folders_if_absent(Vec::new());
        assert!(cache.publish_local(
            &mailbox,
            cache.revision(),
            vec![
                (
                    LocalFolder::Outbox,
                    outbox.clone(),
                    vec![summary(&outbox.0, "queued", 0)],
                ),
                (LocalFolder::Sent, sent.clone(), Vec::new()),
            ],
            &backend.action_queue,
        ));
        assert_eq!(backend.list_conversations(&outbox, 0, 10).unwrap().len(), 1);
        assert!(backend.list_conversations(&sent, 0, 10).unwrap().is_empty());

        let submitted = Cell::new(false);
        let notified = Cell::new(false);
        let assert_visible_delivery = || {
            let folders = backend.list_folders().unwrap();
            assert!(folders.iter().all(|folder| folder.id != outbox));
            assert!(cache.conversations(&outbox).is_none());
            assert!(cache.conversations(&sent).is_some());
            assert_eq!(
                backend.list_conversations(&sent, 0, 10).unwrap(),
                vec![delivered.clone()],
            );
        };
        let result = backend.refresh_cache_with(
            &mailbox,
            &|| {
                assert!(submitted.get());
                assert_visible_delivery();
                assert!(!notified.replace(true));
            },
            |placement_changed| {
                submitted.set(true);
                placement_changed();
                Ok(HashSet::from([std::num::NonZeroU64::MIN]))
            },
            |id, role| {
                assert!(submitted.get());
                match role {
                    LocalFolder::Outbox => {
                        assert_eq!(id, &outbox);
                        Ok(Vec::new())
                    }
                    LocalFolder::Sent => {
                        assert_eq!(id, &sent);
                        Ok(vec![delivered.clone()])
                    }
                    LocalFolder::Drafts => {
                        panic!("submitting Outbox does not replace the Drafts projection");
                    }
                }
            },
            || {
                assert!(notified.get(), "delivery must be visible before opening IMAP");
                Err(std::io::Error::from(std::io::ErrorKind::ConnectionRefused).into())
            },
        );

        let error = result.expect_err("IMAP connection failure must propagate");
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::ConnectionRefused,
        );
        assert_visible_delivery();
    }
}
