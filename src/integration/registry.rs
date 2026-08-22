use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::os::raw::c_void;
use std::ptr;
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Result, anyhow};

#[derive(Debug, Clone, Default)]
pub struct Source {
    pub object_path: String,
    pub uid: Option<String>,
    pub parent: Option<String>,
    pub goa_account_id: Option<String>,
    pub goa_name: Option<String>,
    pub goa_address: Option<String>,
    pub mail_enabled: Option<bool>,
    pub identity_name: Option<String>,
    pub identity_address: Option<String>,
    pub identity_reply_to: Option<String>,
    pub identity_aliases: Option<String>,
    pub backend_name: Option<String>,
    pub auth_method: Option<String>,
    pub drafts_folder: Option<String>,
    pub sent_folder: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MailTriplet {
    pub account: Option<Source>,
    pub identity: Option<Source>,
    pub transport: Option<Source>,
}

#[derive(Debug, Clone)]
pub struct Relationship {
    pub account_uid: Option<String>,
    pub identity_uid: Option<String>,
    pub transport_uid: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub accounts: Vec<Source>,
    pub identities: Vec<Source>,
    pub transports: Vec<Source>,
    pub triplets: Vec<MailTriplet>,
    pub relationships: Vec<Relationship>,
}

const EXTENSION_GOA: &[&str] = &["GNOME Online Accounts", "goa"];
const EXTENSION_COLLECTION: &[&str] = &["Collection", "collection"];
const EXTENSION_MAIL_ACCOUNT: &[&str] = &["Mail Account", "mail-account"];
const EXTENSION_MAIL_IDENTITY: &[&str] = &["Mail Identity", "mail-identity"];
const EXTENSION_MAIL_COMPOSITION: &[&str] = &["Mail Composition", "mail-composition"];
const EXTENSION_MAIL_SUBMISSION: &[&str] = &["Mail Submission", "mail-submission"];
const EXTENSION_MAIL_TRANSPORT: &[&str] = &["Mail Transport", "mail-transport"];
const EXTENSION_AUTHENTICATION: &[&str] = &["Authentication", "authentication"];

enum ESourceRegistry {}
enum ESource {}
enum ESourceExtension {}
enum ESourceGoa {}
enum ESourceMailAccount {}
enum ESourceMailComposition {}
enum ESourceMailSubmission {}
enum ESourceMailIdentity {}
enum ESourceCollection {}
enum ESourceAuthentication {}
enum ESourceBackend {}
enum ESourceLocal {}
enum GFile {}
enum RegistryWatch {}

type RegistryChangeCallback = Arc<dyn Fn(String) + Send + Sync>;
type RegistryFailureCallback = Arc<dyn Fn(anyhow::Error) + Send + Sync>;

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
    direct_goa_account_id: Option<String>,
    direct_goa_name: Option<String>,
    direct_goa_address: Option<String>,
    direct_collection_mail_enabled: Option<bool>,
    direct_mail_account_backend_name: Option<String>,
    direct_mail_transport_backend_name: Option<String>,
    direct_identity_uid: Option<String>,
    direct_transport_uid: Option<String>,
    direct_name: Option<String>,
    direct_address: Option<String>,
    direct_reply_to: Option<String>,
    direct_aliases: Option<String>,
    direct_auth_method: Option<String>,
    direct_drafts_folder: Option<String>,
    direct_sent_folder: Option<String>,
    is_account: bool,
    is_identity: bool,
    is_transport: bool,
}

