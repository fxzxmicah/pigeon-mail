use std::collections::{HashMap, HashSet};
use std::num::NonZeroU64;

use anyhow::anyhow;

use crate::integration::camel::{AccountSession, AppendMessageRequest, MessageSyncState};
use crate::model::mail::{
    ConversationId, ConversationSummary, FolderId, MailFolder, PreparedMessage, StoredMessageRef,
};

use super::{LocalFolder, LocalMailbox, message_states_by_id};

struct DraftRevision {
    conversation_id: ConversationId,
    message_id_hash: NonZeroU64,
    synced: bool,
    recover_superseded_marker: bool,
    obsolete: Vec<ConversationId>,
}

pub(crate) struct DraftReplayOutcome {
    pub(crate) remote_changed: bool,
    pub(crate) convergence_incomplete: bool,
}

struct DraftSet {
    revisions: Vec<DraftRevision>,
}

struct DraftReplay {
    pending: Vec<DraftRevision>,
    delivered: Vec<ConversationId>,
}

impl DraftSet {
    fn from_states(states: Vec<MessageSyncState>) -> anyhow::Result<Self> {
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
        Ok(Self { revisions })
    }

    fn revision_visibility(&self) -> HashMap<&ConversationId, bool> {
        self.revisions
            .iter()
            .flat_map(|revision| {
                std::iter::once((&revision.conversation_id, true))
                    .chain(revision.obsolete.iter().map(|id| (id, false)))
            })
            .collect()
    }

    fn project_complete(
        &self,
        summaries: Vec<ConversationSummary>,
    ) -> anyhow::Result<Vec<ConversationSummary>> {
        let mut revisions = self.revision_visibility();
        let mut visible = Vec::with_capacity(self.revisions.len());
        for summary in summaries {
            let Some(is_current) = revisions.remove(&summary.id) else {
                return Err(anyhow!(
                    "local draft '{}' has an unmatched or duplicate message summary",
                    summary.id.0
                ));
            };
            if is_current {
                visible.push(summary);
            }
        }
        if let Some(conversation_id) = revisions.keys().next() {
            return Err(anyhow!(
                "local draft synchronization state '{}' has no message summary",
                conversation_id.0
            ));
        }
        Ok(visible)
    }

    fn project_matches(
        &self,
        summaries: Vec<ConversationSummary>,
    ) -> anyhow::Result<Vec<ConversationSummary>> {
        let revisions = self.revision_visibility();
        let mut visible = Vec::new();
        for summary in summaries {
            let Some(&is_current) = revisions.get(&summary.id) else {
                return Err(anyhow!(
                    "local draft search result '{}' has no Camel synchronization state",
                    summary.id.0
                ));
            };
            if is_current {
                visible.push(summary);
            }
        }
        Ok(visible)
    }

