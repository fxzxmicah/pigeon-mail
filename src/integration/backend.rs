use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use anyhow::anyhow;
use futures::future::BoxFuture;

use crate::integration::camel::FolderUri;
use crate::integration::journal::{PendingMailActionStore, PendingMove};
use crate::integration::registry::Snapshot;
use crate::integration::stub::StubMailboxStore;
use crate::model::account::{MailAccount, MailAccountId};
use crate::model::mail::{
    ConversationId, ConversationSummary, DraftMessage, FolderId, FolderKind, MailFolder,
    MailboxMode, MessageDetail, StoredMessageRef,
};

mod pool;

pub use pool::lazy_mail_backend;

#[cfg(debug_assertions)]
macro_rules! development_probe_log {
    ($($arg:tt)*) => {
        tracing::debug!(target: "pigeon::development::eds", "{}", format_args!($($arg)*));
    };
}

#[cfg(not(debug_assertions))]
macro_rules! development_probe_log {
    ($($arg:tt)*) => {};
}

pub(crate) struct AccountCatalog {
    accounts: Vec<MailAccount>,
    bindings: Vec<EdsAccountBinding>,
}

pub(crate) fn discover_accounts() -> anyhow::Result<AccountCatalog> {
    let snapshot = crate::integration::registry::load_snapshot_via_ffi()?;
    let catalog = catalog_from_registry(&snapshot);
    tracing::debug!(
        triplets = snapshot.triplets.len(),
        bindings = catalog.bindings.len(),
        "EDS registry topology resolved"
    );
    tracing::info!(
        accounts = catalog.accounts.len(),
        bindings = catalog.bindings.len(),
        "EDS mail accounts discovered"
    );
    Ok(catalog)
}

impl AccountCatalog {
    pub(crate) fn into_parts(self) -> (Vec<MailAccount>, Vec<EdsAccountBinding>) {
        (self.accounts, self.bindings)
    }
}

fn catalog_from_registry(snapshot: &Snapshot) -> AccountCatalog {
    let mut seen = HashSet::new();
    let mut entries = snapshot
        .triplets
        .iter()
        .filter_map(|triplet| {
            let identity = triplet.identity.as_ref()?;
            let transport = triplet.transport.as_ref()?;
            let account_id = usable_goa_triplet_id(triplet)?;
            let primary_address = identity
                .identity_address
                .as_deref()
                .or(triplet.goa_address.as_deref())?
                .trim();
            if primary_address.is_empty() {
                return None;
            }
            if !seen.insert(account_id.to_string()) {
                return None;
            }
            let display_name = identity
                .identity_name
                .as_deref()
                .or(triplet.goa_name.as_deref())
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or(primary_address);

            let mut account = MailAccount {
                id: MailAccountId(account_id.to_string()),
                display_name: display_name.to_string(),
                aliases: vec![crate::model::account::SendingIdentity::with_id(
                    crate::model::account::AliasId(format!("{account_id}:primary")),
                    primary_address.to_string(),
                    display_name.to_string(),
                    identity.identity_reply_to.clone(),
                    format!("<p>{display_name}</p>"),
                    display_name.to_string(),
                    true,
                    true,
                )],
            };
            crate::core::identity::merge_eds_profile(
                &mut account,
                identity.identity_name.as_deref(),
                identity.identity_reply_to.as_deref(),
                identity.identity_aliases.as_deref(),
            );
            let binding = EdsAccountBinding {
                account_id: account.id.clone(),
                account_uid: triplet.account.uid.clone(),
                account_parent_uid: triplet.account.parent.clone()?,
                account_backend_name: triplet.account.backend_name.clone()?,
                account_auth_method: triplet.account.auth_method.clone(),
                identity_uid: identity.uid.clone(),
                transport_uid: transport.uid.clone(),
                transport_backend_name: transport.backend_name.clone()?,
                transport_auth_method: transport.auth_method.clone(),
                drafts_folder: identity
                    .drafts_folder
                    .clone()
                    .or_else(|| triplet.account.drafts_folder.clone()),
                sent_folder: transport
                    .sent_folder
                    .clone()
                    .or_else(|| identity.sent_folder.clone()),
            };
            Some((account, binding))
        })
        .collect::<Vec<_>>();
    entries.sort_by(|(left, _), (right, _)| {
        left.display_name
            .cmp(&right.display_name)
            .then_with(|| left.id.0.cmp(&right.id.0))
    });
    let (accounts, bindings) = entries.into_iter().unzip();
    AccountCatalog { accounts, bindings }
}

fn usable_goa_triplet_id(triplet: &crate::integration::registry::MailTriplet) -> Option<&str> {
    let account = &triplet.account;
    let identity = triplet.identity.as_ref()?;
    let transport = triplet.transport.as_ref()?;
    if triplet.mail_enabled == Some(false)
        || account.uid.is_empty()
        || identity.uid.is_empty()
        || transport.uid.is_empty()
        || account.parent.as_deref().is_none_or(str::is_empty)
        || account.backend_name.as_deref().is_none_or(str::is_empty)
        || transport.backend_name.as_deref().is_none_or(str::is_empty)
    {
        return None;
    }

    let account_id = triplet
        .goa_account_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())?;
    Some(account_id)
}

const LOCAL_MESSAGE_CONVERSATION_ID_SEPARATOR: char = '\u{1f}';

#[derive(Default)]
struct LocalDeliveryView {
    outbox: Vec<ConversationSummary>,
    sent: Vec<ConversationSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CachedDeliveryPlacement {
    Outbox,
    Sent,
    Retire,
}

fn classify_cached_delivery(
    submitted: bool,
    local_sent_fallback: bool,
    message_id_hash: u64,
    unclaimed_remote_sent: &mut HashMap<u64, usize>,
) -> CachedDeliveryPlacement {
    if submitted || local_sent_fallback {
        let matching_count = unclaimed_remote_sent
            .get_mut(&message_id_hash)
            .filter(|count| message_id_hash != 0 && **count > 0);
        if let Some(count) = matching_count {
            *count -= 1;
            CachedDeliveryPlacement::Retire
        } else {
            CachedDeliveryPlacement::Sent
        }
    } else {
        CachedDeliveryPlacement::Outbox
    }
}

pub trait MailBackend: Send + Sync + 'static {
    fn activate_account(
        &self,
        account_id: &MailAccountId,
    ) -> BoxFuture<'_, anyhow::Result<MailboxMode>>;

    fn update_binding_catalog(&self, _: &[EdsAccountBinding], _: &[MailAccountId]) {}

    fn eds_binding(&self, account_id: &MailAccountId) -> Option<EdsAccountBinding>;

    fn list_folders(
        &self,
        account_id: &MailAccountId,
    ) -> BoxFuture<'_, anyhow::Result<Vec<MailFolder>>>;

    fn list_conversations(
        &self,
        account_id: &MailAccountId,
        folder_id: &FolderId,
        offset: usize,
        limit: usize,
    ) -> BoxFuture<'_, anyhow::Result<Vec<ConversationSummary>>>;

    fn get_message_detail(
        &self,
        account_id: &MailAccountId,
        conversation_id: &ConversationId,
    ) -> BoxFuture<'_, anyhow::Result<Option<MessageDetail>>>;

    fn open_attachment(
        &self,
        account_id: &MailAccountId,
        conversation_id: &ConversationId,
        attachment_uri: &str,
    ) -> BoxFuture<'_, anyhow::Result<Option<String>>>;

    fn search(
        &self,
        account_id: &MailAccountId,
        query: &str,
    ) -> BoxFuture<'_, anyhow::Result<Vec<ConversationSummary>>>;

    fn refresh(&self, account_id: &MailAccountId) -> BoxFuture<'_, anyhow::Result<()>>;

    fn set_starred(
        &self,
        account_id: &MailAccountId,
        conversation_id: &ConversationId,
        starred: bool,
    ) -> BoxFuture<'_, anyhow::Result<()>>;

    fn set_read(
        &self,
        account_id: &MailAccountId,
        conversation_id: &ConversationId,
        read: bool,
    ) -> BoxFuture<'_, anyhow::Result<()>>;

    fn move_to_folder(
        &self,
        account_id: &MailAccountId,
        conversation_id: &ConversationId,
        folder_id: &FolderId,
    ) -> BoxFuture<'_, anyhow::Result<()>>;

    fn save_draft(
        &self,
        draft: &DraftMessage,
    ) -> BoxFuture<'_, anyhow::Result<Option<StoredMessageRef>>>;

    fn send_draft(&self, draft: &DraftMessage) -> BoxFuture<'_, anyhow::Result<bool>>;
}

pub type SharedMailBackend = Arc<dyn MailBackend>;

#[derive(Debug, Clone)]
pub struct EdsAccountBinding {
    pub account_id: MailAccountId,
    pub account_uid: String,
    pub account_parent_uid: String,
    pub account_backend_name: String,
    pub account_auth_method: Option<String>,
    pub identity_uid: String,
    pub transport_uid: String,
    pub transport_backend_name: String,
    pub transport_auth_method: Option<String>,
    pub drafts_folder: Option<String>,
    pub sent_folder: Option<String>,
}