#[allow(clashing_extern_declarations)]
#[link(name = "edataserver-1.2")]
unsafe extern "C" {
    fn e_source_registry_new_sync(
        cancellable: *mut gio::ffi::GCancellable,
        error: *mut *mut glib::ffi::GError,
    ) -> *mut ESourceRegistry;
    fn e_source_registry_ref_source(
        registry: *mut ESourceRegistry,
        uid: *const c_char,
    ) -> *mut ESource;
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

    fn e_source_has_extension(
        source: *mut ESource,
        extension_name: *const c_char,
    ) -> glib::ffi::gboolean;
    fn e_source_get_extension(
        source: *mut ESource,
        extension_name: *const c_char,
    ) -> *mut ESourceExtension;

    fn e_source_get_uid(source: *mut ESource) -> *const c_char;
    fn e_source_get_parent(source: *mut ESource) -> *const c_char;

    fn e_source_goa_get_account_id(extension: *mut ESourceGoa) -> *const c_char;
    fn e_source_goa_get_name(extension: *mut ESourceGoa) -> *const c_char;
    fn e_source_goa_get_address(extension: *mut ESourceGoa) -> *const c_char;
    fn e_source_collection_get_mail_enabled(
        extension: *mut ESourceCollection,
    ) -> glib::ffi::gboolean;
    fn e_source_backend_get_backend_name(extension: *mut ESourceBackend) -> *const c_char;
    fn e_source_mail_account_get_identity_uid(extension: *mut ESourceMailAccount) -> *const c_char;
    fn e_source_mail_submission_get_transport_uid(
        extension: *mut ESourceMailSubmission,
    ) -> *const c_char;
    fn e_source_mail_composition_dup_drafts_folder(
        extension: *mut ESourceMailComposition,
    ) -> *mut c_char;
    fn e_source_mail_submission_dup_sent_folder(
        extension: *mut ESourceMailSubmission,
    ) -> *mut c_char;
    fn e_source_mail_composition_set_drafts_folder(
        extension: *mut ESourceMailComposition,
        drafts_folder: *const c_char,
    );
    fn e_source_mail_submission_set_sent_folder(
        extension: *mut ESourceMailSubmission,
        sent_folder: *const c_char,
    );
    fn e_source_mail_identity_get_name(extension: *mut ESourceMailIdentity) -> *const c_char;
    fn e_source_mail_identity_get_address(extension: *mut ESourceMailIdentity) -> *const c_char;
    fn e_source_mail_identity_get_reply_to(extension: *mut ESourceMailIdentity) -> *const c_char;
    fn e_source_mail_identity_get_aliases(extension: *mut ESourceMailIdentity) -> *const c_char;
    fn e_source_mail_identity_set_name(extension: *mut ESourceMailIdentity, name: *const c_char);
    fn e_source_mail_identity_set_reply_to(
        extension: *mut ESourceMailIdentity,
        reply_to: *const c_char,
    );
    fn e_source_mail_identity_set_aliases(
        extension: *mut ESourceMailIdentity,
        aliases: *const c_char,
    );
    fn e_source_local_set_custom_file(extension: *mut ESourceLocal, custom_file: *mut GFile);
    fn e_source_authentication_get_method(extension: *mut ESourceAuthentication) -> *const c_char;
    fn g_file_new_for_path(path: *const c_char) -> *mut GFile;

    fn mail_bridge_eds_registry_watch_new(
        callback: Option<unsafe extern "C" fn(*const c_char, *mut c_void)>,
        user_data: *mut c_void,
        destroy: Option<unsafe extern "C" fn(*mut c_void)>,
        error: *mut *mut glib::ffi::GError,
    ) -> *mut RegistryWatch;
    fn mail_bridge_eds_registry_watch_iteration(watch: *mut RegistryWatch);
    fn mail_bridge_eds_registry_watch_free(watch: *mut RegistryWatch);
}

impl Monitor {
    pub(crate) fn start(
        callback: impl Fn(String) + Send + Sync + 'static,
        failure: impl Fn(anyhow::Error) + Send + Sync + 'static,
    ) -> Self {
        let callback: RegistryChangeCallback = Arc::new(callback);
        let failure: RegistryFailureCallback = Arc::new(failure);
        let (stop, stop_receiver) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut failure_reported = false;
            loop {
                if stop_receiver.try_recv().is_ok() {
                    break;
                }
                let monitor = match RawMonitor::open(Arc::clone(&callback)) {
                    Ok(monitor) => monitor,
                    Err(error) => {
                        if !failure_reported {
                            failure(error);
                            failure_reported = true;
                        }
                        match stop_receiver.recv_timeout(Duration::from_secs(30)) {
                            Err(RecvTimeoutError::Timeout) => continue,
                            Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                        }
                    }
                };

                loop {
                    monitor.iterate();
                    match stop_receiver.recv_timeout(Duration::from_millis(50)) {
                        Err(RecvTimeoutError::Timeout) => {}
                        Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
            }
        });
        Self {
            stop,
            worker: Some(worker),
        }
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        drop(self.worker.take());
    }
}

impl RawMonitor {
    fn open(callback: RegistryChangeCallback) -> Result<Self> {
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
    account_id: *const c_char,
    user_data: *mut c_void,
) {
    let callback = unsafe { &*(user_data as *const RegistryChangeCallback) };
    if let Some(account_id) = cstr_to_string(account_id) {
        callback(account_id);
    }
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
        let mut error = ptr::null_mut();
        let registry = e_source_registry_new_sync(ptr::null_mut(), &mut error);
        if registry.is_null() {
            return Err(take_gerror(error)
                .unwrap_or_else(|| anyhow!("e_source_registry_new_sync returned NULL")));
        }

        let result = load_snapshot_from_registry(registry);
        glib::gobject_ffi::g_object_unref(registry as *mut _);
        result
    }
}