    fn into_replay(
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

impl LocalMailbox<'_> {
    pub(super) fn search_drafts(
        &self,
        session: &mut AccountSession,
        query: &str,
        visible_folders: &[MailFolder],
    ) -> anyhow::Result<Vec<ConversationSummary>> {
        let drafts_folder = self.folder_id(LocalFolder::Drafts);
        session.ensure_folder_path(&drafts_folder)?;
        let mut matches = DraftSet::from_states(
            session.list_message_sync_states(&drafts_folder)?,
        )?
        .project_matches(session.search_folder_conversations(&drafts_folder, query)?)?;
        let visible_drafts_folder =
            self.projected_folder_id(visible_folders, LocalFolder::Drafts);
        for summary in &mut matches {
            summary.folder_id = visible_drafts_folder.clone();
        }
        Ok(matches)
    }

    pub(super) fn load_drafts(&self) -> anyhow::Result<Vec<ConversationSummary>> {
        let folder_id = self.folder_id(LocalFolder::Drafts);
        let mut session = self.open()?;
        session.ensure_folder_path(&folder_id)?;
        session.refresh_folder_info(&folder_id)?;
        let conversations = session.list_conversations(&folder_id)?;
        DraftSet::from_states(session.list_message_sync_states(&folder_id)?)?
            .project_complete(conversations)
    }

    pub(crate) fn save_draft(
        &self,
        message: &PreparedMessage,
    ) -> anyhow::Result<StoredMessageRef> {
        let mut session = self.open()?;
        let (stored, superseded) = self.append_draft(&mut session, message)?;
        retire_superseded_draft(&mut session, superseded.as_ref());
        Ok(stored)
    }

    pub(crate) fn queue_delivery(
        &self,
        message: &PreparedMessage,
    ) -> anyhow::Result<()> {
        let mut session = self.open()?;
        let (stored, superseded) = self.append_draft(&mut session, message)?;
        let outbox = self.folder_id(LocalFolder::Outbox);
        let transition = (|| {
            session.ensure_folder_path(&outbox)?;
            session.set_draft(&stored.conversation_id, false)?;
            session.move_message(&stored.conversation_id, &outbox)
        })();
        if let Err(error) = transition {
            rollback_staged_draft(&mut session, &stored.conversation_id, superseded.as_ref());
            return Err(error.context("could not move the staged draft into local Outbox"));
        }
        retire_superseded_draft(&mut session, superseded.as_ref());
        Ok(())
    }

    fn append_draft(
        &self,
        session: &mut AccountSession,
        message: &PreparedMessage,
    ) -> anyhow::Result<(StoredMessageRef, Option<ConversationId>)> {
        let folder_id = self.folder_id(LocalFolder::Drafts);
        session.ensure_folder_path(&folder_id)?;
        let request = AppendMessageRequest {
            message_id: message
                .message_id
                .as_ref()
                .map(|message_id| message_id.0.as_str()),
            folder_id: &folder_id,
            from: &message.from,
            reply_to: message.reply_to.as_deref(),
            to: &message.to,
            cc: &message.cc,
            bcc: &message.bcc,
            subject: &message.subject,
            html_body: message.body.html(),
            plain_body: message.body.text(),
            attachment_uris: &message.attachment_uris,
            is_draft: true,
        };
        let superseded_draft = message
            .conversation_id
            .as_ref()
            .filter(|previous_id| self.contains_in(LocalFolder::Drafts, previous_id))
            .cloned();
        if let Some(previous_id) = superseded_draft.as_ref() {
            session.set_draft_superseded(previous_id, true)?;
        }
        let stored = match session.append_message(&request) {
            Ok(stored) => {
                stored
            }
            Err(error) => {
                if let Some(previous_id) = superseded_draft.as_ref()
                    && let Err(error) = session.set_draft_superseded(previous_id, false)
                {
                    crate::logging::report_failure(
                        "draft-superseded-marker-recovery",
                        &error,
                    );
                }
                return Err(error.context(format!(
                    "EDS local message append failed for folder '{}'", folder_id.0
                )));
            }
        };

        Ok((stored, superseded_draft))
    }

    pub(crate) fn replay_drafts(
        &self,
        remote_folder_id: Option<&FolderId>,
        online_session: &mut AccountSession,
        delivered_message_id_hashes: &HashSet<NonZeroU64>,
    ) -> anyhow::Result<DraftReplayOutcome> {
        let mut local_session = self.open()?;
        let local_folder_id = self.folder_id(LocalFolder::Drafts);
        local_session.ensure_folder_path(&local_folder_id)?;
        let replay = DraftSet::from_states(
            local_session.list_message_sync_states(&local_folder_id)?,
        )?
        .into_replay(delivered_message_id_hashes);
        let remote_states = remote_folder_id
            .map(|folder_id| online_session.list_message_sync_states(folder_id))
            .transpose()?
            .unwrap_or_default();
        let mut remote_by_message_id = HashMap::<NonZeroU64, Vec<ConversationId>>::new();
        let mut retired_remote = 0usize;
        for state in remote_states {
            let Some(message_id_hash) = NonZeroU64::new(state.message_id_hash) else {
                continue;
            };
            if delivered_message_id_hashes.contains(&message_id_hash) {
                online_session.mark_message_deleted(&state.conversation_id)?;
                retired_remote += 1;
            } else {
                remote_by_message_id
                    .entry(message_id_hash)
                    .or_default()
                    .push(state.conversation_id);
            }
        }
        let mut uploaded = 0usize;
        let mut retired_local = 0usize;
        for delivered in replay.delivered {
            local_session.delete_local_message(&delivered)?;
            retired_local += 1;
        }
        for revision in replay.pending {
            if revision.recover_superseded_marker {
                local_session.set_draft_superseded(&revision.conversation_id, false)?;
            }
            for obsolete in &revision.obsolete {
                local_session.delete_local_message(obsolete)?;
                retired_local += 1;
            }
            if revision.synced {
                local_session.delete_local_message(&revision.conversation_id)?;
                retired_local += 1;
                continue;
            }
            let Some(remote_folder_id) = remote_folder_id else {
                continue;
            };
            online_session.append_cached_draft_from(
                &mut local_session,
                &revision.conversation_id,
                remote_folder_id,
            )?;
            for previous in remote_by_message_id
                .remove(&revision.message_id_hash)
                .into_iter()
                .flatten()
            {
                online_session.mark_message_deleted(&previous)?;
                retired_remote += 1;
            }
            local_session.set_draft_synced(&revision.conversation_id, true)?;
            local_session.delete_local_message(&revision.conversation_id)?;
            uploaded += 1;
            retired_local += 1;
        }
        if retired_remote != 0 && let Some(folder) = remote_folder_id {
            online_session.synchronize_folder_flags(folder)?;
        }
        let pending = local_session
            .list_message_sync_states(&local_folder_id)?
            .len();
        tracing::debug!(
            target: "pigeon::eds",
            remaining_local = pending,
            uploaded,
            retired = retired_local,
            retired_remote,
            "reconciled cached drafts"
        );
        Ok(DraftReplayOutcome {
            remote_changed: uploaded != 0 || retired_remote != 0,
            convergence_incomplete: remote_folder_id.is_some() && pending != 0,
        })
    }
}

fn retire_superseded_draft(
    session: &mut AccountSession,
    superseded: Option<&ConversationId>,
) {
    let Some(previous_id) = superseded else {
        return;
    };
    if let Err(error) = session.delete_local_message(previous_id) {
        // The replacement is durable and the old revision remains marked for
        // ordinary draft replay to retire.
        crate::logging::report_failure("draft-cache-replace-cleanup", &error);
    }
}

fn rollback_staged_draft(
    session: &mut AccountSession,
    staged: &ConversationId,
    superseded: Option<&ConversationId>,
) {
    if let Err(error) = session.delete_local_message(staged) {
        crate::logging::report_failure("outbox-stage-rollback", &error);
        return;
    }
    if let Some(previous_id) = superseded
        && let Err(error) = session.set_draft_superseded(previous_id, false)
    {
        crate::logging::report_failure("draft-superseded-marker-recovery", &error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integration::camel::DELIVERY_PROVIDER_COPY;

    #[test]
    fn draft_replay_requires_one_explicit_current_revision_per_message_id() {
        let state = |uid: &str, superseded: bool| MessageSyncState {
            conversation_id: ConversationId(format!(
                "collection-source/Drafts\u{1f}{uid}"
            )),
            message_id_hash: 17,
            delivery_state: None,
            draft_synced: false,
            draft_superseded: superseded,
        };

        let revisions = DraftSet::from_states(vec![
            state("old", true),
            state("current", false),
        ])
        .expect("one current revision should be unambiguous")
        .revisions;
        assert_eq!(revisions.len(), 1);
        assert_eq!(
            revisions[0].conversation_id.0,
            "collection-source/Drafts\u{1f}current"
        );
        assert_eq!(revisions[0].obsolete.len(), 1);
        assert_eq!(
            revisions[0].obsolete[0].0,
            "collection-source/Drafts\u{1f}old"
        );
        assert!(!revisions[0].recover_superseded_marker);

        let recovered = DraftSet::from_states(vec![state("only", true)])
            .expect("a sole interrupted revision remains the current draft")
            .revisions;
        assert!(recovered[0].recover_superseded_marker);

        assert!(
            DraftSet::from_states(vec![state("one", false), state("two", false)]).is_err()
        );
        let mut missing_message_id = state("missing-id", false);
        missing_message_id.message_id_hash = 0;
        assert!(DraftSet::from_states(vec![missing_message_id]).is_err());
        let mut delivery_state = state("delivery-state", false);
        delivery_state.delivery_state = Some(DELIVERY_PROVIDER_COPY.into());
        assert!(DraftSet::from_states(vec![delivery_state]).is_err());
    }

    #[test]
    fn draft_projection_exposes_only_the_current_revision() {
        let summary = |uid: &str| ConversationSummary {
            id: ConversationId(format!(
                "collection-source/Drafts\u{1f}{uid}"
            )),
            folder_id: FolderId("collection-source/Drafts".into()),
            subject: uid.into(),
            participants: Vec::new(),
            message_count: 1,
            unread_count: 0,
            attachment_count: 0,
            starred: false,
            last_updated_unix_ms: 0,
            preview: String::new(),
        };
        let state = |uid: &str, superseded: bool| MessageSyncState {
            conversation_id: summary(uid).id,
            message_id_hash: 17,
            delivery_state: None,
            draft_synced: false,
            draft_superseded: superseded,
        };
        let drafts = DraftSet::from_states(vec![
            state("old", true),
            state("current", false),
        ])
        .expect("draft state should be unambiguous");

        let projected = drafts
            .project_complete(vec![summary("old"), summary("current")])
            .expect("complete Camel snapshots should project");
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].subject, "current");
        assert!(
            drafts
                .project_matches(vec![summary("old")])
                .expect("superseded search matches are valid but invisible")
                .is_empty()
        );
        assert!(drafts.project_complete(vec![summary("current")]).is_err());
        assert!(drafts
            .project_complete(vec![summary("old"), summary("current"), summary("current")])
            .is_err());
        assert!(drafts.project_matches(vec![summary("unknown")]).is_err());
    }

    #[test]
    fn delivery_retires_every_local_revision_before_draft_replay() {
        let state = |uid: &str, message_id_hash: u64, superseded: bool| MessageSyncState {
            conversation_id: ConversationId(format!(
                "collection-source/Drafts\u{1f}{uid}"
            )),
            message_id_hash,
            delivery_state: None,
            draft_synced: false,
            draft_superseded: superseded,
        };
        let replay = DraftSet::from_states(vec![
            state("sent-old", 17, true),
            state("sent-current", 17, false),
            state("pending", 18, false),
        ])
        .expect("draft revisions should be unambiguous")
        .into_replay(&HashSet::from([NonZeroU64::new(17).unwrap()]));

        assert_eq!(replay.pending.len(), 1);
        assert_eq!(replay.pending[0].message_id_hash.get(), 18);
        assert_eq!(
            replay
                .delivered
                .iter()
                .map(|id| id.0.rsplit_once('\u{1f}').unwrap().1)
                .collect::<HashSet<_>>(),
            HashSet::from(["sent-old", "sent-current"])
        );
    }
}
