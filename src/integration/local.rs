//! Account-scoped Drafts, Outbox, and Sent state in EDS local mail.

mod delivery;
mod draft;

use std::collections::{HashMap, HashSet};
use std::num::NonZeroU64;

use anyhow::anyhow;

use crate::i18n::gettext;
use crate::integration::account::EdsAccountBinding;
use crate::integration::camel::{
    AccountSession, AppendMessageRequest, MessageSyncState, TransportSession,
};
use crate::model::mail::{
    ConversationId, ConversationSummary, FolderId, FolderKind, MailFolder, MessageDetail,
    PreparedMessage, StoredMessageRef, sort_and_deduplicate_conversations,
};

pub(crate) use delivery::{DeliverySynchronization, OutboxSubmission};
use delivery::{
    CachedDeliveryProjection, DeliveryPlacement, DeliveryState, RemoteSentEvidence,
    classify_delivery, outbox_delivery_metadata, outbox_delivery_records,
    sent_delivery_metadata, sent_delivery_records,
};
use draft::DraftSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalFolder {
    Drafts,
    Outbox,
    Sent,
}

impl LocalFolder {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Drafts => "Drafts",
            Self::Outbox => "Outbox",
            Self::Sent => "Sent",
        }
    }

    pub(crate) fn kind(self) -> FolderKind {
        match self {
            Self::Drafts => FolderKind::Drafts,
            Self::Outbox => FolderKind::Outbox,
            Self::Sent => FolderKind::Sent,
        }
    }

    fn display_name(self) -> String {
        match self {
            Self::Drafts => gettext("Drafts"),
            Self::Outbox => gettext("Outbox"),
            Self::Sent => gettext("Sent"),
        }
    }

    fn from_kind(kind: FolderKind) -> Option<Self> {
        match kind {
            FolderKind::Drafts => Some(Self::Drafts),
            FolderKind::Outbox => Some(Self::Outbox),
            FolderKind::Sent => Some(Self::Sent),
            _ => None,
        }
    }
}

pub(crate) struct LocalMailbox<'a> {
    binding: &'a EdsAccountBinding,
}

pub(crate) struct DraftReplayOutcome {
    pub(crate) remote_changed: bool,
    pub(crate) convergence_incomplete: bool,
}

impl<'a> LocalMailbox<'a> {
    pub(crate) fn new(binding: &'a EdsAccountBinding) -> Self {
        Self { binding }
    }

    pub(crate) fn folder_id(&self, folder: LocalFolder) -> FolderId {
        FolderId(format!(
            "{}/{}",
            self.binding.account_parent_uid,
            folder.name()
        ))
    }

    pub(crate) fn folder_uri(&self, folder: LocalFolder) -> anyhow::Result<String> {
        crate::integration::camel::folder_uri("local", &self.folder_id(folder))
    }

    pub(crate) fn contains(&self, conversation_id: &ConversationId) -> bool {
        [LocalFolder::Drafts, LocalFolder::Outbox, LocalFolder::Sent]
            .into_iter()
            .any(|folder| self.contains_in(folder, conversation_id))
    }

    fn contains_in(
        &self,
        folder: LocalFolder,
        conversation_id: &ConversationId,
    ) -> bool {
        let folder_id = self.folder_id(folder);
        conversation_folder_id(conversation_id)
            .is_some_and(|candidate| candidate == folder_id.0)
    }

    pub(crate) fn folder_for_projection(
        &self,
        folders: &[MailFolder],
        folder_id: &FolderId,
    ) -> Option<LocalFolder> {
        folders
            .iter()
            .find(|folder| folder.id == *folder_id)
            .and_then(|folder| LocalFolder::from_kind(folder.kind))
    }

    fn open(&self) -> anyhow::Result<AccountSession> {
        AccountSession::open_local_mailbox()
    }