unsafe fn load_snapshot_from_registry(registry: *mut ESourceRegistry) -> Result<Snapshot> {
    let sources = unsafe { list_sources(registry)? };
    let infos = sources
        .iter()
        .copied()
        .filter_map(|source| unsafe { load_source_info(source) })
        .collect::<Vec<_>>();
    unref_sources(&sources);
    build_snapshot(&infos)
}

unsafe fn list_sources(registry: *mut ESourceRegistry) -> Result<Vec<*mut ESource>> {
    let list = unsafe { e_source_registry_list_sources(registry, ptr::null()) };
    if list.is_null() {
        return Ok(Vec::new());
    }

    let mut sources = Vec::new();
    let mut cursor = list;
    while !cursor.is_null() {
        let source = unsafe { (*cursor).data as *mut ESource };
        if !source.is_null() {
            sources.push(source);
        }
        cursor = unsafe { (*cursor).next };
    }

    unsafe { glib::ffi::g_list_free(list) };
    Ok(sources)
}

fn unref_sources(sources: &[*mut ESource]) {
    for source in sources {
        if !source.is_null() {
            unsafe { glib::gobject_ffi::g_object_unref(*source as *mut _) };
        }
    }
}

pub(crate) fn write_identity_via_ffi(
    identity_uid: &str,
    name: Option<&str>,
    reply_to: Option<&str>,
    aliases: Option<&str>,
) -> Result<()> {
    unsafe {
        let mut error = ptr::null_mut();
        let registry = e_source_registry_new_sync(ptr::null_mut(), &mut error);
        if registry.is_null() {
            return Err(take_gerror(error)
                .unwrap_or_else(|| anyhow!("e_source_registry_new_sync returned NULL")));
        }

        let uid = CString::new(identity_uid)
            .map_err(|_| anyhow!("identity uid contains interior NUL"))?;
        let source = e_source_registry_ref_source(registry, uid.as_ptr());
        if source.is_null() {
            glib::gobject_ffi::g_object_unref(registry as *mut _);
            return Err(anyhow!(
                "could not resolve EDS identity source for uid {}",
                identity_uid
            ));
        }

        let extension = get_extension(source, EXTENSION_MAIL_IDENTITY)
            .ok_or_else(|| anyhow!("EDS source {} has no Mail Identity extension", identity_uid))?
            as *mut ESourceMailIdentity;

        let name = CString::new(name.unwrap_or(""))
            .map_err(|_| anyhow!("identity name contains interior NUL"))?;
        let reply_to = CString::new(reply_to.unwrap_or(""))
            .map_err(|_| anyhow!("identity reply-to contains interior NUL"))?;
        let aliases = CString::new(aliases.unwrap_or(""))
            .map_err(|_| anyhow!("identity aliases contain interior NUL"))?;

        e_source_mail_identity_set_name(extension, name.as_ptr());
        e_source_mail_identity_set_reply_to(extension, reply_to.as_ptr());
        e_source_mail_identity_set_aliases(extension, aliases.as_ptr());

        let success =
            e_source_registry_commit_source_sync(registry, source, ptr::null_mut(), &mut error)
                != 0;

        glib::gobject_ffi::g_object_unref(source as *mut _);
        glib::gobject_ffi::g_object_unref(registry as *mut _);

        if success {
            Ok(())
        } else {
            Err(take_gerror(error)
                .unwrap_or_else(|| anyhow!("e_source_registry_commit_source_sync failed")))
        }
    }
}

pub(crate) fn ensure_local_drafts_configuration(
    identity_uid: &str,
    drafts_uri: &str,
) -> Result<()> {
    unsafe {
        let mut error = ptr::null_mut();
        let registry = e_source_registry_new_sync(ptr::null_mut(), &mut error);
        if registry.is_null() {
            return Err(take_gerror(error)
                .unwrap_or_else(|| anyhow!("e_source_registry_new_sync returned NULL")));
        }

        let identity_uid_c = CString::new(identity_uid)
            .map_err(|_| anyhow!("identity uid contains interior NUL"))?;
        let identity_source = e_source_registry_ref_source(registry, identity_uid_c.as_ptr());
        if identity_source.is_null() {
            glib::gobject_ffi::g_object_unref(registry as *mut _);
            return Err(anyhow!(
                "could not resolve EDS identity source for uid {}",
                identity_uid
            ));
        }

        let mail_composition = get_extension(identity_source, EXTENSION_MAIL_COMPOSITION)
            .ok_or_else(|| anyhow!("identity source has no Mail Composition extension"))?
            as *mut ESourceMailComposition;
        let drafts_uri_c =
            CString::new(drafts_uri).map_err(|_| anyhow!("drafts uri contains interior NUL"))?;
        e_source_mail_composition_set_drafts_folder(mail_composition, drafts_uri_c.as_ptr());

        let success = e_source_registry_commit_source_sync(
            registry,
            identity_source,
            ptr::null_mut(),
            &mut error,
        ) != 0;

        glib::gobject_ffi::g_object_unref(identity_source as *mut _);
        glib::gobject_ffi::g_object_unref(registry as *mut _);

        if success {
            Ok(())
        } else {
            Err(take_gerror(error)
                .unwrap_or_else(|| anyhow!("failed to commit identity drafts folder")))
        }
    }
}

