use std::collections::{HashMap, HashSet};
use std::num::NonZeroU64;

use anyhow::anyhow;

use crate::integration::camel::{
    AccountSession, DELIVERY_COPY_REQUIRED, DELIVERY_PROVIDER_COPY, DELIVERY_SUBMITTING,
    MessageSyncState, TransportSession,
};
use crate::model::mail::{ConversationId, ConversationSummary, FolderId, MailFolder};

use super::{LocalFolder, LocalMailbox, message_states_by_id};

pub(crate) struct DeliverySynchronization {
    pub(crate) outbox: Vec<ConversationSummary>,
    pub(crate) sent: Vec<ConversationSummary>,
    pub(crate) delivered_message_id_hashes: HashSet<NonZeroU64>,
    pub(crate) convergence_incomplete: bool,
}

#[derive(Default)]
struct RemoteSentEvidence {
    unclaimed: HashMap<NonZeroU64, usize>,
}

impl RemoteSentEvidence {
    fn from_states(states: Vec<MessageSyncState>) -> Self {
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
enum DeliveryPlacement {
    Outbox,
    Held,
    Sent,
    Retire,
}

struct DeliveryRecord {
    summary: ConversationSummary,
    message_id_hash: NonZeroU64,
    state: DeliveryState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeliveryState {
    Queued,
    SubmissionUncertain,
    ProviderCopyExpected,
    NeedsRemoteCopy,
}

impl DeliveryState {
    fn from_marker(marker: Option<&str>, unmarked: Self) -> anyhow::Result<Self> {
        match marker {
            None => {
                Ok(unmarked)
            }
            Some(DELIVERY_SUBMITTING) => {
                Ok(Self::SubmissionUncertain)
            }
            Some(DELIVERY_PROVIDER_COPY) => {
                Ok(Self::ProviderCopyExpected)
            }
            Some(DELIVERY_COPY_REQUIRED) => {
                Ok(Self::NeedsRemoteCopy)
            }
            Some(marker) => {
                Err(anyhow!("unknown local delivery state '{marker}'"))
            }
        }
    }

    fn belongs_in_outbox(self) -> bool {
        matches!(self, Self::Queued | Self::SubmissionUncertain)
    }
}

pub(super) struct CachedDeliveryProjection {
    pub(super) outbox: Vec<ConversationSummary>,
    pub(super) sent: Vec<ConversationSummary>,
}

fn classify_delivery(
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

fn append_missing_sent_copies(
    outbox: &[DeliveryRecord],
    sent: &mut [DeliveryRecord],
    evidence: &RemoteSentEvidence,
    mut append_and_record: impl FnMut(&ConversationId) -> anyhow::Result<()>,
) -> anyhow::Result<usize> {
    let mut unclaimed = evidence.unclaimed.clone();
    for record in outbox {
        classify_delivery(record.state, record.message_id_hash, &mut unclaimed);
    }
    let mut appended = 0;
    for record in sent {
        let placement = classify_delivery(record.state, record.message_id_hash, &mut unclaimed);
        if placement == DeliveryPlacement::Sent && record.state == DeliveryState::NeedsRemoteCopy {
            append_and_record(&record.summary.id)?;
            record.state = DeliveryState::ProviderCopyExpected;
            appended += 1;
        }
    }
    Ok(appended)
}

fn outbox_delivery_records(
    summaries: Vec<ConversationSummary>,
    states: Vec<MessageSyncState>,
) -> anyhow::Result<Vec<DeliveryRecord>> {
    delivery_records(summaries, states, DeliveryState::Queued)
}

fn sent_delivery_records(
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

fn outbox_delivery_metadata(
    state: &MessageSyncState,
) -> anyhow::Result<(DeliveryState, NonZeroU64)> {
    delivery_metadata(state, DeliveryState::Queued)
}

fn sent_delivery_metadata(
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

impl LocalMailbox<'_> {
    pub(super) fn search_delivery(
        &self,
        session: &mut AccountSession,
        query: &str,
        visible_folders: &[MailFolder],
    ) -> anyhow::Result<Vec<ConversationSummary>> {
        let mut matches = Vec::new();
        let outbox_folder = self.folder_id(LocalFolder::Outbox);
        session.ensure_folder_path(&outbox_folder)?;
        let outbox_states = message_states_by_id(
            session.list_message_sync_states(&outbox_folder)?,
        )?;
        let visible_outbox_folder =
            self.projected_folder_id(visible_folders, LocalFolder::Outbox);
        let sent_folder = self.folder_id(LocalFolder::Sent);
        session.ensure_folder_path(&sent_folder)?;
        let visible_sent_folder =
            self.projected_folder_id(visible_folders, LocalFolder::Sent);
        for mut summary in session.search_folder_conversations(&outbox_folder, query)? {
            let state = outbox_states.get(&summary.id).ok_or_else(|| {
                anyhow!(
                    "local delivery search result '{}' has no Camel synchronization state",
                    summary.id.0
                )
            })?;
            let (delivery_state, _) = outbox_delivery_metadata(state)?;
            summary.folder_id = if delivery_state.belongs_in_outbox() {
                visible_outbox_folder.clone()
            } else {
                visible_sent_folder.clone()
            };
            matches.push(summary);
        }

        let sent_states = message_states_by_id(
            session.list_message_sync_states(&sent_folder)?,
        )?;
        let mut sent_matches = session.search_folder_conversations(&sent_folder, query)?;
        for summary in &mut sent_matches {
            let state = sent_states.get(&summary.id).ok_or_else(|| {
                anyhow!(
                    "local Sent search result '{}' has no Camel synchronization state",
                    summary.id.0
                )
            })?;
            sent_delivery_metadata(state)?;
            summary.folder_id = visible_sent_folder.clone();
        }
        matches.extend(sent_matches);
        Ok(matches)
    }

    /// Submits only messages which are still physically and logically queued
    /// in the local Outbox. A successful SMTP submission is committed to the
    /// local Sent folder before this stage returns; remote Sent publication is
    /// deliberately left to `synchronize_delivery`.
    pub(crate) fn submit_outbox(
        &self,
        mut placement_changed: impl FnMut(),
    ) -> anyhow::Result<HashSet<NonZeroU64>> {
        let outbox_folder_id = self.folder_id(LocalFolder::Outbox);
        let sent_folder_id = self.folder_id(LocalFolder::Sent);
        let mut session = self.open()?;
        session.ensure_folder_path(&outbox_folder_id)?;
        let queued = outbox_delivery_records(
            session.list_conversations(&outbox_folder_id)?,
            session.list_message_sync_states(&outbox_folder_id)?,
        )?;
        let queued_count = queued.len();

        let mut delivered_message_id_hashes = HashSet::new();
        let mut submissions = Vec::new();
        for record in &queued {
            match record.state {
                DeliveryState::ProviderCopyExpected | DeliveryState::NeedsRemoteCopy => {
                    delivered_message_id_hashes.insert(record.message_id_hash);
                    // Delivery was acknowledged before an interruption. Finish
                    // its local placement; never submit it again.
                    session.ensure_folder_path(&sent_folder_id)?;
                    session.move_message(&record.summary.id, &sent_folder_id)?;
                    placement_changed();
                }
                DeliveryState::SubmissionUncertain => {}
                DeliveryState::Queued => {
                    submissions.push(record);
                }
            }
        }

        let mut transport = if submissions.is_empty() {
            None
        } else {
            Some(TransportSession::open_online(self.binding)?)
        };
        let submitted = submissions.len();
        for record in submissions {
            session.begin_submission(&record.summary.id)?;
            let provider_saved_copy = transport
                .as_mut()
                .expect("transport exists while unsent messages are present")
                .send_cached_message(&mut session, &record.summary.id)?;
            delivered_message_id_hashes.insert(record.message_id_hash);
            session.complete_submission(&record.summary.id, provider_saved_copy)?;
            placement_changed();
            session.ensure_folder_path(&sent_folder_id)?;
            session.move_message(&record.summary.id, &sent_folder_id)?;
        }

        tracing::debug!(
            target: "pigeon::eds",
            queued = queued_count,
            submitted,
            "reconciled cached Outbox"
        );
        Ok(delivered_message_id_hashes)
    }

    /// Reconciles acknowledged local Sent messages with the provider Sent
    /// folder. This stage never submits mail and can therefore be retried
    /// independently of SMTP delivery.
    pub(crate) fn synchronize_delivery(
        &self,
        remote_sent_folder: Option<&FolderId>,
        online_session: &mut AccountSession,
        mut delivered_message_id_hashes: HashSet<NonZeroU64>,
    ) -> anyhow::Result<DeliverySynchronization> {
        let mut remote_sent = remote_sent_folder
            .map(|folder_id| online_session.list_message_sync_states(folder_id))
            .transpose()?
            .map(RemoteSentEvidence::from_states)
            .unwrap_or_default();
        let mut session = self.open()?;
        let outbox_folder_id = self.folder_id(LocalFolder::Outbox);
        let sent_folder_id = self.folder_id(LocalFolder::Sent);
        session.ensure_folder_path(&outbox_folder_id)?;
        session.ensure_folder_path(&sent_folder_id)?;
        let outbox = outbox_delivery_records(
            session.list_conversations(&outbox_folder_id)?,
            session.list_message_sync_states(&outbox_folder_id)?,
        )?;
        let mut local_sent = sent_delivery_records(
            session.list_conversations(&sent_folder_id)?,
            session.list_message_sync_states(&sent_folder_id)?,
        )?;
        let mut uploaded_local_sent = 0;
        if let Some(remote_folder) = remote_sent_folder
            && local_sent
                .iter()
                .any(|record| record.state == DeliveryState::NeedsRemoteCopy)
        {
            uploaded_local_sent = append_missing_sent_copies(
                &outbox,
                &mut local_sent,
                &remote_sent,
                |id| {
                    online_session.append_cached_sent_from(&mut session, id, remote_folder)?;
                    session.expect_provider_sent_copy(id)
                },
            )?;
            if uploaded_local_sent != 0 {
                online_session.refresh_folder_info(remote_folder)?;
                remote_sent = RemoteSentEvidence::from_states(
                    online_session.list_message_sync_states(remote_folder)?,
                );
            }
        }

        for record in outbox {
            if classify_delivery(
                record.state,
                record.message_id_hash,
                &mut remote_sent.unclaimed,
            ) == DeliveryPlacement::Retire
            {
                session.delete_local_message(&record.summary.id)?;
                delivered_message_id_hashes.insert(record.message_id_hash);
            }
        }
        let mut retired_local_sent = 0usize;
        let mut awaiting_server_copy = 0usize;
        for record in &local_sent {
            let delivery_state = record.state;
            let message_id_hash = record.message_id_hash;
            delivered_message_id_hashes.insert(message_id_hash);
            if classify_delivery(
                delivery_state,
                message_id_hash,
                &mut remote_sent.unclaimed,
            ) == DeliveryPlacement::Retire
            {
                session.delete_local_message(&record.summary.id)?;
                retired_local_sent += 1;
            } else if remote_sent_folder.is_some()
                && matches!(
                    delivery_state,
                    DeliveryState::ProviderCopyExpected | DeliveryState::NeedsRemoteCopy
                )
            {
                awaiting_server_copy += 1;
            }
        }
        let convergence_incomplete = awaiting_server_copy != 0;
        let sent = sent_delivery_records(
            session.list_conversations(&sent_folder_id)?,
            session.list_message_sync_states(&sent_folder_id)?,
        )?
        .into_iter()
        .map(|record| record.summary)
        .collect::<Vec<_>>();
        tracing::debug!(
            target: "pigeon::eds",
            examined = local_sent.len(),
            remaining_local = sent.len(),
            retired = retired_local_sent,
            uploaded = uploaded_local_sent,
            awaiting_provider = awaiting_server_copy,
            "reconciled cached Sent"
        );
        Ok(DeliverySynchronization {
            outbox: self.load_outbox_projection_from(&mut session)?.outbox,
            sent,
            delivered_message_id_hashes,
            convergence_incomplete,
        })
    }

    pub(super) fn has_outbox_messages(&self) -> anyhow::Result<bool> {
        let mut session = self.open()?;
        let outbox_folder_id = self.folder_id(LocalFolder::Outbox);
        session.ensure_folder_path(&outbox_folder_id)?;
        session.refresh_folder_info(&outbox_folder_id)?;
        for state in session.list_message_sync_states(&outbox_folder_id)? {
            if outbox_delivery_metadata(&state)?.0.belongs_in_outbox() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(super) fn load_outbox_projection(&self) -> anyhow::Result<CachedDeliveryProjection> {
        let mut session = self.open()?;
        self.load_outbox_projection_from(&mut session)
    }

    fn load_outbox_projection_from(
        &self,
        session: &mut AccountSession,
    ) -> anyhow::Result<CachedDeliveryProjection> {
        let outbox_folder_id = self.folder_id(LocalFolder::Outbox);
        session.ensure_folder_path(&outbox_folder_id)?;
        session.refresh_folder_info(&outbox_folder_id)?;
        let mut outbox = Vec::new();
        let mut sent = Vec::new();
        for record in outbox_delivery_records(
            session.list_conversations(&outbox_folder_id)?,
            session.list_message_sync_states(&outbox_folder_id)?,
        )? {
            if record.state.belongs_in_outbox() {
                outbox.push(record.summary);
            } else {
                sent.push(record.summary);
            }
        }
        Ok(CachedDeliveryProjection { outbox, sent })
    }

    pub(super) fn load_sent_projection(&self) -> anyhow::Result<Vec<ConversationSummary>> {
        let mut session = self.open()?;
        let mut sent = self.load_outbox_projection_from(&mut session)?.sent;
        let sent_folder_id = self.folder_id(LocalFolder::Sent);
        session.ensure_folder_path(&sent_folder_id)?;
        session.refresh_folder_info(&sent_folder_id)?;
        sent.extend(
            sent_delivery_records(
                session.list_conversations(&sent_folder_id)?,
                session.list_message_sync_states(&sent_folder_id)?,
            )?
            .into_iter()
            .map(|record| record.summary),
        );
        Ok(sent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn maildir_record(folder: &str, uid: &str) -> (ConversationSummary, MessageSyncState) {
        let id = ConversationId(format!("collection/{folder}\u{1f}{uid}"));
        let summary = ConversationSummary {
            id: id.clone(),
            folder_id: FolderId(format!("collection/{folder}")),
            subject: uid.into(),
            participants: Vec::new(),
            message_count: 1,
            unread_count: 0,
            attachment_count: 0,
            starred: false,
            last_updated_unix_ms: 0,
            preview: String::new(),
        };
        let state = MessageSyncState {
            conversation_id: id,
            message_id_hash: 17,
            delivery_state: None,
            draft_synced: false,
            draft_superseded: false,
        };
        (summary, state)
    }

    #[test]
    fn provider_evidence_resolves_only_matching_previously_attempted_deliveries() {
        let (unsent, unsent_state) = maildir_record("Outbox", "unsent");
        let (attempted, mut attempted_state) = maildir_record("Outbox", "interrupted");
        let (sent, mut sent_state) = maildir_record("Sent", "sent");
        attempted_state.delivery_state = Some(DELIVERY_SUBMITTING.into());
        sent_state.delivery_state = Some(DELIVERY_PROVIDER_COPY.into());

        let outbox = outbox_delivery_records(
            vec![unsent, attempted],
            vec![attempted_state.clone(), unsent_state],
        )
        .unwrap();
        let sent = sent_delivery_records(vec![sent], vec![sent_state]).unwrap();
        let place = |record: &DeliveryRecord, evidence: &mut RemoteSentEvidence| {
            classify_delivery(record.state, record.message_id_hash, &mut evidence.unclaimed)
        };

        let mut unrelated = attempted_state.clone();
        unrelated.message_id_hash += 1;
        let mut evidence = RemoteSentEvidence::from_states(vec![unrelated]);
        assert_eq!(place(&outbox[1], &mut evidence), DeliveryPlacement::Held);
        assert_eq!(place(&sent[0], &mut evidence), DeliveryPlacement::Sent);

        let mut evidence = RemoteSentEvidence::from_states(vec![attempted_state]);
        assert_eq!(place(&outbox[0], &mut evidence), DeliveryPlacement::Outbox);
        assert_eq!(place(&outbox[1], &mut evidence), DeliveryPlacement::Retire);
        assert_eq!(place(&sent[0], &mut evidence), DeliveryPlacement::Sent);
    }

    #[test]
    fn delivery_never_infers_send_state_from_an_incomplete_camel_snapshot() {
        let (summary, state) = maildir_record("Outbox", "queued");

        assert!(outbox_delivery_records(vec![summary.clone()], Vec::new()).is_err());
        assert!(outbox_delivery_records(Vec::new(), vec![state.clone()]).is_err());
        let mut missing_message_id = state.clone();
        missing_message_id.message_id_hash = 0;
        assert!(
            outbox_delivery_records(vec![summary.clone()], vec![missing_message_id]).is_err()
        );
        let mut invalid_delivery_state = state.clone();
        invalid_delivery_state.delivery_state = Some("invalid".into());
        assert!(
            outbox_delivery_records(
                vec![summary.clone()],
                vec![invalid_delivery_state],
            )
            .is_err()
        );
        let mut draft_state = state.clone();
        draft_state.draft_superseded = true;
        assert!(outbox_delivery_records(vec![summary.clone()], vec![draft_state]).is_err());
        assert!(outbox_delivery_records(vec![summary], vec![state.clone(), state]).is_err());
    }

    #[test]
    fn retry_after_a_partial_sent_upload_resumes_from_the_committed_marker() {
        let (first_summary, first_state) = maildir_record("Sent", "first");
        let (second_summary, mut second_state) = maildir_record("Sent", "second");
        second_state.message_id_hash += 1;

        // The first entry is an ordinary unmarked local Sent record. The second
        // was acknowledged by SMTP before being moved from Outbox.
        second_state.delivery_state = Some(DELIVERY_COPY_REQUIRED.into());
        let summaries = vec![first_summary.clone(), second_summary.clone()];
        let mut persisted = vec![first_state, second_state];
        let mut sent = sent_delivery_records(summaries.clone(), persisted.clone()).unwrap();
        let evidence = RemoteSentEvidence::default();
        let mut uploaded = Vec::new();

        let result = append_missing_sent_copies(&[], &mut sent, &evidence, |id| {
            if id == &second_summary.id {
                anyhow::bail!("connection lost during the second append");
            }
            assert_eq!(id, &first_summary.id);
            uploaded.push(id.clone());
            persisted[0].delivery_state = Some(DELIVERY_PROVIDER_COPY.into());
            Ok(())
        });
        assert!(result.is_err());

        // Rebuild from durable Camel state, as the next refresh does. Neither
        // message has appeared in the provider summary yet.
        let mut sent = sent_delivery_records(summaries, persisted).unwrap();
        let appended = append_missing_sent_copies(&[], &mut sent, &evidence, |id| {
            uploaded.push(id.clone());
            Ok(())
        })
        .unwrap();
        assert_eq!(appended, 1);
        assert_eq!(uploaded, vec![first_summary.id, second_summary.id]);
        assert_eq!(
            append_missing_sent_copies(&[], &mut sent, &evidence, |_| {
                panic!("accepted messages must not be uploaded again while awaiting evidence")
            })
            .unwrap(),
            0,
        );
    }
}