    pub(crate) fn ensure_folders(&self) -> anyhow::Result<()> {
        let mut session = self.open()?;
        for folder in [
            LocalFolder::Drafts,
            LocalFolder::Outbox,
            LocalFolder::Sent,
        ] {
            session.ensure_folder_path(&self.folder_id(folder))?;
        }
        Ok(())
    }

    fn load_drafts(&self) -> anyhow::Result<Vec<ConversationSummary>> {
        let folder_id = self.folder_id(LocalFolder::Drafts);
        let mut session = self.open()?;
        session.ensure_folder_path(&folder_id)?;
        session.refresh_folder_info(&folder_id)?;
        let conversations = session.list_conversations(&folder_id, 0, 0)?;
        DraftSet::from_states(session.list_message_sync_states(&folder_id)?)?
            .project_complete(conversations)
    }

    /// Loads the user-visible local side of a mailbox role without touching
    /// the network. Delivery acknowledgement, rather than the physical
    /// Maildir folder alone, determines whether a record belongs in Outbox or
    /// Sent after an interrupted move.
    pub(crate) fn load_projection(
        &self,
        folder: LocalFolder,
    ) -> anyhow::Result<Vec<ConversationSummary>> {
        match folder {
            LocalFolder::Drafts => self.load_drafts(),
            LocalFolder::Outbox => Ok(self.load_outbox_projection()?.outbox),
            LocalFolder::Sent => self.load_sent_projection(),
        }
    }

    pub(crate) fn ensure_projection_folders(
        &self,
        folders: &mut Vec<MailFolder>,
    ) -> anyhow::Result<()> {
        for folder in [LocalFolder::Drafts, LocalFolder::Sent] {
            self.ensure_projection_folder(folders, folder);
        }
        if self.has_outbox_messages()? {
            self.ensure_projection_folder(folders, LocalFolder::Outbox);
        }
        Ok(())
    }

    pub(crate) fn ensure_projection_folder(
        &self,
        folders: &mut Vec<MailFolder>,
        folder: LocalFolder,
    ) -> FolderId {
        let display_folder_id = self.projected_folder_id(folders, folder);
        ensure_projected_folder(folders, &display_folder_id, folder);
        display_folder_id
    }

    pub(crate) fn merge_cached_role(
        &self,
        folder: LocalFolder,
        display_folder_id: &FolderId,
        current: &mut Vec<ConversationSummary>,
    ) -> anyhow::Result<()> {
        let mut projected = self.load_projection(folder)?;
        for summary in &mut projected {
            summary.folder_id = display_folder_id.clone();
        }
        replace_projected_rows(current, projected, &self.folder_id(folder));
        Ok(())
    }

    fn has_outbox_messages(&self) -> anyhow::Result<bool> {
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

    fn load_outbox_projection(&self) -> anyhow::Result<CachedDeliveryProjection> {
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
            session.list_conversations(&outbox_folder_id, 0, 0)?,
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

    fn load_sent_projection(&self) -> anyhow::Result<Vec<ConversationSummary>> {
        let mut session = self.open()?;
        let mut sent = self.load_outbox_projection_from(&mut session)?.sent;
        let sent_folder_id = self.folder_id(LocalFolder::Sent);
        session.ensure_folder_path(&sent_folder_id)?;
        session.refresh_folder_info(&sent_folder_id)?;
        sent.extend(
            sent_delivery_records(
                session.list_conversations(&sent_folder_id, 0, 0)?,
                session.list_message_sync_states(&sent_folder_id)?,
            )?
            .into_iter()
            .map(|record| record.summary),
        );
        Ok(sent)
    }

    pub(crate) fn project_folder(
        &self,
        folder: LocalFolder,
        mut local_conversations: Vec<ConversationSummary>,
        folders: &mut Vec<MailFolder>,
        conversations: &mut HashMap<FolderId, Vec<ConversationSummary>>,
    ) {
        let local_folder_id = self.folder_id(folder);
        let display_folder_id = self.ensure_projection_folder(folders, folder);
        for summary in &mut local_conversations {
            summary.folder_id = display_folder_id.clone();
        }
        replace_projected_rows(
            conversations.entry(display_folder_id.clone()).or_default(),
            local_conversations,
            &local_folder_id,
        );
        update_unread_count(folders, &display_folder_id, conversations);
    }

    pub(crate) fn project_delivery(
        &self,
        outbox: Vec<ConversationSummary>,
        sent: Vec<ConversationSummary>,
        folders: &mut Vec<MailFolder>,
        conversations: &mut HashMap<FolderId, Vec<ConversationSummary>>,
    ) {
        if !outbox.is_empty() {
            self.project_folder(
                LocalFolder::Outbox,
                outbox,
                folders,
                conversations,
            );
        }
        self.project_folder(
            LocalFolder::Sent,
            sent,
            folders,
            conversations,
        );
    }

    pub(crate) fn provider_folder<'b>(
        &self,
        folders: &'b [MailFolder],
        folder: LocalFolder,
    ) -> Option<&'b MailFolder> {
        folders
            .iter()
            .find(|candidate| candidate.kind == folder.kind())
    }