pub(crate) fn set_sent_folder_configuration(identity_uid: &str, sent_uri: &str) -> Result<()> {
    unsafe {
        let mut error = ptr::null_mut();
        let registry = e_source_registry_new_sync(ptr::null_mut(), &mut error);
        if registry.is_null() {
            return Err(take_gerror(error)
                .unwrap_or_else(|| anyhow!("e_source_registry_new_sync returned NULL")));
        }

        let identity_uid_c = CString::new(identity_uid)
            .map_err(|_| anyhow!("identity uid contains interior NUL"))?;
        let identity_source = e_source_registry_ref_source(registry, identity_uid_c.as_ptr());
        if identity_source.is_null() {
            glib::gobject_ffi::g_object_unref(registry as *mut _);
            return Err(anyhow!(
                "could not resolve EDS identity source for uid {}",
                identity_uid
            ));
        }
        let mail_submission = get_extension(identity_source, EXTENSION_MAIL_SUBMISSION)
            .ok_or_else(|| anyhow!("identity source has no Mail Submission extension"))?
            as *mut ESourceMailSubmission;
        let sent_uri_c =
            CString::new(sent_uri).map_err(|_| anyhow!("sent uri contains interior NUL"))?;
        e_source_mail_submission_set_sent_folder(mail_submission, sent_uri_c.as_ptr());

        let success = e_source_registry_commit_source_sync(
            registry,
            identity_source,
            ptr::null_mut(),
            &mut error,
        ) != 0;
        glib::gobject_ffi::g_object_unref(identity_source as *mut _);
        glib::gobject_ffi::g_object_unref(registry as *mut _);
        if success {
            Ok(())
        } else {
            Err(take_gerror(error)
                .unwrap_or_else(|| anyhow!("failed to commit identity sent folder")))
        }
    }
}

pub(crate) fn ensure_builtin_local_mail_root(local_root_path: &str) -> Result<()> {
    unsafe {
        let mut error = ptr::null_mut();
        let registry = e_source_registry_new_sync(ptr::null_mut(), &mut error);
        if registry.is_null() {
            return Err(take_gerror(error)
                .unwrap_or_else(|| anyhow!("e_source_registry_new_sync returned NULL")));
        }

        let local_uid =
            CString::new("local").map_err(|_| anyhow!("local uid contains interior NUL"))?;
        let local_source = e_source_registry_ref_source(registry, local_uid.as_ptr());
        if local_source.is_null() {
            glib::gobject_ffi::g_object_unref(registry as *mut _);
            return Err(anyhow!("could not resolve built-in EDS local source"));
        }

        let local_extension = get_extension(local_source, &["Local Backend"])
            .ok_or_else(|| anyhow!("built-in local source has no Local Backend extension"))?
            as *mut ESourceLocal;
        let local_root_path = CString::new(local_root_path)
            .map_err(|_| anyhow!("local root path contains interior NUL"))?;
        let custom_file = g_file_new_for_path(local_root_path.as_ptr());
        if custom_file.is_null() {
            glib::gobject_ffi::g_object_unref(local_source as *mut _);
            glib::gobject_ffi::g_object_unref(registry as *mut _);
            return Err(anyhow!("g_file_new_for_path returned NULL"));
        }

        e_source_local_set_custom_file(local_extension, custom_file);
        glib::gobject_ffi::g_object_unref(custom_file as *mut _);

        let success = e_source_registry_commit_source_sync(
            registry,
            local_source,
            ptr::null_mut(),
            &mut error,
        ) != 0;

        glib::gobject_ffi::g_object_unref(local_source as *mut _);
        glib::gobject_ffi::g_object_unref(registry as *mut _);

        if success {
            Ok(())
        } else {
            Err(take_gerror(error)
                .unwrap_or_else(|| anyhow!("failed to commit built-in local source root")))
        }
    }
}
unsafe fn load_source_info(source: *mut ESource) -> Option<SourceInfo> {
    let uid = unsafe { cstr_to_string(e_source_get_uid(source)) }?;

    Some(SourceInfo {
        uid,
        parent: unsafe { cstr_to_string(e_source_get_parent(source)) },
        direct_goa_account_id: unsafe { source_goa_account_id(source) },
        direct_goa_name: unsafe { source_goa_name(source) },
        direct_goa_address: unsafe { source_goa_address(source) },
        direct_collection_mail_enabled: unsafe { source_collection_mail_enabled(source) },
        direct_mail_account_backend_name: unsafe { source_mail_account_backend_name(source) },
        direct_mail_transport_backend_name: unsafe { source_mail_transport_backend_name(source) },
        direct_identity_uid: unsafe { source_mail_account_identity_uid(source) },
        direct_transport_uid: unsafe { source_mail_submission_transport_uid(source) },
        direct_name: unsafe { source_mail_identity_name(source) },
        direct_address: unsafe { source_mail_identity_address(source) },
        direct_reply_to: unsafe { source_mail_identity_reply_to(source) },
        direct_aliases: unsafe { source_mail_identity_aliases(source) },
        direct_auth_method: unsafe { source_authentication_method(source) },
        direct_drafts_folder: unsafe { source_mail_composition_drafts_folder(source) },
        direct_sent_folder: unsafe { source_mail_submission_sent_folder(source) },
        is_account: unsafe { has_any_extension(source, EXTENSION_MAIL_ACCOUNT) },
        is_identity: unsafe { has_any_extension(source, EXTENSION_MAIL_IDENTITY) },
        is_transport: unsafe {
            has_any_extension(source, EXTENSION_MAIL_TRANSPORT)
                || has_any_extension(source, EXTENSION_MAIL_SUBMISSION)
        },
    })
}

