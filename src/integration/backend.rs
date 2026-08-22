use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use anyhow::anyhow;
use futures::future::BoxFuture;

use crate::integration::journal::{PendingMailActionStore, PendingMove};
use crate::integration::registry::Snapshot;
use crate::integration::stub::StubMailboxStore;
use crate::model::account::{MailAccount, MailAccountId};
use crate::model::mail::{
    ConversationId, ConversationSummary, DraftMessage, FolderId, FolderKind, MailFolder,
    MailboxMode, MessageDetail,
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
    let accounts = accounts_from_registry(&snapshot);
    let bindings = bind_accounts_to_triplets(&accounts, &snapshot)
        .iter()
        .map(EdsAccountBinding::from_resolved_triplet)
        .collect::<Vec<_>>();
    tracing::debug!(
        accounts = snapshot.accounts.len(),
        identities = snapshot.identities.len(),
        transports = snapshot.transports.len(),
        relationships = snapshot.relationships.len(),
        triplets = snapshot.triplets.len(),
        bindings = bindings.len(),
        "EDS registry topology resolved"
    );
    tracing::info!(
        accounts = accounts.len(),
        bindings = bindings.len(),
        "EDS mail accounts discovered"
    );
    Ok(AccountCatalog { accounts, bindings })
}

impl AccountCatalog {
    pub(crate) fn into_parts(self) -> (Vec<MailAccount>, Vec<EdsAccountBinding>) {
        (self.accounts, self.bindings)
    }
}

fn accounts_from_registry(snapshot: &Snapshot) -> Vec<MailAccount> {
    let mut seen = HashSet::new();
    let mut accounts = snapshot
        .triplets
        .iter()
        .filter_map(|triplet| {
            let account = triplet.account.as_ref()?;
            let identity = triplet.identity.as_ref()?;
            let transport = triplet.transport.as_ref()?;
            let account_id = usable_goa_triplet_id(triplet)?;
            if !seen.insert(account_id.to_string()) {
                return None;
            }
            let primary_address = identity
                .identity_address
                .as_deref()
                .or(identity.goa_address.as_deref())
                .or(account.goa_address.as_deref())
                .or(transport.goa_address.as_deref())?
                .trim();
            if primary_address.is_empty() {
                return None;
            }
            let display_name = identity
                .identity_name
                .as_deref()
                .or(identity.goa_name.as_deref())
                .or(account.goa_name.as_deref())
                .or(transport.goa_name.as_deref())
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or(primary_address);

            Some(MailAccount {
                id: MailAccountId(account_id.to_string()),
                display_name: display_name.to_string(),
                primary_address: primary_address.to_string(),
                aliases: vec![crate::model::account::SendingIdentity::with_id(
                    crate::model::account::AliasId(format!("{account_id}:primary")),
                    primary_address.to_string(),
                    display_name.to_string(),
                    identity.identity_reply_to.clone(),
                    format!("<p>{display_name}</p>"),
                    display_name.to_string(),
                    true,
                )],
            })
        })
        .collect::<Vec<_>>();
    accounts.sort_by(|left, right| {
        left.display_name
            .cmp(&right.display_name)
            .then_with(|| left.id.0.cmp(&right.id.0))
    });
    accounts
}

fn usable_goa_triplet_id(triplet: &crate::integration::registry::MailTriplet) -> Option<&str> {
    let account = triplet.account.as_ref()?;
    let identity = triplet.identity.as_ref()?;
    let transport = triplet.transport.as_ref()?;
    if account.mail_enabled == Some(false)
        || account.uid.as_deref().is_none_or(str::is_empty)
        || identity.uid.as_deref().is_none_or(str::is_empty)
        || transport.uid.as_deref().is_none_or(str::is_empty)
        || account.backend_name.as_deref().is_none_or(str::is_empty)
        || transport.backend_name.as_deref().is_none_or(str::is_empty)
    {
        return None;
    }

    let account_id = account
        .goa_account_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())?;
    let identity_id = identity
        .goa_account_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())?;
    let transport_id = transport
        .goa_account_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())?;
    (identity_id == account_id && transport_id == account_id).then_some(account_id)
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
    fn activate_account(&self, account: &MailAccount)
    -> BoxFuture<'_, anyhow::Result<MailboxMode>>;

    fn invalidate_account(&self, _account_id: &MailAccountId) {}

    fn replace_available_bindings(&self, _bindings: &[EdsAccountBinding]) {}

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
        account_id: &MailAccountId,
        draft: &DraftMessage,
    ) -> BoxFuture<'_, anyhow::Result<Option<MessageDetail>>>;

    fn send_draft(
        &self,
        account_id: &MailAccountId,
        draft: &DraftMessage,
    ) -> BoxFuture<'_, anyhow::Result<Option<MessageDetail>>>;
}

pub type SharedMailBackend = Arc<dyn MailBackend>;

#[derive(Debug, Clone)]
pub struct EdsAccountBinding {
    pub account_id: String,
    pub account_label: String,
    pub account_uid: Option<String>,
    pub account_parent_uid: Option<String>,
    pub account_backend_name: Option<String>,
    pub account_auth_method: Option<String>,
    pub identity_uid: Option<String>,
    pub identity_name: Option<String>,
    pub identity_reply_to: Option<String>,
    pub identity_aliases: Option<String>,
    pub transport_uid: Option<String>,
    pub transport_backend_name: Option<String>,
    pub transport_auth_method: Option<String>,
    pub drafts_folder: Option<String>,
    pub sent_folder: Option<String>,
}

fn ensure_eds_local_mail_configuration(binding: &mut EdsAccountBinding) -> anyhow::Result<()> {
    let Some(identity_uid) = binding.identity_uid.clone() else {
        return Ok(());
    };
    let local_root_path = crate::integration::camel::account_cache_root_for_binding(binding)?;
    let collection_uid = binding
        .account_parent_uid
        .clone()
        .ok_or_else(|| anyhow!("EDS binding is missing account_parent_uid"))?;
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
            &identity_uid,
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
    let Some(identity_uid) = binding.identity_uid.as_deref() else {
        return Ok(());
    };

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
    Arc::new(Backend::from_accounts(&[], Vec::new())) as SharedMailBackend
}