fn ensure_eds_local_mail_configuration(binding: &mut EdsAccountBinding) -> anyhow::Result<()> {
    let local_root_path = crate::integration::camel::account_cache_root_for_binding(binding)?;
    let collection_uid = &binding.account_parent_uid;
    let mail_root = std::path::Path::new(&local_root_path)
        .parent()
        .map(|path| path.join("mail").to_string_lossy().into_owned())
        .ok_or_else(|| {
            anyhow!("could not derive built-in local mail root from '{local_root_path}'")
        })?;
    crate::integration::registry::ensure_builtin_local_mail_root(&mail_root)?;
    let drafts_uri = format!("folder://local/{collection_uid}/Drafts");
    let outbox_uri = format!("folder://local/{collection_uid}/Outbox");
    if binding.drafts_folder.as_deref() != Some(drafts_uri.as_str()) {
        crate::integration::registry::ensure_local_drafts_configuration(
            &binding.identity_uid,
            &drafts_uri,
        )?;
    }
    crate::integration::camel::ensure_local_maildir_folders(&[
        drafts_uri.as_str(),
        outbox_uri.as_str(),
    ])?;
    binding.drafts_folder = Some(drafts_uri);
    Ok(())
}

pub fn sync_account_identity_to_eds(
    binding: &EdsAccountBinding,
    account: &MailAccount,
) -> anyhow::Result<()> {
    let identity_uid = binding.identity_uid.as_str();

    let primary_identity = account
        .aliases
        .iter()
        .find(|identity| identity.is_primary_address)
        .or_else(|| account.default_identity());
    let Some(primary_identity) = primary_identity else {
        return Ok(());
    };

    let trimmed_display_name = primary_identity.display_name.trim();
    let name = (!trimmed_display_name.is_empty()).then_some(trimmed_display_name);
    let reply_to = primary_identity
        .reply_to
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let aliases = serialize_identity_aliases(account);

    crate::integration::registry::write_identity_via_ffi(
        identity_uid,
        name,
        reply_to,
        aliases.as_deref(),
    )
}

fn serialize_identity_aliases(account: &MailAccount) -> Option<String> {
    let mut seen = HashSet::new();
    let aliases = account
        .aliases
        .iter()
        .filter(|identity| !identity.is_primary_address)
        .filter_map(|identity| {
            let address = identity.address.trim();
            if address.is_empty() {
                return None;
            }

            let display_name = identity.display_name.trim();
            let key = format!(
                "{}\u{0}{}",
                display_name.to_ascii_lowercase(),
                address.to_ascii_lowercase()
            );
            if !seen.insert(key) {
                return None;
            }

            if display_name.is_empty() || display_name.eq_ignore_ascii_case(address) {
                Some(address.to_string())
            } else {
                Some(format!("{} <{}>", display_name, address))
            }
        })
        .collect::<Vec<_>>();

    (!aliases.is_empty()).then(|| aliases.join(", "))
}

struct BackendSelection {
    backend: SharedMailBackend,
    mode: MailboxMode,
}

pub fn stub_backend() -> SharedMailBackend {
    Arc::new(Backend::new(None)) as SharedMailBackend
}

fn select_mail_backend(
    mut eds_binding: EdsAccountBinding,
    pending_mail_actions: PendingMailActionStore,
) -> BackendSelection {
    let eds_binding = if let Err(error) = ensure_eds_local_mail_configuration(&mut eds_binding) {
        crate::logging::report_failure("local-mail-cache-initialization", &error);
        None
    } else {
        Some(eds_binding)
    };
    let mode = if eds_binding.is_some() {
        MailboxMode::Live
    } else {
        MailboxMode::StubUnavailable
    };
    let backend = Arc::new(Backend::with_pending_actions(
        eds_binding,
        pending_mail_actions,
    )) as SharedMailBackend;
    BackendSelection { backend, mode }
}

#[derive(Clone)]
pub(crate) struct Backend {
    stub_store: Arc<StubMailboxStore>,
    eds_binding: Option<EdsAccountBinding>,
    cached_session: Arc<Mutex<Option<Arc<Mutex<crate::integration::camel::AccountSession>>>>>,
    cached_folders: Arc<Mutex<Option<Vec<MailFolder>>>>,
    cached_conversations: Arc<Mutex<HashMap<String, Vec<ConversationSummary>>>>,
    configured_remote_sent: Arc<Mutex<Option<String>>>,
    pending_mail_actions: PendingMailActionStore,
}

impl Backend {
    pub(crate) fn new(eds_binding: Option<EdsAccountBinding>) -> Self {
        Self::with_pending_actions(eds_binding, PendingMailActionStore::open_default())
    }

    fn with_pending_actions(
        eds_binding: Option<EdsAccountBinding>,
        pending_mail_actions: PendingMailActionStore,
    ) -> Self {
        Self {
            stub_store: Arc::new(StubMailboxStore::seeded()),
            eds_binding,
            cached_session: Arc::new(Mutex::new(None)),
            cached_folders: Arc::new(Mutex::new(None)),
            cached_conversations: Arc::new(Mutex::new(HashMap::new())),
            configured_remote_sent: Arc::new(Mutex::new(None)),
            pending_mail_actions,
        }
    }

    fn live_binding(&self, account_id: &MailAccountId) -> Option<EdsAccountBinding> {
        self.eds_binding(account_id)
    }

    fn pending_moves_for_account(&self, account_id: &MailAccountId) -> Vec<PendingMove> {
        self.pending_mail_actions.for_account(account_id)
    }

    fn queue_pending_move(&self, pending: PendingMove) -> anyhow::Result<()> {
        self.pending_mail_actions.queue_move(pending)
    }

    fn apply_pending_move_overlay(&self, account_id: &MailAccountId) {
        let pending = self.pending_moves_for_account(account_id);
        if pending.is_empty() {
            return;
        }

        let mut conversations = self
            .cached_conversations
            .lock()
            .expect("conversation cache lock poisoned");
        self.pending_mail_actions
            .apply_move_overlay(account_id, &mut conversations);
        drop(conversations);
        self.refresh_cached_folder_counts();
    }

    fn apply_pending_flag_overlay(&self, account_id: &MailAccountId) {
        let mut conversations = self
            .cached_conversations
            .lock()
            .expect("conversation cache lock poisoned");
        self.pending_mail_actions
            .apply_flag_overlay(account_id, &mut conversations);
        drop(conversations);
        self.refresh_cached_folder_counts();
    }

    fn update_cached_conversation(
        &self,
        conversation_id: &ConversationId,
        update: impl Fn(&mut ConversationSummary),
    ) {
        let mut conversations = self
            .cached_conversations
            .lock()
            .expect("conversation cache lock poisoned");
        let mut changed = false;
        for summaries in conversations.values_mut() {
            if let Some(summary) = summaries
                .iter_mut()
                .find(|summary| summary.id == *conversation_id)
            {
                update(summary);
                changed = true;
            }
        }
        drop(conversations);
        if changed {
            self.refresh_cached_folder_counts();
        }
    }

    fn refresh_cached_folder_counts(&self) {
        let unread_counts = self
            .cached_conversations
            .lock()
            .expect("conversation cache lock poisoned")
            .iter()
            .map(|(folder_id, summaries)| {
                (
                    folder_id.clone(),
                    summaries.iter().map(|summary| summary.unread_count).sum(),
                )
            })
            .collect::<HashMap<_, _>>();
        let mut folders = self
            .cached_folders
            .lock()
            .expect("folder cache lock poisoned");
        let Some(folders) = folders.as_mut() else {
            return;
        };
        for folder in folders {
            if let Some(unread_count) = unread_counts.get(&folder.id.0) {
                folder.unread_count = *unread_count;
            }
        }
    }

    fn flush_pending_moves(
        &self,
        account_id: &MailAccountId,
        session: &mut crate::integration::camel::AccountSession,
    ) -> anyhow::Result<()> {
        let pending = self.pending_moves_for_account(account_id);
        if pending.is_empty() {
            return Ok(());
        }

        let mut completed = HashSet::new();
        for move_request in pending {
            match session.move_message(
                &move_request.summary.id,
                &move_request.destination_folder_id,
            ) {
                Ok(()) => {
                    completed.insert(move_request.summary.id);
                }
                Err(error) => {
                    crate::logging::report_deferred("pending-move-sync", &error);
                    development_probe_log!(
                        "pending move remains queued for account {} conversation {}: {}",
                        account_id.0,
                        move_request.summary.id.0,
                        error
                    );
                }
            }
        }

        if completed.is_empty() {
            return Ok(());
        }
        self.pending_mail_actions
            .remove_completed(account_id, &completed)
    }

    fn replay_pending_flags(
        &self,
        account_id: &MailAccountId,
        session: &mut crate::integration::camel::AccountSession,
    ) -> HashSet<ConversationId> {
        let pending = self.pending_mail_actions.flags_for_account(account_id);
        let mut completed = HashSet::new();
        for request in pending {
            let result = (|| -> anyhow::Result<()> {
                if let Some(read) = request.read {
                    session.set_read_for_sync(&request.conversation_id, read)?;
                }
                if let Some(starred) = request.starred {
                    session.set_starred_for_sync(&request.conversation_id, starred)?;
                }
                Ok(())
            })();
            match result {
                Ok(()) => {
                    completed.insert(request.conversation_id);
                }
                Err(error) => {
                    crate::logging::report_deferred("pending-flag-apply", &error);
                    development_probe_log!(
                        "pending flags remain queued for account {} conversation {}: {}",
                        account_id.0,
                        request.conversation_id.0,
                        error
                    );
                }
            }
        }
        completed
    }

    fn discard_pending_moves_without_remote_source(
        &self,
        account_id: &MailAccountId,
        remote_conversations: &HashMap<String, Vec<ConversationSummary>>,
    ) -> anyhow::Result<()> {
        let remote_ids = remote_conversations
            .values()
            .flat_map(|summaries| summaries.iter().map(|summary| summary.id.clone()))
            .collect::<HashSet<_>>();
        self.pending_mail_actions
            .discard_missing_remote_sources(account_id, &remote_ids)
    }

