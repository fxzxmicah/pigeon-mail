use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::integration::account::EdsAccountBinding;
use crate::model::mail::{
    ConversationId, ConversationSummary, FolderId, MessageDetail,
    sort_and_deduplicate_conversations,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MoveIntent {
    pub(crate) destination_folder_id: FolderId,
    pub(crate) summary: ConversationSummary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MessageFlagIntent {
    pub(crate) conversation_id: ConversationId,
    pub(crate) read: Option<bool>,
    pub(crate) starred: Option<bool>,
}

#[derive(Clone, Default)]
pub(crate) struct MessageActionQueue {
    state: Arc<Mutex<MessageActionQueueState>>,
}

#[derive(Default)]
struct MessageActionQueueState {
    routes: HashMap<EdsAccountBinding, RouteActions>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct MessageFlagValues {
    read: Option<bool>,
    starred: Option<bool>,
}

#[derive(Clone, Default)]
struct RouteActions {
    moves: HashMap<ConversationId, MoveIntent>,
    flags: HashMap<ConversationId, MessageFlagValues>,
}

impl RouteActions {
    fn len(&self) -> usize {
        self.moves.len() + self.flags.len()
    }

    fn is_empty(&self) -> bool {
        self.moves.is_empty() && self.flags.is_empty()
    }
}

impl MessageActionQueue {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn len(&self) -> usize {
        let state = self
            .state
            .lock()
            .expect("message action queue lock poisoned");
        state.routes.values().map(RouteActions::len).sum()
    }

    pub(crate) fn len_for_route(&self, route: &EdsAccountBinding) -> usize {
        self.state
            .lock()
            .expect("message action queue lock poisoned")
            .routes
            .get(route)
            .map_or(0, RouteActions::len)
    }

    pub(crate) fn moves_for_route(&self, route: &EdsAccountBinding) -> Vec<MoveIntent> {
        self.state
            .lock()
            .expect("message action queue lock poisoned")
            .routes
            .get(route)
            .map(|actions| actions.moves.values().cloned().collect())
            .unwrap_or_default()
    }

    pub(crate) fn queue_move(&self, route: EdsAccountBinding, intent: MoveIntent) {
        self.state
            .lock()
            .expect("message action queue lock poisoned")
            .routes
            .entry(route)
            .or_default()
            .moves
            .insert(intent.summary.id.clone(), intent);
    }

    pub(crate) fn flags_for_route(
        &self,
        route: &EdsAccountBinding,
    ) -> Vec<MessageFlagIntent> {
        self.state
            .lock()
            .expect("message action queue lock poisoned")
            .routes
            .get(route)
            .map(|actions| {
                actions
                    .flags
                    .iter()
                    .map(|(conversation_id, values)| MessageFlagIntent {
                        conversation_id: conversation_id.clone(),
                        read: values.read,
                        starred: values.starred,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn queue_read(
        &self,
        route: EdsAccountBinding,
        conversation_id: ConversationId,
        read: bool,
    ) {
        self.update_flags(route, conversation_id, |flags| flags.read = Some(read));
    }

    pub(crate) fn queue_starred(
        &self,
        route: EdsAccountBinding,
        conversation_id: ConversationId,
        starred: bool,
    ) {
        self.update_flags(route, conversation_id, |flags| {
            flags.starred = Some(starred)
        });
    }

    fn update_flags(
        &self,
        route: EdsAccountBinding,
        conversation_id: ConversationId,
        update: impl FnOnce(&mut MessageFlagValues),
    ) {
        let mut state = self
            .state
            .lock()
            .expect("message action queue lock poisoned");
        update(
            state
                .routes
                .entry(route)
                .or_default()
                .flags
                .entry(conversation_id)
                .or_default(),
        );
    }

    pub(crate) fn remove_completed_flags(
        &self,
        route: &EdsAccountBinding,
        completed: &[MessageFlagIntent],
    ) {
        let mut state = self
            .state
            .lock()
            .expect("message action queue lock poisoned");
        let Some(actions) = state.routes.get_mut(route) else {
            return;
        };
        for completed in completed {
            let Some(current) = actions.flags.get_mut(&completed.conversation_id) else {
                continue;
            };
            if completed.read.is_some() && current.read == completed.read {
                current.read = None;
            }
            if completed.starred.is_some() && current.starred == completed.starred {
                current.starred = None;
            }
            if current.read.is_none() && current.starred.is_none() {
                actions.flags.remove(&completed.conversation_id);
            }
        }
        if actions.is_empty() {
            state.routes.remove(route);
        }
    }

    pub(crate) fn remove_completed_moves(
        &self,
        route: &EdsAccountBinding,
        completed: &[MoveIntent],
    ) {
        let mut state = self
            .state
            .lock()
            .expect("message action queue lock poisoned");
        let Some(actions) = state.routes.get_mut(route) else {
            return;
        };
        for completed in completed {
            if actions.moves.get(&completed.summary.id) == Some(completed) {
                actions.moves.remove(&completed.summary.id);
            }
        }
        if actions.is_empty() {
            state.routes.remove(route);
        }
    }

    pub(crate) fn apply_to_projection(
        &self,
        route: &EdsAccountBinding,
        conversations: &mut HashMap<FolderId, Vec<ConversationSummary>>,
    ) -> bool {
        let snapshot = self.snapshot_for_route(route);
        let changed = !snapshot.moves.is_empty() || !snapshot.flags.is_empty();
        if !snapshot.moves.is_empty() {
            apply_move_overlay(&snapshot.moves, conversations);
        }
        for summaries in conversations.values_mut() {
            apply_flag_overlay(&snapshot.flags, summaries);
        }
        changed
    }

    pub(crate) fn apply_to_summaries(
        &self,
        route: &EdsAccountBinding,
        summaries: &mut [ConversationSummary],
    ) {
        let snapshot = self.snapshot_for_route(route);
        for summary in &mut *summaries {
            if let Some(intent) = snapshot.moves.get(&summary.id) {
                summary.folder_id = intent.destination_folder_id.clone();
            }
        }
        apply_flag_overlay(&snapshot.flags, summaries);
    }

    pub(crate) fn apply_flag_overlay_to_detail(
        &self,
        route: &EdsAccountBinding,
        detail: &mut MessageDetail,
    ) {
        let flags = self
            .state
            .lock()
            .expect("message action queue lock poisoned")
            .routes
            .get(route)
            .and_then(|actions| actions.flags.get(&detail.conversation_id))
            .copied();
        let Some(flags) = flags else {
            return;
        };
        if let Some(read) = flags.read {
            detail.unread = !read;
        }
        if let Some(starred) = flags.starred {
            detail.starred = starred;
        }
    }

    fn snapshot_for_route(&self, route: &EdsAccountBinding) -> RouteActions {
        let state = self
            .state
            .lock()
            .expect("message action queue lock poisoned");
        state.routes.get(route).cloned().unwrap_or_default()
    }
}

fn apply_flag_overlay(
    intents: &HashMap<ConversationId, MessageFlagValues>,
    summaries: &mut [ConversationSummary],
) {
    for summary in summaries {
        let Some(flags) = intents.get(&summary.id) else {
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

fn apply_move_overlay(
    intents: &HashMap<ConversationId, MoveIntent>,
    conversations: &mut HashMap<FolderId, Vec<ConversationSummary>>,
) {
    for intent in intents.values() {
        let summary = conversations
            .values_mut()
            .find_map(|summaries| {
                summaries
                    .iter()
                    .position(|summary| summary.id == intent.summary.id)
                    .map(|index| summaries.remove(index))
            });
        let Some(destination) = conversations.get_mut(&intent.destination_folder_id) else {
            continue;
        };
        let mut summary = summary.unwrap_or_else(|| intent.summary.clone());
        summary.folder_id = intent.destination_folder_id.clone();
        destination.push(summary);
        sort_and_deduplicate_conversations(destination);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integration::account::RouteFingerprint;
    use crate::model::account::MailAccountId;

    fn route(id: &str) -> EdsAccountBinding {
        EdsAccountBinding {
            account_id: MailAccountId(id.into()),
            account_uid: format!("{id}-account"),
            account_parent_uid: format!("{id}-collection"),
            account_backend_name: "imapx".into(),
            account_auth_method: None,
            identity_uid: format!("{id}-identity"),
            transport_uid: format!("{id}-transport"),
            transport_backend_name: "smtp".into(),
            transport_auth_method: None,
            route_fingerprint: RouteFingerprint {
                account: 0,
                transport: 0,
                mailbox: 0,
            },
        }
    }

    fn summary(id: &str, subject: &str, unread_count: u32) -> ConversationSummary {
        ConversationSummary {
            id: ConversationId(id.into()),
            folder_id: FolderId("inbox".into()),
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

    fn intent(id: &str, destination: &str) -> MoveIntent {
        MoveIntent {
            destination_folder_id: FolderId(destination.into()),
            summary: summary(id, id, 1),
        }
    }

    #[test]
    fn overlay_moves_one_cached_summary_to_destination() {
        let mut cache = HashMap::from([
            (FolderId("inbox".into()), vec![summary("m1", "one", 1)]),
            (FolderId("archive".into()), vec![summary("m2", "two", 0)]),
        ]);

        apply_move_overlay(
            &HashMap::from([(ConversationId("m1".into()), intent("m1", "archive"))]),
            &mut cache,
        );

        assert!(cache[&FolderId("inbox".into())].is_empty());
        let archived = &cache[&FolderId("archive".into())];
        assert_eq!(archived.len(), 2);
        assert_eq!(archived.iter().filter(|item| item.id.0 == "m1").count(), 1);
        assert_eq!(
            archived
                .iter()
                .find(|item| item.id.0 == "m1")
                .unwrap()
                .folder_id,
            FolderId("archive".into())
        );
    }

    #[test]
    fn overlay_is_idempotent() {
        let mut cache = HashMap::from([
            (FolderId("inbox".into()), vec![summary("m1", "one", 1)]),
            (FolderId("archive".into()), Vec::new()),
        ]);
        let moves = HashMap::from([(ConversationId("m1".into()), intent("m1", "archive"))]);

        apply_move_overlay(&moves, &mut cache);
        apply_move_overlay(&moves, &mut cache);

        assert_eq!(cache[&FolderId("archive".into())].len(), 1);
        assert_eq!(cache[&FolderId("archive".into())][0].id.0, "m1");
    }

    #[test]
    fn repeated_move_projection_preserves_current_message_metadata() {
        let route = route("account-1");
        let queue = MessageActionQueue::new();
        queue.queue_move(route.clone(), intent("m1", "archive"));
        let mut cache = HashMap::from([
            (FolderId("inbox".into()), vec![summary("m1", "updated", 0)]),
            (FolderId("archive".into()), Vec::new()),
        ]);
        cache.get_mut(&FolderId("inbox".into())).unwrap()[0].starred = true;

        queue.apply_to_projection(&route, &mut cache);
        let archived = &cache[&FolderId("archive".into())][0];
        assert_eq!(archived.subject, "updated");
        assert_eq!(archived.unread_count, 0);
        assert!(archived.starred);

        cache.get_mut(&FolderId("archive".into())).unwrap()[0].preview = "new preview".into();
        queue.apply_to_projection(&route, &mut cache);
        let archived = &cache[&FolderId("archive".into())];
        assert_eq!(archived.len(), 1);
        assert_eq!(archived[0].subject, "updated");
        assert_eq!(archived[0].preview, "new preview");
        assert_eq!(archived[0].unread_count, 0);
        assert!(archived[0].starred);
    }

    #[test]
    fn move_projection_uses_retained_metadata_when_the_source_is_not_loaded() {
        let mut cache = HashMap::from([(FolderId("archive".into()), Vec::new())]);
        let moves = HashMap::from([(ConversationId("m1".into()), intent("m1", "archive"))]);

        apply_move_overlay(&moves, &mut cache);

        let archived = &cache[&FolderId("archive".into())];
        assert_eq!(archived.len(), 1);
        assert_eq!(archived[0].id, ConversationId("m1".into()));
        assert_eq!(archived[0].folder_id, FolderId("archive".into()));
    }

    #[test]
    fn flat_projection_applies_every_pending_change_to_search_results() {
        let route = route("account-1");
        let store = MessageActionQueue::new();
        store.queue_move(route.clone(), intent("m1", "archive"));
        store.queue_read(route.clone(), ConversationId("m1".into()), true);
        store.queue_starred(route.clone(), ConversationId("m1".into()), true);
        let mut matches = vec![summary("m1", "one", 1)];

        store.apply_to_summaries(&route, &mut matches);

        assert_eq!(matches[0].folder_id, FolderId("archive".into()));
        assert_eq!(matches[0].unread_count, 0);
        assert!(matches[0].starred);
    }

    #[test]
    fn mailbox_projection_applies_one_route_snapshot_across_moves_and_flags() {
        let route = route("account-1");
        let store = MessageActionQueue::new();
        store.queue_move(route.clone(), intent("m1", "archive"));
        store.queue_read(route.clone(), ConversationId("m1".into()), true);
        let mut cache = HashMap::from([
            (FolderId("inbox".into()), vec![summary("m1", "one", 1)]),
            (FolderId("archive".into()), Vec::new()),
        ]);

        assert!(store.apply_to_projection(&route, &mut cache));

        assert!(cache[&FolderId("inbox".into())].is_empty());
        assert_eq!(cache[&FolderId("archive".into())].len(), 1);
        assert_eq!(cache[&FolderId("archive".into())][0].unread_count, 0);
    }

    #[test]
    fn overlay_does_not_make_an_unloaded_destination_look_complete() {
        let mut cache = HashMap::from([(
            FolderId("inbox".into()),
            vec![summary("m1", "one", 1)],
        )]);

        apply_move_overlay(
            &HashMap::from([(ConversationId("m1".into()), intent("m1", "archive"))]),
            &mut cache,
        );

        assert!(cache[&FolderId("inbox".into())].is_empty());
        assert!(!cache.contains_key(&FolderId("archive".into())));
    }

    #[test]
    fn newer_move_replaces_the_destination() {
        let store = MessageActionQueue::new();
        let route = route("account-1");
        store.queue_move(route.clone(), intent("m1", "archive"));
        store.queue_move(route.clone(), intent("m1", "trash"));

        let moves = store.moves_for_route(&route);
        assert_eq!(moves.len(), 1);
        assert_eq!(moves[0].destination_folder_id.0, "trash");
    }

    #[test]
    fn old_refresh_acknowledgement_cannot_remove_a_newer_move() {
        let store = MessageActionQueue::new();
        let route = route("account-1");

        store.queue_move(route.clone(), intent("message", "archive"));
        let replayed = store.moves_for_route(&route);
        store.queue_move(route.clone(), intent("message", "trash"));
        store.remove_completed_moves(&route, &replayed);

        let retained = store.moves_for_route(&route);
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].destination_folder_id.0, "trash");
        store.remove_completed_moves(&route, &retained);
        assert!(store.moves_for_route(&route).is_empty());
    }

    #[test]
    fn newer_flag_values_merge_without_losing_the_other_flag() {
        let route = route("account-1");
        let conversation = ConversationId("inbox\u{1f}uid".into());
        let store = MessageActionQueue::new();
        store.queue_read(route.clone(), conversation.clone(), true);
        store.queue_starred(route.clone(), conversation.clone(), true);
        store.queue_read(route.clone(), conversation, false);

        let flags = store.flags_for_route(&route);
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].read, Some(false));
        assert_eq!(flags[0].starred, Some(true));
    }

    #[test]
    fn old_refresh_acknowledgement_cannot_remove_newer_flag_intent() {
        let store = MessageActionQueue::new();
        let route = route("account-1");
        let conversation = ConversationId("inbox\u{1f}uid".into());

        store.queue_read(route.clone(), conversation.clone(), true);
        let replayed = store.flags_for_route(&route);
        store.queue_read(route.clone(), conversation, false);
        store.remove_completed_flags(&route, &replayed);

        let retained = store.flags_for_route(&route);
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].read, Some(false));
        store.remove_completed_flags(&route, &retained);
        assert!(store.flags_for_route(&route).is_empty());
    }

    #[test]
    fn acknowledgement_clears_observed_fields_without_replaying_a_new_independent_flag() {
        let store = MessageActionQueue::new();
        let route = route("account-1");
        let conversation = ConversationId("inbox\u{1f}uid".into());

        store.queue_read(route.clone(), conversation.clone(), true);
        let replayed = store.flags_for_route(&route);
        store.queue_starred(route.clone(), conversation, true);
        store.remove_completed_flags(&route, &replayed);

        let retained = store.flags_for_route(&route);
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].read, None);
        assert_eq!(retained[0].starred, Some(true));
    }

    #[test]
    fn pending_flags_overlay_remote_summaries() {
        let id = ConversationId("inbox\u{1f}uid".into());
        let intents = HashMap::from([(id.clone(), MessageFlagValues {
            read: Some(true),
            starred: Some(true),
        })]);
        let mut conversations = HashMap::from([(
            FolderId("inbox".into()),
            vec![summary(&id.0, "one", 1)],
        )]);

        for summaries in conversations.values_mut() {
            apply_flag_overlay(&intents, summaries);
        }

        assert_eq!(conversations[&FolderId("inbox".into())][0].unread_count, 0);
        assert!(conversations[&FolderId("inbox".into())][0].starred);
    }

    #[test]
    fn cloned_queues_synchronize_concurrent_account_updates_without_losing_intent() {
        let queue = MessageActionQueue::new();
        let workers = (0..8)
            .map(|index| {
                let queue = queue.clone();
                std::thread::spawn(move || {
                    queue.queue_move(
                        route(&format!("account-{index}")),
                        intent(
                            &format!("message-{index}"),
                            if index % 2 == 0 { "archive" } else { "trash" },
                        ),
                    )
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().unwrap();
        }

        assert_eq!(queue.len(), 8);
        for index in 0..8 {
            let route = route(&format!("account-{index}"));
            assert_eq!(queue.moves_for_route(&route).len(), 1);
        }
    }

    #[test]
    fn changed_route_cannot_observe_or_acknowledge_an_older_routes_actions() {
        let store = MessageActionQueue::new();
        let old_route = route("account-1");
        let mut new_route = old_route.clone();
        new_route.route_fingerprint.transport = 1;
        store.queue_move(old_route.clone(), intent("message", "archive"));
        store.queue_read(
            new_route.clone(),
            ConversationId("message".into()),
            true,
        );

        let old_moves = store.moves_for_route(&old_route);
        assert_eq!(old_moves.len(), 1);
        assert!(store.moves_for_route(&new_route).is_empty());
        assert!(store.flags_for_route(&old_route).is_empty());
        assert_eq!(store.flags_for_route(&new_route).len(), 1);
        assert_eq!(store.len_for_route(&old_route), 1);
        assert_eq!(store.len_for_route(&new_route), 1);

        store.remove_completed_moves(&new_route, &old_moves);
        assert_eq!(store.moves_for_route(&old_route), old_moves);
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn acknowledging_one_action_kind_preserves_the_routes_other_intents() {
        let store = MessageActionQueue::new();
        let route = route("account-1");
        store.queue_move(route.clone(), intent("message", "archive"));
        store.queue_read(
            route.clone(),
            ConversationId("message".into()),
            true,
        );

        let moves = store.moves_for_route(&route);
        store.remove_completed_moves(&route, &moves);

        assert!(store.moves_for_route(&route).is_empty());
        assert_eq!(store.flags_for_route(&route).len(), 1);
        assert_eq!(store.len(), 1);
    }
}
