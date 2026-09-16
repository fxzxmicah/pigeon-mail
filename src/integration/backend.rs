use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::anyhow;
use crate::integration::account::EdsAccountBinding;
use crate::integration::local::{LocalFolder, LocalMailbox};
use crate::model::account::MailAccountId;
use crate::model::mail::{
    ConversationId, ConversationSummary, FolderId, MailFolder, MessageDetail,
    PreparedMessage, StoredMessageRef, WriteOutcome, sort_and_deduplicate_conversations,
};

mod actions;
mod cache;
mod pool;

pub(crate) use pool::mail_backend_router;
use actions::{MessageActionQueue, MessageFlagIntent, MoveIntent};
use cache::{
    ConversationLoad, ConversationPublication, MailboxCache, RemotePublication,
};

pub(crate) type BackendChangeMonitor = Box<dyn Send>;
pub(crate) type BackendChangeCallback = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RefreshOutcome {
    pub(crate) convergence_incomplete: bool,
    pub(crate) projection_stale: bool,
}

#[cfg(debug_assertions)]
macro_rules! development_probe_log {
    ($($arg:tt)*) => {
        tracing::debug!(target: "pigeon::development::eds", "{}", format_args!($($arg)*));
    };
}

#[cfg(not(debug_assertions))]
macro_rules! development_probe_log {
    ($($arg:tt)*) => {
        if false {
            let _ = format_args!($($arg)*);
        }
    };
}

pub(crate) trait MailBackendRouter: Send + Sync + 'static {
    fn activate_account(&self, account_id: &MailAccountId) -> anyhow::Result<ActivatedBackend>;

    /// Installs the EDS binding catalog and returns mail routes that changed.
    fn update_binding_catalog(&self, bindings: &[EdsAccountBinding]) -> Vec<MailAccountId>;

    fn lease_account_backend(&self, account_id: &MailAccountId) -> Option<SharedMailBackend>;

    fn unresolved_message_action_count(&self) -> usize;
}

pub(crate) trait MailBackend: Send + Sync + 'static {
    fn account_id(&self) -> &MailAccountId;

    fn unresolved_message_action_count(&self) -> usize;

    fn open_change_monitor(
        &self,
        callback: BackendChangeCallback,
    ) -> anyhow::Result<BackendChangeMonitor>;

    fn list_folders(&self) -> anyhow::Result<Vec<MailFolder>>;

    fn list_conversations(
        &self,
        folder_id: &FolderId,
        offset: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<ConversationSummary>>;

    fn fill_message_cache(&self, conversation_id: &ConversationId) -> anyhow::Result<bool>;

    fn get_cached_message_detail(
        &self,
        conversation_id: &ConversationId,
    ) -> anyhow::Result<Option<MessageDetail>>;

    fn materialize_attachment(
        &self,
        conversation_id: &ConversationId,
        attachment_token: &str,
    ) -> anyhow::Result<Option<String>>;

    fn search(&self, query: &str) -> anyhow::Result<Vec<ConversationSummary>>;

    fn refresh(&self) -> anyhow::Result<RefreshOutcome>;

    fn set_starred(
        &self,
        conversation_id: &ConversationId,
        starred: bool,
    ) -> anyhow::Result<WriteOutcome>;

    fn set_read(
        &self,
        conversation_id: &ConversationId,
        read: bool,
    ) -> anyhow::Result<WriteOutcome>;

    fn move_to_folder(
        &self,
        conversation_id: &ConversationId,
        folder_id: &FolderId,
    ) -> anyhow::Result<WriteOutcome>;

    fn save_draft(&self, message: &PreparedMessage) -> anyhow::Result<Option<StoredMessageRef>>;

    fn queue_delivery(&self, message: &PreparedMessage) -> anyhow::Result<WriteOutcome>;
}

pub(crate) type SharedMailBackend = Arc<dyn MailBackend>;
pub(crate) type SharedMailBackendRouter = Arc<dyn MailBackendRouter>;