    fn session_for_binding(
        &self,
        binding: &EdsAccountBinding,
    ) -> anyhow::Result<Arc<Mutex<crate::integration::camel::AccountSession>>> {
        let mut cached_session = self
            .cached_session
            .lock()
            .expect("camel session cache lock poisoned");
        if let Some(session) = cached_session.as_ref() {
            return Ok(Arc::clone(session));
        }

        let session = Arc::new(Mutex::new(
            crate::integration::camel::AccountSession::open_cached(binding)?,
        ));
        *cached_session = Some(Arc::clone(&session));
        Ok(session)
    }

    fn clear_live_cache(&self) {
        *self
            .cached_folders
            .lock()
            .expect("folder cache lock poisoned") = None;
        self.cached_conversations
            .lock()
            .expect("conversation cache lock poisoned")
            .clear();
        *self
            .cached_session
            .lock()
            .expect("camel session cache lock poisoned") = None;
    }

    fn refresh_live_cache_for_account(&self, account_id: &MailAccountId) -> anyhow::Result<()> {
        let Some(binding) = self.live_binding(account_id) else {
            self.clear_live_cache();
            return Ok(());
        };

        let mut online_session = crate::integration::camel::AccountSession::open_online(&binding)?;
        let refresh_result = (|| -> anyhow::Result<()> {
            let initial_folders = online_session.list_folders()?;
            self.ensure_remote_sent_configuration(&binding, &initial_folders)?;
            if let Some(remote_drafts) = initial_folders
                .iter()
                .find(|folder| folder.kind == FolderKind::Drafts)
            {
                self.replay_local_drafts(&binding, &remote_drafts.id, &mut online_session)?;
            }
            let mut folders_to_refresh = initial_folders
                .iter()
                .map(|folder| folder.id.0.clone())
                .collect::<Vec<_>>();
            folders_to_refresh.sort();
            folders_to_refresh.dedup();

            // Store synchronization visits folders opened by this session.
            for folder_id in &folders_to_refresh {
                online_session.list_conversations(&FolderId(folder_id.clone()), 0, 0)?;
            }
            let replayed_flags = self.replay_pending_flags(account_id, &mut online_session);
            online_session.synchronize()?;
            if !replayed_flags.is_empty() {
                self.pending_mail_actions
                    .remove_completed_flags(account_id, &replayed_flags)?;
            }
            self.flush_pending_moves(account_id, &mut online_session)?;

            for folder_id in &folders_to_refresh {
                if let Err(error) = online_session.refresh_folder_info(&FolderId(folder_id.clone()))
                {
                    development_probe_log!(
                        "EDS/Camel folder info refresh failed for folder {}: {}",
                        folder_id,
                        error
                    );
                    drop(error);
                }
            }

            let mut folders = online_session.list_folders()?;

            let mut refreshed_conversations = HashMap::new();
            for folder_id in &folders_to_refresh {
                let folder_key = FolderId(folder_id.clone());
                let conversations = online_session.list_conversations(&folder_key, 0, 0)?;
                refreshed_conversations.insert(folder_id.clone(), conversations);
            }

            let remote_conversations = refreshed_conversations.clone();
            self.discard_pending_moves_without_remote_source(account_id, &remote_conversations)?;
            let remote_sent_hashes = folders
                .iter()
                .find(|folder| folder.kind == FolderKind::Sent)
                .map(|folder| online_session.list_message_sync_states(&folder.id))
                .transpose()?
                .unwrap_or_default()
                .into_iter()
                .filter_map(|state| (state.message_id_hash != 0).then_some(state.message_id_hash))
                .fold(HashMap::new(), |mut counts, message_id_hash| {
                    *counts.entry(message_id_hash).or_insert(0) += 1;
                    counts
                });
            let local_delivery = self.replay_local_outbox(&binding, &remote_sent_hashes)?;
            self.merge_local_drafts_source(&binding, &mut folders, &mut refreshed_conversations)?;
            merge_local_delivery_view(&mut folders, &mut refreshed_conversations, local_delivery);

            *self
                .cached_folders
                .lock()
                .expect("folder cache lock poisoned") = Some(folders);

            development_probe_log!(
                "EDS/Camel refresh rebuilt cache: folders={} conversation_folders={}",
                folders_to_refresh.len(),
                refreshed_conversations.len()
            );
            *self
                .cached_conversations
                .lock()
                .expect("conversation cache lock poisoned") = refreshed_conversations;
            self.apply_pending_move_overlay(account_id);
            self.apply_pending_flag_overlay(account_id);

            Ok(())
        })();
        refresh_result
    }

    fn merge_local_drafts_source(
        &self,
        binding: &EdsAccountBinding,
        folders: &mut Vec<MailFolder>,
        conversations: &mut HashMap<String, Vec<ConversationSummary>>,
    ) -> anyhow::Result<()> {
        self.merge_local_folder_source(
            binding.drafts_folder.as_deref(),
            FolderKind::Drafts,
            "Drafts",
            folders,
            conversations,
        )
    }

    fn replay_local_drafts(
        &self,
        binding: &EdsAccountBinding,
        remote_folder_id: &FolderId,
        online_session: &mut crate::integration::camel::AccountSession,
    ) -> anyhow::Result<()> {
        let Some(local_drafts_uri) = binding.drafts_folder.as_deref() else {
            return Ok(());
        };
        let Some(local_drafts) = FolderUri::parse_local(local_drafts_uri) else {
            return Ok(());
        };

        let mut local_session = crate::integration::camel::AccountSession::open_cached_source(
            local_drafts.source_uid(),
            "maildir",
        )?;
        let local_folder_id = FolderId(local_drafts.folder_path().to_string());
        local_session.ensure_folder_path(&local_folder_id)?;
        let remote_by_message_id = online_session
            .list_message_sync_states(remote_folder_id)?
            .into_iter()
            .filter(|state| state.message_id_hash != 0)
            .fold(
                HashMap::<u64, Vec<ConversationId>>::new(),
                |mut matches, state| {
                    matches
                        .entry(state.message_id_hash)
                        .or_default()
                        .push(state.conversation_id);
                    matches
                },
            );
        let states = local_session.list_message_sync_states(&local_folder_id)?;
        let mut uploaded = 0usize;
        let mut retired_local = 0usize;
        for state in states {
            if state.draft_synced {
                local_session.delete_message_permanently(&state.conversation_id)?;
                retired_local += 1;
                continue;
            }
            match online_session.append_cached_draft_from(
                &mut local_session,
                &state.conversation_id,
                remote_folder_id,
            ) {
                Ok(()) => {
                    // Persist acknowledgement before cleanup. If the process stops
                    // here, the next refresh retires the local copy without another
                    // remote append.
                    local_session.set_draft_synced(&state.conversation_id, true)?;
                    for previous in remote_by_message_id
                        .get(&state.message_id_hash)
                        .into_iter()
                        .flatten()
                    {
                        if let Err(error) = online_session.delete_message_permanently(previous) {
                            crate::logging::report_deferred("remote-draft-replace-cleanup", &error);
                        }
                    }
                    local_session.delete_message_permanently(&state.conversation_id)?;
                    uploaded += 1;
                    retired_local += 1;
                }
                Err(error) => {
                    crate::logging::report_deferred("remote-draft-upload", &error);
                    development_probe_log!(
                        "local draft remains queued because remote append failed: {}",
                        error
                    );
                }
            }
        }
        local_session.synchronize()?;
        tracing::debug!(
            target: "pigeon::eds",
            pending = local_session
                .list_message_sync_states(&local_folder_id)?
                .len(),
            uploaded,
            retired_local,
            "reconciled cached drafts"
        );
        Ok(())
    }

    fn ensure_remote_sent_configuration(
        &self,
        binding: &EdsAccountBinding,
        folders: &[MailFolder],
    ) -> anyhow::Result<()> {
        let identity_uid = binding.identity_uid.as_str();
        let account_uid = binding.account_uid.as_str();
        let Some(sent_folder) = folders
            .iter()
            .find(|folder| folder.kind == FolderKind::Sent)
        else {
            return Ok(());
        };
        let sent_uri = crate::integration::camel::folder_uri(account_uid, &sent_folder.id)?;
        let already_configured = self
            .configured_remote_sent
            .lock()
            .expect("remote Sent configuration lock poisoned")
            .as_ref()
            .is_some_and(|configured| configured == &sent_uri);
        if !already_configured && binding.sent_folder.as_deref() != Some(sent_uri.as_str()) {
            crate::integration::registry::set_sent_folder_configuration(identity_uid, &sent_uri)?;
        }
        *self
            .configured_remote_sent
            .lock()
            .expect("remote Sent configuration lock poisoned") = Some(sent_uri);
        Ok(())
    }

