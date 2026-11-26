use std::collections::{BTreeSet, HashSet};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::os::raw::c_void;
use std::ptr;
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Result, anyhow};

use super::{OwnedGObject, OwnedGlibString, take_gerror};

mod topology;

use topology::build_snapshot;

#[derive(Debug, Clone, Default)]
pub struct Source {
    pub uid: String,
    pub parent: Option<String>,
    pub enabled: bool,
    pub identity_name: Option<String>,
    pub identity_address: Option<String>,
    pub identity_reply_to: Option<String>,
    pub identity_aliases: Option<String>,
    pub identity_signature: Option<SignatureMetadata>,
    pub identity_extension: IdentityExtensionState,
    pub backend_name: Option<String>,
    pub auth_method: Option<String>,
    pub mailbox_configuration_hash: u64,
    pub configuration_hash: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum IdentityExtensionState {
    #[default]
    Absent,
    Valid(LoadedIdentityExtension),
    Invalid,
}

impl IdentityExtensionState {
    pub(crate) fn as_valid(&self) -> Option<&IdentityExtension> {
        match self {
            Self::Valid(loaded) => {
                Some(&loaded.extension)
            }
            Self::Absent | Self::Invalid => {
                None
            }
        }
    }

    pub(crate) fn needs_rewrite(&self) -> bool {
        matches!(self, Self::Valid(loaded) if loaded.needs_rewrite)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedIdentityExtension {
    pub extension: IdentityExtension,
    pub needs_rewrite: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityExtension {
    pub account_label: String,
    pub default_address: String,
    pub identities: Vec<IdentityMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityMetadata {
    pub address: String,
    pub reply_to: String,
    pub signatures: SignatureSources,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureSources {
    pub html: SignatureMetadata,
    pub text: SignatureMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureMetadata {
    pub uid: String,
    pub display_name: String,
    pub contents: String,
    pub mime_type: String,
}

#[derive(Debug, Clone)]
pub struct MailTriplet {
    pub account: Source,
    pub identity: Option<Source>,
    pub transport: Option<Source>,
    pub goa_account_id: Option<String>,
    pub goa_name: Option<String>,
    pub mail_enabled: Option<bool>,
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub triplets: Vec<MailTriplet>,
}

/// One current EDS view used for a complete read/modify/write identity operation.
pub(crate) struct IdentityRegistry {
    registry: OwnedGObject<ESourceRegistry>,
    snapshot: Snapshot,
}

const EXTENSION_GOA: &CStr = c"GNOME Online Accounts";
const EXTENSION_COLLECTION: &CStr = c"Collection";
const EXTENSION_MAIL_ACCOUNT: &CStr = c"Mail Account";
const EXTENSION_MAIL_IDENTITY: &CStr = c"Mail Identity";
const EXTENSION_MAIL_SIGNATURE: &CStr = c"Mail Signature";
const EXTENSION_MAIL_SUBMISSION: &CStr = c"Mail Submission";
const EXTENSION_MAIL_COMPOSITION: &CStr = c"Mail Composition";
const EXTENSION_MAIL_TRANSPORT: &CStr = c"Mail Transport";
const EXTENSION_AUTHENTICATION: &CStr = c"Authentication";

enum ESourceRegistry {}
enum ESource {}
enum ESourceExtension {}
enum ESourceGoa {}
enum ESourceMailAccount {}
enum ESourceMailSubmission {}
enum ESourceMailComposition {}
enum ESourceMailIdentity {}
enum ESourceMailSignature {}
enum ESourceCollection {}
enum ESourceAuthentication {}
enum ESourceBackend {}
enum RegistryWatch {}

type RegistryChangeCallback = Arc<dyn Fn() + Send + Sync>;

pub(crate) struct Monitor {
    stop: mpsc::Sender<()>,
    worker: Option<JoinHandle<()>>,
}

struct RawMonitor {
    watch: *mut RegistryWatch,
}

#[derive(Debug, Clone)]
struct SourceInfo {
    uid: String,
    parent: Option<String>,
    enabled: bool,
    direct_goa_account_id: Option<String>,
    direct_goa_name: Option<String>,
    direct_collection_mail_enabled: Option<bool>,
    direct_mail_account_backend_name: Option<String>,
    direct_mail_transport_backend_name: Option<String>,
    direct_identity_uid: Option<String>,
    direct_transport_uid: Option<String>,
    direct_name: Option<String>,
    direct_address: Option<String>,
    direct_reply_to: Option<String>,
    direct_aliases: Option<String>,
    direct_signature: Option<SignatureMetadata>,
    direct_identity_extension: IdentityExtensionState,
    direct_auth_method: Option<String>,
    mailbox_configuration_hash: u64,
    serialized_configuration: String,
    is_account: bool,
    is_identity: bool,
    is_transport: bool,
}

#[allow(clashing_extern_declarations)]
#[link(name = "edataserver-1.2")]
unsafe extern "C" {
    fn e_util_generate_uid() -> *mut c_char;
    fn e_source_registry_new_sync(
        cancellable: *mut gio::ffi::GCancellable,
        error: *mut *mut glib::ffi::GError,
    ) -> *mut ESourceRegistry;
    fn e_source_registry_ref_source(
        registry: *mut ESourceRegistry,
        uid: *const c_char,
    ) -> *mut ESource;
    fn e_source_registry_check_enabled(
        registry: *mut ESourceRegistry,
        source: *mut ESource,
    ) -> glib::ffi::gboolean;
    fn e_source_registry_list_sources(
        registry: *mut ESourceRegistry,
        extension_name: *const c_char,
    ) -> *mut glib::ffi::GList;
    fn e_source_registry_commit_source_sync(
        registry: *mut ESourceRegistry,
        source: *mut ESource,
        cancellable: *mut gio::ffi::GCancellable,
        error: *mut *mut glib::ffi::GError,
    ) -> glib::ffi::gboolean;
    fn e_source_new_with_uid(
        uid: *const c_char,
        main_context: *mut c_void,
        error: *mut *mut glib::ffi::GError,
    ) -> *mut ESource;
    fn e_source_set_display_name(source: *mut ESource, display_name: *const c_char);
    fn e_source_remove_sync(
        source: *mut ESource,
        cancellable: *mut gio::ffi::GCancellable,
        error: *mut *mut glib::ffi::GError,
    ) -> glib::ffi::gboolean;

    fn e_source_has_extension(
        source: *mut ESource,
        extension_name: *const c_char,
    ) -> glib::ffi::gboolean;
    fn e_source_get_extension(
        source: *mut ESource,
        extension_name: *const c_char,
    ) -> *mut ESourceExtension;

    fn e_source_get_uid(source: *mut ESource) -> *const c_char;
    fn e_source_dup_display_name(source: *mut ESource) -> *mut c_char;
    fn e_source_dup_parent(source: *mut ESource) -> *mut c_char;
    fn e_source_to_string(source: *mut ESource, length: *mut usize) -> *mut c_char;

    fn e_source_goa_dup_account_id(extension: *mut ESourceGoa) -> *mut c_char;
    fn e_source_goa_dup_name(extension: *mut ESourceGoa) -> *mut c_char;
    fn e_source_collection_get_mail_enabled(
        extension: *mut ESourceCollection,
    ) -> glib::ffi::gboolean;
    fn e_source_backend_dup_backend_name(extension: *mut ESourceBackend) -> *mut c_char;
    fn e_source_mail_account_dup_identity_uid(extension: *mut ESourceMailAccount) -> *mut c_char;
    fn e_source_mail_submission_dup_transport_uid(
        extension: *mut ESourceMailSubmission,
    ) -> *mut c_char;
    fn e_source_mail_submission_dup_sent_folder(
        extension: *mut ESourceMailSubmission,
    ) -> *mut c_char;
    fn e_source_mail_submission_set_sent_folder(
        extension: *mut ESourceMailSubmission,
        sent_folder: *const c_char,
    );
    fn e_source_mail_submission_get_use_sent_folder(
        extension: *mut ESourceMailSubmission,
    ) -> glib::ffi::gboolean;
    fn e_source_mail_submission_set_use_sent_folder(
        extension: *mut ESourceMailSubmission,
        use_sent_folder: glib::ffi::gboolean,
    );
    fn e_source_mail_composition_dup_drafts_folder(
        extension: *mut ESourceMailComposition,
    ) -> *mut c_char;
    fn e_source_mail_composition_set_drafts_folder(
        extension: *mut ESourceMailComposition,
        drafts_folder: *const c_char,
    );
    fn e_source_mail_identity_dup_name(extension: *mut ESourceMailIdentity) -> *mut c_char;
    fn e_source_mail_identity_dup_address(extension: *mut ESourceMailIdentity) -> *mut c_char;
    fn e_source_mail_identity_dup_reply_to(extension: *mut ESourceMailIdentity) -> *mut c_char;
    fn e_source_mail_identity_dup_aliases(extension: *mut ESourceMailIdentity) -> *mut c_char;
    fn e_source_mail_identity_dup_signature_uid(
        extension: *mut ESourceMailIdentity,
    ) -> *mut c_char;
    fn e_source_mail_identity_set_name(extension: *mut ESourceMailIdentity, name: *const c_char);
    fn e_source_mail_identity_set_reply_to(
        extension: *mut ESourceMailIdentity,
        reply_to: *const c_char,
    );
    fn e_source_mail_identity_set_aliases(
        extension: *mut ESourceMailIdentity,
        aliases: *const c_char,
    );
    fn e_source_mail_identity_set_signature_uid(
        extension: *mut ESourceMailIdentity,
        signature_uid: *const c_char,
    );
    fn e_source_mail_signature_set_mime_type(
        extension: *mut ESourceMailSignature,
        mime_type: *const c_char,
    );
    fn e_source_mail_signature_dup_mime_type(
        extension: *mut ESourceMailSignature,
    ) -> *mut c_char;
    fn e_source_mail_signature_load_sync(
        source: *mut ESource,
        contents: *mut *mut c_char,
        length: *mut usize,
        cancellable: *mut gio::ffi::GCancellable,
        error: *mut *mut glib::ffi::GError,
    ) -> glib::ffi::gboolean;
    fn e_source_mail_signature_replace_sync(
        source: *mut ESource,
        contents: *const c_char,
        length: usize,
        cancellable: *mut gio::ffi::GCancellable,
        error: *mut *mut glib::ffi::GError,
    ) -> glib::ffi::gboolean;
    fn e_source_authentication_dup_method(extension: *mut ESourceAuthentication) -> *mut c_char;

    fn mail_bridge_eds_registry_watch_new(
        callback: Option<unsafe extern "C" fn(*mut c_void)>,
        user_data: *mut c_void,
        destroy: Option<unsafe extern "C" fn(*mut c_void)>,
        error: *mut *mut glib::ffi::GError,
    ) -> *mut RegistryWatch;
    fn mail_bridge_eds_registry_watch_iteration(watch: *mut RegistryWatch);
    fn mail_bridge_eds_registry_watch_free(watch: *mut RegistryWatch);
    fn mail_bridge_eds_register_source_types();
    fn mail_bridge_eds_source_has_identity_extension(source: *mut ESource) -> glib::ffi::gboolean;
    fn mail_bridge_eds_source_dup_identity_account_label(source: *mut ESource) -> *mut c_char;
    fn mail_bridge_eds_source_dup_identity_default_address(source: *mut ESource) -> *mut c_char;
    fn mail_bridge_eds_source_dup_identity_records(source: *mut ESource) -> *mut *mut c_char;
    fn mail_bridge_eds_source_set_identity_records(
        source: *mut ESource,
        account_label: *const c_char,
        default_address: *const c_char,
        identity_records: *const *const c_char,
    );
}

pub(crate) fn generate_uid() -> String {
    take_owned_c_string(unsafe { e_util_generate_uid() })
        .expect("EDS must return a generated UID")
}

impl Monitor {
    pub(crate) fn start(
        callback: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self> {
        let callback: RegistryChangeCallback = Arc::new(callback);
        let (stop, stop_receiver) = mpsc::channel();
        let (started, started_receiver) = mpsc::sync_channel(0);
        let worker = std::thread::spawn(move || {
            let monitor = match RawMonitor::open(callback) {
                Ok(monitor) => {
                    monitor
                }
                Err(error) => {
                    let _ = started.send(Err(error));
                    return;
                }
            };
            if started.send(Ok(())).is_err() {
                return;
            }
            loop {
                monitor.iterate();
                match stop_receiver.recv_timeout(Duration::from_millis(50)) {
                    Err(RecvTimeoutError::Timeout) => {}
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                        return;
                    }
                }
            }
        });
        match started_receiver.recv() {
            Ok(Ok(())) => {
                Ok(Self {
                    stop,
                    worker: Some(worker),
                })
            }
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(error)
            }
            Err(error) => {
                let _ = worker.join();
                Err(anyhow!("EDS registry monitor worker failed to start: {error}"))
            }
        }
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl RawMonitor {
    fn open(callback: RegistryChangeCallback) -> Result<Self> {
        unsafe { mail_bridge_eds_register_source_types() };
        let user_data = Box::into_raw(Box::new(callback)) as *mut c_void;
        let mut error = ptr::null_mut();
        let watch = unsafe {
            mail_bridge_eds_registry_watch_new(
                Some(invoke_registry_change_callback),
                user_data,
                Some(drop_registry_change_callback),
                &mut error,
            )
        };
        if watch.is_null() {
            unsafe { drop_registry_change_callback(user_data) };
            return Err(take_gerror(error)
                .unwrap_or_else(|| anyhow!("failed to watch ESourceRegistry changes")));
        }
        Ok(Self { watch })
    }

    fn iterate(&self) {
        unsafe { mail_bridge_eds_registry_watch_iteration(self.watch) };
    }
}

unsafe extern "C" fn invoke_registry_change_callback(
    user_data: *mut c_void,
) {
    let callback = unsafe { &*(user_data as *const RegistryChangeCallback) };
    callback();
}

unsafe extern "C" fn drop_registry_change_callback(user_data: *mut c_void) {
    drop(unsafe { Box::from_raw(user_data as *mut RegistryChangeCallback) });
}

impl Drop for RawMonitor {
    fn drop(&mut self) {
        let watch = self.watch;
        self.watch = ptr::null_mut();
        unsafe { mail_bridge_eds_registry_watch_free(watch) };
    }
}

pub(crate) fn load_snapshot_via_ffi() -> Result<Snapshot> {
    unsafe {
        let registry = owned_registry()?;
        load_snapshot_from_registry(registry.as_ptr())
    }
}

impl IdentityRegistry {
    pub(crate) fn load() -> Result<Self> {
        unsafe {
            let registry = owned_registry()?;
            let snapshot = load_snapshot_from_registry(registry.as_ptr())?;
            Ok(Self { registry, snapshot })
        }
    }

    pub(crate) fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    pub(crate) fn write_identity(
        &self,
        identity_uid: &str,
        name: Option<&str>,
        reply_to: Option<&str>,
        aliases: Option<&str>,
        account_label: &str,
        default_address: &str,
        primary_signature_uid: &str,
        identities: &[IdentityMetadata],
    ) -> Result<()> {
        let previous_signature_uids = self
            .snapshot
            .triplets
            .iter()
            .filter_map(|triplet| triplet.identity.as_ref())
            .find(|identity| identity.uid == identity_uid)
            .and_then(|identity| identity.identity_extension.as_valid())
            .into_iter()
            .flat_map(|extension| &extension.identities)
            .flat_map(|identity| {
                [
                    identity.signatures.html.uid.clone(),
                    identity.signatures.text.uid.clone(),
                ]
            })
            .filter(|uid| !uid.is_empty() && uid != "none")
            .collect();
        write_identity(
            self.registry.as_ptr(),
            identity_uid,
            name,
            reply_to,
            aliases,
            account_label,
            default_address,
            primary_signature_uid,
            identities,
            previous_signature_uids,
        )
    }
}

unsafe fn owned_registry() -> Result<OwnedGObject<ESourceRegistry>> {
    unsafe { mail_bridge_eds_register_source_types() };
    let mut error = ptr::null_mut();
    let registry = unsafe { e_source_registry_new_sync(ptr::null_mut(), &mut error) };
    unsafe { OwnedGObject::from_ptr(registry) }.ok_or_else(|| {
        take_gerror(error).unwrap_or_else(|| anyhow!("e_source_registry_new_sync returned NULL"))
    })
}

unsafe fn owned_source(
    registry: *mut ESourceRegistry,
    uid: *const c_char,
) -> Option<OwnedGObject<ESource>> {
    let source = unsafe { e_source_registry_ref_source(registry, uid) };
    unsafe { OwnedGObject::from_ptr(source) }
}

unsafe fn load_snapshot_from_registry(registry: *mut ESourceRegistry) -> Result<Snapshot> {
    let sources = unsafe { list_sources(registry) }?;
    let infos = sources
        .iter()
        .map(|source| unsafe { load_source_info(registry, source.as_ptr()) })
        .collect::<Result<Vec<_>>>()?;
    Ok(build_snapshot(&infos))
}

unsafe fn list_sources(registry: *mut ESourceRegistry) -> Result<Vec<OwnedGObject<ESource>>> {
    let list = unsafe { e_source_registry_list_sources(registry, ptr::null()) };
    if list.is_null() {
        return Ok(Vec::new());
    }

    let mut source_pointers = Vec::new();
    let mut cursor = list;
    while !cursor.is_null() {
        source_pointers.push(unsafe { (*cursor).data as *mut ESource });
        cursor = unsafe { (*cursor).next };
    }
    unsafe { glib::ffi::g_list_free(list) };

    if source_pointers.iter().any(|source| source.is_null()) {
        for source in source_pointers.into_iter().filter(|source| !source.is_null()) {
            unsafe { glib::gobject_ffi::g_object_unref(source as *mut _) };
        }
        return Err(anyhow!("ESourceRegistry returned a NULL source entry"));
    }

    let sources = source_pointers
        .into_iter()
        .map(|source| {
            unsafe { OwnedGObject::from_ptr(source) }
                .expect("validated ESourceRegistry source pointer")
        })
        .collect();
    Ok(sources)
}

fn write_identity(
    registry: *mut ESourceRegistry,
    identity_uid: &str,
    name: Option<&str>,
    reply_to: Option<&str>,
    aliases: Option<&str>,
    account_label: &str,
    default_address: &str,
    primary_signature_uid: &str,
    identities: &[IdentityMetadata],
    previous_signature_uids: BTreeSet<String>,
) -> Result<()> {
    let uid = CString::new(identity_uid)
        .map_err(|_| anyhow!("identity uid contains interior NUL"))?;
    let name = CString::new(name.unwrap_or(""))
        .map_err(|_| anyhow!("identity name contains interior NUL"))?;
    let reply_to = CString::new(reply_to.unwrap_or(""))
        .map_err(|_| anyhow!("identity reply-to contains interior NUL"))?;
    let aliases = CString::new(aliases.unwrap_or(""))
        .map_err(|_| anyhow!("identity aliases contain interior NUL"))?;
    let account_label = CString::new(account_label)
        .map_err(|_| anyhow!("identity account label contains interior NUL"))?;
    let default_address = CString::new(default_address)
        .map_err(|_| anyhow!("default identity address contains interior NUL"))?;
    let primary_signature_uid = CString::new(primary_signature_uid)
        .map_err(|_| anyhow!("primary signature uid contains interior NUL"))?;
    let identity_records = identities
        .iter()
        .flat_map(|identity| {
            [
                identity.address.as_str(),
                identity.reply_to.as_str(),
                identity.signatures.html.uid.as_str(),
                identity.signatures.text.uid.as_str(),
            ]
        })
        .map(CString::new)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| anyhow!("identity metadata contains interior NUL"))?;
    let mut identity_record_pointers = identity_records
        .iter()
        .map(|value| value.as_ptr())
        .collect::<Vec<_>>();
    identity_record_pointers.push(ptr::null());
    unsafe {
        let mut error = ptr::null_mut();
        let source = owned_source(registry, uid.as_ptr()).ok_or_else(|| {
            anyhow!(
                "could not resolve EDS identity source for uid {}",
                identity_uid
            )
        })?;
        let extension = existing_extension(source.as_ptr(), EXTENSION_MAIL_IDENTITY)
            .ok_or_else(|| {
                anyhow!(
                    "EDS source {} has no Mail Identity extension",
                    identity_uid
                )
            })? as *mut ESourceMailIdentity;
        let mut staged_signatures: Vec<OwnedGObject<ESource>> = Vec::new();
        for signature in identities.iter().flat_map(|identity| {
            [&identity.signatures.html, &identity.signatures.text]
        }) {
            if signature.uid == "none" || previous_signature_uids.contains(&signature.uid) {
                continue;
            }
            let source = match write_signature_source(
                registry,
                &signature.uid,
                &signature.display_name,
                &signature.contents,
                &signature.mime_type,
            ) {
                Ok(source) => {
                    source
                }
                Err(error) => {
                    for source in &staged_signatures {
                        remove_source(source.as_ptr());
                    }
                    return Err(error);
                }
            };
            staged_signatures.push(source);
        }
        e_source_mail_identity_set_name(extension, name.as_ptr());
        e_source_mail_identity_set_reply_to(extension, reply_to.as_ptr());
        e_source_mail_identity_set_aliases(extension, aliases.as_ptr());
        e_source_mail_identity_set_signature_uid(extension, primary_signature_uid.as_ptr());
        mail_bridge_eds_source_set_identity_records(
            source.as_ptr(),
            account_label.as_ptr(),
            default_address.as_ptr(),
            identity_record_pointers.as_ptr(),
        );

        let success = e_source_registry_commit_source_sync(
            registry,
            source.as_ptr(),
            ptr::null_mut(),
            &mut error,
        ) != 0;

        if !success {
            for source in &staged_signatures {
                remove_source(source.as_ptr());
            }
            return Err(take_gerror(error)
                .unwrap_or_else(|| anyhow!("e_source_registry_commit_source_sync failed")));
        }
        let current_signature_uids = identities
            .iter()
            .flat_map(|identity| {
                [
                    identity.signatures.html.uid.clone(),
                    identity.signatures.text.uid.clone(),
                ]
            })
            .collect::<BTreeSet<_>>();
        for stale_uid in previous_signature_uids {
            if !current_signature_uids.contains(&stale_uid) {
                remove_signature_source(registry, &stale_uid);
            }
        }
        Ok(())
    }
}

fn write_signature_source(
    registry: *mut ESourceRegistry,
    uid: &str,
    display_name: &str,
    contents: &str,
    mime_type: &str,
) -> Result<OwnedGObject<ESource>> {
    let uid_c = CString::new(uid).map_err(|_| anyhow!("signature uid contains interior NUL"))?;
    let display_name_c = CString::new(display_name)
        .map_err(|_| anyhow!("signature display name contains interior NUL"))?;
    let mime_type_c = CString::new(mime_type)
        .map_err(|_| anyhow!("signature MIME type contains interior NUL"))?;
    unsafe {
        if owned_source(registry, uid_c.as_ptr()).is_some() {
            return Err(anyhow!("refusing to replace an existing EDS signature source"));
        }
        let mut error = ptr::null_mut();
        let source = OwnedGObject::from_ptr(e_source_new_with_uid(
            uid_c.as_ptr(),
            ptr::null_mut(),
            &mut error,
        ))
        .ok_or_else(|| {
            take_gerror(error).unwrap_or_else(|| anyhow!("could not create EDS signature source"))
        })?;
        let extension = create_extension(source.as_ptr(), EXTENSION_MAIL_SIGNATURE)
            .ok_or_else(|| anyhow!("EDS signature source has no Mail Signature extension"))?
            as *mut ESourceMailSignature;
        e_source_set_display_name(source.as_ptr(), display_name_c.as_ptr());
        e_source_mail_signature_set_mime_type(extension, mime_type_c.as_ptr());

        let mut error = ptr::null_mut();
        if e_source_registry_commit_source_sync(
            registry,
            source.as_ptr(),
            ptr::null_mut(),
            &mut error,
        ) == 0
        {
            let error = take_gerror(error)
                .unwrap_or_else(|| anyhow!("failed to commit EDS signature source"));
            remove_signature_source(registry, uid);
            return Err(error);
        }

        let mut error = ptr::null_mut();
        if e_source_mail_signature_replace_sync(
            source.as_ptr(),
            contents.as_ptr() as *const c_char,
            contents.len(),
            ptr::null_mut(),
            &mut error,
        ) == 0
        {
            let error = take_gerror(error)
                .unwrap_or_else(|| anyhow!("failed to write EDS signature contents"));
            remove_source(source.as_ptr());
            return Err(error);
        }
        Ok(source)
    }
}

fn remove_source(source: *mut ESource) {
    unsafe {
        let mut error = ptr::null_mut();
        if e_source_remove_sync(source, ptr::null_mut(), &mut error) == 0
            && let Some(error) = take_gerror(error)
        {
            crate::logging::report_failure("eds-signature-cleanup", &error);
        }
    }
}

fn remove_signature_source(registry: *mut ESourceRegistry, uid: &str) {
    let Ok(uid) = CString::new(uid) else {
        return;
    };
    unsafe {
        let Some(source) = owned_source(registry, uid.as_ptr()) else {
            return;
        };
        remove_source(source.as_ptr());
    }
}

pub(crate) fn ensure_local_mailbox_configuration(
    identity_uid: &str,
    drafts_uri: &str,
    sent_uri: &str,
) -> Result<()> {
    let identity_uid_c = CString::new(identity_uid)
        .map_err(|_| anyhow!("identity uid contains interior NUL"))?;
    let drafts_uri_c =
        CString::new(drafts_uri).map_err(|_| anyhow!("drafts uri contains interior NUL"))?;
    let sent_uri_c =
        CString::new(sent_uri).map_err(|_| anyhow!("sent uri contains interior NUL"))?;
    unsafe {
        let mut error = ptr::null_mut();
        let registry = owned_registry()?;
        let identity_source = owned_source(registry.as_ptr(), identity_uid_c.as_ptr())
            .ok_or_else(|| {
                anyhow!(
                    "could not resolve EDS identity source for uid {}",
                    identity_uid
                )
            })?;
        let mail_submission =
            existing_extension(identity_source.as_ptr(), EXTENSION_MAIL_SUBMISSION)
            .ok_or_else(|| anyhow!("identity source has no Mail Submission extension"))?
            as *mut ESourceMailSubmission;
        let mail_composition =
            existing_extension(identity_source.as_ptr(), EXTENSION_MAIL_COMPOSITION)
            .ok_or_else(|| anyhow!("identity source has no Mail Composition extension"))?
            as *mut ESourceMailComposition;
        let current_drafts = take_owned_c_string(e_source_mail_composition_dup_drafts_folder(
            mail_composition,
        ));
        let current_sent = take_owned_c_string(e_source_mail_submission_dup_sent_folder(
            mail_submission,
        ));
        if current_drafts.as_deref() == Some(drafts_uri)
            && current_sent.as_deref() == Some(sent_uri)
            && e_source_mail_submission_get_use_sent_folder(mail_submission) != 0
        {
            return Ok(());
        }
        e_source_mail_composition_set_drafts_folder(mail_composition, drafts_uri_c.as_ptr());
        e_source_mail_submission_set_sent_folder(mail_submission, sent_uri_c.as_ptr());
        e_source_mail_submission_set_use_sent_folder(mail_submission, 1);

        let success = e_source_registry_commit_source_sync(
            registry.as_ptr(),
            identity_source.as_ptr(),
            ptr::null_mut(),
            &mut error,
        ) != 0;
        if success {
            Ok(())
        } else {
            Err(take_gerror(error)
                .unwrap_or_else(|| anyhow!("failed to commit identity local mailbox folders")))
        }
    }
}

unsafe fn load_source_info(
    registry: *mut ESourceRegistry,
    source: *mut ESource,
) -> Result<SourceInfo> {
    let uid = unsafe { cstr_to_string(e_source_get_uid(source)) }
        .filter(|uid| !uid.is_empty())
        .ok_or_else(|| anyhow!("ESourceRegistry returned a source without a UID"))?;

    Ok(SourceInfo {
        uid,
        parent: unsafe { take_owned_c_string(e_source_dup_parent(source)) },
        enabled: unsafe { e_source_registry_check_enabled(registry, source) != 0 },
        direct_goa_account_id: unsafe { source_goa_account_id(source) },
        direct_goa_name: unsafe { source_goa_name(source) },
        direct_collection_mail_enabled: unsafe { source_collection_mail_enabled(source) },
        direct_mail_account_backend_name: unsafe { source_mail_account_backend_name(source) },
        direct_mail_transport_backend_name: unsafe { source_mail_transport_backend_name(source) },
        direct_identity_uid: unsafe { source_mail_account_identity_uid(source) },
        direct_transport_uid: unsafe { source_mail_submission_transport_uid(source) },
        direct_name: unsafe { source_mail_identity_name(source) },
        direct_address: unsafe { source_mail_identity_address(source) },
        direct_reply_to: unsafe { source_mail_identity_reply_to(source) },
        direct_aliases: unsafe { source_mail_identity_aliases(source) },
        direct_signature: unsafe { source_mail_identity_signature(registry, source) },
        direct_identity_extension: unsafe { source_identity_extension(registry, source) },
        direct_auth_method: unsafe { source_authentication_method(source) },
        mailbox_configuration_hash: unsafe { source_mailbox_configuration_hash(source) },
        serialized_configuration: unsafe { source_configuration(source) },
        is_account: unsafe { has_extension(source, EXTENSION_MAIL_ACCOUNT) },
        is_identity: unsafe { has_extension(source, EXTENSION_MAIL_IDENTITY) },
        is_transport: unsafe {
            has_extension(source, EXTENSION_MAIL_TRANSPORT)
                || has_extension(source, EXTENSION_MAIL_SUBMISSION)
        },
    })
}

unsafe fn has_extension(source: *mut ESource, extension_name: &CStr) -> bool {
    unsafe { e_source_has_extension(source, extension_name.as_ptr()) != 0 }
}

unsafe fn create_extension(
    source: *mut ESource,
    extension_name: &CStr,
) -> Option<*mut ESourceExtension> {
    let extension = unsafe { e_source_get_extension(source, extension_name.as_ptr()) };
    (!extension.is_null()).then_some(extension)
}

unsafe fn existing_extension(
    source: *mut ESource,
    extension_name: &CStr,
) -> Option<*mut ESourceExtension> {
    if !unsafe { has_extension(source, extension_name) } {
        return None;
    }
    let extension = unsafe { e_source_get_extension(source, extension_name.as_ptr()) };
    (!extension.is_null()).then_some(extension)
}

unsafe fn source_goa_account_id(source: *mut ESource) -> Option<String> {
    let extension = unsafe { existing_extension(source, EXTENSION_GOA)? };
    take_owned_c_string(unsafe { e_source_goa_dup_account_id(extension as *mut ESourceGoa) })
}

unsafe fn source_goa_name(source: *mut ESource) -> Option<String> {
    let extension = unsafe { existing_extension(source, EXTENSION_GOA)? };
    take_owned_c_string(unsafe { e_source_goa_dup_name(extension as *mut ESourceGoa) })
}

unsafe fn source_collection_mail_enabled(source: *mut ESource) -> Option<bool> {
    let extension = unsafe { existing_extension(source, EXTENSION_COLLECTION)? };
    Some(unsafe { e_source_collection_get_mail_enabled(extension as *mut ESourceCollection) != 0 })
}

unsafe fn source_mail_account_backend_name(source: *mut ESource) -> Option<String> {
    let extension = unsafe { existing_extension(source, EXTENSION_MAIL_ACCOUNT)? };
    take_owned_c_string(unsafe {
        e_source_backend_dup_backend_name(extension as *mut ESourceBackend)
    })
}

unsafe fn source_mail_transport_backend_name(source: *mut ESource) -> Option<String> {
    let extension = unsafe { existing_extension(source, EXTENSION_MAIL_TRANSPORT)? };
    take_owned_c_string(unsafe {
        e_source_backend_dup_backend_name(extension as *mut ESourceBackend)
    })
}

unsafe fn source_mail_account_identity_uid(source: *mut ESource) -> Option<String> {
    let extension = unsafe { existing_extension(source, EXTENSION_MAIL_ACCOUNT)? };
    take_owned_c_string(unsafe {
        e_source_mail_account_dup_identity_uid(extension as *mut ESourceMailAccount)
    })
}

unsafe fn source_mail_submission_transport_uid(source: *mut ESource) -> Option<String> {
    let extension = unsafe { existing_extension(source, EXTENSION_MAIL_SUBMISSION)? };
    take_owned_c_string(unsafe {
        e_source_mail_submission_dup_transport_uid(extension as *mut ESourceMailSubmission)
    })
}

unsafe fn source_mailbox_configuration_hash(source: *mut ESource) -> u64 {
    let submission = unsafe { existing_extension(source, EXTENSION_MAIL_SUBMISSION) }
        .map(|extension| extension as *mut ESourceMailSubmission);
    let sent = submission.and_then(|extension| {
        take_owned_c_string(unsafe {
            e_source_mail_submission_dup_sent_folder(extension)
        })
    });
    let use_sent_folder = submission.map(|extension| unsafe {
        e_source_mail_submission_get_use_sent_folder(extension) != 0
    });
    let drafts = unsafe { existing_extension(source, EXTENSION_MAIL_COMPOSITION) }.and_then(
        |extension| {
            take_owned_c_string(unsafe {
                e_source_mail_composition_dup_drafts_folder(
                    extension as *mut ESourceMailComposition,
                )
            })
        },
    );
    let mut hasher = DefaultHasher::new();
    sent.hash(&mut hasher);
    use_sent_folder.hash(&mut hasher);
    drafts.hash(&mut hasher);
    hasher.finish()
}

unsafe fn source_configuration(source: *mut ESource) -> String {
    let mut length = 0usize;
    let Some(serialized) = (unsafe {
        OwnedGlibString::from_ptr(e_source_to_string(source, &mut length))
    }) else {
        return String::new();
    };
    String::from_utf8_lossy(unsafe {
        std::slice::from_raw_parts(serialized.as_ptr() as *const u8, length)
    })
    .into_owned()
}

unsafe fn source_mail_identity_name(source: *mut ESource) -> Option<String> {
    let extension = unsafe { existing_extension(source, EXTENSION_MAIL_IDENTITY)? };
    take_owned_c_string(unsafe {
        e_source_mail_identity_dup_name(extension as *mut ESourceMailIdentity)
    })
}

unsafe fn source_mail_identity_address(source: *mut ESource) -> Option<String> {
    let extension = unsafe { existing_extension(source, EXTENSION_MAIL_IDENTITY)? };
    take_owned_c_string(unsafe {
        e_source_mail_identity_dup_address(extension as *mut ESourceMailIdentity)
    })
}

unsafe fn source_mail_identity_reply_to(source: *mut ESource) -> Option<String> {
    let extension = unsafe { existing_extension(source, EXTENSION_MAIL_IDENTITY)? };
    take_owned_c_string(unsafe {
        e_source_mail_identity_dup_reply_to(extension as *mut ESourceMailIdentity)
    })
}

unsafe fn source_mail_identity_aliases(source: *mut ESource) -> Option<String> {
    let extension = unsafe { existing_extension(source, EXTENSION_MAIL_IDENTITY)? };
    take_owned_c_string(unsafe {
        e_source_mail_identity_dup_aliases(extension as *mut ESourceMailIdentity)
    })
}

unsafe fn source_mail_identity_signature(
    registry: *mut ESourceRegistry,
    source: *mut ESource,
) -> Option<SignatureMetadata> {
    let extension = unsafe { existing_extension(source, EXTENSION_MAIL_IDENTITY)? };
    let uid = take_owned_c_string(unsafe {
        e_source_mail_identity_dup_signature_uid(extension as *mut ESourceMailIdentity)
    })?;
    if uid.is_empty() || uid == "none" {
        return None;
    }
    unsafe { load_signature_source(registry, uid) }
}

unsafe fn source_identity_extension(
    registry: *mut ESourceRegistry,
    source: *mut ESource,
) -> IdentityExtensionState {
    if unsafe { mail_bridge_eds_source_has_identity_extension(source) } == 0 {
        return IdentityExtensionState::Absent;
    }
    let Some(account_label) =
        take_owned_c_string(unsafe { mail_bridge_eds_source_dup_identity_account_label(source) })
            .filter(|value| !value.trim().is_empty())
    else {
        tracing::warn!("EDS identity extension has no account label");
        return IdentityExtensionState::Invalid;
    };
    let Some(default_address) =
        take_owned_c_string(unsafe { mail_bridge_eds_source_dup_identity_default_address(source) })
            .filter(|value| !value.is_empty())
    else {
        tracing::warn!("EDS identity extension has no default identity address");
        return IdentityExtensionState::Invalid;
    };
    let identity_records =
        take_owned_c_string_vector(unsafe { mail_bridge_eds_source_dup_identity_records(source) });
    if identity_records.len() % 4 != 0 {
        tracing::warn!("EDS identity extension contains an incomplete metadata record");
        return IdentityExtensionState::Invalid;
    }
    let mut standard_addresses = HashSet::new();
    if let Some(address) = unsafe { source_mail_identity_address(source) } {
        standard_addresses.insert(crate::model::address::normalized_mailbox_address(&address));
    }
    if let Some(aliases) = unsafe { source_mail_identity_aliases(source) } {
        standard_addresses.extend(
            super::camel::decode_addresses(&aliases)
                .expect("an EDS C string cannot contain an interior NUL")
                .into_iter()
                .map(|(address, _)| crate::model::address::normalized_mailbox_address(&address)),
        );
    }
    standard_addresses.remove("");
    let mut represented = HashSet::new();
    let stored_count = identity_records.len() / 4;
    let Some(identities) = identity_records
        .chunks_exact(4)
        .filter(|fields| {
            let address = crate::model::address::normalized_mailbox_address(&fields[0]);
            standard_addresses.contains(&address) && represented.insert(address)
        })
        .map(|fields| {
            Some(IdentityMetadata {
                address: fields[0].clone(),
                reply_to: fields[1].clone(),
                signatures: SignatureSources {
                    html: unsafe { load_signature_source(registry, fields[2].clone())? },
                    text: unsafe { load_signature_source(registry, fields[3].clone())? },
                },
            })
        })
        .collect::<Option<Vec<_>>>()
    else {
        return IdentityExtensionState::Invalid;
    };
    let needs_rewrite = identities.len() != stored_count;
    IdentityExtensionState::Valid(LoadedIdentityExtension {
        extension: IdentityExtension { account_label, default_address, identities },
        needs_rewrite,
    })
}

unsafe fn load_signature_source(
    registry: *mut ESourceRegistry,
    uid: String,
) -> Option<SignatureMetadata> {
    if uid == "none" {
        return Some(empty_signature_metadata(uid));
    }
    if uid.is_empty() {
        tracing::warn!("EDS identity extension contains an empty signature uid");
        return None;
    }
    let Ok(c_uid) = CString::new(uid.as_str()) else {
        tracing::warn!("EDS identity extension contains an invalid signature uid");
        return None;
    };
    let Some(source) = (unsafe { owned_source(registry, c_uid.as_ptr()) }) else {
        tracing::warn!("EDS identity extension references a missing signature source");
        return None;
    };
    let Some(extension) =
        (unsafe { existing_extension(source.as_ptr(), EXTENSION_MAIL_SIGNATURE) })
    else {
        tracing::warn!("EDS identity extension references a non-signature source");
        return None;
    };
    let mime_type = take_owned_c_string(unsafe {
        e_source_mail_signature_dup_mime_type(extension as *mut ESourceMailSignature)
    })
    .unwrap_or_else(|| "text/plain".into());
    let mut contents = ptr::null_mut();
    let mut length = 0usize;
    let mut error = ptr::null_mut();
    let success = unsafe {
        e_source_mail_signature_load_sync(
            source.as_ptr(),
            &mut contents,
            &mut length,
            ptr::null_mut(),
            &mut error,
        )
    };
    let contents = unsafe { OwnedGlibString::from_ptr(contents) };
    if success == 0 {
        if let Some(error) = take_gerror(error) {
            crate::logging::report_failure("eds-signature-load", &error);
        }
        return None;
    }
    let Some(contents) = contents else {
        tracing::warn!("EDS signature source returned no contents");
        return None;
    };
    let text = String::from_utf8_lossy(unsafe {
        std::slice::from_raw_parts(contents.as_ptr() as *const u8, length)
    })
    .into_owned();
    Some(SignatureMetadata {
        uid,
        display_name: take_owned_c_string(unsafe { e_source_dup_display_name(source.as_ptr()) })
            .unwrap_or_default(),
        contents: text,
        mime_type,
    })
}

fn empty_signature_metadata(uid: String) -> SignatureMetadata {
    SignatureMetadata {
        uid,
        display_name: String::new(),
        contents: String::new(),
        mime_type: "text/plain".into(),
    }
}

unsafe fn source_authentication_method(source: *mut ESource) -> Option<String> {
    let extension = unsafe { existing_extension(source, EXTENSION_AUTHENTICATION)? };
    take_owned_c_string(unsafe {
        e_source_authentication_dup_method(extension as *mut ESourceAuthentication)
    })
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

fn take_owned_c_string(value: *mut c_char) -> Option<String> {
    let value = unsafe { OwnedGlibString::from_ptr(value) }?;
    Some(value.to_string_lossy())
}

fn take_owned_c_string_vector(value: *mut *mut c_char) -> Vec<String> {
    if value.is_null() {
        return Vec::new();
    }
    let mut strings = Vec::new();
    let mut cursor = value;
    unsafe {
        while !(*cursor).is_null() {
            strings.push(CStr::from_ptr(*cursor).to_string_lossy().into_owned());
            glib::ffi::g_free(*cursor as glib::ffi::gpointer);
            cursor = cursor.add(1);
        }
        glib::ffi::g_free(value as glib::ffi::gpointer);
    }
    strings
}