    fn projected_folder_id(
        &self,
        folders: &[MailFolder],
        folder: LocalFolder,
    ) -> FolderId {
        self.provider_folder(folders, folder)
            .map(|candidate| candidate.id.clone())
            .unwrap_or_else(|| self.folder_id(folder))
    }

    pub(crate) fn message_detail(
        &self,
        conversation_id: &ConversationId,
    ) -> anyhow::Result<Option<MessageDetail>> {
        let mut session = self.open()?;
        refresh_conversation_folder(&mut session, conversation_id)?;
        session.get_message_detail(conversation_id)
    }

    pub(crate) fn export_attachment(
        &self,
        conversation_id: &ConversationId,
        attachment_token: &str,
    ) -> anyhow::Result<Option<String>> {
        let mut session = self.open()?;
        refresh_conversation_folder(&mut session, conversation_id)?;
        session.export_attachment(conversation_id, attachment_token)
    }

    pub(crate) fn set_read(
        &self,
        conversation_id: &ConversationId,
        read: bool,
    ) -> anyhow::Result<()> {
        let mut session = self.open()?;
        refresh_conversation_folder(&mut session, conversation_id)?;
        session.set_read(conversation_id, read)
    }

    pub(crate) fn set_starred(
        &self,
        conversation_id: &ConversationId,
        starred: bool,
    ) -> anyhow::Result<()> {
        let mut session = self.open()?;
        refresh_conversation_folder(&mut session, conversation_id)?;
        session.set_starred(conversation_id, starred)
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
                online_session.delete_message_permanently(&state.conversation_id)?;
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
            local_session.delete_message_permanently(&delivered)?;
            retired_local += 1;
        }
        for revision in replay.pending {
            if revision.recover_superseded_marker {
                local_session.set_draft_superseded(&revision.conversation_id, false)?;
            }
            for obsolete in &revision.obsolete {
                local_session.delete_message_permanently(obsolete)?;
                retired_local += 1;
            }
            if revision.synced {
                local_session.delete_message_permanently(&revision.conversation_id)?;
                retired_local += 1;
                continue;
            }
            let Some(remote_folder_id) = remote_folder_id else {
                continue;
            };
            for previous in remote_by_message_id
                .remove(&revision.message_id_hash)
                .into_iter()
                .flatten()
            {
                online_session.delete_message_permanently(&previous)?;
                retired_remote += 1;
            }
            online_session.append_cached_draft_from(
                &mut local_session,
                &revision.conversation_id,
                remote_folder_id,
            )?;
            local_session.set_draft_synced(&revision.conversation_id, true)?;
            local_session.delete_message_permanently(&revision.conversation_id)?;
            uploaded += 1;
            retired_local += 1;
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

    /// Submits only messages which are still physically and logically queued
    /// in the local Outbox. A successful SMTP submission is committed to the
    /// local Sent folder before this stage returns; remote Sent publication is
    /// deliberately left to `synchronize_delivery`.
    pub(crate) fn submit_outbox(&self) -> anyhow::Result<OutboxSubmission> {
        let outbox_folder_id = self.folder_id(LocalFolder::Outbox);
        let sent_folder_id = self.folder_id(LocalFolder::Sent);
        let mut session = self.open()?;
        session.ensure_folder_path(&outbox_folder_id)?;
        let queued = outbox_delivery_records(
            session.list_conversations(&outbox_folder_id, 0, 0)?,
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
                }
                DeliveryState::SubmissionUncertain => {}
                DeliveryState::Queued => submissions.push(record),
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
            session.ensure_folder_path(&sent_folder_id)?;
            session.move_message(&record.summary.id, &sent_folder_id)?;
        }

        let remaining = outbox_delivery_records(
            session.list_conversations(&outbox_folder_id, 0, 0)?,
            session.list_message_sync_states(&outbox_folder_id)?,
        )?;
        let unsent = remaining
            .into_iter()
            .map(|record| record.summary)
            .collect::<Vec<_>>();

        tracing::debug!(
            target: "pigeon::eds",
            queued = queued_count,
            submitted,
            remaining = unsent.len(),
            "reconciled cached Outbox"
        );
        Ok(OutboxSubmission {
            outbox: unsent,
            delivered_message_id_hashes,
        })
    }