pub(crate) struct ActivatedBackend {
    pub(crate) backend: SharedMailBackend,
    pub(crate) mode: crate::model::mail::MailboxMode,
}

fn initialize_local_mailbox(binding: &EdsAccountBinding) -> anyhow::Result<()> {
    let local_mailbox = LocalMailbox::new(binding);
    local_mailbox.ensure_folders()?;
    crate::integration::registry::ensure_local_mailbox_configuration(
        &binding.identity_uid,
        &local_mailbox.folder_uri(LocalFolder::Drafts)?,
        &local_mailbox.folder_uri(LocalFolder::Sent)?,
    )
}

fn build_live_account_backend(
    eds_binding: EdsAccountBinding,
    action_queue: MessageActionQueue,
) -> anyhow::Result<SharedMailBackend> {
    initialize_local_mailbox(&eds_binding)
        .map_err(|error| error.context("local mailbox initialization failed"))?;
    Ok(Arc::new(AccountBackend::new(
        eds_binding,
        action_queue,
    )) as SharedMailBackend)
}

#[derive(Clone)]
struct AccountBackend {
    binding: EdsAccountBinding,
    action_queue: MessageActionQueue,
    cached_session: Arc<Mutex<Option<Arc<Mutex<crate::integration::camel::AccountSession>>>>>,
    cached_mailbox: MailboxCache,
}

#[derive(Clone, Copy)]
enum MessageFlagChange {
    Read(bool),
    Starred(bool),
}

impl AccountBackend {
    #[cfg(test)]
    fn for_test(eds_binding: EdsAccountBinding) -> Self {
        Self::new(
            eds_binding,
            MessageActionQueue::new(),
        )
    }

    fn new(binding: EdsAccountBinding, action_queue: MessageActionQueue) -> Self {
        Self {
            binding,
            action_queue,
            cached_session: Arc::new(Mutex::new(None)),
            cached_mailbox: MailboxCache::default(),
        }
    }

    fn commit_action_projection(&self) {
        self.cached_mailbox
            .apply_action_projection(&self.binding, &self.action_queue);
    }

    fn record_cached_message_change(
        &self,
        conversation_id: &ConversationId,
        update: impl Fn(&mut ConversationSummary),
    ) {
        self.cached_mailbox
            .record_message_change(conversation_id, update);
    }

    fn folders(&self) -> anyhow::Result<Vec<MailFolder>> {
        if let Some(folders) = self.cached_mailbox.folders() {
            return Ok(folders);
        }
        let session = self.session()?;
        let mut folders = session
            .lock()
            .expect("camel account session lock poisoned")
            .list_folders()?;
        LocalMailbox::new(&self.binding).ensure_projection_folders(&mut folders)?;
        Ok(self.cached_mailbox.install_folders_if_absent(folders))
    }

