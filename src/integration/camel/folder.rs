use std::cmp::Ordering;
use std::os::raw::{c_char, c_int, c_uint};
use std::ptr::{self, NonNull};

use anyhow::{Result, anyhow};

use crate::model::mail::{FolderId, FolderKind, MailFolder};

use super::{CamelStore, cstr_to_string, take_gerror};

const CAMEL_STORE_FOLDER_INFO_RECURSIVE: c_uint = 1 << 1;
const CAMEL_STORE_FOLDER_INFO_SUBSCRIBED: c_uint = 1 << 2;
const CAMEL_STORE_FOLDER_INFO_NO_VIRTUAL: c_uint = 1 << 3;
const CAMEL_FOLDER_TYPE_BIT: c_uint = 10;
const CAMEL_FOLDER_TYPE_MASK: c_uint = 0x3F << CAMEL_FOLDER_TYPE_BIT;
const CAMEL_FOLDER_TYPE_INBOX: c_uint = 1 << CAMEL_FOLDER_TYPE_BIT;
const CAMEL_FOLDER_TYPE_OUTBOX: c_uint = 2 << CAMEL_FOLDER_TYPE_BIT;
const CAMEL_FOLDER_TYPE_TRASH: c_uint = 3 << CAMEL_FOLDER_TYPE_BIT;
const CAMEL_FOLDER_TYPE_JUNK: c_uint = 4 << CAMEL_FOLDER_TYPE_BIT;
const CAMEL_FOLDER_TYPE_SENT: c_uint = 5 << CAMEL_FOLDER_TYPE_BIT;
const CAMEL_FOLDER_TYPE_ARCHIVE: c_uint = 11 << CAMEL_FOLDER_TYPE_BIT;
const CAMEL_FOLDER_TYPE_DRAFTS: c_uint = 12 << CAMEL_FOLDER_TYPE_BIT;

#[repr(C)]
struct CamelFolderInfo {
    next: *mut CamelFolderInfo,
    parent: *mut CamelFolderInfo,
    child: *mut CamelFolderInfo,
    full_name: *mut c_char,
    display_name: *mut c_char,
    flags: c_uint,
    unread: c_int,
    total: c_int,
}

struct OwnedFolderInfo(NonNull<CamelFolderInfo>);

impl OwnedFolderInfo {
    unsafe fn from_ptr(pointer: *mut CamelFolderInfo) -> Option<Self> {
        NonNull::new(pointer).map(Self)
    }

    fn as_ptr(&self) -> *mut CamelFolderInfo {
        self.0.as_ptr()
    }
}

impl Drop for OwnedFolderInfo {
    fn drop(&mut self) {
        unsafe { camel_folder_info_free(self.0.as_ptr()) };
    }
}

#[link(name = "camel-1.2")]
unsafe extern "C" {
    fn camel_store_get_folder_info_sync(
        store: *mut CamelStore,
        top: *const c_char,
        flags: c_uint,
        cancellable: *mut gio::ffi::GCancellable,
        error: *mut *mut glib::ffi::GError,
    ) -> *mut CamelFolderInfo;
    fn camel_folder_info_free(info: *mut CamelFolderInfo);
}

pub(super) unsafe fn load_folder_tree(store: *mut CamelStore) -> Result<Vec<MailFolder>> {
    let mut error = ptr::null_mut();
    let info = unsafe {
        camel_store_get_folder_info_sync(
            store,
            ptr::null(),
            CAMEL_STORE_FOLDER_INFO_RECURSIVE
                | CAMEL_STORE_FOLDER_INFO_SUBSCRIBED
                | CAMEL_STORE_FOLDER_INFO_NO_VIRTUAL,
            ptr::null_mut(),
            &mut error,
        )
    };
    let Some(info) = (unsafe { OwnedFolderInfo::from_ptr(info) }) else {
        return match take_gerror(error) {
            Some(error) => {
                Err(error)
            }
            None => {
                Ok(Vec::new())
            }
        };
    };

    let mut folders = Vec::new();
    collect_folder_info(info.as_ptr(), &mut folders)?;

    folders.sort_by(compare_folders);
    folders.dedup_by(|left, right| left.id == right.id);
    Ok(folders)
}

fn collect_folder_info(
    info: *mut CamelFolderInfo,
    folders: &mut Vec<MailFolder>,
) -> Result<()> {
    let mut cursor = info;
    while !cursor.is_null() {
        let full_name = unsafe { cstr_to_string((*cursor).full_name) }
            .filter(|name| !name.is_empty())
            .ok_or_else(|| anyhow!("Camel folder info has no full name"))?;
        let display_name = unsafe { cstr_to_string((*cursor).display_name) };
        let folder_name = display_name.unwrap_or_else(|| full_name.clone());
        folders.push(MailFolder {
            id: FolderId(full_name),
            name: folder_name,
            unread_count: unsafe { (*cursor).unread.max(0) as u32 },
            kind: classify_folder_kind(unsafe { (*cursor).flags }),
        });

        let child = unsafe { (*cursor).child };
        if !child.is_null() {
            collect_folder_info(child, folders)?;
        }
        cursor = unsafe { (*cursor).next };
    }
    Ok(())
}

fn classify_folder_kind(flags: c_uint) -> FolderKind {
    match flags & CAMEL_FOLDER_TYPE_MASK {
        CAMEL_FOLDER_TYPE_INBOX => {
            FolderKind::Inbox
        }
        CAMEL_FOLDER_TYPE_OUTBOX => {
            FolderKind::Outbox
        }
        CAMEL_FOLDER_TYPE_DRAFTS => {
            FolderKind::Drafts
        }
        CAMEL_FOLDER_TYPE_SENT => {
            FolderKind::Sent
        }
        CAMEL_FOLDER_TYPE_ARCHIVE => {
            FolderKind::Archive
        }
        CAMEL_FOLDER_TYPE_TRASH => {
            FolderKind::Trash
        }
        CAMEL_FOLDER_TYPE_JUNK => {
            FolderKind::Spam
        }
        _ => {
            FolderKind::Custom
        }
    }
}

fn compare_folders(left: &MailFolder, right: &MailFolder) -> Ordering {
    folder_rank(left.kind)
        .cmp(&folder_rank(right.kind))
        .then_with(|| left.name.cmp(&right.name))
}

fn folder_rank(kind: FolderKind) -> usize {
    match kind {
        FolderKind::Inbox => {
            0
        }
        FolderKind::Drafts => {
            1
        }
        FolderKind::Outbox => {
            2
        }
        FolderKind::Sent => {
            3
        }
        FolderKind::Archive => {
            4
        }
        FolderKind::Trash => {
            5
        }
        FolderKind::Spam => {
            6
        }
        FolderKind::Custom => {
            7
        }
    }
}