    /// Reconciles acknowledged local Sent messages with the provider Sent
    /// folder. This stage never submits mail and can therefore be retried
    /// independently of SMTP delivery.
    pub(crate) fn synchronize_delivery(
        &self,
        remote_sent_folder: Option<&FolderId>,
        online_session: &mut AccountSession,
        mut outbox: OutboxSubmission,
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
        let mut resolved_outbox = HashSet::new();
        for record in outbox_delivery_records(
            session.list_conversations(&outbox_folder_id, 0, 0)?,
            session.list_message_sync_states(&outbox_folder_id)?,
        )? {
            if classify_delivery(
                record.state,
                record.message_id_hash,
                &mut remote_sent.unclaimed,
            ) == DeliveryPlacement::Retire
            {
                session.delete_message_permanently(&record.summary.id)?;
                outbox
                    .delivered_message_id_hashes
                    .insert(record.message_id_hash);
                resolved_outbox.insert(record.summary.id);
            }
        }
        if !resolved_outbox.is_empty() {
            outbox
                .outbox
                .retain(|summary| !resolved_outbox.contains(&summary.id));
        }
        let local_sent = sent_delivery_records(
            session.list_conversations(&sent_folder_id, 0, 0)?,
            session.list_message_sync_states(&sent_folder_id)?,
        )?;
        let mut retired_local_sent = 0usize;
        let mut uploaded_local_sent = 0usize;
        let mut awaiting_server_copy = 0usize;
        for record in &local_sent {
            let delivery_state = record.state;
            let message_id_hash = record.message_id_hash;
            outbox.delivered_message_id_hashes.insert(message_id_hash);
            if classify_delivery(
                delivery_state,
                message_id_hash,
                &mut remote_sent.unclaimed,
            ) == DeliveryPlacement::Retire
            {
                session.delete_message_permanently(&record.summary.id)?;
                retired_local_sent += 1;
            } else if delivery_state == DeliveryState::NeedsRemoteCopy
                && let Some(remote_sent_folder) = remote_sent_folder
            {
                online_session.append_cached_sent_from(
                    &mut session,
                    &record.summary.id,
                    remote_sent_folder,
                )?;
                session.delete_message_permanently(&record.summary.id)?;
                uploaded_local_sent += 1;
            } else if delivery_state == DeliveryState::ProviderCopyExpected
                && remote_sent_folder.is_some()
            {
                awaiting_server_copy += 1;
            }
        }
        let convergence_incomplete = awaiting_server_copy != 0;
        let sent = sent_delivery_records(
            session.list_conversations(&sent_folder_id, 0, 0)?,
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
            outbox: outbox.outbox,
            sent,
            delivered_message_id_hashes: outbox.delivered_message_id_hashes,
            remote_changed: uploaded_local_sent != 0,
            convergence_incomplete,
        })
    }

    pub(crate) fn save_draft(
        &self,
        message: &PreparedMessage,
    ) -> anyhow::Result<StoredMessageRef> {
        let (mut session, stored, superseded) = self.append_draft(message)?;
        retire_superseded_draft(&mut session, superseded.as_ref());
        Ok(stored)
    }

    pub(crate) fn queue_delivery(
        &self,
        message: &PreparedMessage,
    ) -> anyhow::Result<()> {
        let (mut session, stored, superseded) = self.append_draft(message)?;
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
        message: &PreparedMessage,
    ) -> anyhow::Result<(AccountSession, StoredMessageRef, Option<ConversationId>)> {
        let folder_id = self.folder_id(LocalFolder::Drafts);
        let mut session = self.open()?;
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
            Ok(stored) => stored,
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

        Ok((session, stored, superseded_draft))
    }

    pub(crate) fn search(
        &self,
        query: &str,
        visible_folders: &[MailFolder],
    ) -> anyhow::Result<Vec<ConversationSummary>> {
        let mut session = self.open()?;
        let drafts_folder = self.folder_id(LocalFolder::Drafts);
        session.ensure_folder_path(&drafts_folder)?;
        let mut results = DraftSet::from_states(
            session.list_message_sync_states(&drafts_folder)?,
        )?
        .project_matches(session.search_folder_conversations(&drafts_folder, query)?)?;
        let visible_drafts_folder =
            self.projected_folder_id(visible_folders, LocalFolder::Drafts);
        for summary in &mut results {
            summary.folder_id = visible_drafts_folder.clone();
        }

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
            results.push(summary);
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
        results.extend(sent_matches);
        Ok(results)
    }
}