    fn conversations(
        &self,
        folder_id: &FolderId,
        offset: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<ConversationSummary>> {
        loop {
            let revision = match self.cached_mailbox.begin_conversation_load(folder_id) {
                ConversationLoad::Cached(conversations) => {
                    return Ok(slice_conversations(&conversations, offset, limit));
                }
                ConversationLoad::Required { revision } => revision,
            };
            let conversations = match self.load_conversation_projection(folder_id) {
                Ok(conversations) => conversations,
                Err(error) => match self.cached_mailbox.begin_conversation_load(folder_id) {
                    ConversationLoad::Cached(conversations) => {
                        return Ok(slice_conversations(&conversations, offset, limit));
                    }
                    ConversationLoad::Required {
                        revision: current_revision,
                    } if current_revision != revision => continue,
                    ConversationLoad::Required { .. } => return Err(error),
                },
            };
            match self.cached_mailbox.publish_conversations(
                &self.binding,
                folder_id,
                revision,
                conversations,
                &self.action_queue,
            ) {
                ConversationPublication::Published(conversations) => {
                    return Ok(slice_conversations(&conversations, offset, limit));
                }
                ConversationPublication::Stale => {}
            }
        }
    }

    fn load_conversation_projection(
        &self,
        folder_id: &FolderId,
    ) -> anyhow::Result<Vec<ConversationSummary>> {
        let folders = self.folders()?;
        let local_mailbox = LocalMailbox::new(&self.binding);
        let local_projection = local_mailbox.folder_for_projection(&folders, folder_id);
        let session = self.session()?;
        let mut conversations = if local_projection
            .is_some_and(|folder| local_mailbox.folder_id(folder) == *folder_id)
        {
            Vec::new()
        } else {
            session
                .lock()
                .expect("camel account session lock poisoned")
                .list_conversations(folder_id, 0, 0)?
        };
        if let Some(folder) = local_projection {
            local_mailbox.merge_cached_role(folder, folder_id, &mut conversations)?;
        }
        Ok(conversations)
    }

    fn refresh_local_projection(
        &self,
        mailbox: &LocalMailbox<'_>,
        folders: &[LocalFolder],
    ) {
        for folder_id in self.cached_mailbox.invalidate_local(mailbox, folders) {
            if let Err(error) = self.conversations(&folder_id, 0, 0) {
                crate::logging::report_deferred("local-cache-projection", &error);
            }
        }
    }
}

impl AccountBackend {
    fn flush_move_intents(
        &self,
        intents: &[MoveIntent],
        session: &mut crate::integration::camel::AccountSession,
    ) -> anyhow::Result<()> {
        if intents.is_empty() {
            return Ok(());
        }

        for intent in intents {
            session.move_message(
                &intent.summary.id,
                &intent.destination_folder_id,
            )?;
            self.action_queue
                .remove_completed_moves(&self.binding, std::slice::from_ref(intent));
        }
        Ok(())
    }

    fn replay_flag_intents(
        &self,
        session: &mut crate::integration::camel::AccountSession,
    ) -> anyhow::Result<Vec<MessageFlagIntent>> {
        let intents = self.action_queue.flags_for_route(&self.binding);
        for intent in &intents {
            if let Some(read) = intent.read {
                session.set_read_for_sync(&intent.conversation_id, read)?;
            }
            if let Some(starred) = intent.starred {
                session.set_starred_for_sync(&intent.conversation_id, starred)?;
            }
        }
        Ok(intents)
    }

    fn session(&self) -> anyhow::Result<Arc<Mutex<crate::integration::camel::AccountSession>>> {
        let mut cached_session = self
            .cached_session
            .lock()
            .expect("camel session cache lock poisoned");
        if let Some(session) = cached_session.as_ref() {
            return Ok(Arc::clone(session));
        }

        let session = Arc::new(Mutex::new(
            crate::integration::camel::AccountSession::open_cached(&self.binding)?,
        ));
        *cached_session = Some(Arc::clone(&session));
        Ok(session)
    }

    fn read_cached_message_detail(
        &self,
        conversation_id: &ConversationId,
    ) -> anyhow::Result<Option<MessageDetail>> {
        let local_mailbox = LocalMailbox::new(&self.binding);
        if local_mailbox.contains(conversation_id) {
            return local_mailbox.message_detail(conversation_id);
        }

        let session = self.session()?;
        let mut session = session.lock().expect("camel session lock poisoned");
        let mut detail = session.get_message_detail(conversation_id)?;
        if let Some(detail) = detail.as_mut() {
            self.action_queue
                .apply_flag_overlay_to_detail(&self.binding, detail);
        }
        Ok(detail)
    }