    fn merge_local_folder_source(
        &self,
        folder_uri: Option<&str>,
        kind: FolderKind,
        fallback_name: &str,
        folders: &mut Vec<MailFolder>,
        conversations: &mut HashMap<String, Vec<ConversationSummary>>,
    ) -> anyhow::Result<()> {
        let Some(folder_uri) = folder_uri else {
            return Ok(());
        };
        let Some(folder_uri) = FolderUri::parse_local(folder_uri) else {
            return Ok(());
        };

        let mut session = crate::integration::camel::AccountSession::open_cached_source(
            folder_uri.source_uid(),
            "maildir",
        )?;
        let local_folder_id = FolderId(folder_uri.folder_path().to_string());
        session.ensure_folder_path(&local_folder_id)?;
        session.refresh_folder_info(&local_folder_id)?;
        let local_conversations = session.list_conversations(&local_folder_id, 0, 0)?;

        let display_folder_id = folders
            .iter()
            .find(|folder| folder.kind == kind)
            .map(|folder| folder.id.clone())
            .unwrap_or_else(|| FolderId(folder_uri.folder_path().to_string()));
        if !folders.iter().any(|folder| folder.id == display_folder_id) {
            folders.push(MailFolder {
                id: display_folder_id.clone(),
                name: fallback_name.into(),
                unread_count: 0,
                kind,
            });
        }

        let merged = conversations
            .entry(display_folder_id.0.clone())
            .or_default();
        merged.extend(local_conversations);
        merged.sort_by(|left, right| {
            right
                .last_updated_unix_ms
                .cmp(&left.last_updated_unix_ms)
                .then_with(|| left.subject.cmp(&right.subject))
        });
        merged.dedup_by(|left, right| left.id == right.id);
        if let Some(folder) = folders
            .iter_mut()
            .find(|folder| folder.id == display_folder_id)
        {
            folder.unread_count = merged
                .iter()
                .map(|conversation| conversation.unread_count)
                .sum();
        }
        Ok(())
    }

    fn replay_local_outbox(
        &self,
        binding: &EdsAccountBinding,
        remote_sent_hashes: &HashMap<u64, usize>,
    ) -> anyhow::Result<LocalDeliveryView> {
        let Some(outbox_folder_id) =
            local_sibling_folder_id(binding.drafts_folder.as_deref(), "Outbox")
        else {
            return Ok(LocalDeliveryView::default());
        };
        let mut local_session =
            crate::integration::camel::AccountSession::open_cached_source("local", "maildir")?;
        let local_sent_folder_id =
            local_sibling_folder_id(binding.drafts_folder.as_deref(), "Sent")
                .ok_or_else(|| anyhow!("could not resolve local EDS Sent fallback"))?;
        local_session.ensure_folder_path(&outbox_folder_id)?;
        let queued = local_session.list_conversations(&outbox_folder_id, 0, 0)?;
        let sync_states = local_session
            .list_message_sync_states(&outbox_folder_id)?
            .into_iter()
            .map(|state| (state.conversation_id.clone(), state))
            .collect::<HashMap<_, _>>();

        let mut unclaimed_remote_sent = remote_sent_hashes.clone();
        let mut retired_outbox = 0usize;
        for state in queued
            .iter()
            .filter_map(|summary| sync_states.get(&summary.id))
        {
            if classify_cached_delivery(
                state.submitted,
                state.local_sent_fallback,
                state.message_id_hash,
                &mut unclaimed_remote_sent,
            ) == CachedDeliveryPlacement::Retire
            {
                local_session.delete_message_permanently(&state.conversation_id)?;
                retired_outbox += 1;
            }
        }

        let unsent = queued
            .iter()
            .filter(|summary| {
                sync_states
                    .get(&summary.id)
                    .is_none_or(|state| !state.submitted)
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut transport = if unsent.is_empty() {
            None
        } else {
            match crate::integration::camel::TransportSession::open_online(binding) {
                Ok(transport) => Some(transport),
                Err(error) => {
                    crate::logging::report_deferred("outbox-connect", &error);
                    development_probe_log!(
                        "Outbox remains queued because transport is unavailable: {}",
                        error
                    );
                    None
                }
            }
        };
        for summary in unsent {
            let Some(transport) = transport.as_mut() else {
                break;
            };
            match transport.send_cached_message(&mut local_session, &summary.id) {
                Ok(sent_message_saved) => {
                    if sent_message_saved {
                        // Submission acknowledgement stops retries immediately.  The durable
                        // Outbox copy is retired only after its Message-ID appears in remote Sent.
                        local_session.set_submitted(&summary.id, true)?;
                    } else {
                        // A successful send must never be retried. Providers without a
                        // server-side Sent facility retain the sole copy in local Maildir.
                        local_session.set_local_sent_fallback(&summary.id)?;
                        local_session.ensure_folder_path(&local_sent_folder_id)?;
                        local_session.move_message(&summary.id, &local_sent_folder_id)?;
                    }
                }
                Err(error) => {
                    crate::logging::report_deferred("outbox-send", &error);
                    development_probe_log!("queued message remains in Outbox: {}", error);
                }
            }
        }
        local_session.synchronize()?;

        let remaining_outbox = local_session.list_conversations(&outbox_folder_id, 0, 0)?;
        let remaining_states = local_session
            .list_message_sync_states(&outbox_folder_id)?
            .into_iter()
            .map(|state| (state.conversation_id, state.submitted))
            .collect::<HashMap<_, _>>();
        let (submitted, unsent): (Vec<ConversationSummary>, Vec<ConversationSummary>) =
            remaining_outbox
                .into_iter()
                .partition(|summary| remaining_states.get(&summary.id).copied().unwrap_or(false));

        local_session.ensure_folder_path(&local_sent_folder_id)?;
        let local_sent_states = local_session.list_message_sync_states(&local_sent_folder_id)?;
        let mut retired_local_sent = 0usize;
        for state in local_sent_states
            .iter()
            .filter(|state| state.local_sent_fallback)
        {
            if classify_cached_delivery(
                state.submitted,
                state.local_sent_fallback,
                state.message_id_hash,
                &mut unclaimed_remote_sent,
            ) == CachedDeliveryPlacement::Retire
            {
                local_session.delete_message_permanently(&state.conversation_id)?;
                retired_local_sent += 1;
            }
        }
        let fallback_states = local_session
            .list_message_sync_states(&local_sent_folder_id)?
            .into_iter()
            .filter_map(|state| state.local_sent_fallback.then_some(state.conversation_id))
            .collect::<HashSet<_>>();
        let mut sent = local_session
            .list_conversations(&local_sent_folder_id, 0, 0)?
            .into_iter()
            .filter(|summary| fallback_states.contains(&summary.id))
            .collect::<Vec<_>>();
        sent.extend(submitted);
        tracing::debug!(
            target: "pigeon::eds",
            remote_sent = remote_sent_hashes.values().sum::<usize>(),
            pending_outbox = unsent.len(),
            pending_sent = sent.len(),
            retired_outbox,
            retired_local_sent,
            "reconciled cached delivery state"
        );
        Ok(LocalDeliveryView {
            outbox: unsent,
            sent,
        })
    }
}

fn merge_local_delivery_view(
    folders: &mut Vec<MailFolder>,
    conversations: &mut HashMap<String, Vec<ConversationSummary>>,
    local: LocalDeliveryView,
) {
    for (kind, local_conversations) in [
        (FolderKind::Outbox, local.outbox),
        (FolderKind::Sent, local.sent),
    ] {
        if local_conversations.is_empty() {
            continue;
        }
        let display_folder_id = folders
            .iter()
            .find(|folder| folder.kind == kind)
            .map(|folder| folder.id.clone())
            .or_else(|| {
                local_conversations
                    .first()
                    .and_then(|summary| conversation_local_folder_id(&summary.id))
                    .map(|folder_id| FolderId(folder_id.to_string()))
            });
        let Some(display_folder_id) = display_folder_id else {
            continue;
        };
        if !folders.iter().any(|folder| folder.id == display_folder_id) {
            folders.push(MailFolder {
                id: display_folder_id.clone(),
                name: match kind {
                    FolderKind::Outbox => "Outbox",
                    FolderKind::Sent => "Sent",
                    _ => unreachable!("local delivery only targets Outbox or Sent"),
                }
                .into(),
                unread_count: 0,
                kind,
            });
        }
        let merged = conversations
            .entry(display_folder_id.0.clone())
            .or_default();
        merged.extend(local_conversations);
        merged.sort_by(|left, right| {
            right
                .last_updated_unix_ms
                .cmp(&left.last_updated_unix_ms)
                .then_with(|| left.subject.cmp(&right.subject))
        });
        merged.dedup_by(|left, right| left.id == right.id);
        if let Some(folder) = folders
            .iter_mut()
            .find(|folder| folder.id == display_folder_id)
        {
            folder.unread_count = merged
                .iter()
                .map(|conversation| conversation.unread_count)
                .sum();
        }
    }
}

fn append_local_message(
    folder_id: &FolderId,
    draft: &DraftMessage,
    is_draft: bool,
) -> anyhow::Result<StoredMessageRef> {
    let mut session =
        crate::integration::camel::AccountSession::open_cached_source("local", "maildir")?;
    session.ensure_folder_path(folder_id)?;
    let attachment_uris = draft
        .attachments
        .iter()
        .map(|attachment| attachment.uri.clone())
        .collect::<Vec<_>>();
    let request = crate::integration::camel::AppendMessageRequest {
        message_id: draft
            .message_id
            .as_ref()
            .map(|message_id| message_id.0.as_str()),
        folder_id,
        from: &draft.from,
        reply_to: draft.reply_to.as_deref(),
        to: &draft.to,
        cc: &draft.cc,
        bcc: &draft.bcc,
        subject: &draft.subject,
        html_body: &draft.html_body,
        plain_body: &draft.text_body,
        attachment_uris: &attachment_uris,
        is_draft,
    };

    let stored = session.append_message(&request).map_err(|error| {
        anyhow!(
            "EDS local message append failed for folder_name='{}': {}",
            folder_id.0,
            error
        )
    })?;

    let stored = stored.ok_or_else(|| {
        anyhow!(
            "EDS local message append returned no message detail for folder_name='{}'",
            folder_id.0
        )
    })?;

    if let Some(previous_id) = draft.conversation_id.as_ref()
        && previous_id != &stored.conversation_id
        && replaces_local_draft(previous_id, &folder_id.0)
        && let Err(error) = session.delete_message_permanently(previous_id)
    {
        // The new MIME is already durable. Reporting the whole save as failed
        // would cause a retry to append yet another copy, so retain the new
        // version and diagnose only the stale-version cleanup.
        crate::logging::report_failure("draft-cache-replace-cleanup", &error);
    }

    Ok(stored)
}

fn replaces_local_draft(previous_id: &ConversationId, destination_folder: &str) -> bool {
    let Some(previous_folder) = conversation_local_folder_id(previous_id) else {
        return false;
    };
    let (previous_parent, previous_leaf) = previous_folder
        .rsplit_once('/')
        .map_or(("", previous_folder), |(parent, leaf)| (parent, leaf));
    let destination_parent = destination_folder
        .rsplit_once('/')
        .map_or("", |(parent, _)| parent);
    previous_leaf == "Drafts" && previous_parent == destination_parent
}

fn local_sibling_folder_id(configured_uri: Option<&str>, sibling_name: &str) -> Option<FolderId> {
    let configured = FolderUri::parse_local(configured_uri?)?;
    let parent = configured
        .folder_path()
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or("");
    let sibling_path = if parent.is_empty() {
        sibling_name.to_string()
    } else {
        format!("{parent}/{sibling_name}")
    };
    Some(FolderId(sibling_path))
}

fn is_local_conversation_id(binding: &EdsAccountBinding, conversation_id: &ConversationId) -> bool {
    conversation_belongs_to_local_collection(binding.drafts_folder.as_deref(), conversation_id)
}

fn conversation_belongs_to_local_collection(
    drafts_uri: Option<&str>,
    conversation_id: &ConversationId,
) -> bool {
    let Some(local_folder) = conversation_local_folder_id(conversation_id) else {
        return false;
    };
    let Some(drafts_uri) = drafts_uri.and_then(FolderUri::parse_local) else {
        return false;
    };
    let drafts_folder = drafts_uri.folder_path();
    match drafts_folder.rsplit_once('/') {
        Some((collection, _)) if !collection.is_empty() => {
            local_folder == collection || local_folder.starts_with(&format!("{collection}/"))
        }
        _ => matches!(local_folder, "Drafts" | "Outbox" | "Sent"),
    }
}

fn conversation_local_folder_id(conversation_id: &ConversationId) -> Option<&str> {
    conversation_id
        .0
        .split_once(LOCAL_MESSAGE_CONVERSATION_ID_SEPARATOR)
        .map(|(folder_name, _)| folder_name)
}

fn refresh_local_conversation_folder(
    session: &mut crate::integration::camel::AccountSession,
    conversation_id: &ConversationId,
) -> anyhow::Result<()> {
    let folder_name = conversation_local_folder_id(conversation_id)
        .ok_or_else(|| anyhow!("local conversation id has no folder component"))?;
    let local_folder_id = FolderId(folder_name.to_string());
    session.refresh_folder_info(&local_folder_id)
}

fn slice_conversations(
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

fn allow_stub_write(account_id: &MailAccountId) -> anyhow::Result<()> {
    if crate::integration::stub::is_stub_account_id(account_id) {
        Ok(())
    } else {
        Err(anyhow!(
            "mail cache is unavailable for account '{}'",
            account_id.0
        ))
    }
}

impl MailBackend for Backend {
    fn activate_account(
        &self,
        account_id: &MailAccountId,
    ) -> BoxFuture<'_, anyhow::Result<MailboxMode>> {
        let mode = if self.live_binding(account_id).is_some() {
            MailboxMode::Live
        } else {
            MailboxMode::StubUnavailable
        };
        Box::pin(async move { Ok(mode) })
    }

    fn eds_binding(&self, account_id: &MailAccountId) -> Option<EdsAccountBinding> {
        self.eds_binding
            .as_ref()
            .filter(|binding| binding.account_id == *account_id)
            .cloned()
    }

    fn list_folders(
        &self,
        account_id: &MailAccountId,
    ) -> BoxFuture<'_, anyhow::Result<Vec<MailFolder>>> {
        let store = Arc::clone(&self.stub_store);
        let account_id = account_id.clone();
        let eds_binding = self.live_binding(&account_id);
        let cached_folders = Arc::clone(&self.cached_folders);
        let this = self.clone();
        Box::pin(async move {
            if let Some(binding) = &eds_binding {
                if let Some(cached) = cached_folders
                    .lock()
                    .expect("folder cache lock poisoned")
                    .clone()
                {
                    return Ok(cached);
                }
                let session = this.session_for_binding(binding)?;
                let folders = session
                    .lock()
                    .expect("camel account session lock poisoned")
                    .list_folders()?;
                *cached_folders.lock().expect("folder cache lock poisoned") = Some(folders.clone());
                return Ok(folders);
            }

            Ok(store.folders())
        })
    }

    fn list_conversations(
        &self,
        account_id: &MailAccountId,
        folder_id: &FolderId,
        offset: usize,
        limit: usize,
    ) -> BoxFuture<'_, anyhow::Result<Vec<ConversationSummary>>> {
        let store = Arc::clone(&self.stub_store);
        let account_id = account_id.clone();
        let folder_id = folder_id.clone();
        let eds_binding = self.live_binding(&account_id);
        let cached_conversations = Arc::clone(&self.cached_conversations);
        let this = self.clone();
        Box::pin(async move {
            if let Some(binding) = &eds_binding {
                if let Some(cached) = cached_conversations
                    .lock()
                    .expect("conversation cache lock poisoned")
                    .get(&folder_id.0)
                    .cloned()
                {
                    return Ok(slice_conversations(&cached, offset, limit));
                }
                let session = this.session_for_binding(binding)?;
                let conversations = session
                    .lock()
                    .expect("camel account session lock poisoned")
                    .list_conversations(&folder_id, 0, 0)?;
                cached_conversations
                    .lock()
                    .expect("conversation cache lock poisoned")
                    .insert(folder_id.0.clone(), conversations);
                this.apply_pending_move_overlay(&account_id);
                this.apply_pending_flag_overlay(&account_id);
                let conversations = cached_conversations
                    .lock()
                    .expect("conversation cache lock poisoned")
                    .get(&folder_id.0)
                    .cloned()
                    .unwrap_or_default();
                return Ok(slice_conversations(&conversations, offset, limit));
            }

            let conversations = store.conversations(&folder_id);
            Ok(slice_conversations(&conversations, offset, limit))
        })
    }