#[derive(Debug, Clone)]
struct ResolvedTripletBinding {
    account_id: String,
    account_label: String,
    account_uid: Option<String>,
    account_parent_uid: Option<String>,
    account_backend_name: Option<String>,
    account_auth_method: Option<String>,
    identity_uid: Option<String>,
    identity_name: Option<String>,
    identity_reply_to: Option<String>,
    identity_aliases: Option<String>,
    transport_uid: Option<String>,
    transport_backend_name: Option<String>,
    transport_auth_method: Option<String>,
    drafts_folder: Option<String>,
    sent_folder: Option<String>,
}

impl EdsAccountBinding {
    fn from_resolved_triplet(binding: &ResolvedTripletBinding) -> Self {
        Self {
            account_id: binding.account_id.clone(),
            account_label: binding.account_label.clone(),
            account_uid: binding.account_uid.clone(),
            account_parent_uid: binding.account_parent_uid.clone(),
            account_backend_name: binding.account_backend_name.clone(),
            account_auth_method: binding.account_auth_method.clone(),
            identity_uid: binding.identity_uid.clone(),
            identity_name: binding.identity_name.clone(),
            identity_reply_to: binding.identity_reply_to.clone(),
            identity_aliases: binding.identity_aliases.clone(),
            transport_uid: binding.transport_uid.clone(),
            transport_backend_name: binding.transport_backend_name.clone(),
            transport_auth_method: binding.transport_auth_method.clone(),
            drafts_folder: binding.drafts_folder.clone(),
            sent_folder: binding.sent_folder.clone(),
        }
    }
}

fn select_mail_backend(
    accounts: &[MailAccount],
    mut eds_bindings: Vec<EdsAccountBinding>,
    pending_mail_actions: PendingMailActionStore,
) -> BackendSelection {
    if let Err(error) = eds_bindings
        .iter_mut()
        .try_for_each(ensure_eds_local_mail_configuration)
    {
        crate::logging::report_failure("local-mail-cache-initialization", &error);
        eds_bindings.clear();
    }
    let mode = if eds_bindings.is_empty() {
        MailboxMode::StubUnavailable
    } else {
        MailboxMode::Live
    };
    let backend = Arc::new(Backend::from_accounts_with_pending_actions(
        accounts,
        eds_bindings,
        pending_mail_actions,
    )) as SharedMailBackend;
    BackendSelection { backend, mode }
}

#[derive(Clone)]
pub(crate) struct Backend {
    stub_store: Arc<StubMailboxStore>,
    eds_bindings: Vec<EdsAccountBinding>,
    camel_sessions:
        Arc<Mutex<HashMap<String, Arc<Mutex<crate::integration::camel::AccountSession>>>>>,
    cached_folders: Arc<Mutex<HashMap<String, Vec<MailFolder>>>>,
    cached_conversations: Arc<Mutex<HashMap<(String, String), Vec<ConversationSummary>>>>,
    configured_remote_sent: Arc<Mutex<HashMap<String, String>>>,
    pending_mail_actions: PendingMailActionStore,
    sending_addresses: Arc<HashMap<(String, String), String>>,
}

impl Backend {
    pub(crate) fn from_accounts(
        accounts: &[MailAccount],
        eds_bindings: Vec<EdsAccountBinding>,
    ) -> Self {
        Self::from_accounts_with_pending_actions(
            accounts,
            eds_bindings,
            PendingMailActionStore::open_default(),
        )
    }

    fn from_accounts_with_pending_actions(
        accounts: &[MailAccount],
        eds_bindings: Vec<EdsAccountBinding>,
        pending_mail_actions: PendingMailActionStore,
    ) -> Self {
        let sending_addresses = accounts
            .iter()
            .flat_map(|account| {
                account.aliases.iter().map(move |identity| {
                    (
                        (account.id.0.clone(), identity.id.0.clone()),
                        identity.mailbox(),
                    )
                })
            })
            .collect();
        Self {
            stub_store: Arc::new(StubMailboxStore::seeded()),
            eds_bindings,
            camel_sessions: Arc::new(Mutex::new(HashMap::new())),
            cached_folders: Arc::new(Mutex::new(HashMap::new())),
            cached_conversations: Arc::new(Mutex::new(HashMap::new())),
            configured_remote_sent: Arc::new(Mutex::new(HashMap::new())),
            pending_mail_actions,
            sending_addresses: Arc::new(sending_addresses),
        }
    }

    fn sender_for_draft(
        &self,
        account_id: &MailAccountId,
        draft: &DraftMessage,
    ) -> anyhow::Result<String> {
        if !draft.from.trim().is_empty() {
            return Ok(draft.from.clone());
        }
        self.sending_addresses
            .get(&(account_id.0.clone(), draft.alias_id.0.clone()))
            .cloned()
            .ok_or_else(|| anyhow!("the selected sending identity is no longer available"))
    }

    fn live_binding(&self, account_id: &MailAccountId) -> Option<EdsAccountBinding> {
        self.eds_binding(account_id).filter(|binding| {
            binding.account_uid.is_some() && binding.account_backend_name.is_some()
        })
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

        let mut folders = self
            .cached_folders
            .lock()
            .expect("folder cache lock poisoned");
        if let Some(folders) = folders.get_mut(&account_id.0) {
            update_folder_unread_counts(account_id, folders, &self.cached_conversations);
        }
    }

