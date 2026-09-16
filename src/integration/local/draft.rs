use std::collections::{HashMap, HashSet};
use std::num::NonZeroU64;

use anyhow::anyhow;

use crate::integration::camel::MessageSyncState;
use crate::model::mail::{ConversationId, ConversationSummary};

use super::message_states_by_id;

pub(super) struct DraftRevision {
    pub(super) conversation_id: ConversationId,
    pub(super) message_id_hash: NonZeroU64,
    pub(super) synced: bool,
    pub(super) recover_superseded_marker: bool,
    pub(super) obsolete: Vec<ConversationId>,
}

pub(super) struct DraftSet {
    pub(super) revisions: Vec<DraftRevision>,
    current_ids: HashSet<ConversationId>,
    all_ids: HashSet<ConversationId>,
}

pub(super) struct DraftReplay {
    pub(super) pending: Vec<DraftRevision>,
    pub(super) delivered: Vec<ConversationId>,
}

impl DraftSet {
    pub(super) fn from_states(states: Vec<MessageSyncState>) -> anyhow::Result<Self> {
        let mut by_message_id = HashMap::<NonZeroU64, Vec<MessageSyncState>>::new();
        for state in message_states_by_id(states)?.into_values() {
            if state.delivery_state.is_some() {
                return Err(anyhow!(
                    "local draft '{}' contains delivery synchronization state",
                    state.conversation_id.0
                ));
            }
            let message_id_hash = NonZeroU64::new(state.message_id_hash).ok_or_else(|| {
                anyhow!(
                    "local draft '{}' has no Message-ID hash",
                    state.conversation_id.0
                )
            })?;
            by_message_id
                .entry(message_id_hash)
                .or_default()
                .push(state);
        }

        let revisions = by_message_id
            .into_iter()
            .map(|(message_id_hash, states)| {
                let (mut current, mut superseded): (Vec<_>, Vec<_>) = states
                    .into_iter()
                    .partition(|state| !state.draft_superseded);
                let recover_superseded_marker = current.is_empty() && superseded.len() == 1;
                let current = if current.len() == 1 {
                    current.pop().expect("length was checked")
                } else if recover_superseded_marker {
                    superseded.pop().expect("length was checked")
                } else {
                    return Err(anyhow!(
                        "local draft Message-ID hash {} has no unique current revision",
                        message_id_hash
                    ));
                };
                let obsolete = superseded
                    .into_iter()
                    .map(|state| state.conversation_id)
                    .collect();
                Ok(DraftRevision {
                    conversation_id: current.conversation_id,
                    message_id_hash,
                    synced: current.draft_synced,
                    recover_superseded_marker,
                    obsolete,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let current_ids = revisions
            .iter()
            .map(|revision| revision.conversation_id.clone())
            .collect::<HashSet<_>>();
        let all_ids = revisions
            .iter()
            .flat_map(|revision| {
                std::iter::once(revision.conversation_id.clone())
                    .chain(revision.obsolete.iter().cloned())
            })
            .collect();
        Ok(Self {
            revisions,
            current_ids,
            all_ids,
        })
    }

    pub(super) fn project_complete(
        &self,
        summaries: Vec<ConversationSummary>,
    ) -> anyhow::Result<Vec<ConversationSummary>> {
        let mut summary_ids = HashSet::with_capacity(summaries.len());
        for summary in &summaries {
            if !summary_ids.insert(summary.id.clone()) {
                return Err(anyhow!(
                    "local draft '{}' has duplicate message summaries",
                    summary.id.0
                ));
            }
        }
        if let Some(conversation_id) = self.all_ids.difference(&summary_ids).next() {
            return Err(anyhow!(
                "local draft synchronization state '{}' has no message summary",
                conversation_id.0
            ));
        }
        if let Some(conversation_id) = summary_ids.difference(&self.all_ids).next() {
            return Err(anyhow!(
                "local draft '{}' has no Camel synchronization state",
                conversation_id.0
            ));
        }
        Ok(summaries
            .into_iter()
            .filter(|summary| self.current_ids.contains(&summary.id))
            .collect())
    }

    pub(super) fn project_matches(
        &self,
        summaries: Vec<ConversationSummary>,
    ) -> anyhow::Result<Vec<ConversationSummary>> {
        if let Some(summary) = summaries
            .iter()
            .find(|summary| !self.all_ids.contains(&summary.id))
        {
            return Err(anyhow!(
                "local draft search result '{}' has no Camel synchronization state",
                summary.id.0
            ));
        }
        Ok(summaries
            .into_iter()
            .filter(|summary| self.current_ids.contains(&summary.id))
            .collect())
    }

    pub(super) fn into_replay(
        self,
        delivered_message_id_hashes: &HashSet<NonZeroU64>,
    ) -> DraftReplay {
        let mut pending = Vec::new();
        let mut delivered = Vec::new();
        for revision in self.revisions {
            if delivered_message_id_hashes.contains(&revision.message_id_hash) {
                delivered.extend(revision.obsolete);
                delivered.push(revision.conversation_id);
            } else {
                pending.push(revision);
            }
        }
        DraftReplay { pending, delivered }
    }
}