    fn set_message_flag(
        &self,
        conversation_id: &ConversationId,
        change: MessageFlagChange,
    ) -> anyhow::Result<WriteOutcome> {
        let local_mailbox = LocalMailbox::new(&self.binding);
        let outcome = if local_mailbox.contains(conversation_id) {
            match change {
                MessageFlagChange::Read(read) => {
                    local_mailbox.set_read(conversation_id, read)?
                }
                MessageFlagChange::Starred(starred) => {
                    local_mailbox.set_starred(conversation_id, starred)?
                }
            }
            WriteOutcome::Applied
        } else {
            let session = self.session()?;
            apply_message_flag(
                &mut session.lock().expect("camel account session lock poisoned"),
                conversation_id,
                change,
            )?;
            match change {
                MessageFlagChange::Read(read) => self.action_queue.queue_read(
                    self.binding.clone(),
                    conversation_id.clone(),
                    read,
                ),
                MessageFlagChange::Starred(starred) => self.action_queue.queue_starred(
                    self.binding.clone(),
                    conversation_id.clone(),
                    starred,
                ),
            }
            WriteOutcome::Queued
        };
        self.record_cached_message_change(conversation_id, |conversation| match change {
            MessageFlagChange::Read(read) => conversation.unread_count = u32::from(!read),
            MessageFlagChange::Starred(starred) => conversation.starred = starred,
        });
        Ok(outcome)
    }

    fn refresh_cache(&self) -> anyhow::Result<RefreshOutcome> {
        let binding = &self.binding;
        let local_mailbox = LocalMailbox::new(binding);
        let local_revision = self.cached_mailbox.revision();
        let move_intents = self.action_queue.moves_for_route(binding);
        let outbox_submission = local_mailbox.submit_outbox()?;
        let mut online_session = crate::integration::camel::AccountSession::open_online(binding)?;
        let initial_folders = online_session.list_folders()?;
        let mut folders_to_refresh = initial_folders
            .iter()
            .map(|folder| folder.id.clone())
            .collect::<Vec<_>>();
        folders_to_refresh.sort_by(|left, right| left.0.cmp(&right.0));
        folders_to_refresh.dedup();

        // Store synchronization visits folders opened by this session.
        for folder_id in &folders_to_refresh {
            online_session.prepare_folder_for_synchronization(folder_id)?;
        }
        let replayed_flags = self.replay_flag_intents(&mut online_session)?;
        online_session.synchronize()?;
        self.action_queue
            .remove_completed_flags(binding, &replayed_flags);
        self.flush_move_intents(&move_intents, &mut online_session)?;

        for folder_id in &folders_to_refresh {
            online_session.refresh_folder_info(folder_id)?;
        }

        let mut folders = online_session.list_folders()?;
        let remote_sent = local_mailbox
            .provider_folder(&folders, LocalFolder::Sent)
            .map(|folder| folder.id.clone());
        let delivery_synchronization = local_mailbox.synchronize_delivery(
            remote_sent.as_ref(),
            &mut online_session,
            outbox_submission,
        )?;
        if delivery_synchronization.remote_changed
            && let Some(remote_sent) = remote_sent.as_ref()
        {
            online_session.refresh_folder_info(remote_sent)?;
        }
        let remote_drafts = local_mailbox
            .provider_folder(&folders, LocalFolder::Drafts)
            .map(|folder| folder.id.clone());
        let draft_replay = local_mailbox.replay_drafts(
            remote_drafts.as_ref(),
            &mut online_session,
            &delivery_synchronization.delivered_message_id_hashes,
        )?;
        if let Some(remote_drafts) = remote_drafts
            && draft_replay.remote_changed
        {
            online_session.refresh_folder_info(&remote_drafts)?;
            folders = online_session.list_folders()?;
        }

        let mut refreshed_conversations = HashMap::new();
        for folder_id in &folders_to_refresh {
            let conversations = online_session.list_conversations(folder_id, 0, 0)?;
            refreshed_conversations.insert(folder_id.clone(), conversations);
        }
        local_mailbox.project_folder(
            LocalFolder::Drafts,
            local_mailbox.load_projection(LocalFolder::Drafts)?,
            &mut folders,
            &mut refreshed_conversations,
        );
        let delivery_incomplete = delivery_synchronization.convergence_incomplete;
        local_mailbox.project_delivery(
            delivery_synchronization.outbox,
            delivery_synchronization.sent,
            &mut folders,
            &mut refreshed_conversations,
        );

        development_probe_log!(
            "EDS/Camel refresh rebuilt cache: folders={} conversation_folders={}",
            folders_to_refresh.len(),
            refreshed_conversations.len()
        );
        let projection_stale = self.cached_mailbox.publish_remote(
            binding,
            local_revision,
            folders,
            refreshed_conversations,
            &self.action_queue,
        ) == RemotePublication::Stale;
        if projection_stale {
            development_probe_log!(
                "discarded stale remote cache publication after a concurrent local write"
            );
        }

        Ok(RefreshOutcome {
            convergence_incomplete: delivery_incomplete || draft_replay.convergence_incomplete,
            projection_stale,
        })
    }
}

fn apply_message_flag(
    session: &mut crate::integration::camel::AccountSession,
    conversation_id: &ConversationId,
    change: MessageFlagChange,
) -> anyhow::Result<()> {
    match change {
        MessageFlagChange::Read(read) => session.set_read(conversation_id, read),
        MessageFlagChange::Starred(starred) => session.set_starred(conversation_id, starred),
    }
}

pub(super) fn slice_conversations(
    conversations: &[ConversationSummary],
    offset: usize,
    limit: usize,
) -> Vec<ConversationSummary> {
    if offset >= conversations.len() {
        return Vec::new();
    }

    let end = if limit == 0 {
        conversations.len()
    } else {
        (offset + limit).min(conversations.len())
    };

    conversations[offset..end].to_vec()
}

impl MailBackend for AccountBackend {
    fn account_id(&self) -> &MailAccountId {
        &self.binding.account_id
    }