fn build_snapshot(infos: &[SourceInfo]) -> Result<Snapshot> {
    let info_by_uid = infos
        .iter()
        .map(|info| (info.uid.clone(), info.clone()))
        .collect::<BTreeMap<_, _>>();

    let mut accounts = Vec::new();
    let mut identities = Vec::new();
    let mut transports = Vec::new();

    for info in infos {
        let inherited_goa_account_id = inherited_source_value(info, &info_by_uid, |source| {
            source.direct_goa_account_id.as_ref()
        });
        let inherited_goa_name =
            inherited_source_value(info, &info_by_uid, |source| source.direct_goa_name.as_ref());
        let inherited_goa_address = inherited_source_value(info, &info_by_uid, |source| {
            source.direct_goa_address.as_ref()
        });
        let inherited_collection_mail_enabled =
            inherited_source_value(info, &info_by_uid, |source| {
                source.direct_collection_mail_enabled.as_ref()
            });
        let resolved_backend_name = if info.is_account {
            info.direct_mail_account_backend_name.clone()
        } else if info.is_transport {
            info.direct_mail_transport_backend_name.clone()
        } else {
            None
        };

        let entry = Source {
            object_path: format!("esource:{}", info.uid),
            uid: Some(info.uid.clone()),
            parent: info.parent.clone(),
            goa_account_id: inherited_goa_account_id,
            goa_name: inherited_goa_name,
            goa_address: inherited_goa_address,
            mail_enabled: inherited_collection_mail_enabled,
            identity_name: info.direct_name.clone(),
            identity_address: info.direct_address.clone(),
            identity_reply_to: info.direct_reply_to.clone(),
            identity_aliases: info.direct_aliases.clone(),
            backend_name: resolved_backend_name,
            auth_method: info.direct_auth_method.clone(),
            drafts_folder: info.direct_drafts_folder.clone(),
            sent_folder: info.direct_sent_folder.clone(),
        };

        if info.is_account {
            accounts.push(entry);
        } else if info.is_identity {
            identities.push(entry);
        } else if info.is_transport {
            transports.push(entry);
        }
    }

    accounts.sort_by(|left, right| left.object_path.cmp(&right.object_path));
    identities.sort_by(|left, right| left.object_path.cmp(&right.object_path));
    transports.sort_by(|left, right| left.object_path.cmp(&right.object_path));

    let entries_by_uid = accounts
        .iter()
        .chain(identities.iter())
        .chain(transports.iter())
        .filter_map(|entry| entry.uid.as_ref().map(|uid| (uid.clone(), entry.clone())))
        .collect::<BTreeMap<_, _>>();

    let mut relationships = Vec::new();
    let mut referenced_identity_uids = BTreeSet::new();
    let mut referenced_transport_uids = BTreeSet::new();

    for info in infos.iter().filter(|info| info.is_account) {
        let account_entry = match entries_by_uid.get(&info.uid) {
            Some(entry) => entry,
            None => continue,
        };

        let identity_uid = info.direct_identity_uid.clone();
        let transport_uid = identity_uid
            .as_ref()
            .and_then(|uid| info_by_uid.get(uid))
            .and_then(|identity_info| identity_info.direct_transport_uid.clone());
        if let Some(uid) = identity_uid.clone() {
            referenced_identity_uids.insert(uid);
        }
        if let Some(uid) = transport_uid.clone() {
            referenced_transport_uids.insert(uid);
        }

        relationships.push(Relationship {
            account_uid: account_entry.uid.clone(),
            identity_uid: identity_uid.clone(),
            transport_uid: transport_uid.clone(),
        });
    }

    let mut triplets = relationships
        .iter()
        .map(|relationship| MailTriplet {
            account: relationship
                .account_uid
                .as_ref()
                .and_then(|uid| entries_by_uid.get(uid))
                .cloned(),
            identity: relationship
                .identity_uid
                .as_ref()
                .and_then(|uid| entries_by_uid.get(uid))
                .cloned(),
            transport: relationship
                .transport_uid
                .as_ref()
                .and_then(|uid| entries_by_uid.get(uid))
                .cloned(),
        })
        .collect::<Vec<_>>();

    triplets.extend(
        transports
            .iter()
            .filter(|entry| {
                entry
                    .uid
                    .as_ref()
                    .map(|uid| !referenced_transport_uids.contains(uid))
                    .unwrap_or(true)
            })
            .cloned()
            .map(|transport| MailTriplet {
                account: None,
                identity: None,
                transport: Some(transport),
            }),
    );

    triplets.extend(
        identities
            .iter()
            .filter(|entry| {
                entry
                    .uid
                    .as_ref()
                    .map(|uid| !referenced_identity_uids.contains(uid))
                    .unwrap_or(true)
            })
            .cloned()
            .map(|identity| MailTriplet {
                account: None,
                identity: Some(identity),
                transport: None,
            }),
    );

    Ok(Snapshot {
        accounts,
        identities,
        transports,
        triplets,
        relationships,
    })
}