    fn apply_pending_flag_overlay(&self, account_id: &MailAccountId) {
        let mut conversations = self
            .cached_conversations
            .lock()
            .expect("conversation cache lock poisoned");
        self.pending_mail_actions
            .apply_flag_overlay(account_id, &mut conversations);
        drop(conversations);
        let mut folders = self
            .cached_folders
            .lock()
            .expect("folder cache lock poisoned");
        if let Some(folders) = folders.get_mut(&account_id.0) {
            update_folder_unread_counts(account_id, folders, &self.cached_conversations);
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
                &move_request.conversation_id,
                &move_request.destination_folder_id,
            ) {
                Ok(()) => {
                    completed.insert(move_request.conversation_id);
                }
                Err(error) => {
                    crate::logging::report_deferred("pending-move-sync", &error);
                    development_probe_log!(
                        "pending move remains queued for account {} conversation {}: {}",
                        account_id.0,
                        move_request.conversation_id.0,
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
        remote_conversations: &HashMap<(String, String), Vec<ConversationSummary>>,
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
        let key = binding.account_id.clone();

        let mut sessions = self
            .camel_sessions
            .lock()
            .expect("camel session cache lock poisoned");
        if let Some(session) = sessions.get(&key) {
            return Ok(Arc::clone(session));
        }

        let session = Arc::new(Mutex::new(
            crate::integration::camel::AccountSession::open_cached(binding)?,
        ));
        sessions.insert(key, Arc::clone(&session));
        Ok(session)
    }

    fn clear_live_cache_for_account(&self, account_id: &MailAccountId) {
        self.cached_folders
            .lock()
            .expect("folder cache lock poisoned")
            .remove(&account_id.0);
        self.cached_conversations
            .lock()
            .expect("conversation cache lock poisoned")
            .retain(|(cached_account_id, _), _| cached_account_id != &account_id.0);
        self.camel_sessions
            .lock()
            .expect("camel session cache lock poisoned")
            .remove(&account_id.0);
    }

    fn refresh_live_cache_for_account(&self, account_id: &MailAccountId) -> anyhow::Result<()> {
        let Some(binding) = self.live_binding(account_id) else {
            self.clear_live_cache_for_account(account_id);
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
                self.replay_local_drafts(
                    &binding,
                    account_id,
                    &remote_drafts.id,
                    &mut online_session,
                )?;
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
                if let Err(_error) =
                    online_session.refresh_folder_info(&FolderId(folder_id.clone()))
                {
                    development_probe_log!(
                        "EDS/Camel folder info refresh failed for {} folder {}: {}",
                        binding.account_label,
                        folder_id,
                        _error
                    );
                }
            }

            let mut folders = online_session.list_folders()?;

            let mut refreshed_conversations = HashMap::new();
            for folder_id in &folders_to_refresh {
                let folder_key = FolderId(folder_id.clone());
                let conversations = online_session.list_conversations(&folder_key, 0, 0)?;
                refreshed_conversations
                    .insert((account_id.0.clone(), folder_id.clone()), conversations);
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
            let local_delivery =
                self.replay_local_outbox(&binding, account_id, &remote_sent_hashes)?;
            self.merge_local_drafts_source(
                &binding,
                account_id,
                &mut folders,
                &mut refreshed_conversations,
            )?;
            merge_local_delivery_view(
                account_id,
                &mut folders,
                &mut refreshed_conversations,
                local_delivery,
            );

            self.cached_folders
                .lock()
                .expect("folder cache lock poisoned")
                .insert(account_id.0.clone(), folders);

            {
                let mut cache = self
                    .cached_conversations
                    .lock()
                    .expect("conversation cache lock poisoned");
                cache.retain(|(cached_account_id, _), _| cached_account_id != &account_id.0);
                for (key, value) in &refreshed_conversations {
                    cache.insert(key.clone(), value.clone());
                }
            }
            self.apply_pending_move_overlay(account_id);
            self.apply_pending_flag_overlay(account_id);

            development_probe_log!(
                "EDS/Camel refresh rebuilt cache for {}: folders={} conversation_folders={}",
                binding.account_label,
                folders_to_refresh.len(),
                refreshed_conversations.len()
            );

            Ok(())
        })();
        refresh_result
    }

    fn merge_local_drafts_source(
        &self,
        binding: &EdsAccountBinding,
        account_id: &MailAccountId,
        folders: &mut Vec<MailFolder>,
        conversations: &mut HashMap<(String, String), Vec<ConversationSummary>>,
    ) -> anyhow::Result<()> {
        self.merge_local_folder_source(
            binding.drafts_folder.as_deref(),
            account_id,
            FolderKind::Drafts,
            "Drafts",
            folders,
            conversations,
        )
    }

    fn replay_local_drafts(
        &self,
        binding: &EdsAccountBinding,
        _account_id: &MailAccountId,
        remote_folder_id: &FolderId,
        online_session: &mut crate::integration::camel::AccountSession,
    ) -> anyhow::Result<()> {
        let Some(local_drafts_uri) = binding.drafts_folder.as_deref() else {
            return Ok(());
        };
        let Some((source_uid, local_folder_name)) = parse_folder_uri(local_drafts_uri) else {
            return Ok(());
        };
        if source_uid != "local" {
            return Ok(());
        }

        let mut local_session =
            crate::integration::camel::AccountSession::open_cached_source(source_uid, "maildir")?;
        let local_folder_id = FolderId(local_folder_name.to_string());
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
                        "local draft remains queued for account {} because remote append failed: {}",
                        _account_id.0,
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
        let Some(identity_uid) = binding.identity_uid.as_deref() else {
            return Ok(());
        };
        let Some(account_uid) = binding.account_uid.as_deref() else {
            return Ok(());
        };
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
            .get(&binding.account_id)
            .is_some_and(|configured| configured == &sent_uri);
        if !already_configured && binding.sent_folder.as_deref() != Some(sent_uri.as_str()) {
            crate::integration::registry::set_sent_folder_configuration(identity_uid, &sent_uri)?;
        }
        self.configured_remote_sent
            .lock()
            .expect("remote Sent configuration lock poisoned")
            .insert(binding.account_id.clone(), sent_uri);
        Ok(())
    }

    fn merge_local_folder_source(
        &self,
        folder_uri: Option<&str>,
        account_id: &MailAccountId,
        kind: FolderKind,
        fallback_name: &str,
        folders: &mut Vec<MailFolder>,
        conversations: &mut HashMap<(String, String), Vec<ConversationSummary>>,
    ) -> anyhow::Result<()> {
        let Some(folder_uri) = folder_uri else {
            return Ok(());
        };
        let Some((source_uid, folder_name)) = parse_folder_uri(folder_uri) else {
            return Ok(());
        };
        if source_uid != "local" {
            return Ok(());
        }

        let mut session =
            crate::integration::camel::AccountSession::open_cached_source(source_uid, "maildir")?;
        let local_folder_id = FolderId(folder_name.to_string());
        session.ensure_folder_path(&local_folder_id)?;
        refresh_local_folder_best_effort(&mut session, &local_folder_id);
        let local_conversations = session.list_conversations(&local_folder_id, 0, 0)?;

        let display_folder_id = folders
            .iter()
            .find(|folder| folder.kind == kind)
            .map(|folder| folder.id.clone())
            .unwrap_or_else(|| FolderId(folder_name.to_string()));
        if !folders.iter().any(|folder| folder.id == display_folder_id) {
            folders.push(MailFolder {
                id: display_folder_id.clone(),
                name: fallback_name.into(),
                unread_count: 0,
                kind,
            });
        }

        let merged = conversations
            .entry((account_id.0.clone(), display_folder_id.0.clone()))
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
        _account_id: &MailAccountId,
        remote_sent_hashes: &HashMap<u64, usize>,
    ) -> anyhow::Result<LocalDeliveryView> {
        let Some(outbox_uri) = local_sibling_folder_uri(binding.drafts_folder.as_deref(), "Outbox")
        else {
            return Ok(LocalDeliveryView::default());
        };
        let (outbox_source_uid, outbox_folder_name) = parse_folder_uri(&outbox_uri)
            .ok_or_else(|| anyhow!("could not resolve local EDS Outbox target"))?;
        if outbox_source_uid != "local" {
            return Err(anyhow!("Outbox is not backed by the local EDS source"));
        }

        let mut local_session = crate::integration::camel::AccountSession::open_cached_source(
            outbox_source_uid,
            "maildir",
        )?;
        let outbox_folder_id = FolderId(outbox_folder_name.to_string());
        let local_sent_uri = local_sibling_folder_uri(binding.drafts_folder.as_deref(), "Sent")
            .ok_or_else(|| anyhow!("could not resolve local EDS Sent fallback"))?;
        let (_, local_sent_folder_name) = parse_folder_uri(&local_sent_uri)
            .ok_or_else(|| anyhow!("could not parse local EDS Sent fallback"))?;
        let local_sent_folder_id = FolderId(local_sent_folder_name.to_string());
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
                        "Outbox remains queued for account {} because transport is unavailable: {}",
                        _account_id.0,
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
                    development_probe_log!(
                        "queued message remains in Outbox for account {}: {}",
                        _account_id.0,
                        error
                    );
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
    account_id: &MailAccountId,
    folders: &mut Vec<MailFolder>,
    conversations: &mut HashMap<(String, String), Vec<ConversationSummary>>,
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
            .entry((account_id.0.clone(), display_folder_id.0.clone()))
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

fn merge_local_drafts_folder_view(mut folders: Vec<MailFolder>) -> Vec<MailFolder> {
    let drafts_indices = folders
        .iter()
        .enumerate()
        .filter_map(|(index, folder)| matches!(folder.kind, FolderKind::Drafts).then_some(index))
        .collect::<Vec<_>>();

    if drafts_indices.len() <= 1 {
        return folders;
    }

    let primary_index = drafts_indices[0];
    let total_unread = drafts_indices
        .iter()
        .filter_map(|index| folders.get(*index))
        .map(|folder| folder.unread_count)
        .sum();

    if let Some(primary) = folders.get_mut(primary_index) {
        primary.unread_count = total_unread;
    }

    for index in drafts_indices.into_iter().skip(1).rev() {
        folders.remove(index);
    }

    folders
}

fn update_folder_unread_counts(
    account_id: &MailAccountId,
    folders: &mut [MailFolder],
    cached_conversations: &Mutex<HashMap<(String, String), Vec<ConversationSummary>>>,
) {
    let conversations = cached_conversations
        .lock()
        .expect("conversation cache lock poisoned");
    for folder in folders {
        if let Some(summaries) = conversations.get(&(account_id.0.clone(), folder.id.0.clone())) {
            folder.unread_count = summaries.iter().map(|summary| summary.unread_count).sum();
        }
    }
}

fn append_draft_via_local_eds_cache(
    folder_uri: &str,
    from: &str,
    draft: &DraftMessage,
    is_draft: bool,
) -> anyhow::Result<MessageDetail> {
    let (source_uid, folder_name) = parse_folder_uri(folder_uri).ok_or_else(|| {
        anyhow!(
            "could not resolve local EDS folder target from '{}'",
            folder_uri
        )
    })?;

    let mut session =
        crate::integration::camel::AccountSession::open_cached_source(source_uid, "maildir")?;
    let folder_id = FolderId(folder_name.to_string());
    session.ensure_folder_path(&folder_id)?;
    let attachment_uris = draft
        .attachments
        .iter()
        .map(|attachment| attachment.uri.clone())
        .collect::<Vec<_>>();
    let request = crate::integration::camel::AppendMessageRequest {
        source_uid,
        message_id: draft
            .message_id
            .as_ref()
            .map(|message_id| message_id.0.as_str()),
        folder_id: &folder_id,
        from,
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

    let detail = session.append_message(&request).map_err(|error| {
        anyhow!(
            "EDS local message append failed for source_uid='{}' folder_name='{}': {}",
            source_uid,
            folder_name,
            error
        )
    })?;

    let detail = detail.ok_or_else(|| {
        anyhow!(
            "EDS local message append returned no message detail for source_uid='{}' folder_name='{}'",
            source_uid,
            folder_name
        )
    })?;

    if source_uid == "local"
        && let Some(previous_id) = draft.conversation_id.as_ref()
        && previous_id != &detail.conversation_id
        && replaces_local_draft(previous_id, folder_name)
        && let Err(error) = session.delete_message_permanently(previous_id)
    {
        // The new MIME is already durable. Reporting the whole save as failed
        // would cause a retry to append yet another copy, so retain the new
        // version and diagnose only the stale-version cleanup.
        crate::logging::report_failure("draft-cache-replace-cleanup", &error);
    }

    Ok(detail)
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

fn local_sibling_folder_uri(configured_uri: Option<&str>, sibling_name: &str) -> Option<String> {
    let (source_uid, folder_name) = parse_folder_uri(configured_uri?)?;
    if source_uid != "local" {
        return None;
    }
    let parent = folder_name
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or("");
    let sibling_path = if parent.is_empty() {
        sibling_name.to_string()
    } else {
        format!("{parent}/{sibling_name}")
    };
    Some(format!("folder://{source_uid}/{sibling_path}"))
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
    let Some((source_uid, drafts_folder)) = drafts_uri.and_then(parse_folder_uri) else {
        return false;
    };
    if source_uid != "local" {
        return false;
    }
    match drafts_folder.rsplit_once('/') {
        Some((collection, _)) if !collection.is_empty() => {
            local_folder == collection || local_folder.starts_with(&format!("{collection}/"))
        }
        _ => matches!(local_folder, "Drafts" | "Outbox" | "Sent"),
    }
}

fn parse_folder_uri(folder_uri: &str) -> Option<(&str, &str)> {
    folder_uri.strip_prefix("folder://")?.split_once('/')
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
) {
    let Some(folder_name) = conversation_local_folder_id(conversation_id) else {
        return;
    };
    let local_folder_id = FolderId(folder_name.to_string());
    refresh_local_folder_best_effort(session, &local_folder_id);
}

fn refresh_local_folder_best_effort(
    session: &mut crate::integration::camel::AccountSession,
    folder_id: &FolderId,
) {
    if let Err(_error) = session.refresh_folder_info(folder_id) {
        development_probe_log!("EDS/Camel local folder refresh failed: {}", _error);
    }
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

fn bind_accounts_to_triplets(
    accounts: &[MailAccount],
    snapshot: &Snapshot,
) -> Vec<ResolvedTripletBinding> {
    accounts
        .iter()
        .filter_map(|account| {
            let triplet = snapshot
                .triplets
                .iter()
                .find(|triplet| usable_goa_triplet_id(triplet) == Some(account.id.0.as_str()))?;

            Some(ResolvedTripletBinding {
                account_id: account.id.0.clone(),
                account_label: account.display_label(),
                account_uid: triplet.account.as_ref().and_then(|entry| entry.uid.clone()),
                account_parent_uid: triplet
                    .account
                    .as_ref()
                    .and_then(|entry| entry.parent.clone()),
                account_backend_name: triplet
                    .account
                    .as_ref()
                    .and_then(|entry| entry.backend_name.clone()),
                account_auth_method: triplet
                    .account
                    .as_ref()
                    .and_then(|entry| entry.auth_method.clone()),
                identity_uid: triplet
                    .identity
                    .as_ref()
                    .and_then(|entry| entry.uid.clone()),
                identity_name: triplet
                    .identity
                    .as_ref()
                    .and_then(|entry| entry.identity_name.clone()),
                identity_reply_to: triplet
                    .identity
                    .as_ref()
                    .and_then(|entry| entry.identity_reply_to.clone()),
                identity_aliases: triplet
                    .identity
                    .as_ref()
                    .and_then(|entry| entry.identity_aliases.clone()),
                transport_uid: triplet
                    .transport
                    .as_ref()
                    .and_then(|entry| entry.uid.clone()),
                transport_backend_name: triplet
                    .transport
                    .as_ref()
                    .and_then(|entry| entry.backend_name.clone()),
                transport_auth_method: triplet
                    .transport
                    .as_ref()
                    .and_then(|entry| entry.auth_method.clone()),
                drafts_folder: triplet
                    .identity
                    .as_ref()
                    .and_then(|entry| entry.drafts_folder.clone())
                    .or_else(|| {
                        triplet
                            .account
                            .as_ref()
                            .and_then(|entry| entry.drafts_folder.clone())
                    }),
                sent_folder: triplet
                    .transport
                    .as_ref()
                    .and_then(|entry| entry.sent_folder.clone())
                    .or_else(|| {
                        triplet
                            .identity
                            .as_ref()
                            .and_then(|entry| entry.sent_folder.clone())
                    }),
            })
        })
        .collect()
}

impl MailBackend for Backend {
    fn activate_account(
        &self,
        account: &MailAccount,
    ) -> BoxFuture<'_, anyhow::Result<MailboxMode>> {
        let mode = if self.live_binding(&account.id).is_some() {
            MailboxMode::Live
        } else {
            MailboxMode::StubUnavailable
        };
        Box::pin(async move { Ok(mode) })
    }

    fn eds_binding(&self, account_id: &MailAccountId) -> Option<EdsAccountBinding> {
        self.eds_bindings
            .iter()
            .find(|binding| binding.account_id == account_id.0)
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
            if let Some(cached) = cached_folders
                .lock()
                .expect("folder cache lock poisoned")
                .get(&account_id.0)
                .cloned()
            {
                return Ok(merge_local_drafts_folder_view(cached));
            }

            if let Some(binding) = &eds_binding {
                let session = this.session_for_binding(binding)?;
                let folders = session
                    .lock()
                    .expect("camel account session lock poisoned")
                    .list_folders()?;
                let folders = merge_local_drafts_folder_view(folders);
                cached_folders
                    .lock()
                    .expect("folder cache lock poisoned")
                    .insert(account_id.0.clone(), folders.clone());
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
            if let Some(cached) = cached_conversations
                .lock()
                .expect("conversation cache lock poisoned")
                .get(&(account_id.0.clone(), folder_id.0.clone()))
                .cloned()
            {
                return Ok(slice_conversations(&cached, offset, limit));
            }

            if let Some(binding) = &eds_binding {
                let session = this.session_for_binding(binding)?;
                let conversations = session
                    .lock()
                    .expect("camel account session lock poisoned")
                    .list_conversations(&folder_id, 0, 0)?;
                cached_conversations
                    .lock()
                    .expect("conversation cache lock poisoned")
                    .insert((account_id.0.clone(), folder_id.0.clone()), conversations);
                this.apply_pending_move_overlay(&account_id);
                this.apply_pending_flag_overlay(&account_id);
                let conversations = cached_conversations
                    .lock()
                    .expect("conversation cache lock poisoned")
                    .get(&(account_id.0.clone(), folder_id.0.clone()))
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
                    refresh_local_conversation_folder(&mut session, &conversation_id);
                    return session.get_message_detail(&conversation_id);
                }
                match this.session_for_binding(&binding) {
                    Ok(session) => {
                        let mut session = session.lock().expect("camel session lock poisoned");
                        match session.get_message_detail(&conversation_id) {
                            Ok(Some(detail)) => return Ok(Some(detail)),
                            Ok(None) => {}
                            Err(_error) => {
                                development_probe_log!(
                                    "EDS/Camel detail load failed for {} conversation {}: {}",
                                    binding.account_label,
                                    conversation_id.0,
                                    _error
                                );
                            }
                        }
                    }
                    Err(_error) => {
                        development_probe_log!(
                            "EDS/Camel session open failed for {} while loading detail {}: {}",
                            binding.account_label,
                            conversation_id.0,
                            _error
                        );
                    }
                }

                match crate::integration::camel::AccountSession::open_online(&binding) {
                    Ok(mut session) => match session.get_message_detail(&conversation_id) {
                        Ok(detail) => return Ok(detail),
                        Err(error) => {
                            return Err(anyhow!(
                                "EDS/Camel on-demand detail load failed for {} conversation {}: {}",
                                binding.account_label,
                                conversation_id.0,
                                error
                            ));
                        }
                    },
                    Err(error) => {
                        return Err(anyhow!(
                            "EDS/Camel online session open failed for {} while loading detail {}: {}",
                            binding.account_label,
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
                    refresh_local_conversation_folder(&mut session, &conversation_id);
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
                            Err(_error) => {
                                development_probe_log!(
                                    "cached attachment export failed for {} conversation {} attachment {}: {}",
                                    binding.account_label,
                                    conversation_id.0,
                                    attachment_uri,
                                    _error
                                );
                            }
                        }
                    }
                    Err(_error) => {
                        development_probe_log!(
                            "cached session open failed for {} while exporting attachment {}: {}",
                            binding.account_label,
                            attachment_uri,
                            _error
                        );
                    }
                }

                let mut session = crate::integration::camel::AccountSession::open_online(&binding)
                    .map_err(|error| {
                        anyhow!(
                            "online session open failed for {} while exporting attachment {}: {}",
                            binding.account_label,
                            attachment_uri,
                            error
                        )
                    })?;
                return session
                    .export_attachment(&conversation_id, &attachment_uri)
                    .map_err(|error| {
                        anyhow!(
                            "on-demand attachment export failed for {} conversation {} attachment {}: {}",
                            binding.account_label,
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
                        .iter()
                        .filter(|((cached_account_id, _), _)| cached_account_id == &account_id.0)
                        .flat_map(|(_, conversations)| conversations.iter())
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
        let cached_conversations = Arc::clone(&self.cached_conversations);
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
                    refresh_local_conversation_folder(&mut session, &conversation_id);
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
                let mut cache = cached_conversations
                    .lock()
                    .expect("conversation cache lock poisoned");
                for ((cached_account_id, _), conversations) in cache.iter_mut() {
                    if cached_account_id != &account_id.0 {
                        continue;
                    }
                    for conversation in conversations.iter_mut() {
                        if conversation.id == conversation_id {
                            conversation.starred = starred;
                        }
                    }
                }
            }
            Ok(())
        })
    }

    fn set_read(
        &self,
        account_id: &MailAccountId,
        conversation_id: &ConversationId,
        read: bool,
    ) -> BoxFuture<'_, anyhow::Result<()>> {
        let cached_conversations = Arc::clone(&self.cached_conversations);
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
                    refresh_local_conversation_folder(&mut session, &conversation_id);
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
                let mut cache = cached_conversations
                    .lock()
                    .expect("conversation cache lock poisoned");
                for ((cached_account_id, _), conversations) in cache.iter_mut() {
                    if cached_account_id != &account_id.0 {
                        continue;
                    }
                    for conversation in conversations.iter_mut() {
                        if conversation.id == conversation_id {
                            conversation.unread_count = if read { 0 } else { 1 };
                        }
                    }
                }
            }
            Ok(())
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
                        .get(&account_id.0)
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
                .ok_or_else(|| {
                    anyhow!(
                        "could not resolve destination folder '{}' for {}",
                        folder_id.0,
                        binding.account_label
                    )
                })?;
                let summary = cached_conversations
                    .lock()
                    .expect("conversation cache lock poisoned")
                    .iter()
                    .filter(|((cached_account_id, _), _)| cached_account_id == &account_id.0)
                    .flat_map(|(_, conversations)| conversations.iter())
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
                    conversation_id: conversation_id.clone(),
                    destination_folder_id,
                    summary,
                })?;
                this.apply_pending_move_overlay(&account_id);
                return Ok(());
            }
            Ok(())
        })
    }

    fn save_draft(
        &self,
        account_id: &MailAccountId,
        draft: &DraftMessage,
    ) -> BoxFuture<'_, anyhow::Result<Option<MessageDetail>>> {
        let account_id = account_id.clone();
        let draft = draft.clone();
        let backend = self.clone();
        Box::pin(async move {
            let Some(binding) = backend.live_binding(&account_id) else {
                return Ok(None);
            };
            let drafts_folder_uri = binding
                .drafts_folder
                .as_deref()
                .ok_or_else(|| anyhow!("EDS binding has no configured drafts folder"))?;
            let from = backend.sender_for_draft(&account_id, &draft)?;
            let detail = append_draft_via_local_eds_cache(drafts_folder_uri, &from, &draft, true)?;
            {
                let mut folders = backend
                    .cached_folders
                    .lock()
                    .expect("folder cache lock poisoned")
                    .get(&account_id.0)
                    .cloned()
                    .unwrap_or_default();
                let mut conversations = backend
                    .cached_conversations
                    .lock()
                    .expect("conversation cache lock poisoned")
                    .iter()
                    .filter(|((cached_account_id, _), _)| cached_account_id == &account_id.0)
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect::<HashMap<_, _>>();
                backend.merge_local_drafts_source(
                    &binding,
                    &account_id,
                    &mut folders,
                    &mut conversations,
                )?;
                backend
                    .cached_folders
                    .lock()
                    .expect("folder cache lock poisoned")
                    .insert(account_id.0.clone(), folders);
                let mut cache = backend
                    .cached_conversations
                    .lock()
                    .expect("conversation cache lock poisoned");
                cache.retain(|(cached_account_id, _), _| cached_account_id != &account_id.0);
                for (key, value) in conversations {
                    cache.insert(key, value);
                }
            }
            Ok(Some(detail))
        })
    }

