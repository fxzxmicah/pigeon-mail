use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_uint, c_ulong, c_void};
use std::ptr::{self, NonNull};
use std::sync::Arc;

use anyhow::{Result, anyhow};

use super::{OwnedGObject, OwnedGlibString, take_gerror};
use crate::integration::account::EdsAccountBinding;
use crate::model::mail::{
    ConversationId, ConversationSummary, FolderId, MailFolder, MessageDetail, StoredMessageRef,
    sort_and_deduplicate_conversations, sort_conversations,
};

#[cfg(debug_assertions)]
macro_rules! development_probe_log {
    ($($arg:tt)*) => {
        tracing::debug!(target: "pigeon::development::camel", "{}", format_args!($($arg)*));
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

mod folder;
mod message;

use folder::load_folder_tree;
pub(crate) use message::AppendMessageRequest;
pub(super) use message::decode_addresses;
use message::{
    append_cached_message, append_message_to_folder, export_attachment, load_message_detail,
    load_mime_message,
};

const CAMEL_PROVIDER_STORE: c_int = 0;
const CAMEL_PROVIDER_TRANSPORT: c_int = 1;
const CAMEL_STORE_FOLDER_CREATE: c_uint = 1 << 0;
const CAMEL_SERVICE_CONNECTED: c_int = 2;
const CAMEL_MESSAGE_FLAGGED: c_uint = 1 << 3;
const CAMEL_MESSAGE_SEEN: c_uint = 1 << 4;
const CAMEL_MESSAGE_DELETED: c_uint = 1 << 1;
const CAMEL_MESSAGE_DRAFT: c_uint = 1 << 2;
const CAMEL_MESSAGE_ATTACHMENTS: c_uint = 1 << 5;
const CAMEL_MESSAGE_FOLDER_FLAGGED: c_uint = 1 << 16;
const DELIVERY_STATE_USER_TAG: &str = "$DeliveryState";
pub(crate) const DELIVERY_SUBMITTING: &str = "submitting";
pub(crate) const DELIVERY_PROVIDER_COPY: &str = "provider-copy";
pub(crate) const DELIVERY_COPY_REQUIRED: &str = "copy-required";
const DRAFT_SYNCED_USER_FLAG: &str = "$DraftSynced";
const DRAFT_SUPERSEDED_USER_FLAG: &str = "$DraftSuperseded";
const CONVERSATION_ID_SEPARATOR: char = '\u{1f}';

enum ESourceRegistry {}
enum ESource {}
enum CamelSession {}
enum CamelService {}
enum CamelStore {}
enum CamelTransport {}
enum CamelOfflineStore {}
enum CamelFolder {}
enum CamelFolderSummary {}
enum CamelMessageInfo {}

struct OwnedPtrArray(NonNull<glib::ffi::GPtrArray>);

impl OwnedPtrArray {
    unsafe fn from_ptr(pointer: *mut glib::ffi::GPtrArray) -> Option<Self> {
        NonNull::new(pointer).map(Self)
    }

    fn as_ptr(&self) -> *mut glib::ffi::GPtrArray {
        self.0.as_ptr()
    }

    fn values(&self) -> Result<&[glib::ffi::gpointer]> {
        let array = unsafe { self.0.as_ref() };
        let len = array.len as usize;
        if len == 0 {
            return Ok(&[]);
        }
        if array.pdata.is_null() {
            return Err(anyhow!("GPtrArray contains elements without storage"));
        }
        Ok(unsafe { std::slice::from_raw_parts(array.pdata, len) })
    }
}

impl Drop for OwnedPtrArray {
    fn drop(&mut self) {
        unsafe { glib::ffi::g_ptr_array_unref(self.0.as_ptr()) };
    }
}

pub(crate) struct AccountSession {
    account_uid: String,
    backend_name: String,
    auth_method: String,
    access_mode: CamelAccessMode,
    resources: CamelSessionResources,
    store: *mut CamelStore,
}

pub(crate) struct TransportSession {
    _resources: CamelSessionResources,
    transport: *mut CamelTransport,
}

struct CamelSessionResources {
    service_uid: String,
    registry: *mut ESourceRegistry,
    account_source: *mut ESource,
    config_source: *mut ESource,
    session: *mut CamelSession,
    service: *mut CamelService,
}

pub(crate) struct ChangeMonitor {
    session: Option<AccountSession>,
    folders: Vec<WatchedFolder>,
}

struct WatchedFolder {
    folder: *mut CamelFolder,
    handler_id: c_ulong,
}

unsafe impl Send for TransportSession {}

unsafe impl Send for AccountSession {}

unsafe impl Send for ChangeMonitor {}

#[derive(Clone, Copy)]
enum CamelAccessMode {
    CachedOnly,
    Local,
    Online,
}

impl CamelAccessMode {
    fn may_load_message(self) -> bool {
        matches!(self, Self::Local | Self::Online)
    }
}

type ChangeCallback = Arc<dyn Fn() + Send + Sync>;

impl ChangeMonitor {
    pub(crate) fn open(
        binding: &EdsAccountBinding,
        callback: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self> {
        let started = std::time::Instant::now();
        tracing::debug!(
            target: "pigeon::eds",
            "foreground account change monitor connecting"
        );
        let mut session = AccountSession::open_online(binding)?;
        development_probe_log!(
            "change monitor online session connected in {:?}",
            started.elapsed()
        );
        let callback: ChangeCallback = Arc::new(callback);
        let folder_infos = session.list_folders()?;
        development_probe_log!(
            "change monitor discovered {} folders in {:?}",
            folder_infos.len(),
            started.elapsed()
        );
        let store = session.store;
        let mut monitor = Self {
            session: Some(session),
            folders: Vec::with_capacity(folder_infos.len()),
        };

        for folder_info in folder_infos {
            let folder = unsafe { get_folder(store, &folder_info.id.0) }?;
            let user_data = Box::into_raw(Box::new(Arc::clone(&callback))) as *mut c_void;
            let handler_id = unsafe {
                mail_bridge_camel_folder_watch_changes(
                    folder.as_ptr(),
                    Some(invoke_change_callback),
                    user_data,
                    Some(drop_change_callback),
                )
            };
            if handler_id == 0 {
                unsafe { drop_change_callback(user_data) };
                return Err(anyhow!(
                    "failed to subscribe to Camel folder '{}' changes",
                    folder_info.id.0
                ));
            }
            monitor.folders.push(WatchedFolder {
                folder: folder.into_raw(),
                handler_id,
            });
            development_probe_log!(
                "change monitor subscribed folder {} in {:?}",
                folder_info.id.0,
                started.elapsed()
            );
        }

        if monitor.folders.is_empty() {
            return Err(anyhow!(
                "Camel account exposes no folders that can be monitored"
            ));
        }

        tracing::debug!(
            target: "pigeon::eds",
            folders = monitor.folders.len(),
            elapsed = ?started.elapsed(),
            "foreground account change monitor connected"
        );
        Ok(monitor)
    }
}

unsafe extern "C" fn invoke_change_callback(user_data: *mut c_void) {
    let callback = unsafe { &*(user_data as *const ChangeCallback) };
    callback();
}

unsafe extern "C" fn drop_change_callback(user_data: *mut c_void) {
    drop(unsafe { Box::from_raw(user_data as *mut ChangeCallback) });
}

#[derive(Debug, Clone)]
pub(crate) struct MessageSyncState {
    pub(crate) conversation_id: ConversationId,
    pub(crate) message_id_hash: u64,
    pub(crate) delivery_state: Option<String>,
    pub(crate) draft_synced: bool,
    pub(crate) draft_superseded: bool,
}

#[allow(clashing_extern_declarations)]
#[link(name = "camel-1.2")]
#[link(name = "edataserver-1.2")]
unsafe extern "C" {
    fn camel_store_get_type() -> glib::ffi::GType;
    fn camel_transport_get_type() -> glib::ffi::GType;
    fn camel_offline_store_get_type() -> glib::ffi::GType;
    fn camel_session_add_service(
        session: *mut CamelSession,
        uid: *const c_char,
        protocol: *const c_char,
        provider_type: c_int,
        error: *mut *mut glib::ffi::GError,
    ) -> *mut CamelService;
    fn camel_session_remove_services(session: *mut CamelSession);
    fn camel_service_connect_sync(
        service: *mut CamelService,
        cancellable: *mut gio::ffi::GCancellable,
        error: *mut *mut glib::ffi::GError,
    ) -> glib::ffi::gboolean;
    fn camel_service_get_connection_status(service: *mut CamelService) -> c_int;
    fn camel_store_get_folder_sync(
        store: *mut CamelStore,
        folder_name: *const c_char,
        flags: c_uint,
        cancellable: *mut gio::ffi::GCancellable,
        error: *mut *mut glib::ffi::GError,
    ) -> *mut CamelFolder;
    fn camel_folder_get_folder_summary(folder: *mut CamelFolder) -> *mut CamelFolderSummary;
    fn camel_folder_summary_save(
        summary: *mut CamelFolderSummary,
        error: *mut *mut glib::ffi::GError,
    ) -> glib::ffi::gboolean;
    fn camel_folder_get_message_flags(
        folder: *mut CamelFolder,
        message_uid: *const c_char,
    ) -> c_uint;
    fn camel_folder_set_message_flags(
        folder: *mut CamelFolder,
        message_uid: *const c_char,
        mask: c_uint,
        set: c_uint,
    ) -> glib::ffi::gboolean;
    fn camel_folder_expunge_sync(
        folder: *mut CamelFolder,
        cancellable: *mut gio::ffi::GCancellable,
        error: *mut *mut glib::ffi::GError,
    ) -> glib::ffi::gboolean;
    fn camel_folder_synchronize_sync(
        folder: *mut CamelFolder,
        expunge: glib::ffi::gboolean,
        cancellable: *mut gio::ffi::GCancellable,
        error: *mut *mut glib::ffi::GError,
    ) -> glib::ffi::gboolean;
    fn camel_folder_dup_uids(folder: *mut CamelFolder) -> *mut glib::ffi::GPtrArray;
    fn camel_folder_transfer_messages_to_sync(
        source: *mut CamelFolder,
        message_uids: *mut glib::ffi::GPtrArray,
        destination: *mut CamelFolder,
        delete_originals: glib::ffi::gboolean,
        transferred_uids: *mut *mut glib::ffi::GPtrArray,
        cancellable: *mut gio::ffi::GCancellable,
        error: *mut *mut glib::ffi::GError,
    ) -> glib::ffi::gboolean;
    fn camel_store_synchronize_sync(
        store: *mut CamelStore,
        expunge: glib::ffi::gboolean,
        cancellable: *mut gio::ffi::GCancellable,
        error: *mut *mut glib::ffi::GError,
    ) -> glib::ffi::gboolean;
    fn camel_folder_refresh_info_sync(
        folder: *mut CamelFolder,
        cancellable: *mut gio::ffi::GCancellable,
        error: *mut *mut glib::ffi::GError,
    ) -> glib::ffi::gboolean;
    fn camel_folder_search_sync(
        folder: *mut CamelFolder,
        expression: *const c_char,
        out_uids: *mut *mut glib::ffi::GPtrArray,
        cancellable: *mut gio::ffi::GCancellable,
        error: *mut *mut glib::ffi::GError,
    ) -> glib::ffi::gboolean;
    fn camel_session_set_online(session: *mut CamelSession, online: glib::ffi::gboolean);
    fn camel_offline_store_set_online_sync(
        store: *mut CamelOfflineStore,
        online: glib::ffi::gboolean,
        cancellable: *mut gio::ffi::GCancellable,
        error: *mut *mut glib::ffi::GError,
    ) -> glib::ffi::gboolean;
    fn camel_folder_summary_prepare_fetch_all(
        summary: *mut CamelFolderSummary,
        error: *mut *mut glib::ffi::GError,
    ) -> glib::ffi::gboolean;
    fn camel_folder_get_message_info(
        folder: *mut CamelFolder,
        uid: *const c_char,
    ) -> *mut CamelMessageInfo;
    fn camel_message_info_get_flags(info: *const CamelMessageInfo) -> c_uint;
    fn camel_message_info_property_lock(info: *const CamelMessageInfo);
    fn camel_message_info_property_unlock(info: *const CamelMessageInfo);
    fn camel_message_info_get_message_id(info: *const CamelMessageInfo) -> u64;
    fn camel_message_info_get_user_flag(
        info: *const CamelMessageInfo,
        name: *const c_char,
    ) -> glib::ffi::gboolean;
    fn camel_message_info_get_user_tag(
        info: *const CamelMessageInfo,
        name: *const c_char,
    ) -> *const c_char;
    fn camel_message_info_dup_user_tag(
        info: *const CamelMessageInfo,
        name: *const c_char,
    ) -> *mut c_char;
    fn camel_message_info_set_user_tag(
        info: *mut CamelMessageInfo,
        name: *const c_char,
        value: *const c_char,
    ) -> glib::ffi::gboolean;
    fn camel_message_info_set_user_flag(
        info: *mut CamelMessageInfo,
        name: *const c_char,
        enabled: glib::ffi::gboolean,
    ) -> glib::ffi::gboolean;
    fn camel_message_info_set_folder_flagged(
        info: *mut CamelMessageInfo,
        folder_flagged: glib::ffi::gboolean,
    ) -> glib::ffi::gboolean;
    fn camel_message_info_get_subject(info: *const CamelMessageInfo) -> *const c_char;
    fn camel_message_info_get_from(info: *const CamelMessageInfo) -> *const c_char;
    fn camel_message_info_get_preview(info: *const CamelMessageInfo) -> *const c_char;
    fn camel_message_info_get_date_sent(info: *const CamelMessageInfo) -> i64;
    fn camel_message_info_get_date_received(info: *const CamelMessageInfo) -> i64;

    fn e_source_registry_new_sync(
        cancellable: *mut gio::ffi::GCancellable,
        error: *mut *mut glib::ffi::GError,
    ) -> *mut ESourceRegistry;
    fn e_source_registry_ref_source(
        registry: *mut ESourceRegistry,
        uid: *const c_char,
    ) -> *mut ESource;
    fn e_source_has_extension(
        source: *mut ESource,
        extension_name: *const c_char,
    ) -> glib::ffi::gboolean;
    fn e_source_get_uid(source: *mut ESource) -> *const c_char;
    fn e_source_dup_parent(source: *mut ESource) -> *mut c_char;
    fn e_source_camel_get_extension_name(protocol: *const c_char) -> *const c_char;
    fn e_source_camel_configure_service(source: *mut ESource, service: *mut CamelService);
    fn camel_url_encode(value: *const c_char, extra: *const c_char) -> *mut c_char;

    fn mail_bridge_eds_session_new(registry: *mut ESourceRegistry) -> *mut CamelSession;
    fn mail_bridge_eds_configure_local_store(
        service: *mut CamelService,
    ) -> glib::ffi::gboolean;
    fn mail_bridge_camel_folder_watch_changes(
        folder: *mut CamelFolder,
        callback: Option<unsafe extern "C" fn(*mut c_void)>,
        user_data: *mut c_void,
        destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    ) -> c_ulong;
    fn mail_bridge_camel_folder_unwatch_changes(folder: *mut CamelFolder, handler_id: c_ulong);
    fn mail_bridge_eds_transport_send_cached_message(
        transport: *mut CamelTransport,
        folder: *mut CamelFolder,
        message_uid: *const c_char,
        out_sent_message_saved: *mut glib::ffi::gboolean,
        error: *mut *mut glib::ffi::GError,
    ) -> glib::ffi::gboolean;
}

impl CamelSessionResources {
    fn empty() -> Self {
        Self {
            service_uid: String::new(),
            registry: ptr::null_mut(),
            account_source: ptr::null_mut(),
            config_source: ptr::null_mut(),
            session: ptr::null_mut(),
            service: ptr::null_mut(),
        }
    }

    unsafe fn open(
        account_source_uid: &str,
        backend_name: &str,
        source_kind: &str,
        provider_type: c_int,
        online: bool,
    ) -> Result<Self> {
        let mut resources = Self::empty();
        resources.registry = unsafe { new_registry()? };
        resources.account_source = unsafe { ref_source(resources.registry, account_source_uid) }
            .ok_or_else(|| {
                anyhow!(
                    "EDS registry could not resolve {} source {}",
                    source_kind,
                    account_source_uid
                )
            })?;
        resources.config_source = unsafe {
            ref_service_config_source(
                resources.registry,
                resources.account_source,
                backend_name,
            )
        }
        .ok_or_else(|| {
            anyhow!(
                "could not find an EDS source with Camel backend extension for {} '{}' and backend '{}'",
                source_kind,
                account_source_uid,
                backend_name
            )
        })?;
        resources.session = unsafe { mail_bridge_eds_session_new(resources.registry) };
        if resources.session.is_null() {
            return Err(anyhow!("mail_bridge_eds_session_new returned NULL"));
        }
        resources.service_uid = unsafe { source_uid(resources.config_source) }
            .ok_or_else(|| anyhow!("{} EDS config source has no uid", source_kind))?;
        unsafe { camel_session_set_online(resources.session, if online { 1 } else { 0 }) };
        resources.service = unsafe {
            add_service(
                resources.session,
                &resources.service_uid,
                backend_name,
                provider_type,
            )?
        };
        unsafe {
            e_source_camel_configure_service(resources.config_source, resources.service);
        }
        Ok(resources)
    }
}

impl AccountSession {
    pub(crate) fn open_cached(binding: &EdsAccountBinding) -> Result<Self> {
        Self::open_with_mode(binding, CamelAccessMode::CachedOnly)
    }

    pub(crate) fn open_online(binding: &EdsAccountBinding) -> Result<Self> {
        Self::open_with_mode(binding, CamelAccessMode::Online)
    }

    pub(crate) fn open_local_mailbox() -> Result<Self> {
        Self::open_source_with_mode(
            "local",
            "maildir",
            "unknown",
            CamelAccessMode::Local,
        )
    }

    fn open_with_mode(binding: &EdsAccountBinding, mode: CamelAccessMode) -> Result<Self> {
        let auth_method = binding.account_auth_method.as_deref().unwrap_or("unknown");

        Self::open_source_with_mode(
            &binding.account_uid,
            &binding.account_backend_name,
            auth_method,
            mode,
        )
    }

    fn open_source_with_mode(
        account_source_uid: &str,
        backend_name: &str,
        auth_method: &str,
        mode: CamelAccessMode,
    ) -> Result<Self> {
        let account_uid = account_source_uid.to_string();
        let backend_name = backend_name.to_string();
        let auth_method = auth_method.to_string();

        unsafe {
            let resources = CamelSessionResources::open(
                &account_uid,
                &backend_name,
                "account",
                CAMEL_PROVIDER_STORE,
                matches!(mode, CamelAccessMode::Online),
            )?;
            if matches!(mode, CamelAccessMode::Local)
                && mail_bridge_eds_configure_local_store(resources.service) == 0
            {
                return Err(anyhow!("could not configure the built-in local Camel store"));
            }
            let mut this = Self {
                account_uid,
                backend_name,
                auth_method,
                access_mode: mode,
                resources,
                store: ptr::null_mut(),
            };
            this.ensure_store()?;
            match mode {
                CamelAccessMode::CachedOnly | CamelAccessMode::Local => {
                    this.ensure_store_offline()?
                }
                CamelAccessMode::Online => {
                    this.ensure_connected()?
                }
            }
            Ok(this)
        }
    }

    pub(crate) fn list_folders(&mut self) -> Result<Vec<MailFolder>> {
        unsafe { load_folder_tree(self.store) }
    }

    pub(crate) fn list_conversations(
        &mut self,
        folder_id: &FolderId,
    ) -> Result<Vec<ConversationSummary>> {
        unsafe { load_folder_conversations(self.store, folder_id) }
    }

    pub(crate) fn prepare_folder_for_synchronization(
        &mut self,
        folder_id: &FolderId,
    ) -> Result<()> {
        drop(unsafe { get_folder(self.store, &folder_id.0) }?);
        Ok(())
    }

    pub(crate) fn list_message_sync_states(
        &mut self,
        folder_id: &FolderId,
    ) -> Result<Vec<MessageSyncState>> {
        unsafe { load_message_sync_states(self.store, folder_id) }
    }

    pub(crate) fn search_conversations(&mut self, query: &str) -> Result<Vec<ConversationSummary>> {
        // Provider body-search hooks may perform a server request even on an
        // otherwise cached-only session. Account-store search is header-only.
        let expression = search_expression(query, false)?;
        let folders = self.list_folders()?;
        let mut results = Vec::new();
        for folder in folders {
            results.extend(unsafe {
                search_folder_conversations(self.store, &folder.id, &expression)?
            });
        }
        sort_and_deduplicate_conversations(&mut results);
        Ok(results)
    }

    pub(crate) fn search_folder_conversations(
        &mut self,
        folder_id: &FolderId,
        query: &str,
    ) -> Result<Vec<ConversationSummary>> {
        // This entry point is used for Pigeon's built-in local Maildir, where
        // body search is guaranteed to remain on disk.
        let expression = search_expression(query, true)?;
        unsafe { search_folder_conversations(self.store, folder_id, &expression) }
    }

    pub(crate) fn begin_submission(
        &mut self,
        conversation_id: &ConversationId,
    ) -> Result<()> {
        self.set_delivery_state(conversation_id, DELIVERY_SUBMITTING)
    }

    pub(crate) fn complete_submission(
        &mut self,
        conversation_id: &ConversationId,
        provider_copy_expected: bool,
    ) -> Result<()> {
        if provider_copy_expected {
            self.expect_provider_sent_copy(conversation_id)
        } else {
            self.set_delivery_state(conversation_id, DELIVERY_COPY_REQUIRED)
        }
    }

    pub(crate) fn expect_provider_sent_copy(
        &mut self,
        conversation_id: &ConversationId,
    ) -> Result<()> {
        self.set_delivery_state(conversation_id, DELIVERY_PROVIDER_COPY)
    }

    fn set_delivery_state(
        &mut self,
        conversation_id: &ConversationId,
        state: &str,
    ) -> Result<()> {
        let (folder_name, uid) = conversation_location(conversation_id)?;
        unsafe {
            set_message_user_tag(
                self.store,
                folder_name,
                uid,
                DELIVERY_STATE_USER_TAG,
                state,
            )
        }
    }

    pub(crate) fn set_draft_synced(
        &mut self,
        conversation_id: &ConversationId,
        synced: bool,
    ) -> Result<()> {
        self.set_message_user_flag(conversation_id, DRAFT_SYNCED_USER_FLAG, synced)
    }

    pub(crate) fn set_draft_superseded(
        &mut self,
        conversation_id: &ConversationId,
        superseded: bool,
    ) -> Result<()> {
        self.set_message_user_flag(
            conversation_id,
            DRAFT_SUPERSEDED_USER_FLAG,
            superseded,
        )
    }

    pub(crate) fn append_cached_draft_from(
        &mut self,
        source: &mut AccountSession,
        source_conversation_id: &ConversationId,
        destination_folder_id: &FolderId,
    ) -> Result<()> {
        self.append_cached_from(source, source_conversation_id, destination_folder_id, true)
    }

    pub(crate) fn append_cached_sent_from(
        &mut self,
        source: &mut AccountSession,
        source_conversation_id: &ConversationId,
        destination_folder_id: &FolderId,
    ) -> Result<()> {
        self.append_cached_from(source, source_conversation_id, destination_folder_id, false)
    }

    fn append_cached_from(
        &mut self,
        source: &mut AccountSession,
        source_conversation_id: &ConversationId,
        destination_folder_id: &FolderId,
        is_draft: bool,
    ) -> Result<()> {
        let (source_folder_name, message_uid) = conversation_location(source_conversation_id)?;
        unsafe {
            append_cached_message(
                source.store,
                source_folder_name,
                message_uid,
                self.store,
                destination_folder_id,
                is_draft,
            )
        }
    }

    fn set_message_user_flag(
        &mut self,
        conversation_id: &ConversationId,
        flag: &str,
        enabled: bool,
    ) -> Result<()> {
        let (folder_name, uid) = conversation_location(conversation_id)?;
        unsafe { set_message_user_flag(self.store, folder_name, uid, flag, enabled) }
    }

    pub(crate) fn fill_message_cache(&self, conversation_id: &ConversationId) -> Result<bool> {
        let (folder_name, uid) = conversation_location(conversation_id)?;
        let uid = CString::new(uid).map_err(|_| anyhow!("message uid contains interior NUL"))?;
        let folder = unsafe { get_folder(self.store, folder_name) }?;
        let message = unsafe {
            load_mime_message(folder.as_ptr(), &uid, self.access_mode.may_load_message())?
        };
        Ok(message.is_some())
    }

    pub(crate) fn get_message_detail(
        &mut self,
        conversation_id: &ConversationId,
    ) -> Result<Option<MessageDetail>> {
        let (folder_name, uid) = conversation_location(conversation_id)?;
        unsafe {
            load_message_detail(
                self.store,
                folder_name,
                uid,
                conversation_id,
                self.access_mode.may_load_message(),
            )
        }
    }

    pub(crate) fn set_starred(
        &mut self,
        conversation_id: &ConversationId,
        starred: bool,
    ) -> Result<()> {
        self.set_message_flag(conversation_id, CAMEL_MESSAGE_FLAGGED, starred)
    }

    pub(crate) fn set_read(&mut self, conversation_id: &ConversationId, read: bool) -> Result<()> {
        self.set_message_flag(conversation_id, CAMEL_MESSAGE_SEEN, read)
    }

    pub(crate) fn set_draft(
        &mut self,
        conversation_id: &ConversationId,
        draft: bool,
    ) -> Result<()> {
        self.set_message_flag(conversation_id, CAMEL_MESSAGE_DRAFT, draft)
    }

    pub(crate) fn set_read_for_sync(
        &mut self,
        conversation_id: &ConversationId,
        read: bool,
    ) -> Result<()> {
        self.set_message_flag_for_sync(conversation_id, CAMEL_MESSAGE_SEEN, read)
    }

    pub(crate) fn set_starred_for_sync(
        &mut self,
        conversation_id: &ConversationId,
        starred: bool,
    ) -> Result<()> {
        self.set_message_flag_for_sync(conversation_id, CAMEL_MESSAGE_FLAGGED, starred)
    }

    fn set_message_flag_for_sync(
        &mut self,
        conversation_id: &ConversationId,
        flag: c_uint,
        enabled: bool,
    ) -> Result<()> {
        let (folder_name, uid) = conversation_location(conversation_id)?;
        unsafe { set_message_flag_for_sync(self.store, folder_name, uid, flag, enabled) }
    }

    fn set_message_flag(
        &mut self,
        conversation_id: &ConversationId,
        flag: c_uint,
        enabled: bool,
    ) -> Result<()> {
        let (folder_name, uid) = conversation_location(conversation_id)?;
        unsafe { set_message_flag(self.store, folder_name, uid, flag, enabled) }
    }

    pub(crate) fn move_message(
        &mut self,
        conversation_id: &ConversationId,
        destination_folder_id: &FolderId,
    ) -> Result<()> {
        let (source_folder_name, uid) = conversation_location(conversation_id)?;
        unsafe {
            move_message(
                self.store,
                source_folder_name,
                uid,
                &destination_folder_id.0,
            )
        }
    }

    pub(crate) fn export_attachment(
        &mut self,
        conversation_id: &ConversationId,
        attachment_uri: &str,
    ) -> Result<Option<String>> {
        let (folder_name, uid) = conversation_location(conversation_id)?;
        unsafe {
            export_attachment(
                self.store,
                self.resources.service,
                folder_name,
                uid,
                conversation_id,
                attachment_uri,
                self.access_mode.may_load_message(),
            )
        }
    }

    pub(crate) fn refresh_folder_info(&mut self, folder_id: &FolderId) -> Result<()> {
        unsafe { refresh_folder_info(self.store, &folder_id.0) }
    }

    pub(crate) fn ensure_folder_path(&mut self, folder_id: &FolderId) -> Result<()> {
        for folder_path in local_folder_path_prefixes(&folder_id.0)? {
            drop(unsafe {
                get_folder_with_flags(self.store, folder_path, CAMEL_STORE_FOLDER_CREATE)
            }?);
        }
        Ok(())
    }

    pub(crate) fn synchronize(&mut self) -> Result<()> {
        unsafe { synchronize_store(self.store) }
    }

    pub(crate) fn mark_message_deleted(&mut self, conversation_id: &ConversationId) -> Result<()> {
        let (folder_name, uid) = conversation_location(conversation_id)?;
        unsafe { set_message_flag(self.store, folder_name, uid, CAMEL_MESSAGE_DELETED, true) }
    }

    pub(crate) fn synchronize_folder_flags(&mut self, folder_id: &FolderId) -> Result<()> {
        let folder = unsafe { get_folder(self.store, &folder_id.0) }?;
        let mut error = ptr::null_mut();
        if unsafe {
            camel_folder_synchronize_sync(folder.as_ptr(), 0, ptr::null_mut(), &mut error)
        } == 0
        {
            return Err(take_gerror(error)
                .unwrap_or_else(|| anyhow!("camel_folder_synchronize_sync returned FALSE")));
        }
        Ok(())
    }

    pub(crate) fn delete_local_message(
        &mut self,
        conversation_id: &ConversationId,
    ) -> Result<()> {
        assert!(matches!(self.access_mode, CamelAccessMode::Local));
        let (folder_name, uid) = conversation_location(conversation_id)?;
        unsafe { expunge_local_message(self.store, folder_name, uid) }
    }

    pub(crate) fn append_message(
        &mut self,
        request: &AppendMessageRequest<'_>,
    ) -> Result<StoredMessageRef> {
        unsafe {
            append_message_to_folder(
                self.store,
                &self.account_uid,
                request,
            )
        }
    }

    fn ensure_store(&mut self) -> Result<()> {
        unsafe {
            let is_store = glib::gobject_ffi::g_type_check_instance_is_a(
                self.resources.service as *mut _,
                camel_store_get_type(),
            ) != 0;
            if !is_store {
                #[cfg(debug_assertions)]
                {
                    let type_name = object_type_name(self.resources.service as *mut _)
                        .unwrap_or_else(|| "unknown".into());
                    return Err(anyhow!(
                        "Camel service for backend '{}' and account '{}' is not a CamelStore (actual type: {}, service_uid: {})",
                        self.backend_name,
                        self.account_uid,
                        type_name,
                        self.resources.service_uid,
                    ));
                }
                #[cfg(not(debug_assertions))]
                {
                    return Err(anyhow!(
                        "Camel service for backend '{}' and account '{}' is not a CamelStore",
                        self.backend_name,
                        self.account_uid,
                    ));
                }
            }

            self.store = self.resources.service as *mut CamelStore;
            Ok(())
        }
    }

    fn ensure_connected(&mut self) -> Result<()> {
        unsafe {
            let status = camel_service_get_connection_status(self.resources.service);
            if status != CAMEL_SERVICE_CONNECTED {
                connect_service(self.resources.service).map_err(|error| {
                    error.context(format!(
                        "Camel connect failed for account '{}' (backend='{}', auth='{}', service_uid='{}')",
                        self.account_uid,
                        self.backend_name,
                        self.auth_method,
                        self.resources.service_uid,
                    ))
                })?;
            }
            self.ensure_store_online()
        }
    }

    fn ensure_store_online(&mut self) -> Result<()> {
        unsafe {
            ensure_store_online(self.store).map_err(|error| {
                error.context(format!(
                    "Camel store online setup failed for account '{}' (backend='{}', service_uid='{}')",
                    self.account_uid,
                    self.backend_name,
                    self.resources.service_uid,
                ))
            })
        }
    }

    fn ensure_store_offline(&mut self) -> Result<()> {
        unsafe { ensure_store_offline(self.store) }
    }
}

impl TransportSession {
    pub(crate) fn open_online(binding: &EdsAccountBinding) -> Result<Self> {
        let account_uid = binding.transport_uid.as_str();
        let backend_name = binding.transport_backend_name.as_str();
        let auth_method = binding
            .transport_auth_method
            .as_deref()
            .unwrap_or("unknown");

        unsafe {
            let resources = CamelSessionResources::open(
                account_uid,
                backend_name,
                "transport",
                CAMEL_PROVIDER_TRANSPORT,
                true,
            )?;
            let is_transport = glib::gobject_ffi::g_type_check_instance_is_a(
                resources.service as *mut _,
                camel_transport_get_type(),
            ) != 0;
            if !is_transport {
                return Err(anyhow!(
                    "Camel service for backend '{}' is not a CamelTransport",
                    backend_name
                ));
            }
            if let Err(error) = connect_service(resources.service) {
                let error = anyhow!(
                    "Camel transport connect failed for account '{}' (backend='{}', auth='{}', service_uid='{}'): {}",
                    account_uid,
                    backend_name,
                    auth_method,
                    resources.service_uid,
                    error
                );
                return Err(error);
            }
            Ok(Self {
                transport: resources.service as *mut CamelTransport,
                _resources: resources,
            })
        }
    }

    pub(crate) fn send_cached_message(
        &mut self,
        source: &mut AccountSession,
        conversation_id: &ConversationId,
    ) -> Result<bool> {
        let (folder_name, uid) = conversation_location(conversation_id)?;
        let uid = CString::new(uid).map_err(|_| anyhow!("message uid contains interior NUL"))?;
        let folder = unsafe { get_folder(source.store, folder_name) }?;
        let mut sent_message_saved = 0;
        let mut error = ptr::null_mut();
        let success = unsafe {
            mail_bridge_eds_transport_send_cached_message(
                self.transport,
                folder.as_ptr(),
                uid.as_ptr(),
                &mut sent_message_saved,
                &mut error,
            )
        };
        if success == 0 {
            return Err(take_gerror(error).unwrap_or_else(|| {
                anyhow!("mail_bridge_eds_transport_send_cached_message failed")
            }));
        }
        Ok(sent_message_saved != 0)
    }
}

impl Drop for ChangeMonitor {
    fn drop(&mut self) {
        let session = self.session.take();
        let folders = std::mem::take(&mut self.folders)
            .into_iter()
            .map(|watched| (watched.folder as usize, watched.handler_id))
            .collect::<Vec<_>>();
        glib::MainContext::default().spawn(async move {
            for (folder, handler_id) in folders {
                let folder = folder as *mut CamelFolder;
                unsafe {
                    mail_bridge_camel_folder_unwatch_changes(folder, handler_id);
                    glib::gobject_ffi::g_object_unref(folder as *mut _);
                }
            }
            drop(session);
        });
    }
}

pub(crate) fn folder_uri(service_uid: &str, folder_id: &FolderId) -> Result<String> {
    Ok(format!(
        "folder://{}/{}",
        encode_folder_uri_component(service_uid, ":;@/", "service uid")?,
        encode_folder_uri_component(&folder_id.0, "#", "folder name")?,
    ))
}

fn encode_folder_uri_component(value: &str, allowed: &str, label: &str) -> Result<String> {
    let value = CString::new(value)
        .map_err(|_| anyhow!("{} contains interior NUL", label))?;
    let allowed = CString::new(allowed).expect("static URI allowlist contains no NUL");
    let encoded = unsafe {
        OwnedGlibString::from_ptr(camel_url_encode(value.as_ptr(), allowed.as_ptr()))
    }
    .ok_or_else(|| anyhow!("camel_url_encode returned NULL"))?;
    Ok(encoded.to_string_lossy())
}

impl Drop for CamelSessionResources {
    fn drop(&mut self) {
        // Camel posts service/folder closures to the default GLib context. Destroy the complete
        // object graph on that same context so a worker cannot finalize the store's object bag
        // while the main thread is concurrently releasing one of its folders.
        schedule_camel_session_teardown(
            self.service,
            self.session,
            self.config_source,
            self.account_source,
            self.registry,
        );
    }
}

fn schedule_camel_session_teardown(
    service: *mut CamelService,
    session: *mut CamelSession,
    config_source: *mut ESource,
    account_source: *mut ESource,
    registry: *mut ESourceRegistry,
) {
    if service.is_null()
        && session.is_null()
        && config_source.is_null()
        && account_source.is_null()
        && registry.is_null()
    {
        return;
    }
    let service = service as usize;
    let session = session as usize;
    let config_source = config_source as usize;
    let account_source = account_source as usize;
    let registry = registry as usize;
    glib::MainContext::default().spawn(async move {
        unsafe {
            teardown_camel_session(
                service as *mut CamelService,
                session as *mut CamelSession,
                config_source as *mut ESource,
                account_source as *mut ESource,
                registry as *mut ESourceRegistry,
            );
        }
    });
}

unsafe fn teardown_camel_session(
    service: *mut CamelService,
    session: *mut CamelSession,
    config_source: *mut ESource,
    account_source: *mut ESource,
    registry: *mut ESourceRegistry,
) {
    if !session.is_null() {
        unsafe { camel_session_remove_services(session) };
    }
    if !service.is_null() {
        unsafe { glib::gobject_ffi::g_object_unref(service as *mut _) };
    }
    if !session.is_null() {
        unsafe { glib::gobject_ffi::g_object_unref(session as *mut _) };
    }
    if !config_source.is_null() {
        unsafe { glib::gobject_ffi::g_object_unref(config_source as *mut _) };
    }
    if !account_source.is_null() {
        unsafe { glib::gobject_ffi::g_object_unref(account_source as *mut _) };
    }
    if !registry.is_null() {
        unsafe { glib::gobject_ffi::g_object_unref(registry as *mut _) };
    }
}

unsafe fn new_registry() -> Result<*mut ESourceRegistry> {
    let mut error = ptr::null_mut();
    let registry = unsafe { e_source_registry_new_sync(ptr::null_mut(), &mut error) };
    if registry.is_null() {
        return Err(take_gerror(error)
            .unwrap_or_else(|| anyhow!("e_source_registry_new_sync returned NULL")));
    }
    Ok(registry)
}

unsafe fn ref_source(registry: *mut ESourceRegistry, uid: &str) -> Option<*mut ESource> {
    let uid = CString::new(uid).ok()?;
    let source = unsafe { e_source_registry_ref_source(registry, uid.as_ptr()) };
    (!source.is_null()).then_some(source)
}

unsafe fn ref_service_config_source(
    registry: *mut ESourceRegistry,
    account_source: *mut ESource,
    backend_name: &str,
) -> Option<*mut ESource> {
    let extension_name = backend_extension_name(backend_name)?;

    if unsafe { source_has_extension(account_source, &extension_name) } {
        unsafe { glib::gobject_ffi::g_object_ref(account_source as *mut _) };
        return Some(account_source);
    }

    if let Some(parent_uid) = unsafe { source_parent_uid(account_source) } {
        let parent_source = unsafe {
            OwnedGObject::from_ptr(ref_source(registry, &parent_uid)?)
        }?;
        if unsafe { source_has_extension(parent_source.as_ptr(), &extension_name) } {
            return Some(parent_source.into_raw());
        }
    }

    if unsafe { source_has_extension(account_source, "Mail Account") } {
        unsafe { glib::gobject_ffi::g_object_ref(account_source as *mut _) };
        return Some(account_source);
    }

    None
}

fn backend_extension_name(backend_name: &str) -> Option<String> {
    let protocol = CString::new(backend_name).ok()?;
    let extension_name = unsafe { e_source_camel_get_extension_name(protocol.as_ptr()) };
    cstr_to_string(extension_name)
}

unsafe fn source_has_extension(source: *mut ESource, extension_name: &str) -> bool {
    let extension_name = match CString::new(extension_name) {
        Ok(value) => {
            value
        }
        Err(_) => {
            return false;
        }
    };
    unsafe { e_source_has_extension(source, extension_name.as_ptr()) != 0 }
}

unsafe fn source_parent_uid(source: *mut ESource) -> Option<String> {
    unsafe { OwnedGlibString::from_ptr(e_source_dup_parent(source)) }
        .map(|parent| parent.to_string_lossy())
}

unsafe fn source_uid(source: *mut ESource) -> Option<String> {
    cstr_to_string(unsafe { e_source_get_uid(source) })
}

unsafe fn add_service(
    session: *mut CamelSession,
    uid: &str,
    protocol: &str,
    provider_type: c_int,
) -> Result<*mut CamelService> {
    let uid = CString::new(uid).map_err(|_| anyhow!("service uid contains interior NUL"))?;
    let protocol =
        CString::new(protocol).map_err(|_| anyhow!("backend name contains interior NUL"))?;
    let mut error = ptr::null_mut();
    let service = unsafe {
        camel_session_add_service(
            session,
            uid.as_ptr(),
            protocol.as_ptr(),
            provider_type,
            &mut error,
        )
    };
    if service.is_null() {
        return Err(take_gerror(error)
            .unwrap_or_else(|| anyhow!("camel_session_add_service returned NULL")));
    }
    Ok(service)
}

unsafe fn connect_service(service: *mut CamelService) -> Result<()> {
    let mut error = ptr::null_mut();
    let ok = unsafe { camel_service_connect_sync(service, ptr::null_mut(), &mut error) };
    if ok == 0 {
        return Err(
            take_gerror(error).unwrap_or_else(|| anyhow!("camel_service_connect_sync failed"))
        );
    }
    Ok(())
}

unsafe fn load_folder_conversations(
    store: *mut CamelStore,
    folder_id: &FolderId,
) -> Result<Vec<ConversationSummary>> {
    let folder = unsafe { get_folder(store, &folder_id.0) }?;

    unsafe {
        let uids = load_folder_uids(folder.as_ptr(), &folder_id.0)?;
        collect_conversations(folder.as_ptr(), &folder_id.0, uids.values()?)
    }
}

unsafe fn load_message_sync_states(
    store: *mut CamelStore,
    folder_id: &FolderId,
) -> Result<Vec<MessageSyncState>> {
    let folder = unsafe { get_folder(store, &folder_id.0) }?;
    unsafe {
        let uids = load_folder_uids(folder.as_ptr(), &folder_id.0)?;
        let delivery_state_tag =
            CString::new(DELIVERY_STATE_USER_TAG).expect("static tag is valid");
        let draft_synced_flag =
            CString::new(DRAFT_SYNCED_USER_FLAG).expect("static flag is valid");
        let draft_superseded_flag =
            CString::new(DRAFT_SUPERSEDED_USER_FLAG).expect("static flag is valid");
        let mut states = Vec::new();
        for &uid_ptr in uids.values()? {
            let uid = cstr_to_string(uid_ptr as *const c_char).ok_or_else(|| {
                anyhow!("Camel returned an invalid message UID in folder '{}'", folder_id.0)
            })?;
            let info = get_message_info(folder.as_ptr(), &uid)?;
            let state = with_message_info_properties(info.as_ptr(), || {
                if camel_message_info_get_flags(info.as_ptr()) & CAMEL_MESSAGE_DELETED != 0 {
                    return None;
                }
                Some(MessageSyncState {
                    conversation_id: ConversationId(compose_conversation_id(&folder_id.0, &uid)),
                    message_id_hash: camel_message_info_get_message_id(info.as_ptr()),
                    delivery_state: cstr_to_string(camel_message_info_get_user_tag(
                        info.as_ptr(),
                        delivery_state_tag.as_ptr(),
                    )),
                    draft_synced: camel_message_info_get_user_flag(
                        info.as_ptr(),
                        draft_synced_flag.as_ptr(),
                    ) != 0,
                    draft_superseded: camel_message_info_get_user_flag(
                        info.as_ptr(),
                        draft_superseded_flag.as_ptr(),
                    ) != 0,
                })
            });
            states.extend(state);
        }
        Ok(states)
    }
}

fn search_expression(query: &str, include_body: bool) -> Result<CString> {
    let mut escaped = String::with_capacity(query.len());
    for character in query.trim().chars() {
        if matches!(character, '\\' | '"') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    let body_clause = if include_body {
        format!(" (body-contains \"{escaped}\")")
    } else {
        String::new()
    };
    CString::new(format!(
        "(match-all (or (header-contains \"subject\" \"{escaped}\") \
             (header-contains \"from\" \"{escaped}\") \
             (header-contains \"to\" \"{escaped}\") \
             (header-contains \"cc\" \"{escaped}\"){body_clause}))"
    ))
    .map_err(|_| anyhow!("search query contains an interior NUL"))
}

unsafe fn search_folder_conversations(
    store: *mut CamelStore,
    folder_id: &FolderId,
    expression: &CStr,
) -> Result<Vec<ConversationSummary>> {
    let folder = unsafe { get_folder(store, &folder_id.0) }?;
    unsafe {
        let mut uid_pointer = ptr::null_mut();
        let mut error = ptr::null_mut();
        let success = camel_folder_search_sync(
            folder.as_ptr(),
            expression.as_ptr(),
            &mut uid_pointer,
            ptr::null_mut(),
            &mut error,
        );
        let uids = OwnedPtrArray::from_ptr(uid_pointer);
        if success == 0 {
            return Err(take_gerror(error)
                .unwrap_or_else(|| anyhow!("camel_folder_search_sync returned FALSE")));
        }
        let Some(uids) = uids else {
            return Ok(Vec::new());
        };
        collect_conversations(folder.as_ptr(), &folder_id.0, uids.values()?)
    }
}

unsafe fn set_message_flag(
    store: *mut CamelStore,
    folder_name: &str,
    uid: &str,
    flag: c_uint,
    enabled: bool,
) -> Result<()> {
    let folder = unsafe { get_folder(store, folder_name) }?;
    let uid = CString::new(uid).map_err(|_| anyhow!("message uid contains interior NUL"))?;
    let (changed, actual) = unsafe {
        let changed = camel_folder_set_message_flags(
            folder.as_ptr(),
            uid.as_ptr(),
            flag,
            if enabled { flag } else { 0 },
        );
        let actual = camel_folder_get_message_flags(folder.as_ptr(), uid.as_ptr());
        (changed, actual)
    };
    if (actual & flag != 0) != enabled {
        return Err(anyhow!(
            "Camel did not persist message flag {flag:#x} as enabled={enabled}"
        ));
    }
    unsafe { save_folder_summary(folder.as_ptr()) }?;
    tracing::debug!(
        target: "pigeon::eds",
        flag,
        enabled,
        changed = changed != 0,
        provider_dirty = actual & CAMEL_MESSAGE_FOLDER_FLAGGED != 0,
        "cached message flag"
    );
    development_probe_log!(
        "cached flag detail: folder={} uid={} flag={:#x} enabled={} changed={} provider_dirty={}",
        folder_name,
        uid.to_string_lossy(),
        flag,
        enabled,
        changed != 0,
        actual & CAMEL_MESSAGE_FOLDER_FLAGGED != 0
    );
    Ok(())
}

unsafe fn set_message_user_flag(
    store: *mut CamelStore,
    folder_name: &str,
    uid: &str,
    flag: &str,
    enabled: bool,
) -> Result<()> {
    let folder = unsafe { get_folder(store, folder_name) }?;
    unsafe {
        let uid = CString::new(uid).map_err(|_| anyhow!("message uid contains interior NUL"))?;
        let flag = CString::new(flag).map_err(|_| anyhow!("user flag contains interior NUL"))?;
        let info = OwnedGObject::from_ptr(camel_folder_get_message_info(
            folder.as_ptr(),
            uid.as_ptr(),
        ))
        .ok_or_else(|| anyhow!("Camel message info is missing while setting a user flag"))?;
        camel_message_info_set_user_flag(
            info.as_ptr(),
            flag.as_ptr(),
            if enabled { 1 } else { 0 },
        );
        save_folder_summary(folder.as_ptr())
    }
}

unsafe fn set_message_user_tag(
    store: *mut CamelStore,
    folder_name: &str,
    uid: &str,
    name: &str,
    value: &str,
) -> Result<()> {
    let folder = unsafe { get_folder(store, folder_name) }?;
    unsafe {
        let uid = CString::new(uid).map_err(|_| anyhow!("message uid contains interior NUL"))?;
        let name_c =
            CString::new(name).map_err(|_| anyhow!("user tag name contains interior NUL"))?;
        let value_c =
            CString::new(value).map_err(|_| anyhow!("user tag value contains interior NUL"))?;
        let info = OwnedGObject::from_ptr(camel_folder_get_message_info(
            folder.as_ptr(),
            uid.as_ptr(),
        ))
        .ok_or_else(|| anyhow!("Camel message info is missing while changing delivery state"))?;
        camel_message_info_set_user_tag(info.as_ptr(), name_c.as_ptr(), value_c.as_ptr());
        let stored = OwnedGlibString::from_ptr(camel_message_info_dup_user_tag(
            info.as_ptr(),
            name_c.as_ptr(),
        ));
        if stored.map(|tag| tag.to_string_lossy()).as_deref() != Some(value) {
            return Err(anyhow!("Camel did not persist the message delivery state"));
        }
        save_folder_summary(folder.as_ptr())
    }
}

unsafe fn save_folder_summary(folder: *mut CamelFolder) -> Result<()> {
    let summary = unsafe { camel_folder_get_folder_summary(folder) };
    if summary.is_null() {
        return Err(anyhow!("Camel folder does not expose a summary to save"));
    }
    let mut error = ptr::null_mut();
    if unsafe { camel_folder_summary_save(summary, &mut error) } == 0 {
        return Err(take_gerror(error)
            .unwrap_or_else(|| anyhow!("camel_folder_summary_save returned FALSE")));
    }
    Ok(())
}

unsafe fn set_message_flag_for_sync(
    store: *mut CamelStore,
    folder_name: &str,
    uid: &str,
    flag: c_uint,
    enabled: bool,
) -> Result<()> {
    let folder = unsafe { get_folder(store, folder_name) }?;
    let uid = CString::new(uid).map_err(|_| anyhow!("message uid contains interior NUL"))?;
    let changed = unsafe {
        camel_folder_set_message_flags(
            folder.as_ptr(),
            uid.as_ptr(),
            flag,
            if enabled { flag } else { 0 },
        )
    };
    let info = unsafe {
        OwnedGObject::from_ptr(camel_folder_get_message_info(
            folder.as_ptr(),
            uid.as_ptr(),
        ))
    }
    .ok_or_else(|| anyhow!("Camel message info is missing after setting flags"))?;
    unsafe { camel_message_info_set_folder_flagged(info.as_ptr(), 1) };
    let actual = unsafe { camel_folder_get_message_flags(folder.as_ptr(), uid.as_ptr()) };
    if (actual & flag != 0) != enabled || actual & CAMEL_MESSAGE_FOLDER_FLAGGED == 0 {
        return Err(anyhow!(
            "Camel did not mark the message flag for provider synchronization"
        ));
    }
    tracing::debug!(
        target: "pigeon::eds",
        flag,
        enabled,
        changed = changed != 0,
        provider_dirty = true,
        "replayed message flag"
    );
    development_probe_log!(
        "replayed flag detail: folder={} uid={} flag={:#x} enabled={} changed={}",
        folder_name,
        uid.to_string_lossy(),
        flag,
        enabled,
        changed != 0
    );
    Ok(())
}

unsafe fn move_message(
    store: *mut CamelStore,
    source_folder_name: &str,
    uid: &str,
    destination_folder_name: &str,
) -> Result<()> {
    let source = unsafe { get_folder(store, source_folder_name) }?;
    let destination = unsafe { get_folder(store, destination_folder_name) }?;

    let uid = CString::new(uid).map_err(|_| anyhow!("message uid contains interior NUL"))?;
    let uids = unsafe { OwnedPtrArray::from_ptr(glib::ffi::g_ptr_array_new()) }
        .ok_or_else(|| anyhow!("g_ptr_array_new returned NULL"))?;
    unsafe {
        glib::ffi::g_ptr_array_add(uids.as_ptr(), uid.as_ptr() as glib::ffi::gpointer)
    };
    let mut error = ptr::null_mut();
    let success = unsafe {
        camel_folder_transfer_messages_to_sync(
            source.as_ptr(),
            uids.as_ptr(),
            destination.as_ptr(),
            1,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut error,
        )
    };
    if success == 0 {
        return Err(take_gerror(error)
            .unwrap_or_else(|| anyhow!("camel_folder_transfer_messages_to_sync returned FALSE")));
    }
    Ok(())
}

unsafe fn synchronize_store(store: *mut CamelStore) -> Result<()> {
    let mut error = ptr::null_mut();
    if unsafe { camel_store_synchronize_sync(store, 0, ptr::null_mut(), &mut error) } == 0 {
        return Err(take_gerror(error)
            .unwrap_or_else(|| anyhow!("camel_store_synchronize_sync returned FALSE")));
    }
    Ok(())
}

unsafe fn expunge_local_message(
    store: *mut CamelStore,
    folder_name: &str,
    uid: &str,
) -> Result<()> {
    let folder = unsafe { get_folder(store, folder_name) }?;
    unsafe {
        let uid = CString::new(uid).map_err(|_| anyhow!("message uid contains interior NUL"))?;
        camel_folder_set_message_flags(
            folder.as_ptr(),
            uid.as_ptr(),
            CAMEL_MESSAGE_DELETED,
            CAMEL_MESSAGE_DELETED,
        );
        let flags = camel_folder_get_message_flags(folder.as_ptr(), uid.as_ptr());
        if flags & CAMEL_MESSAGE_DELETED == 0 {
            return Err(anyhow!(
                "Camel did not mark the message deleted"
            ));
        }
        let mut error = ptr::null_mut();
        if camel_folder_expunge_sync(folder.as_ptr(), ptr::null_mut(), &mut error) == 0 {
            return Err(take_gerror(error)
                .unwrap_or_else(|| anyhow!("camel_folder_expunge_sync returned FALSE")));
        }
        Ok(())
    }
}

unsafe fn refresh_folder_info(store: *mut CamelStore, folder_name: &str) -> Result<()> {
    let folder = unsafe { get_folder(store, folder_name) }?;

    unsafe {
        let mut error = ptr::null_mut();
        let ok = camel_folder_refresh_info_sync(folder.as_ptr(), ptr::null_mut(), &mut error);
        if ok == 0 {
            return Err(take_gerror(error)
                .unwrap_or_else(|| anyhow!("camel_folder_refresh_info_sync failed")));
        }
        Ok(())
    }
}

unsafe fn load_folder_uids(
    folder: *mut CamelFolder,
    folder_name: &str,
) -> Result<OwnedPtrArray> {
    let summary = unsafe { camel_folder_get_folder_summary(folder) };
    if summary.is_null() {
        return Err(anyhow!(
            "Camel folder '{}' does not expose a folder summary",
            folder_name
        ));
    }

    let mut error = ptr::null_mut();
    let ok = unsafe { camel_folder_summary_prepare_fetch_all(summary, &mut error) };
    if ok == 0 {
        return Err(take_gerror(error)
            .unwrap_or_else(|| anyhow!("camel_folder_summary_prepare_fetch_all failed")));
    }

    unsafe { OwnedPtrArray::from_ptr(camel_folder_dup_uids(folder)) }
        .ok_or_else(|| anyhow!("camel_folder_dup_uids returned NULL for '{folder_name}'"))
}

unsafe fn ensure_store_online(store: *mut CamelStore) -> Result<()> {
    let is_offline_store = unsafe {
        glib::gobject_ffi::g_type_check_instance_is_a(
            store as *mut _,
            camel_offline_store_get_type(),
        ) != 0
    };
    if !is_offline_store {
        return Ok(());
    }

    let mut error = ptr::null_mut();
    let ok = unsafe {
        camel_offline_store_set_online_sync(
            store as *mut CamelOfflineStore,
            1,
            ptr::null_mut(),
            &mut error,
        )
    };
    if ok == 0 {
        return Err(take_gerror(error)
            .unwrap_or_else(|| anyhow!("camel_offline_store_set_online_sync failed")));
    }
    Ok(())
}

unsafe fn ensure_store_offline(store: *mut CamelStore) -> Result<()> {
    let is_offline_store = unsafe {
        glib::gobject_ffi::g_type_check_instance_is_a(
            store as *mut _,
            camel_offline_store_get_type(),
        ) != 0
    };
    if !is_offline_store {
        return Ok(());
    }

    let mut error = ptr::null_mut();
    let ok = unsafe {
        camel_offline_store_set_online_sync(
            store as *mut CamelOfflineStore,
            0,
            ptr::null_mut(),
            &mut error,
        )
    };
    if ok == 0 {
        return Err(take_gerror(error)
            .unwrap_or_else(|| anyhow!("camel_offline_store_set_online_sync(false) failed")));
    }
    Ok(())
}

unsafe fn get_folder(
    store: *mut CamelStore,
    folder_name: &str,
) -> Result<OwnedGObject<CamelFolder>> {
    unsafe { get_folder_with_flags(store, folder_name, 0) }
}

unsafe fn get_folder_with_flags(
    store: *mut CamelStore,
    folder_name: &str,
    flags: c_uint,
) -> Result<OwnedGObject<CamelFolder>> {
    let folder_name_c =
        CString::new(folder_name).map_err(|_| anyhow!("folder name contains interior NUL"))?;
    let mut error = ptr::null_mut();
    let folder = unsafe {
        camel_store_get_folder_sync(
            store,
            folder_name_c.as_ptr(),
            flags,
            ptr::null_mut(),
            &mut error,
        )
    };
    unsafe { OwnedGObject::from_ptr(folder) }.ok_or_else(|| {
        take_gerror(error)
            .unwrap_or_else(|| anyhow!("camel_store_get_folder_sync returned NULL"))
            .context(format!("failed to open Camel folder '{folder_name}'"))
    })
}

unsafe fn collect_conversations(
    folder: *mut CamelFolder,
    folder_name: &str,
    uid_values: &[glib::ffi::gpointer],
) -> Result<Vec<ConversationSummary>> {
    let mut conversations = Vec::with_capacity(uid_values.len());

    for &uid_ptr in uid_values {
        let uid = cstr_to_string(uid_ptr as *const c_char).ok_or_else(|| {
            anyhow!("Camel returned an invalid message UID in folder '{folder_name}'")
        })?;
        let message_info = unsafe { get_message_info(folder, &uid) }?;
        conversations.extend(unsafe {
            conversation_from_message_info(message_info.as_ptr(), folder_name, &uid)
        });
    }

    sort_conversations(&mut conversations);
    Ok(conversations)
}

unsafe fn get_message_info(
    folder: *mut CamelFolder,
    uid: &str,
) -> Result<OwnedGObject<CamelMessageInfo>> {
    let uid = CString::new(uid).map_err(|_| anyhow!("message uid contains interior NUL"))?;
    let info = unsafe { camel_folder_get_message_info(folder, uid.as_ptr()) };
    unsafe { OwnedGObject::from_ptr(info) }
        .ok_or_else(|| anyhow!("camel_folder_get_message_info returned NULL"))
}

// The property getters return borrowed values. Copy them before releasing Camel's lock.
unsafe fn with_message_info_properties<T>(
    info: *const CamelMessageInfo,
    read: impl FnOnce() -> T,
) -> T {
    struct PropertyGuard(*const CamelMessageInfo);

    impl Drop for PropertyGuard {
        fn drop(&mut self) {
            unsafe { camel_message_info_property_unlock(self.0) };
        }
    }

    unsafe { camel_message_info_property_lock(info) };
    let _guard = PropertyGuard(info);
    read()
}

unsafe fn conversation_from_message_info(
    info: *mut CamelMessageInfo,
    folder_name: &str,
    uid: &str,
) -> Option<ConversationSummary> {
    let (flags, subject, from, preview, sent, received) = unsafe {
        with_message_info_properties(info, || {
            let flags = camel_message_info_get_flags(info);
            if flags & CAMEL_MESSAGE_DELETED != 0 {
                return None;
            }
            Some((
                flags,
                cstr_to_string(camel_message_info_get_subject(info)),
                cstr_to_string(camel_message_info_get_from(info)),
                cstr_to_string(camel_message_info_get_preview(info)),
                camel_message_info_get_date_sent(info),
                camel_message_info_get_date_received(info),
            ))
        })
    }?;
    let subject = subject
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_default();
    let from = from
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_default();
    let participants = if from.is_empty() {
        Vec::new()
    } else {
        vec![compact_participant(&from)]
    };
    let preview = preview.unwrap_or_default().trim().to_string();
    let timestamp_secs = message_timestamp(sent, received);

    Some(ConversationSummary {
        id: ConversationId(compose_conversation_id(folder_name, uid)),
        folder_id: FolderId(folder_name.into()),
        subject,
        participants,
        message_count: 1,
        unread_count: if flags & CAMEL_MESSAGE_SEEN == 0 {
            1
        } else {
            0
        },
        attachment_count: if flags & CAMEL_MESSAGE_ATTACHMENTS != 0 {
            1
        } else {
            0
        },
        starred: flags & CAMEL_MESSAGE_FLAGGED != 0,
        last_updated_unix_ms: timestamp_secs.saturating_mul(1000),
        preview,
    })
}

fn compose_conversation_id(folder_name: &str, uid: &str) -> String {
    format!("{folder_name}{CONVERSATION_ID_SEPARATOR}{uid}")
}

fn message_timestamp(sent: i64, received: i64) -> i64 {
    if sent > 0 { sent } else { received.max(0) }
}

pub(crate) fn conversation_id_parts(value: &str) -> Option<(&str, &str)> {
    let (folder_name, uid) = value.split_once(CONVERSATION_ID_SEPARATOR)?;
    (!folder_name.is_empty() && !uid.is_empty()).then_some((folder_name, uid))
}

fn conversation_location(conversation_id: &ConversationId) -> Result<(&str, &str)> {
    conversation_id_parts(&conversation_id.0)
        .ok_or_else(|| anyhow!("conversation id does not contain a Camel folder and uid"))
}

fn compact_participant(value: &str) -> String {
    let trimmed = value.trim();
    if let Some((display, _)) = trimmed.split_once('<') {
        let display = display.trim().trim_matches('"').trim();
        if !display.is_empty() {
            return display.to_string();
        }
    }
    trimmed.to_string()
}

fn cstr_to_string(value: *const c_char) -> Option<String> {
    if value.is_null() {
        return None;
    }
    Some(
        unsafe { CStr::from_ptr(value) }
            .to_string_lossy()
            .into_owned(),
    )
}

#[cfg(debug_assertions)]
unsafe fn object_type_name(instance: *mut glib::gobject_ffi::GTypeInstance) -> Option<String> {
    if instance.is_null() {
        return None;
    }
    let class = unsafe { (*instance).g_class };
    if class.is_null() {
        return None;
    }
    let gtype = unsafe { (*class).g_type };
    cstr_to_string(unsafe { glib::gobject_ffi::g_type_name(gtype) })
}

fn local_folder_path_prefixes(folder_path: &str) -> Result<Vec<&str>> {
    if folder_path.is_empty()
        || folder_path.starts_with('/')
        || folder_path.ends_with('/')
        || folder_path
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(anyhow!("invalid local Camel folder path: '{folder_path}'"));
    }

    Ok(folder_path
        .match_indices('/')
        .map(|(index, _)| &folder_path[..index])
        .chain(std::iter::once(folder_path))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{
        CAMEL_MESSAGE_DELETED, CAMEL_MESSAGE_FLAGGED, CamelFolderSummary, CamelMessageInfo,
        OwnedGObject, conversation_from_message_info, local_folder_path_prefixes, search_expression,
    };

    unsafe extern "C" {
        fn camel_message_info_new(summary: *mut CamelFolderSummary) -> *mut CamelMessageInfo;
        fn camel_message_info_set_flags(
            info: *mut CamelMessageInfo,
            mask: u32,
            set: u32,
        ) -> glib::ffi::gboolean;
    }

    #[test]
    fn deleted_camel_records_are_excluded_from_the_visible_projection() {
        let info = unsafe {
            OwnedGObject::from_ptr(camel_message_info_new(std::ptr::null_mut())).unwrap()
        };
        let read = || unsafe { conversation_from_message_info(info.as_ptr(), "Drafts", "1") };
        unsafe {
            camel_message_info_set_flags(
                info.as_ptr(),
                CAMEL_MESSAGE_FLAGGED,
                CAMEL_MESSAGE_FLAGGED,
            );
        }
        assert!(read().unwrap().starred);
        unsafe {
            camel_message_info_set_flags(
                info.as_ptr(),
                CAMEL_MESSAGE_DELETED,
                CAMEL_MESSAGE_DELETED,
            );
        }
        assert!(read().is_none());
    }

    #[test]
    fn nested_local_folder_creation_includes_each_parent() {
        assert_eq!(
            local_folder_path_prefixes("collection/Drafts/Autosaved").unwrap(),
            vec![
                "collection",
                "collection/Drafts",
                "collection/Drafts/Autosaved"
            ]
        );
    }

    #[test]
    fn single_local_folder_creation_has_no_synthetic_parent() {
        assert_eq!(
            local_folder_path_prefixes("Drafts").unwrap(),
            vec!["Drafts"]
        );
    }

    #[test]
    fn malformed_local_folder_paths_are_rejected() {
        for path in [
            "",
            "/Drafts",
            "Drafts/",
            "account//Drafts",
            "account/./Drafts",
            "account/../Drafts",
        ] {
            assert!(
                local_folder_path_prefixes(path).is_err(),
                "accepted {path:?}"
            );
        }
    }

    #[test]
    fn search_expression_covers_headers_and_cached_body() {
        let expression = search_expression("project", true).unwrap();
        let expression = expression.to_str().unwrap();
        assert!(expression.starts_with("(match-all (or "));
        assert!(expression.ends_with("))"));
        for field in ["subject", "from", "to", "cc"] {
            assert!(expression.contains(&format!("(header-contains \"{field}\" \"project\")")));
        }
        assert!(expression.contains("(body-contains \"project\")"));
    }

    #[test]
    fn remote_search_expression_never_invokes_provider_body_search() {
        let expression = search_expression("project", false).unwrap();
        assert!(!expression.to_str().unwrap().contains("body-contains"));
    }

    #[test]
    fn search_expression_escapes_sexp_string_metacharacters() {
        let expression = search_expression("a\\b\"c'd", true).unwrap();
        assert!(expression.to_str().unwrap().contains("\"a\\\\b\\\"c'd\""));
    }

    #[test]
    fn search_expression_rejects_interior_nul() {
        assert!(search_expression("before\0after", true).is_err());
    }
}