fn inherited_source_value<T: Clone>(
    info: &SourceInfo,
    infos: &BTreeMap<String, SourceInfo>,
    value: impl for<'a> Fn(&'a SourceInfo) -> Option<&'a T>,
) -> Option<T> {
    if let Some(value) = value(info) {
        return Some(value.clone());
    }

    let mut visited = BTreeSet::new();
    let mut current_parent = info.parent.clone();
    while let Some(parent_uid) = current_parent {
        if !visited.insert(parent_uid.clone()) {
            return None;
        }
        let parent = infos.get(&parent_uid)?;
        if let Some(value) = value(parent) {
            return Some(value.clone());
        }
        current_parent = parent.parent.clone();
    }
    None
}

unsafe fn has_any_extension(source: *mut ESource, extension_names: &[&str]) -> bool {
    extension_names.iter().any(|extension_name| {
        CString::new(*extension_name)
            .ok()
            .is_some_and(|name| unsafe { e_source_has_extension(source, name.as_ptr()) != 0 })
    })
}

unsafe fn get_extension(
    source: *mut ESource,
    extension_names: &[&str],
) -> Option<*mut ESourceExtension> {
    extension_names.iter().find_map(|extension_name| {
        let name = CString::new(*extension_name).ok()?;
        let extension = unsafe { e_source_get_extension(source, name.as_ptr()) };
        (!extension.is_null()).then_some(extension)
    })
}

unsafe fn source_goa_account_id(source: *mut ESource) -> Option<String> {
    if !unsafe { has_any_extension(source, EXTENSION_GOA) } {
        return None;
    }
    let extension = unsafe { get_extension(source, EXTENSION_GOA)? };
    cstr_to_string(unsafe { e_source_goa_get_account_id(extension as *mut ESourceGoa) })
}

unsafe fn source_goa_name(source: *mut ESource) -> Option<String> {
    if !unsafe { has_any_extension(source, EXTENSION_GOA) } {
        return None;
    }
    let extension = unsafe { get_extension(source, EXTENSION_GOA)? };
    cstr_to_string(unsafe { e_source_goa_get_name(extension as *mut ESourceGoa) })
}