    fn get_message_detail(
        &self,
        account_id: &MailAccountId,
        conversation_id: &ConversationId,
    ) -> BoxFuture<'_, anyhow::Result<Option<MessageDetail>>> {
        let store = Arc::clone(&self.stub_store);
        let account_id = account_id.clone();
        let conversation_id = conversation_id.clone();
        let eds_binding = self.live_binding(&account_id);
        let this = self.clone();
        Box::pin(async move {
            if let Some(binding) = eds_binding {
                if is_local_conversation_id(&binding, &conversation_id) {
                    let mut session =
                        crate::integration::camel::AccountSession::open_online_source(
                            "local", "maildir",
                        )?;
                    refresh_local_conversation_folder(&mut session, &conversation_id)?;
                    return session.get_message_detail(&conversation_id);
                }
                match this.session_for_binding(&binding) {
                    Ok(session) => {
                        let mut session = session.lock().expect("camel session lock poisoned");
                        match session.get_message_detail(&conversation_id) {
                            Ok(Some(detail)) => return Ok(Some(detail)),
                            Ok(None) => {}
                            Err(error) => {
                                development_probe_log!(
                                    "EDS/Camel detail load failed for conversation {}: {}",
                                    conversation_id.0,
                                    error
                                );
                                drop(error);
                            }
                        }
                    }
                    Err(error) => {
                        development_probe_log!(
                            "EDS/Camel session open failed while loading detail {}: {}",
                            conversation_id.0,
                            error
                        );
                        drop(error);
                    }
                }

                match crate::integration::camel::AccountSession::open_online(&binding) {
                    Ok(mut session) => match session.get_message_detail(&conversation_id) {
                        Ok(detail) => return Ok(detail),
                        Err(error) => {
                            return Err(anyhow!(
                                "EDS/Camel on-demand detail load failed for conversation {}: {}",
                                conversation_id.0,
                                error
                            ));
                        }
                    },
                    Err(error) => {
                        return Err(anyhow!(
                            "EDS/Camel online session open failed while loading detail {}: {}",
                            conversation_id.0,
                            error
                        ));
                    }
                }
            }

            Ok(store.message_detail(&conversation_id.0))
        })
    }

    fn open_attachment(
        &self,
        account_id: &MailAccountId,
        conversation_id: &ConversationId,
        attachment_uri: &str,
    ) -> BoxFuture<'_, anyhow::Result<Option<String>>> {
        let account_id = account_id.clone();
        let conversation_id = conversation_id.clone();
        let attachment_uri = attachment_uri.to_string();
        let eds_binding = self.live_binding(&account_id);
        Box::pin(async move {
            if let Some(binding) = eds_binding {
                if is_local_conversation_id(&binding, &conversation_id) {
                    let mut session =
                        crate::integration::camel::AccountSession::open_online_source(
                            "local", "maildir",
                        )?;
                    refresh_local_conversation_folder(&mut session, &conversation_id)?;
                    return session.export_attachment(&conversation_id, &attachment_uri);
                }

                match self.session_for_binding(&binding) {
                    Ok(session) => {
                        let result = session
                            .lock()
                            .expect("camel account session lock poisoned")
                            .export_attachment(&conversation_id, &attachment_uri);
                        match result {
                            Ok(uri) => return Ok(uri),
                            Err(error) => {
                                development_probe_log!(
                                    "cached attachment export failed for conversation {} attachment {}: {}",
                                    conversation_id.0,
                                    attachment_uri,
                                    error
                                );
                                drop(error);
                            }
                        }
                    }
                    Err(error) => {
                        development_probe_log!(
                            "cached session open failed while exporting attachment {}: {}",
                            attachment_uri,
                            error
                        );
                        drop(error);
                    }
                }

                let mut session = crate::integration::camel::AccountSession::open_online(&binding)
                    .map_err(|error| {
                        anyhow!(
                            "online session open failed while exporting attachment {}: {}",
                            attachment_uri,
                            error
                        )
                    })?;
                return session
                    .export_attachment(&conversation_id, &attachment_uri)
                    .map_err(|error| {
                        anyhow!(
                            "on-demand attachment export failed for conversation {} attachment {}: {}",
                            conversation_id.0,
                            attachment_uri,
                            error
                        )
                    });
            }

            Ok((attachment_uri.contains("://")
                && !attachment_uri.starts_with("pigeon-eds-attachment:"))
            .then_some(attachment_uri))
        })
    }

    fn search(
        &self,
        account_id: &MailAccountId,
        query: &str,
    ) -> BoxFuture<'_, anyhow::Result<Vec<ConversationSummary>>> {
        let store = Arc::clone(&self.stub_store);
        let cached_conversations = Arc::clone(&self.cached_conversations);
        let account_id = account_id.clone();
        let eds_binding = self.live_binding(&account_id);
        let query = query.to_lowercase();
        Box::pin(async move {
            if let Some(binding) = eds_binding {
                let mut session = crate::integration::camel::AccountSession::open_cached(&binding)?;
                let mut eds_matches = session.search_conversations(&query)?;
                let remote_matches = eds_matches.len();
                let local_matches = search_local_mail_cache(&binding, &query)?;
                let local_match_count = local_matches.len();
                eds_matches.extend(local_matches);
                eds_matches.extend(
                    cached_conversations
                        .lock()
                        .expect("conversation cache lock poisoned")
                        .values()
                        .flat_map(|conversations| conversations.iter())
                        .filter(|summary| summary_contains_query(summary, &query))
                        .cloned(),
                );
                eds_matches.sort_by(|left, right| {
                    right
                        .last_updated_unix_ms
                        .cmp(&left.last_updated_unix_ms)
                        .then_with(|| left.subject.cmp(&right.subject))
                });
                let mut seen = HashSet::new();
                eds_matches.retain(|summary| seen.insert(summary.id.clone()));
                tracing::debug!(
                    target: "pigeon::eds",
                    query_chars = query.chars().count(),
                    remote_matches,
                    local_matches = local_match_count,
                    results = eds_matches.len(),
                    "searched cached mail"
                );
                return Ok(eds_matches);
            }

            Ok(store.search(&query))
        })
    }

    fn refresh(&self, account_id: &MailAccountId) -> BoxFuture<'_, anyhow::Result<()>> {
        let backend = self.clone();
        let account_id = account_id.clone();
        Box::pin(async move { backend.refresh_live_cache_for_account(&account_id) })
    }

    fn set_starred(
        &self,
        account_id: &MailAccountId,
        conversation_id: &ConversationId,
        starred: bool,
    ) -> BoxFuture<'_, anyhow::Result<()>> {
        let account_id = account_id.clone();
        let conversation_id = conversation_id.clone();
        let eds_binding = self.live_binding(&account_id);
        let this = self.clone();
        Box::pin(async move {
            if let Some(binding) = &eds_binding {
                if is_local_conversation_id(binding, &conversation_id) {
                    let mut session =
                        crate::integration::camel::AccountSession::open_cached_source(
                            "local", "maildir",
                        )?;
                    refresh_local_conversation_folder(&mut session, &conversation_id)?;
                    session.set_starred(&conversation_id, starred)?;
                } else {
                    this.session_for_binding(binding)?
                        .lock()
                        .expect("camel account session lock poisoned")
                        .set_starred(&conversation_id, starred)?;
                    this.pending_mail_actions.queue_starred(
                        account_id.clone(),
                        conversation_id.clone(),
                        starred,
                    )?;
                }
                this.update_cached_conversation(&conversation_id, |conversation| {
                    conversation.starred = starred;
                });
                return Ok(());
            }
            allow_stub_write(&account_id)
        })
    }

    fn set_read(
        &self,
        account_id: &MailAccountId,
        conversation_id: &ConversationId,
        read: bool,
    ) -> BoxFuture<'_, anyhow::Result<()>> {
        let account_id = account_id.clone();
        let conversation_id = conversation_id.clone();
        let eds_binding = self.live_binding(&account_id);
        let this = self.clone();
        Box::pin(async move {
            if let Some(binding) = &eds_binding {
                if is_local_conversation_id(binding, &conversation_id) {
                    let mut session =
                        crate::integration::camel::AccountSession::open_cached_source(
                            "local", "maildir",
                        )?;
                    refresh_local_conversation_folder(&mut session, &conversation_id)?;
                    session.set_read(&conversation_id, read)?;
                } else {
                    this.session_for_binding(binding)?
                        .lock()
                        .expect("camel account session lock poisoned")
                        .set_read(&conversation_id, read)?;
                    this.pending_mail_actions.queue_read(
                        account_id.clone(),
                        conversation_id.clone(),
                        read,
                    )?;
                }
                this.update_cached_conversation(&conversation_id, |conversation| {
                    conversation.unread_count = if read { 0 } else { 1 };
                });
                return Ok(());
            }
            allow_stub_write(&account_id)
        })
    }

    fn move_to_folder(
        &self,
        account_id: &MailAccountId,
        conversation_id: &ConversationId,
        folder_id: &FolderId,
    ) -> BoxFuture<'_, anyhow::Result<()>> {
        let cached_conversations = Arc::clone(&self.cached_conversations);
        let account_id = account_id.clone();
        let conversation_id = conversation_id.clone();
        let folder_id = folder_id.clone();
        let eds_binding = self.live_binding(&account_id);
        let this = self.clone();
        Box::pin(async move {
            if let Some(binding) = eds_binding {
                if is_local_conversation_id(&binding, &conversation_id) {
                    return Err(anyhow!(
                        "a locally queued delivery cannot be moved before remote reconciliation"
                    ));
                }
                let destination_folder_id = {
                    let folders = this
                        .cached_folders
                        .lock()
                        .expect("folder cache lock poisoned");
                    folders
                        .as_ref()
                        .and_then(|folders| {
                            folders.iter().find(|folder| {
                                folder.id == folder_id
                                    || (folder_id.0 == "archive"
                                        && matches!(folder.kind, FolderKind::Archive))
                                    || (folder_id.0 == "trash"
                                        && matches!(folder.kind, FolderKind::Trash))
                            })
                        })
                        .map(|folder| folder.id.clone())
                }
                .ok_or_else(|| anyhow!("could not resolve destination folder '{}'", folder_id.0))?;
                let summary = cached_conversations
                    .lock()
                    .expect("conversation cache lock poisoned")
                    .values()
                    .flat_map(|conversations| conversations.iter())
                    .find(|conversation| conversation.id == conversation_id)
                    .cloned()
                    .ok_or_else(|| {
                        anyhow!(
                            "could not locate cached conversation '{}' before moving it",
                            conversation_id.0
                        )
                    })?;
                this.queue_pending_move(PendingMove {
                    account_id: account_id.clone(),
                    destination_folder_id,
                    summary,
                })?;
                this.apply_pending_move_overlay(&account_id);
                return Ok(());
            }
            allow_stub_write(&account_id)
        })
    }

    fn save_draft(
        &self,
        draft: &DraftMessage,
    ) -> BoxFuture<'_, anyhow::Result<Option<StoredMessageRef>>> {
        let account_id = draft.account_id.clone();
        let draft = draft.clone();
        let backend = self.clone();
        Box::pin(async move {
            let Some(binding) = backend.live_binding(&account_id) else {
                allow_stub_write(&account_id)?;
                return Ok(None);
            };
            let drafts_folder_uri = binding
                .drafts_folder
                .as_deref()
                .ok_or_else(|| anyhow!("EDS binding has no configured drafts folder"))?;
            let drafts_folder = FolderUri::parse_local(drafts_folder_uri)
                .map(|uri| FolderId(uri.folder_path().to_string()))
                .ok_or_else(|| anyhow!("EDS binding has no local Drafts configuration"))?;
            let detail = append_local_message(&drafts_folder, &draft, true)?;
            {
                let mut folders = backend
                    .cached_folders
                    .lock()
                    .expect("folder cache lock poisoned")
                    .clone()
                    .unwrap_or_default();
                let mut conversations = backend
                    .cached_conversations
                    .lock()
                    .expect("conversation cache lock poisoned")
                    .clone();
                backend.merge_local_drafts_source(&binding, &mut folders, &mut conversations)?;
                *backend
                    .cached_folders
                    .lock()
                    .expect("folder cache lock poisoned") = Some(folders);
                *backend
                    .cached_conversations
                    .lock()
                    .expect("conversation cache lock poisoned") = conversations;
            }
            Ok(Some(detail))
        })
    }

    fn send_draft(&self, draft: &DraftMessage) -> BoxFuture<'_, anyhow::Result<bool>> {
        let backend = self.clone();
        let account_id = draft.account_id.clone();
        let draft = draft.clone();
        Box::pin(async move {
            if let Some(binding) = backend.live_binding(&account_id) {
                let outbox_folder =
                    local_sibling_folder_id(binding.drafts_folder.as_deref(), "Outbox")
                        .ok_or_else(|| anyhow!("EDS binding has no local Outbox configuration"))?;
                append_local_message(&outbox_folder, &draft, false)?;
                return Ok(true);
            }
            allow_stub_write(&account_id)?;
            Ok(false)
        })
    }
}