    fn unresolved_message_action_count(&self) -> usize {
        self.action_queue.len_for_route(&self.binding)
    }

    fn open_change_monitor(
        &self,
        callback: BackendChangeCallback,
    ) -> anyhow::Result<BackendChangeMonitor> {
        Ok(Box::new(
            crate::integration::camel::ChangeMonitor::open(&self.binding, move || callback())?,
        ))
    }

    fn list_folders(&self) -> anyhow::Result<Vec<MailFolder>> {
        self.folders()
    }

    fn list_conversations(
        &self,
        folder_id: &FolderId,
        offset: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<ConversationSummary>> {
        self.conversations(folder_id, offset, limit)
    }

    fn fill_message_cache(&self, conversation_id: &ConversationId) -> anyhow::Result<bool> {
        if self.read_cached_message_detail(conversation_id)?.is_some() {
            return Ok(true);
        }
        if LocalMailbox::new(&self.binding).contains(conversation_id) {
            return Ok(false);
        }
        let mut session = crate::integration::camel::AccountSession::open_online(&self.binding)?;
        Ok(session.get_message_detail(conversation_id)?.is_some())
    }

    fn get_cached_message_detail(
        &self,
        conversation_id: &ConversationId,
    ) -> anyhow::Result<Option<MessageDetail>> {
        self.read_cached_message_detail(conversation_id)
    }

    fn materialize_attachment(
        &self,
        conversation_id: &ConversationId,
        attachment_token: &str,
    ) -> anyhow::Result<Option<String>> {
        let local_mailbox = LocalMailbox::new(&self.binding);
        if local_mailbox.contains(conversation_id) {
            return local_mailbox.export_attachment(conversation_id, attachment_token);
        }

        let cached_session = self.session()?;
        let cached = cached_session
            .lock()
            .expect("camel account session lock poisoned")
            .export_attachment(conversation_id, attachment_token)?;
        if cached.is_some() {
            return Ok(cached);
        }
        let mut session = crate::integration::camel::AccountSession::open_online(&self.binding)?;
        session.export_attachment(conversation_id, attachment_token)
    }

    fn search(&self, query: &str) -> anyhow::Result<Vec<ConversationSummary>> {
        let query = query.to_lowercase();
        let binding = &self.binding;
        let visible_folders = self.folders()?;
        let mut session = crate::integration::camel::AccountSession::open_cached(binding)?;
        let mut eds_matches = session.search_conversations(&query)?;
        let remote_matches = eds_matches.len();
        let local_matches = LocalMailbox::new(binding).search(&query, &visible_folders)?;
        let local_match_count = local_matches.len();
        eds_matches.extend(local_matches);
        eds_matches.extend(
            self.cached_mailbox
                .matching_conversations(|summary| summary_contains_query(summary, &query)),
        );
        self.action_queue
            .apply_to_summaries(binding, &mut eds_matches);
        sort_and_deduplicate_conversations(&mut eds_matches);
        tracing::debug!(
            target: "pigeon::eds",
            query_chars = query.chars().count(),
            remote_matches,
            local_matches = local_match_count,
            results = eds_matches.len(),
            "searched cached mail"
        );
        Ok(eds_matches)
    }

    fn refresh(&self) -> anyhow::Result<RefreshOutcome> {
        self.refresh_cache()
    }

    fn set_starred(
        &self,
        conversation_id: &ConversationId,
        starred: bool,
    ) -> anyhow::Result<WriteOutcome> {
        self.set_message_flag(conversation_id, MessageFlagChange::Starred(starred))
    }

    fn set_read(
        &self,
        conversation_id: &ConversationId,
        read: bool,
    ) -> anyhow::Result<WriteOutcome> {
        self.set_message_flag(conversation_id, MessageFlagChange::Read(read))
    }

    fn move_to_folder(
        &self,
        conversation_id: &ConversationId,
        folder_id: &FolderId,
    ) -> anyhow::Result<WriteOutcome> {
        if LocalMailbox::new(&self.binding).contains(conversation_id) {
            return Err(anyhow!(
                "local message awaiting mailbox reconciliation cannot be moved"
            ));
        }
        let (destination_folder_id, summary) = self
            .cached_mailbox
            .resolve_move(conversation_id, folder_id)?;
        if summary.folder_id == destination_folder_id {
            return Ok(WriteOutcome::Unchanged);
        }
        self.action_queue.queue_move(
            self.binding.clone(),
            MoveIntent {
                destination_folder_id,
                summary,
            },
        );
        self.commit_action_projection();
        Ok(WriteOutcome::Queued)
    }

    fn save_draft(
        &self,
        message: &PreparedMessage,
    ) -> anyhow::Result<Option<StoredMessageRef>> {
        let local_mailbox = LocalMailbox::new(&self.binding);
        let detail = local_mailbox.save_draft(message)?;
        self.refresh_local_projection(&local_mailbox, &[LocalFolder::Drafts]);
        Ok(Some(detail))
    }

    fn queue_delivery(&self, message: &PreparedMessage) -> anyhow::Result<WriteOutcome> {
        if !message.has_recipient() {
            return Err(anyhow!("message has no recipients"));
        }
        let local_mailbox = LocalMailbox::new(&self.binding);
        local_mailbox.queue_delivery(message)?;
        self.refresh_local_projection(
            &local_mailbox,
            &[LocalFolder::Drafts, LocalFolder::Outbox],
        );
        Ok(WriteOutcome::Queued)
    }
}

fn summary_contains_query(summary: &ConversationSummary, query: &str) -> bool {
    let haystack = format!(
        "{} {} {}",
        summary.subject,
        summary.preview,
        summary.participants.join(" ")
    )
    .to_lowercase();
    haystack.contains(&query.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::summary_contains_query;
    use crate::model::mail::{ConversationId, ConversationSummary, FolderId};

    #[test]
    fn cached_summary_search_covers_subject_preview_and_participants_case_insensitively() {
        let summary = ConversationSummary {
            id: ConversationId("folder\u{1f}uid".into()),
            folder_id: FolderId("folder".into()),
            subject: "Quarterly Plan".into(),
            participants: vec!["Example Person <person@example.com>".into()],
            message_count: 1,
            unread_count: 0,
            attachment_count: 0,
            starred: false,
            last_updated_unix_ms: 0,
            preview: "The launch is ready".into(),
        };

        assert!(summary_contains_query(&summary, "quarterly"));
        assert!(summary_contains_query(&summary, "LAUNCH"));
        assert!(summary_contains_query(&summary, "person@example.com"));
        assert!(!summary_contains_query(&summary, "missing"));
    }
}