unsafe fn source_goa_address(source: *mut ESource) -> Option<String> {
    if !unsafe { has_any_extension(source, EXTENSION_GOA) } {
        return None;
    }
    let extension = unsafe { get_extension(source, EXTENSION_GOA)? };
    cstr_to_string(unsafe { e_source_goa_get_address(extension as *mut ESourceGoa) })
}

unsafe fn source_collection_mail_enabled(source: *mut ESource) -> Option<bool> {
    if !unsafe { has_any_extension(source, EXTENSION_COLLECTION) } {
        return None;
    }
    let extension = unsafe { get_extension(source, EXTENSION_COLLECTION)? };
    Some(unsafe { e_source_collection_get_mail_enabled(extension as *mut ESourceCollection) != 0 })
}

unsafe fn source_mail_account_backend_name(source: *mut ESource) -> Option<String> {
    if !unsafe { has_any_extension(source, EXTENSION_MAIL_ACCOUNT) } {
        return None;
    }
    let extension = unsafe { get_extension(source, EXTENSION_MAIL_ACCOUNT)? };
    cstr_to_string(unsafe { e_source_backend_get_backend_name(extension as *mut ESourceBackend) })
}

unsafe fn source_mail_transport_backend_name(source: *mut ESource) -> Option<String> {
    if !unsafe { has_any_extension(source, EXTENSION_MAIL_TRANSPORT) } {
        return None;
    }
    let extension = unsafe { get_extension(source, EXTENSION_MAIL_TRANSPORT)? };
    cstr_to_string(unsafe { e_source_backend_get_backend_name(extension as *mut ESourceBackend) })
}

unsafe fn source_mail_account_identity_uid(source: *mut ESource) -> Option<String> {
    if !unsafe { has_any_extension(source, EXTENSION_MAIL_ACCOUNT) } {
        return None;
    }
    let extension = unsafe { get_extension(source, EXTENSION_MAIL_ACCOUNT)? };
    cstr_to_string(unsafe {
        e_source_mail_account_get_identity_uid(extension as *mut ESourceMailAccount)
    })
}

unsafe fn source_mail_submission_transport_uid(source: *mut ESource) -> Option<String> {
    if !unsafe { has_any_extension(source, EXTENSION_MAIL_SUBMISSION) } {
        return None;
    }
    let extension = unsafe { get_extension(source, EXTENSION_MAIL_SUBMISSION)? };
    cstr_to_string(unsafe {
        e_source_mail_submission_get_transport_uid(extension as *mut ESourceMailSubmission)
    })
}

unsafe fn source_mail_composition_drafts_folder(source: *mut ESource) -> Option<String> {
    if !unsafe { has_any_extension(source, EXTENSION_MAIL_COMPOSITION) } {
        return None;
    }
    let extension = unsafe { get_extension(source, EXTENSION_MAIL_COMPOSITION)? };
    take_owned_c_string(unsafe {
        e_source_mail_composition_dup_drafts_folder(extension as *mut ESourceMailComposition)
    })
}

unsafe fn source_mail_submission_sent_folder(source: *mut ESource) -> Option<String> {
    if !unsafe { has_any_extension(source, EXTENSION_MAIL_SUBMISSION) } {
        return None;
    }
    let extension = unsafe { get_extension(source, EXTENSION_MAIL_SUBMISSION)? };
    take_owned_c_string(unsafe {
        e_source_mail_submission_dup_sent_folder(extension as *mut ESourceMailSubmission)
    })
}

unsafe fn source_mail_identity_name(source: *mut ESource) -> Option<String> {
    if !unsafe { has_any_extension(source, EXTENSION_MAIL_IDENTITY) } {
        return None;
    }
    let extension = unsafe { get_extension(source, EXTENSION_MAIL_IDENTITY)? };
    cstr_to_string(unsafe {
        e_source_mail_identity_get_name(extension as *mut ESourceMailIdentity)
    })
}

unsafe fn source_mail_identity_address(source: *mut ESource) -> Option<String> {
    if !unsafe { has_any_extension(source, EXTENSION_MAIL_IDENTITY) } {
        return None;
    }
    let extension = unsafe { get_extension(source, EXTENSION_MAIL_IDENTITY)? };
    cstr_to_string(unsafe {
        e_source_mail_identity_get_address(extension as *mut ESourceMailIdentity)
    })
}

unsafe fn source_mail_identity_reply_to(source: *mut ESource) -> Option<String> {
    if !unsafe { has_any_extension(source, EXTENSION_MAIL_IDENTITY) } {
        return None;
    }
    let extension = unsafe { get_extension(source, EXTENSION_MAIL_IDENTITY)? };
    cstr_to_string(unsafe {
        e_source_mail_identity_get_reply_to(extension as *mut ESourceMailIdentity)
    })
}