fn search_local_mail_cache(
    binding: &EdsAccountBinding,
    query: &str,
) -> anyhow::Result<Vec<ConversationSummary>> {
    let Some(drafts_uri) = binding.drafts_folder.as_deref() else {
        return Ok(Vec::new());
    };
    let Some(drafts_target) = FolderUri::parse_local(drafts_uri) else {
        return Ok(Vec::new());
    };

    let mut session = crate::integration::camel::AccountSession::open_cached_source(
        drafts_target.source_uid(),
        "maildir",
    )?;
    let mut results = Vec::new();
    let drafts_folder = FolderId(drafts_target.folder_path().to_string());
    let outbox_folder = local_sibling_folder_id(Some(drafts_uri), "Outbox");
    let sent_folder = local_sibling_folder_id(Some(drafts_uri), "Sent");

    for folder in std::iter::once(drafts_folder).chain(outbox_folder) {
        session.ensure_folder_path(&folder)?;
        results.extend(session.search_folder_conversations(&folder, query)?);
    }
    if let Some(sent_folder) = sent_folder {
        session.ensure_folder_path(&sent_folder)?;
        let visible_fallback_ids = session
            .list_message_sync_states(&sent_folder)?
            .into_iter()
            .filter_map(|state| state.local_sent_fallback.then_some(state.conversation_id))
            .collect::<HashSet<_>>();
        results.extend(
            session
                .search_folder_conversations(&sent_folder, query)?
                .into_iter()
                .filter(|summary| visible_fallback_ids.contains(&summary.id)),
        );
    }
    Ok(results)
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
    use std::collections::HashMap;

    use super::{
        Backend, CachedDeliveryPlacement, EdsAccountBinding, MailBackend, classify_cached_delivery,
        conversation_belongs_to_local_collection, local_sibling_folder_id, replaces_local_draft,
        summary_contains_query,
    };
    use crate::integration::registry::Snapshot;
    use crate::model::account::MailAccountId;
    use crate::model::mail::{
        ConversationId, ConversationSummary, DraftMessage, FolderId, FolderKind, MailFolder,
    };

    fn registry_entry(uid: &str) -> crate::integration::registry::Source {
        crate::integration::registry::Source {
            uid: uid.into(),
            ..Default::default()
        }
    }

    fn registry_triplet(goa_id: &str, address: &str) -> crate::integration::registry::MailTriplet {
        let mut account = registry_entry("account-source");
        account.parent = Some("collection-source".into());
        account.backend_name = Some("imapx".into());
        let mut identity = registry_entry("identity-source");
        identity.identity_name = Some("Mail Identity".into());
        identity.identity_address = Some(address.into());
        identity.identity_reply_to = Some("reply@example.invalid".into());
        identity.identity_aliases = Some("Alternate <alternate@example.invalid>".into());
        let mut transport = registry_entry("transport-source");
        transport.backend_name = Some("smtp".into());
        crate::integration::registry::MailTriplet {
            account,
            identity: Some(identity),
            transport: Some(transport),
            goa_account_id: Some(goa_id.into()),
            goa_name: Some("GOA Name".into()),
            goa_address: Some(address.into()),
            mail_enabled: Some(true),
        }
    }

    #[test]
    fn registry_discovery_accepts_only_complete_goa_mail_triplets() {
        let valid = registry_triplet("goa-account", "owner@example.invalid");
        let mut disabled = registry_triplet("disabled", "disabled@example.invalid");
        disabled.mail_enabled = Some(false);
        let mut non_goa = registry_triplet("temporary", "local@example.invalid");
        non_goa.goa_account_id = None;
        let incomplete = crate::integration::registry::MailTriplet {
            transport: None,
            ..registry_triplet("incomplete", "incomplete@example.invalid")
        };
        let mut missing_backend = registry_triplet("missing-backend", "backend@example.invalid");
        missing_backend.transport.as_mut().unwrap().backend_name = None;
        let mut missing_parent = registry_triplet("missing-parent", "parent@example.invalid");
        missing_parent.account.parent = None;
        let missing_address = registry_triplet("missing-address", "   ");
        let mut partially_linked = registry_triplet("partially-linked", "partial@example.invalid");
        partially_linked.goa_account_id = None;
        let snapshot = Snapshot {
            triplets: vec![
                valid,
                disabled,
                non_goa,
                incomplete,
                missing_backend,
                missing_parent,
                missing_address,
                partially_linked,
            ],
            ..Default::default()
        };

        let catalog = super::catalog_from_registry(&snapshot);
        let accounts = &catalog.accounts;

        assert_eq!(accounts.len(), 1);
        assert_eq!(catalog.bindings.len(), 1);
        assert_eq!(catalog.bindings[0].account_id, accounts[0].id);
        assert_eq!(accounts[0].id.0, "goa-account");
        assert_eq!(accounts[0].display_name, "Mail Identity");
        assert_eq!(
            accounts[0].primary_identity().unwrap().address,
            "owner@example.invalid"
        );
        assert_eq!(accounts[0].aliases[0].id.0, "goa-account:primary");
        assert_eq!(
            accounts[0].aliases[0].reply_to.as_deref(),
            Some("reply@example.invalid")
        );
        assert_eq!(accounts[0].aliases.len(), 2);
        assert_eq!(accounts[0].aliases[1].address, "alternate@example.invalid");
    }

    #[test]
    fn registry_discovery_rejects_conflicting_ids_and_deduplicates_goa_accounts() {
        let first = registry_triplet("same-account", "first@example.invalid");
        let duplicate = registry_triplet("same-account", "second@example.invalid");
        let mut conflicting = registry_triplet("conflicting", "third@example.invalid");
        conflicting.goa_account_id = None;
        let snapshot = Snapshot {
            triplets: vec![first, duplicate, conflicting],
            ..Default::default()
        };

        let accounts = super::catalog_from_registry(&snapshot).accounts;

        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].id.0, "same-account");
        assert_eq!(
            accounts[0].primary_identity().unwrap().address,
            "first@example.invalid"
        );
    }

    #[test]
    fn registry_binding_skips_an_incomplete_duplicate_of_a_discovered_account() {
        let valid = registry_triplet("same-account", "owner@example.invalid");
        let mut incomplete = valid.clone();
        incomplete.account.uid = "stale-account-source".into();
        incomplete.transport.as_mut().unwrap().backend_name = None;
        let mut malformed = valid.clone();
        malformed.account.uid = "malformed-account-source".into();
        malformed.identity.as_mut().unwrap().identity_address = Some("   ".into());
        let snapshot = Snapshot {
            triplets: vec![incomplete, malformed, valid],
            ..Default::default()
        };
        let catalog = super::catalog_from_registry(&snapshot);

        assert_eq!(catalog.accounts.len(), 1);
        assert_eq!(catalog.bindings.len(), 1);
        assert_eq!(catalog.bindings[0].account_uid, "account-source");
        assert_eq!(catalog.bindings[0].transport_backend_name, "smtp");
    }

    #[test]
    fn stub_writes_always_succeed_without_changing_the_stub() {
        let backend = Backend::new(None);
        let stub_account_id = crate::integration::stub::stub_account().id;
        let unknown_message = ConversationId("not-a-stub-message".into());
        let before = backend.stub_store.conversations(&FolderId("inbox".into()));

        futures::executor::block_on(backend.set_starred(&stub_account_id, &unknown_message, true))
            .expect("stub star writes should be accepted");
        futures::executor::block_on(backend.set_read(&stub_account_id, &unknown_message, true))
            .expect("stub read writes should be accepted");
        futures::executor::block_on(backend.move_to_folder(
            &stub_account_id,
            &unknown_message,
            &FolderId("trash".into()),
        ))
        .expect("stub moves should be accepted");

        let draft = DraftMessage::empty(stub_account_id, "Sender <sender@example.invalid>".into());
        assert!(
            futures::executor::block_on(backend.save_draft(&draft))
                .expect("stub draft saves should be accepted")
                .is_none()
        );
        assert!(
            !futures::executor::block_on(backend.send_draft(&draft))
                .expect("stub sends should be accepted")
        );

        let store = &backend.stub_store;
        assert_eq!(store.conversations(&FolderId("inbox".into())), before);
        assert!(store.conversations(&FolderId("drafts".into())).is_empty());
        assert!(store.conversations(&FolderId("sent".into())).is_empty());
    }

    #[test]
    fn non_stub_writes_are_rejected_without_a_mail_cache() {
        let backend = Backend::new(None);
        let account_id = MailAccountId("unavailable-account".into());
        let conversation_id = ConversationId("unavailable-conversation".into());
        let folder_id = FolderId("trash".into());
        let draft =
            DraftMessage::empty(account_id.clone(), "Sender <sender@example.invalid>".into());

        assert!(
            futures::executor::block_on(backend.set_starred(&account_id, &conversation_id, true,))
                .is_err()
        );
        assert!(
            futures::executor::block_on(backend.set_read(&account_id, &conversation_id, true,))
                .is_err()
        );
        assert!(
            futures::executor::block_on(backend.move_to_folder(
                &account_id,
                &conversation_id,
                &folder_id,
            ))
            .is_err()
        );
        assert!(futures::executor::block_on(backend.save_draft(&draft)).is_err());
        assert!(futures::executor::block_on(backend.send_draft(&draft)).is_err());
    }

    #[test]
    fn instance_cache_is_never_exposed_for_a_different_account() {
        let backend = Backend::new(Some(EdsAccountBinding {
            account_id: MailAccountId("bound-account".into()),
            account_uid: "account-source".into(),
            account_parent_uid: "collection-source".into(),
            account_backend_name: "imapx".into(),
            account_auth_method: None,
            identity_uid: "identity-source".into(),
            transport_uid: "transport-source".into(),
            transport_backend_name: "smtp".into(),
            transport_auth_method: None,
            drafts_folder: None,
            sent_folder: None,
        }));
        *backend.cached_folders.lock().unwrap() = Some(vec![MailFolder {
            id: FolderId("private".into()),
            name: "Private cache".into(),
            unread_count: 0,
            kind: FolderKind::Custom,
        }]);
        backend.cached_conversations.lock().unwrap().insert(
            "inbox".into(),
            vec![ConversationSummary {
                id: ConversationId("inbox\u{1f}private".into()),
                subject: "Private cache".into(),
                participants: Vec::new(),
                message_count: 1,
                unread_count: 0,
                attachment_count: 0,
                starred: false,
                last_updated_unix_ms: 0,
                preview: String::new(),
            }],
        );

        let other_account = MailAccountId("other-account".into());
        let folders = futures::executor::block_on(backend.list_folders(&other_account)).unwrap();
        let conversations = futures::executor::block_on(backend.list_conversations(
            &other_account,
            &FolderId("inbox".into()),
            0,
            0,
        ))
        .unwrap();

        assert!(!folders.iter().any(|folder| folder.id.0 == "private"));
        assert!(
            conversations
                .iter()
                .all(|summary| summary.subject != "Private cache")
        );
    }

    #[test]
    fn cached_summary_search_covers_subject_preview_and_participants_case_insensitively() {
        let summary = ConversationSummary {
            id: ConversationId("folder\u{1f}uid".into()),
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

    #[test]
    fn cached_summary_updates_refresh_the_instance_folder_counts() {
        let backend = Backend::new(None);
        let folder_id = FolderId("inbox".into());
        let conversation_id = ConversationId("inbox\u{1f}message-1".into());
        let summary = ConversationSummary {
            id: conversation_id.clone(),
            subject: "Account-scoped cache".into(),
            participants: vec!["Person <person@example.test>".into()],
            message_count: 1,
            unread_count: 1,
            attachment_count: 0,
            starred: false,
            last_updated_unix_ms: 0,
            preview: "Cache state".into(),
        };
        let folder = MailFolder {
            id: folder_id.clone(),
            name: "Inbox".into(),
            unread_count: 1,
            kind: FolderKind::Inbox,
        };
        *backend.cached_folders.lock().unwrap() = Some(vec![folder]);
        backend
            .cached_conversations
            .lock()
            .unwrap()
            .insert(folder_id.0.clone(), vec![summary]);

        backend.update_cached_conversation(&conversation_id, |conversation| {
            conversation.unread_count = 0
        });

        let folders = backend.cached_folders.lock().unwrap();
        assert_eq!(folders.as_ref().unwrap()[0].unread_count, 0);
        drop(folders);
        let conversations = backend.cached_conversations.lock().unwrap();
        assert_eq!(conversations[&folder_id.0][0].unread_count, 0);
    }

    #[test]
    fn local_outbox_is_a_sibling_of_nested_identity_folders() {
        assert_eq!(
            local_sibling_folder_id(Some("folder://local/account-1/Drafts"), "Outbox"),
            Some(FolderId("account-1/Outbox".into()))
        );
    }

    #[test]
    fn draft_replacement_is_limited_to_the_same_local_collection() {
        assert!(replaces_local_draft(
            &ConversationId("account-1/Drafts\u{1f}old".into()),
            "account-1/Drafts",
        ));
        assert!(replaces_local_draft(
            &ConversationId("account-1/Drafts\u{1f}old".into()),
            "account-1/Outbox",
        ));
        assert!(!replaces_local_draft(
            &ConversationId("account-2/Drafts\u{1f}old".into()),
            "account-1/Drafts",
        ));
        assert!(!replaces_local_draft(
            &ConversationId("account-1/Sent\u{1f}old".into()),
            "account-1/Drafts",
        ));
        assert!(!replaces_local_draft(
            &ConversationId("malformed".into()),
            "account-1/Drafts",
        ));
    }

    #[test]
    fn local_sibling_supports_a_top_level_folder() {
        assert_eq!(
            local_sibling_folder_id(Some("folder://local/Drafts"), "Outbox"),
            Some(FolderId("Outbox".into()))
        );
    }

    #[test]
    fn local_sibling_rejects_remote_and_malformed_uris() {
        assert_eq!(
            local_sibling_folder_id(Some("folder://microsoft365/Drafts"), "Outbox"),
            None
        );
        assert_eq!(local_sibling_folder_id(Some("Drafts"), "Outbox"), None);
        assert_eq!(local_sibling_folder_id(None, "Outbox"), None);
    }

    #[test]
    fn unsent_cache_entry_remains_in_outbox() {
        let mut remote = HashMap::from([(17, 1)]);
        assert_eq!(
            classify_cached_delivery(false, false, 17, &mut remote),
            CachedDeliveryPlacement::Outbox
        );
    }

    #[test]
    fn submitted_cache_entry_remains_visible_until_remote_confirmation() {
        let mut remote = HashMap::new();
        assert_eq!(
            classify_cached_delivery(true, false, 17, &mut remote),
            CachedDeliveryPlacement::Sent
        );
        let mut zero_remote = HashMap::from([(0, 1)]);
        assert_eq!(
            classify_cached_delivery(true, false, 0, &mut zero_remote),
            CachedDeliveryPlacement::Sent
        );
    }

    #[test]
    fn submitted_cache_entry_retires_only_on_matching_remote_message_id() {
        let mut matching_remote = HashMap::from([(16, 1), (17, 1), (18, 1)]);
        assert_eq!(
            classify_cached_delivery(true, false, 17, &mut matching_remote),
            CachedDeliveryPlacement::Retire
        );
        let mut other_remote = HashMap::from([(16, 1), (18, 1)]);
        assert_eq!(
            classify_cached_delivery(true, false, 17, &mut other_remote),
            CachedDeliveryPlacement::Sent
        );
    }

    #[test]
    fn local_sent_fallback_remains_visible_without_remote_confirmation() {
        let mut matching_remote = HashMap::from([(17, 1)]);
        assert_eq!(
            classify_cached_delivery(true, true, 17, &mut matching_remote),
            CachedDeliveryPlacement::Retire
        );
        let mut no_remote = HashMap::new();
        assert_eq!(
            classify_cached_delivery(false, true, 0, &mut no_remote),
            CachedDeliveryPlacement::Sent
        );
    }

    #[test]
    fn local_sent_fallback_retires_when_server_later_creates_a_copy() {
        let mut remote = HashMap::from([(42, 1)]);
        assert_eq!(
            classify_cached_delivery(false, true, 42, &mut remote),
            CachedDeliveryPlacement::Retire
        );
    }

    #[test]
    fn remote_confirmation_is_claimed_one_to_one() {
        let mut remote = HashMap::from([(17, 1)]);
        assert_eq!(
            classify_cached_delivery(true, false, 17, &mut remote),
            CachedDeliveryPlacement::Retire
        );
        assert_eq!(
            classify_cached_delivery(true, false, 17, &mut remote),
            CachedDeliveryPlacement::Sent
        );
    }

    #[test]
    fn one_server_copy_retires_only_one_local_fallback() {
        let mut remote = HashMap::from([(17, 1)]);
        assert_eq!(
            classify_cached_delivery(false, true, 17, &mut remote),
            CachedDeliveryPlacement::Retire
        );
        assert_eq!(
            classify_cached_delivery(false, true, 17, &mut remote),
            CachedDeliveryPlacement::Sent
        );
    }

    #[test]
    fn local_delivery_ids_are_scoped_to_the_account_collection() {
        let drafts = Some("folder://local/account-1/Drafts");
        assert!(conversation_belongs_to_local_collection(
            drafts,
            &ConversationId("account-1/Outbox\u{1f}uid-1".into())
        ));
        assert!(conversation_belongs_to_local_collection(
            drafts,
            &ConversationId("account-1/Sent\u{1f}uid-2".into())
        ));
        assert!(!conversation_belongs_to_local_collection(
            drafts,
            &ConversationId("account-10/Sent\u{1f}uid-3".into())
        ));
        assert!(!conversation_belongs_to_local_collection(
            Some("folder://microsoft365/account-1/Drafts"),
            &ConversationId("account-1/Outbox\u{1f}uid-4".into())
        ));
        assert!(!conversation_belongs_to_local_collection(
            drafts,
            &ConversationId("malformed".into())
        ));
        assert!(conversation_belongs_to_local_collection(
            Some("folder://local/Drafts"),
            &ConversationId("Outbox\u{1f}uid-5".into())
        ));
    }
}
