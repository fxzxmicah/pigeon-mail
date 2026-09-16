use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use anyhow::anyhow;

use super::actions::MessageActionQueue;
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
    local_revision: u64,
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

    pub(super) fn begin_conversation_load(&self, folder_id: &FolderId) -> ConversationLoad {
        let cache = self.state.lock().expect("mailbox cache lock poisoned");
        match cache.conversations.get(folder_id) {
            Some(conversations) => ConversationLoad::Cached(conversations.clone()),
            None => ConversationLoad::Required {
                revision: cache.local_revision,
            },
        }
    }

    pub(super) fn publish_conversations(
        &self,
        route: &EdsAccountBinding,
        folder_id: &FolderId,
        expected_revision: u64,
        conversations: Vec<ConversationSummary>,
        actions: &MessageActionQueue,
    ) -> ConversationPublication {
        let mut cache = self.state.lock().expect("mailbox cache lock poisoned");
        if cache.local_revision != expected_revision {
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
            .cloned()
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
        mark_local_change(&mut cache);
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
        mark_local_change(&mut cache);
    }

    pub(super) fn revision(&self) -> u64 {
        self.state
            .lock()
            .expect("mailbox cache lock poisoned")
            .local_revision
    }

    pub(super) fn publish_remote(
        &self,
        route: &EdsAccountBinding,
        expected_local_revision: u64,
        folders: Vec<MailFolder>,
        mut conversations: HashMap<FolderId, Vec<ConversationSummary>>,
        actions: &MessageActionQueue,
    ) -> RemotePublication {
        debug_assert!(conversation_projection_is_consistent(&conversations));
        let mut cache = self.state.lock().expect("mailbox cache lock poisoned");
        if cache.local_revision != expected_local_revision {
            return RemotePublication::Stale;
        }
        actions.apply_to_projection(route, &mut conversations);
        debug_assert!(conversation_projection_is_consistent(&conversations));
        cache.folders = Some(folders);
        cache.conversations = conversations;
        refresh_folder_counts(&mut cache);
        RemotePublication::Published
    }

    pub(super) fn invalidate_local(
        &self,
        mailbox: &LocalMailbox<'_>,
        local_folders: &[LocalFolder],
    ) -> Vec<FolderId> {
        let mut cache = self.state.lock().expect("mailbox cache lock poisoned");
        cache.local_revision = cache.local_revision.wrapping_add(1);
        let MailboxCacheState {
            folders,
            conversations,
            ..
        } = &mut *cache;
        let Some(folders) = folders.as_mut() else {
            return Vec::new();
        };
        let mut invalidated = Vec::with_capacity(local_folders.len());
        for local_folder in local_folders {
            let folder_id = mailbox.ensure_projection_folder(folders, *local_folder);
            conversations.remove(&folder_id);
            if let Some(folder) = folders.iter_mut().find(|folder| folder.id == folder_id) {
                folder.unread_count = 0;
            }
            invalidated.push(folder_id);
        }
        invalidated.sort_by(|left, right| left.0.cmp(&right.0));
        invalidated.dedup();
        invalidated
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

fn mark_local_change(cache: &mut MailboxCacheState) {
    cache.local_revision = cache.local_revision.wrapping_add(1);
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
    use super::*;
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
    fn summary_change_and_folder_count_commit_together() {
        let cache = MailboxCache::default();
        let folder_id = FolderId("inbox".into());
        cache.install_folders_if_absent(vec![MailFolder {
            id: folder_id.clone(),
            name: "Inbox".into(),
            unread_count: 1,
            kind: FolderKind::Inbox,
        }]);
        cache.state.lock().unwrap().conversations.insert(
            folder_id.clone(),
            vec![summary("inbox", "message", 1)],
        );

        cache.record_message_change(&ConversationId("inbox\u{1f}message".into()), |summary| {
            summary.unread_count = 0
        });

        assert_eq!(cache.folders().unwrap()[0].unread_count, 0);
        assert_eq!(cache.revision(), 1);
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
    fn local_change_prevents_an_older_remote_snapshot_from_publishing() {
        let cache = MailboxCache::default();
        let revision = cache.revision();
        cache.state.lock().unwrap().local_revision = revision.wrapping_add(1);
        let actions = MessageActionQueue::new();

        assert_eq!(
            cache.publish_remote(
                &route(),
                revision,
                Vec::new(),
                HashMap::new(),
                &actions,
            ),
            RemotePublication::Stale
        );
    }

    #[test]
    fn local_change_prevents_an_older_lazy_folder_read_from_publishing() {
        let cache = MailboxCache::default();
        let folder_id = FolderId("drafts".into());
        let revision = match cache.begin_conversation_load(&folder_id) {
            ConversationLoad::Required { revision } => revision,
            ConversationLoad::Cached(_) => panic!("an empty cache must require a load"),
        };
        cache.record_message_change(&ConversationId("drafts\u{1f}new".into()), |_| {});

        assert!(matches!(
            cache.publish_conversations(
                &route(),
                &folder_id,
                revision,
                vec![summary("drafts", "old", 1)],
                &MessageActionQueue::new(),
            ),
            ConversationPublication::Stale
        ));
        assert!(cache.conversations(&folder_id).is_none());
    }

    #[test]
    fn local_projection_invalidation_removes_rows_and_derived_count() {
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
        let invalidated = cache.invalidate_local(&mailbox, &[LocalFolder::Drafts]);

        assert_eq!(invalidated, vec![FolderId("drafts".into())]);
        assert!(cache.conversations(&FolderId("drafts".into())).is_none());
        assert_eq!(cache.folders().unwrap()[0].unread_count, 0);
        assert_eq!(cache.revision(), 1);
    }

    #[test]
    fn local_projection_invalidation_adds_a_newly_nonempty_outbox() {
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
        let local_folders = [LocalFolder::Drafts, LocalFolder::Outbox];
        let invalidated = cache.invalidate_local(&mailbox, &local_folders);

        assert_eq!(invalidated.len(), 2);
        assert!(cache.conversations(&FolderId("drafts".into())).is_none());
        assert_eq!(cache.folders().unwrap().len(), 2);
        assert!(cache.conversations(&mailbox.folder_id(LocalFolder::Outbox)).is_none());
        assert_eq!(cache.revision(), 1);
    }
}