unsafe fn source_mail_identity_aliases(source: *mut ESource) -> Option<String> {
    if !unsafe { has_any_extension(source, EXTENSION_MAIL_IDENTITY) } {
        return None;
    }
    let extension = unsafe { get_extension(source, EXTENSION_MAIL_IDENTITY)? };
    cstr_to_string(unsafe {
        e_source_mail_identity_get_aliases(extension as *mut ESourceMailIdentity)
    })
}

unsafe fn source_authentication_method(source: *mut ESource) -> Option<String> {
    if !unsafe { has_any_extension(source, EXTENSION_AUTHENTICATION) } {
        return None;
    }
    let extension = unsafe { get_extension(source, EXTENSION_AUTHENTICATION)? };
    cstr_to_string(unsafe {
        e_source_authentication_get_method(extension as *mut ESourceAuthentication)
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
    if value.is_null() {
        return None;
    }
    let text = unsafe { CStr::from_ptr(value) }
        .to_string_lossy()
        .into_owned();
    unsafe { glib::ffi::g_free(value as glib::ffi::gpointer) };
    Some(text)
}

fn take_gerror(error: *mut glib::ffi::GError) -> Option<anyhow::Error> {
    if error.is_null() {
        return None;
    }

    let message =
        cstr_to_string(unsafe { (*error).message }).unwrap_or_else(|| "unknown GLib error".into());
    unsafe { glib::ffi::g_error_free(error) };
    Some(anyhow!(message))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{SourceInfo, build_snapshot, inherited_source_value};

    fn source(uid: &str, parent: Option<&str>) -> SourceInfo {
        SourceInfo {
            uid: uid.into(),
            parent: parent.map(str::to_string),
            direct_goa_account_id: None,
            direct_goa_name: None,
            direct_goa_address: None,
            direct_collection_mail_enabled: None,
            direct_mail_account_backend_name: None,
            direct_mail_transport_backend_name: None,
            direct_identity_uid: None,
            direct_transport_uid: None,
            direct_name: None,
            direct_address: None,
            direct_reply_to: None,
            direct_aliases: None,
            direct_auth_method: None,
            direct_drafts_folder: None,
            direct_sent_folder: None,
            is_account: false,
            is_identity: false,
            is_transport: false,
        }
    }

    #[test]
    fn snapshot_inherits_goa_metadata_across_a_complete_mail_triplet() {
        let mut collection = source("collection", None);
        collection.direct_goa_account_id = Some("account-example".into());
        collection.direct_goa_name = Some("Example Account".into());
        collection.direct_goa_address = Some("owner@example.invalid".into());
        collection.direct_collection_mail_enabled = Some(true);

        let mut account = source("account", Some("collection"));
        account.is_account = true;
        account.direct_mail_account_backend_name = Some("imapx".into());
        account.direct_identity_uid = Some("identity".into());
        let mut identity = source("identity", Some("collection"));
        identity.is_identity = true;
        identity.direct_transport_uid = Some("transport".into());
        identity.direct_address = Some("identity@example.invalid".into());
        let mut transport = source("transport", Some("collection"));
        transport.is_transport = true;
        transport.direct_mail_transport_backend_name = Some("smtp".into());

        let snapshot = build_snapshot(&[collection, account, identity, transport]).unwrap();

        assert_eq!(snapshot.triplets.len(), 1);
        let triplet = &snapshot.triplets[0];
        for entry in [
            triplet.account.as_ref().unwrap(),
            triplet.identity.as_ref().unwrap(),
            triplet.transport.as_ref().unwrap(),
        ] {
            assert_eq!(entry.goa_account_id.as_deref(), Some("account-example"));
            assert_eq!(entry.goa_name.as_deref(), Some("Example Account"));
            assert_eq!(entry.goa_address.as_deref(), Some("owner@example.invalid"));
            assert_eq!(entry.mail_enabled, Some(true));
        }
        assert_eq!(
            triplet
                .identity
                .as_ref()
                .unwrap()
                .identity_address
                .as_deref(),
            Some("identity@example.invalid")
        );
    }

    #[test]
    fn inherited_values_stop_at_a_parent_cycle() {
        let first = source("first", Some("second"));
        let second = source("second", Some("first"));
        let sources = [first.clone(), second]
            .into_iter()
            .map(|source| (source.uid.clone(), source))
            .collect::<BTreeMap<_, _>>();

        let inherited = inherited_source_value(&first, &sources, |source| {
            source.direct_goa_account_id.as_ref()
        });

        assert_eq!(inherited, None);
    }
}