fn retire_superseded_draft(
    session: &mut AccountSession,
    superseded: Option<&ConversationId>,
) {
    let Some(previous_id) = superseded else {
        return;
    };
    if let Err(error) = session.delete_message_permanently(previous_id) {
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
    if let Err(error) = session.delete_message_permanently(staged) {
        crate::logging::report_failure("outbox-stage-rollback", &error);
        return;
    }
    if let Some(previous_id) = superseded
        && let Err(error) = session.set_draft_superseded(previous_id, false)
    {
        crate::logging::report_failure("draft-superseded-marker-recovery", &error);
    }
}

fn message_states_by_id(
    states: Vec<MessageSyncState>,
) -> anyhow::Result<HashMap<ConversationId, MessageSyncState>> {
    let mut states_by_id = HashMap::with_capacity(states.len());
    for state in states {
        let conversation_id = state.conversation_id.clone();
        if states_by_id.insert(conversation_id.clone(), state).is_some() {
            return Err(anyhow!(
                "local message '{}' has duplicate Camel synchronization state",
                conversation_id.0
            ));
        }
    }
    Ok(states_by_id)
}

fn ensure_projected_folder(
    folders: &mut Vec<MailFolder>,
    folder_id: &FolderId,
    folder: LocalFolder,
) {
    if folders.iter().any(|candidate| candidate.id == *folder_id) {
        return;
    }
    folders.push(MailFolder {
        id: folder_id.clone(),
        name: folder.display_name(),
        unread_count: 0,
        kind: folder.kind(),
    });
}

fn replace_projected_rows(
    current: &mut Vec<ConversationSummary>,
    projected: Vec<ConversationSummary>,
    local_folder_id: &FolderId,
) {
    let projected_ids = projected
        .iter()
        .map(|summary| summary.id.clone())
        .collect::<HashSet<_>>();
    current.retain(|summary| {
        !projected_ids.contains(&summary.id)
            && conversation_folder_id(&summary.id) != Some(local_folder_id.0.as_str())
    });
    current.extend(projected);
    sort_and_deduplicate_conversations(current);
}

fn update_unread_count(
    folders: &mut [MailFolder],
    folder_id: &FolderId,
    conversations: &HashMap<FolderId, Vec<ConversationSummary>>,
) {
    let unread_count = conversations
        .get(folder_id)
        .into_iter()
        .flatten()
        .map(|conversation| conversation.unread_count)
        .sum();
    if let Some(folder) = folders.iter_mut().find(|folder| folder.id == *folder_id) {
        folder.unread_count = unread_count;
    }
}

fn conversation_folder_id(conversation_id: &ConversationId) -> Option<&str> {
    crate::integration::camel::conversation_id_parts(&conversation_id.0)
        .map(|(folder_name, _)| folder_name)
}

fn refresh_conversation_folder(
    session: &mut AccountSession,
    conversation_id: &ConversationId,
) -> anyhow::Result<()> {
    let folder_name = conversation_folder_id(conversation_id)
        .ok_or_else(|| anyhow!("local conversation id has no folder component"))?;
    session.refresh_folder_info(&FolderId(folder_name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integration::account::RouteFingerprint;
    use crate::integration::camel::{
        DELIVERY_COPY_REQUIRED, DELIVERY_PROVIDER_COPY, DELIVERY_SUBMITTING,
    };
    use crate::model::account::MailAccountId;

    fn binding() -> EdsAccountBinding {
        EdsAccountBinding {
            account_id: MailAccountId("account-1".into()),
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

    fn message_id_hash(value: u64) -> NonZeroU64 {
        NonZeroU64::new(value).expect("test Message-ID hashes are nonzero")
    }

    #[test]
    fn folder_ids_and_conversation_ownership_share_one_collection_scope() {
        let binding = binding();
        let mailbox = LocalMailbox::new(&binding);
        assert_eq!(
            mailbox.folder_id(LocalFolder::Outbox),
            FolderId("collection-source/Outbox".into())
        );
        assert_eq!(
            mailbox.folder_uri(LocalFolder::Drafts).unwrap(),
            "folder://local/collection-source/Drafts"
        );
        assert_eq!(
            mailbox.folder_uri(LocalFolder::Sent).unwrap(),
            "folder://local/collection-source/Sent"
        );
        assert!(mailbox.contains(&ConversationId(
            "collection-source/Outbox\u{1f}uid-1".into()
        )));
        assert!(mailbox.contains(&ConversationId(
            "collection-source/Sent\u{1f}uid-2".into()
        )));
        assert!(!mailbox.contains(&ConversationId(
            "collection-source-other/Outbox\u{1f}uid-1".into()
        )));
        assert!(!mailbox.contains(&ConversationId(
            "collection-source/Other\u{1f}uid-2".into()
        )));
        assert!(!mailbox.contains(&ConversationId(
            "collection-source/Drafts/Nested\u{1f}uid-2".into()
        )));
        assert!(!mailbox.contains(&ConversationId("malformed".into())));
        assert!(!mailbox.contains(&ConversationId(
            "collection-source\u{1f}uid-3".into()
        )));
    }

    #[test]
    fn draft_replacement_cannot_cross_local_collections_or_folder_roles() {
        let binding = binding();
        let mailbox = LocalMailbox::new(&binding);
        assert!(mailbox.contains_in(
            LocalFolder::Drafts,
            &ConversationId("collection-source/Drafts\u{1f}old".into()),
        ));
        assert!(!mailbox.contains_in(
            LocalFolder::Drafts,
            &ConversationId("collection-source/Outbox\u{1f}old".into()),
        ));
        assert!(!mailbox.contains_in(
            LocalFolder::Drafts,
            &ConversationId("other/Drafts\u{1f}old".into()),
        ));
        assert!(!mailbox.contains_in(
            LocalFolder::Drafts,
            &ConversationId("collection-source/Sent\u{1f}old".into()),
        ));
    }

    #[test]
    fn local_projection_replaces_only_its_previous_local_rows() {
        let summary = |id: &str| ConversationSummary {
            id: ConversationId(id.into()),
            folder_id: FolderId(
                id.split_once('\u{1f}')
                    .expect("fixture conversation IDs contain a folder")
                    .0
                    .into(),
            ),
            subject: id.into(),
            participants: Vec::new(),
            message_count: 1,
            unread_count: 1,
            attachment_count: 0,
            starred: false,
            last_updated_unix_ms: 0,
            preview: String::new(),
        };
        let binding = binding();
        let mailbox = LocalMailbox::new(&binding);
        let mut folders = vec![MailFolder {
            id: FolderId("remote-drafts".into()),
            name: "Drafts".into(),
            unread_count: 0,
            kind: FolderKind::Drafts,
        }];
        let mut conversations = HashMap::from([(
            FolderId("remote-drafts".into()),
            vec![
                summary("remote-drafts\u{1f}remote"),
                summary("collection-source/Drafts\u{1f}old-local"),
            ],
        )]);

        mailbox.project_folder(
            LocalFolder::Drafts,
            vec![summary("collection-source/Drafts\u{1f}new-local")],
            &mut folders,
            &mut conversations,
        );

        let ids = conversations[&FolderId("remote-drafts".into())]
            .iter()
            .map(|summary| summary.id.0.as_str())
            .collect::<HashSet<_>>();
        assert_eq!(
            ids,
            HashSet::from([
                "remote-drafts\u{1f}remote",
                "collection-source/Drafts\u{1f}new-local",
            ])
        );
        assert!(
            conversations[&FolderId("remote-drafts".into())]
                .iter()
                .all(|summary| summary.folder_id == FolderId("remote-drafts".into()))
        );
        assert_eq!(folders[0].unread_count, 2);
    }

    #[test]
    fn unsubmitted_delivery_stays_in_outbox() {
        assert_eq!(
            classify_delivery(
                DeliveryState::Queued,
                message_id_hash(17),
                &mut HashMap::from([(message_id_hash(17), 1)]),
            ),
            DeliveryPlacement::Outbox
        );
    }

    #[test]
    fn remote_sent_proof_resolves_an_uncertain_submission() {
        assert_eq!(
            classify_delivery(
                DeliveryState::SubmissionUncertain,
                message_id_hash(17),
                &mut HashMap::from([(message_id_hash(17), 1)]),
            ),
            DeliveryPlacement::Retire
        );
    }

    #[test]
    fn provider_copy_expectation_waits_for_matching_remote_sent_proof() {
        assert_eq!(
            classify_delivery(
                DeliveryState::ProviderCopyExpected,
                message_id_hash(17),
                &mut HashMap::new(),
            ),
            DeliveryPlacement::Sent
        );
        assert_eq!(
            classify_delivery(
                DeliveryState::ProviderCopyExpected,
                message_id_hash(17),
                &mut HashMap::from([(message_id_hash(18), 1)]),
            ),
            DeliveryPlacement::Sent
        );
    }

    #[test]
    fn ordinary_smtp_submission_uses_the_generic_sent_copy_path() {
        let state = DeliveryState::from_marker(
            Some(DELIVERY_COPY_REQUIRED),
            DeliveryState::Queued,
        )
        .expect("ordinary SMTP completion has one unambiguous state");
        assert_eq!(state, DeliveryState::NeedsRemoteCopy);
        assert_eq!(
            classify_delivery(
                state,
                message_id_hash(17),
                &mut HashMap::new(),
            ),
            DeliveryPlacement::Sent
        );
    }

    #[test]
    fn a_transport_managed_sent_copy_waits_for_provider_evidence() {
        assert_eq!(
            DeliveryState::from_marker(Some(DELIVERY_PROVIDER_COPY), DeliveryState::Queued)
                .expect("transport-managed completion has one unambiguous state"),
            DeliveryState::ProviderCopyExpected
        );
    }

    #[test]
    fn interrupted_submission_is_held_without_automatic_resubmission() {
        let state = DeliveryState::from_marker(Some(DELIVERY_SUBMITTING), DeliveryState::Queued)
            .expect("a started submission has one recovery state");
        assert_eq!(state, DeliveryState::SubmissionUncertain);
        assert_eq!(
            classify_delivery(state, message_id_hash(17), &mut HashMap::new()),
            DeliveryPlacement::Held
        );
        assert_ne!(state, DeliveryState::Queued);
        assert!(state.belongs_in_outbox());
    }

    #[test]
    fn durable_delivery_markers_remain_delivery_evidence_after_restart() {
        assert!(matches!(
            classify_delivery(
                DeliveryState::ProviderCopyExpected,
                message_id_hash(17),
                &mut HashMap::new(),
            ),
            DeliveryPlacement::Sent | DeliveryPlacement::Retire
        ));
        assert!(matches!(
            classify_delivery(
                DeliveryState::NeedsRemoteCopy,
                message_id_hash(17),
                &mut HashMap::new(),
            ),
            DeliveryPlacement::Sent | DeliveryPlacement::Retire
        ));
        assert_eq!(
            classify_delivery(
                DeliveryState::Queued,
                message_id_hash(17),
                &mut HashMap::new(),
            ),
            DeliveryPlacement::Outbox
        );
    }

    #[test]
    fn each_remote_copy_retires_only_one_acknowledged_local_copy() {
        let mut remote = HashMap::from([(message_id_hash(17), 1)]);
        assert_eq!(
            classify_delivery(
                DeliveryState::NeedsRemoteCopy,
                message_id_hash(17),
                &mut remote,
            ),
            DeliveryPlacement::Retire
        );
        assert_eq!(
            classify_delivery(
                DeliveryState::ProviderCopyExpected,
                message_id_hash(17),
                &mut remote,
            ),
            DeliveryPlacement::Sent
        );
    }

    #[test]
    fn delivery_never_infers_send_state_from_an_incomplete_camel_snapshot() {
        let id = ConversationId("collection-source/Outbox\u{1f}queued".into());
        let summary = ConversationSummary {
            id: id.clone(),
            folder_id: FolderId("collection-source/Outbox".into()),
            subject: String::new(),
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
        .into_replay(&HashSet::from([message_id_hash(17)]));

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

    #[test]
    fn camel_delivery_marker_forms_one_unambiguous_business_state() {
        assert_eq!(
            DeliveryState::from_marker(None, DeliveryState::Queued).unwrap(),
            DeliveryState::Queued
        );
        assert_eq!(
            DeliveryState::from_marker(Some(DELIVERY_PROVIDER_COPY), DeliveryState::Queued)
                .unwrap(),
            DeliveryState::ProviderCopyExpected
        );
        assert_eq!(
            DeliveryState::from_marker(Some(DELIVERY_COPY_REQUIRED), DeliveryState::Queued)
                .unwrap(),
            DeliveryState::NeedsRemoteCopy
        );
        assert!(DeliveryState::from_marker(Some("invalid"), DeliveryState::Queued).is_err());
        assert_eq!(
            DeliveryState::from_marker(None, DeliveryState::NeedsRemoteCopy).unwrap(),
            DeliveryState::NeedsRemoteCopy
        );
        assert!(DeliveryState::Queued.belongs_in_outbox());
        assert!(DeliveryState::SubmissionUncertain.belongs_in_outbox());
        assert!(!DeliveryState::ProviderCopyExpected.belongs_in_outbox());
        assert!(!DeliveryState::NeedsRemoteCopy.belongs_in_outbox());

        let unmarked = MessageSyncState {
            conversation_id: ConversationId(
                "collection-source/Outbox\u{1f}unmarked".into(),
            ),
            message_id_hash: 17,
            delivery_state: None,
            draft_synced: false,
            draft_superseded: false,
        };
        assert_eq!(
            outbox_delivery_metadata(&unmarked).unwrap().0,
            DeliveryState::Queued
        );
        assert_eq!(
            sent_delivery_metadata(&unmarked).unwrap().0,
            DeliveryState::NeedsRemoteCopy
        );
    }
}