    fn send_draft(
        &self,
        account_id: &MailAccountId,
        draft: &DraftMessage,
    ) -> BoxFuture<'_, anyhow::Result<Option<MessageDetail>>> {
        let backend = self.clone();
        let account_id = account_id.clone();
        let draft = draft.clone();
        Box::pin(async move {
            if let Some(binding) = backend.live_binding(&account_id) {
                let outbox_uri =
                    local_sibling_folder_uri(binding.drafts_folder.as_deref(), "Outbox")
                        .ok_or_else(|| anyhow!("EDS binding has no local Outbox configuration"))?;
                let from = backend.sender_for_draft(&account_id, &draft)?;
                return append_draft_via_local_eds_cache(&outbox_uri, &from, &draft, false)
                    .map(Some);
            }

            Ok(None)
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
    let Some((source_uid, drafts_folder)) = parse_folder_uri(drafts_uri) else {
        return Ok(Vec::new());
    };
    if source_uid != "local" {
        return Ok(Vec::new());
    }

    let mut session =
        crate::integration::camel::AccountSession::open_cached_source(source_uid, "maildir")?;
    let mut results = Vec::new();
    let drafts_folder = FolderId(drafts_folder.to_string());
    let outbox_folder = local_sibling_folder_uri(Some(drafts_uri), "Outbox")
        .and_then(|uri| parse_folder_uri(&uri).map(|(_, folder)| FolderId(folder.to_string())));
    let sent_folder = local_sibling_folder_uri(Some(drafts_uri), "Sent")
        .and_then(|uri| parse_folder_uri(&uri).map(|(_, folder)| FolderId(folder.to_string())));

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
        Backend, CachedDeliveryPlacement, MailBackend, classify_cached_delivery,
        conversation_belongs_to_local_collection, local_sibling_folder_uri, replaces_local_draft,
        summary_contains_query,
    };
    use crate::integration::registry::Snapshot;
    use crate::model::account::{AliasId, MailAccount, MailAccountId, SendingIdentity};
    use crate::model::mail::{ConversationId, ConversationSummary, DraftMessage, FolderId};

    fn registry_entry(uid: &str, goa_id: &str) -> crate::integration::registry::Source {
        crate::integration::registry::Source {
            object_path: format!("esource:{uid}"),
            uid: Some(uid.into()),
            goa_account_id: Some(goa_id.into()),
            mail_enabled: Some(true),
            ..Default::default()
        }
    }

    fn registry_triplet(goa_id: &str, address: &str) -> crate::integration::registry::MailTriplet {
        let mut account = registry_entry("account-source", goa_id);
        account.backend_name = Some("imapx".into());
        account.goa_name = Some("GOA Name".into());
        account.goa_address = Some(address.into());
        let mut identity = registry_entry("identity-source", goa_id);
        identity.identity_name = Some("Mail Identity".into());
        identity.identity_address = Some(address.into());
        identity.identity_reply_to = Some("reply@example.invalid".into());
        let mut transport = registry_entry("transport-source", goa_id);
        transport.backend_name = Some("smtp".into());
        crate::integration::registry::MailTriplet {
            account: Some(account),
            identity: Some(identity),
            transport: Some(transport),
        }
    }

    #[test]
    fn registry_discovery_accepts_only_complete_goa_mail_triplets() {
        let valid = registry_triplet("goa-account", "owner@example.invalid");
        let mut disabled = registry_triplet("disabled", "disabled@example.invalid");
        disabled.account.as_mut().unwrap().mail_enabled = Some(false);
        let mut non_goa = registry_triplet("temporary", "local@example.invalid");
        for entry in [
            non_goa.account.as_mut().unwrap(),
            non_goa.identity.as_mut().unwrap(),
            non_goa.transport.as_mut().unwrap(),
        ] {
            entry.goa_account_id = None;
        }
        let incomplete = crate::integration::registry::MailTriplet {
            transport: None,
            ..registry_triplet("incomplete", "incomplete@example.invalid")
        };
        let mut missing_backend = registry_triplet("missing-backend", "backend@example.invalid");
        missing_backend.transport.as_mut().unwrap().backend_name = None;
        let missing_address = registry_triplet("missing-address", "   ");
        let mut partially_linked = registry_triplet("partially-linked", "partial@example.invalid");
        partially_linked.identity.as_mut().unwrap().goa_account_id = None;
        let snapshot = Snapshot {
            triplets: vec![
                valid,
                disabled,
                non_goa,
                incomplete,
                missing_backend,
                missing_address,
                partially_linked,
            ],
            ..Default::default()
        };

        let accounts = super::accounts_from_registry(&snapshot);

        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].id.0, "goa-account");
        assert_eq!(accounts[0].display_name, "Mail Identity");
        assert_eq!(accounts[0].primary_address, "owner@example.invalid");
        assert_eq!(accounts[0].aliases[0].id.0, "goa-account:primary");
        assert_eq!(
            accounts[0].aliases[0].reply_to.as_deref(),
            Some("reply@example.invalid")
        );
    }

    #[test]
    fn registry_discovery_rejects_conflicting_ids_and_deduplicates_goa_accounts() {
        let first = registry_triplet("same-account", "first@example.invalid");
        let duplicate = registry_triplet("same-account", "second@example.invalid");
        let mut conflicting = registry_triplet("conflicting", "third@example.invalid");
        conflicting.transport.as_mut().unwrap().goa_account_id = Some("different-account".into());
        let snapshot = Snapshot {
            triplets: vec![first, duplicate, conflicting],
            ..Default::default()
        };

        let accounts = super::accounts_from_registry(&snapshot);

        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].id.0, "same-account");
        assert_eq!(accounts[0].primary_address, "first@example.invalid");
    }

    #[test]
    fn registry_binding_skips_an_incomplete_duplicate_of_a_discovered_account() {
        let valid = registry_triplet("same-account", "owner@example.invalid");
        let mut incomplete = valid.clone();
        incomplete.account.as_mut().unwrap().uid = Some("stale-account-source".into());
        incomplete.transport.as_mut().unwrap().backend_name = None;
        let snapshot = Snapshot {
            triplets: vec![incomplete, valid],
            ..Default::default()
        };
        let accounts = super::accounts_from_registry(&snapshot);

        let bindings = super::bind_accounts_to_triplets(&accounts, &snapshot);

        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].account_uid.as_deref(), Some("account-source"));
        assert_eq!(bindings[0].transport_backend_name.as_deref(), Some("smtp"));
    }

    #[test]
    fn stub_writes_always_succeed_without_changing_the_stub() {
        let stale_account_id = MailAccountId("account-one".into());
        let stale_account = MailAccount {
            id: stale_account_id.clone(),
            display_name: "Example Account".into(),
            primary_address: "primary@example.invalid".into(),
            aliases: vec![SendingIdentity::with_id(
                AliasId("operations".into()),
                "operations@example.invalid".into(),
                "Operations".into(),
                None,
                String::new(),
                String::new(),
                true,
            )],
        };
        let backend = Backend::from_accounts(&[stale_account], Vec::new());
        let local_stub = Backend::from_accounts(&[], Vec::new());
        let local_stub_id = crate::integration::stub::stub_account().id;
        let stale_folders = futures::executor::block_on(backend.list_folders(&stale_account_id))
            .expect("an account whose EDS source disappeared should expose the stub folders");
        let local_folders = futures::executor::block_on(local_stub.list_folders(&local_stub_id))
            .expect("no-account mode should expose the stub folders");
        assert_eq!(
            stale_folders
                .iter()
                .map(|folder| &folder.id)
                .collect::<Vec<_>>(),
            local_folders
                .iter()
                .map(|folder| &folder.id)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            futures::executor::block_on(backend.list_conversations(
                &stale_account_id,
                &FolderId("inbox".into()),
                0,
                0,
            ))
            .expect("an account whose EDS source disappeared should expose stub messages"),
            futures::executor::block_on(local_stub.list_conversations(
                &local_stub_id,
                &FolderId("inbox".into()),
                0,
                0,
            ))
            .expect("no-account mode should expose the stub messages")
        );
        let unknown_message = ConversationId("not-a-stub-message".into());
        let before = backend.stub_store.conversations(&FolderId("inbox".into()));

        futures::executor::block_on(backend.set_starred(&stale_account_id, &unknown_message, true))
            .expect("stub star writes should be accepted");
        futures::executor::block_on(backend.set_read(&stale_account_id, &unknown_message, true))
            .expect("stub read writes should be accepted");
        futures::executor::block_on(backend.move_to_folder(
            &stale_account_id,
            &unknown_message,
            &FolderId("trash".into()),
        ))
        .expect("stub moves should be accepted");

        let draft = DraftMessage::empty(stale_account_id.clone(), AliasId("unknown".into()));
        assert!(
            futures::executor::block_on(backend.save_draft(&stale_account_id, &draft))
                .expect("stub draft saves should be accepted")
                .is_none()
        );
        assert!(
            futures::executor::block_on(backend.send_draft(&stale_account_id, &draft))
                .expect("stub sends should be accepted")
                .is_none()
        );

        let store = &backend.stub_store;
        assert_eq!(store.conversations(&FolderId("inbox".into())), before);
        assert!(store.conversations(&FolderId("drafts".into())).is_empty());
        assert!(store.conversations(&FolderId("sent".into())).is_empty());
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
    fn local_outbox_is_a_sibling_of_nested_identity_folders() {
        assert_eq!(
            local_sibling_folder_uri(Some("folder://local/account-1/Drafts"), "Outbox"),
            Some("folder://local/account-1/Outbox".into())
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
            local_sibling_folder_uri(Some("folder://local/Drafts"), "Outbox"),
            Some("folder://local/Outbox".into())
        );
    }

    #[test]
    fn local_sibling_rejects_remote_and_malformed_uris() {
        assert_eq!(
            local_sibling_folder_uri(Some("folder://microsoft365/Drafts"), "Outbox"),
            None
        );
        assert_eq!(local_sibling_folder_uri(Some("Drafts"), "Outbox"), None);
        assert_eq!(local_sibling_folder_uri(None, "Outbox"), None);
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
