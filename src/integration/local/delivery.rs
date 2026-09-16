use std::collections::{HashMap, HashSet};
use std::num::NonZeroU64;

use anyhow::anyhow;

use crate::integration::camel::{
    DELIVERY_COPY_REQUIRED, DELIVERY_PROVIDER_COPY, DELIVERY_SUBMITTING, MessageSyncState,
};
use crate::model::mail::ConversationSummary;

use super::message_states_by_id;

pub(crate) struct OutboxSubmission {
    pub(super) outbox: Vec<ConversationSummary>,
    pub(super) delivered_message_id_hashes: HashSet<NonZeroU64>,
}

pub(crate) struct DeliverySynchronization {
    pub(crate) outbox: Vec<ConversationSummary>,
    pub(crate) sent: Vec<ConversationSummary>,
    pub(crate) delivered_message_id_hashes: HashSet<NonZeroU64>,
    pub(crate) remote_changed: bool,
    pub(crate) convergence_incomplete: bool,
}

#[derive(Default)]
pub(super) struct RemoteSentEvidence {
    pub(super) unclaimed: HashMap<NonZeroU64, usize>,
}

impl RemoteSentEvidence {
    pub(super) fn from_states(states: Vec<MessageSyncState>) -> Self {
        let unclaimed = states
            .into_iter()
            .filter_map(|state| NonZeroU64::new(state.message_id_hash))
            .fold(HashMap::new(), |mut counts, message_id_hash| {
                *counts.entry(message_id_hash).or_insert(0) += 1;
                counts
            });
        Self { unclaimed }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DeliveryPlacement {
    Outbox,
    Held,
    Sent,
    Retire,
}

pub(super) struct DeliveryRecord {
    pub(super) summary: ConversationSummary,
    pub(super) message_id_hash: NonZeroU64,
    pub(super) state: DeliveryState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DeliveryState {
    Queued,
    SubmissionUncertain,
    ProviderCopyExpected,
    NeedsRemoteCopy,
}

impl DeliveryState {
    pub(super) fn from_marker(marker: Option<&str>, unmarked: Self) -> anyhow::Result<Self> {
        match marker {
            None => Ok(unmarked),
            Some(DELIVERY_SUBMITTING) => Ok(Self::SubmissionUncertain),
            Some(DELIVERY_PROVIDER_COPY) => Ok(Self::ProviderCopyExpected),
            Some(DELIVERY_COPY_REQUIRED) => Ok(Self::NeedsRemoteCopy),
            Some(marker) => Err(anyhow!("unknown local delivery state '{marker}'")),
        }
    }

    pub(super) fn belongs_in_outbox(self) -> bool {
        matches!(self, Self::Queued | Self::SubmissionUncertain)
    }
}

pub(super) struct CachedDeliveryProjection {
    pub(super) outbox: Vec<ConversationSummary>,
    pub(super) sent: Vec<ConversationSummary>,
}

pub(super) fn classify_delivery(
    state: DeliveryState,
    message_id_hash: NonZeroU64,
    unclaimed_remote_sent: &mut HashMap<NonZeroU64, usize>,
) -> DeliveryPlacement {
    if state != DeliveryState::Queued
        && let Some(count) = unclaimed_remote_sent
            .get_mut(&message_id_hash)
            .filter(|count| **count > 0)
    {
        *count -= 1;
        DeliveryPlacement::Retire
    } else if state == DeliveryState::Queued {
        DeliveryPlacement::Outbox
    } else if state == DeliveryState::SubmissionUncertain {
        DeliveryPlacement::Held
    } else {
        DeliveryPlacement::Sent
    }
}

pub(super) fn outbox_delivery_records(
    summaries: Vec<ConversationSummary>,
    states: Vec<MessageSyncState>,
) -> anyhow::Result<Vec<DeliveryRecord>> {
    delivery_records(summaries, states, DeliveryState::Queued)
}

pub(super) fn sent_delivery_records(
    summaries: Vec<ConversationSummary>,
    states: Vec<MessageSyncState>,
) -> anyhow::Result<Vec<DeliveryRecord>> {
    delivery_records(summaries, states, DeliveryState::NeedsRemoteCopy)
}

fn delivery_records(
    summaries: Vec<ConversationSummary>,
    states: Vec<MessageSyncState>,
    unmarked: DeliveryState,
) -> anyhow::Result<Vec<DeliveryRecord>> {
    let mut states_by_id = message_states_by_id(states)?;

    let mut records = Vec::with_capacity(summaries.len());
    for summary in summaries {
        let state = states_by_id.remove(&summary.id).ok_or_else(|| {
            anyhow!(
                "local delivery '{}' has no Camel synchronization state",
                summary.id.0.as_str()
            )
        })?;
        let (delivery_state, message_id_hash) = delivery_metadata(&state, unmarked)?;
        records.push(DeliveryRecord {
            summary,
            message_id_hash,
            state: delivery_state,
        });
    }
    if let Some(conversation_id) = states_by_id.keys().next() {
        return Err(anyhow!(
            "local delivery synchronization state '{}' has no message summary",
            conversation_id.0
        ));
    }
    Ok(records)
}

pub(super) fn outbox_delivery_metadata(
    state: &MessageSyncState,
) -> anyhow::Result<(DeliveryState, NonZeroU64)> {
    delivery_metadata(state, DeliveryState::Queued)
}

pub(super) fn sent_delivery_metadata(
    state: &MessageSyncState,
) -> anyhow::Result<(DeliveryState, NonZeroU64)> {
    delivery_metadata(state, DeliveryState::NeedsRemoteCopy)
}

fn delivery_metadata(
    state: &MessageSyncState,
    unmarked: DeliveryState,
) -> anyhow::Result<(DeliveryState, NonZeroU64)> {
    if state.draft_synced || state.draft_superseded {
        return Err(anyhow!(
            "local delivery '{}' contains draft synchronization state",
            state.conversation_id.0
        ));
    }
    let delivery_state = DeliveryState::from_marker(state.delivery_state.as_deref(), unmarked)
        .map_err(|error| anyhow!("local delivery '{}': {error}", state.conversation_id.0))?;
    let message_id_hash = NonZeroU64::new(state.message_id_hash).ok_or_else(|| {
        anyhow!(
            "local delivery '{}' has no Message-ID hash",
            state.conversation_id.0
        )
    })?;
    Ok((delivery_state, message_id_hash))
}
