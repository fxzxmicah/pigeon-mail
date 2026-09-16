use std::ffi::CStr;
use std::os::raw::c_char;

pub(crate) mod account;
pub mod backend;
pub mod camel;
mod identity;
mod local;
pub mod registry;
pub mod settings;
pub(crate) mod stub;
pub mod webkit;

pub(super) struct OwnedGObject<T>(*mut T);

impl<T> OwnedGObject<T> {
    pub(super) unsafe fn from_ptr(pointer: *mut T) -> Option<Self> {
        (!pointer.is_null()).then_some(Self(pointer))
    }

    pub(super) fn as_ptr(&self) -> *mut T {
        self.0
    }

    pub(super) fn into_raw(self) -> *mut T {
        let pointer = self.0;
        std::mem::forget(self);
        pointer
    }
}

impl<T> Drop for OwnedGObject<T> {
    fn drop(&mut self) {
        unsafe { glib::gobject_ffi::g_object_unref(self.0 as *mut _) };
    }
}

pub(super) struct OwnedGlibString(*mut c_char);

impl OwnedGlibString {
    pub(super) unsafe fn from_ptr(pointer: *mut c_char) -> Option<Self> {
        (!pointer.is_null()).then_some(Self(pointer))
    }

    pub(super) fn as_ptr(&self) -> *const c_char {
        self.0
    }

    pub(super) fn to_string_lossy(&self) -> String {
        unsafe { CStr::from_ptr(self.0) }
            .to_string_lossy()
            .into_owned()
    }
}

impl Drop for OwnedGlibString {
    fn drop(&mut self) {
        unsafe { glib::ffi::g_free(self.0 as glib::ffi::gpointer) };
    }
}

fn take_gerror(error: *mut glib::ffi::GError) -> Option<anyhow::Error> {
    if error.is_null() {
        return None;
    }

    let error: glib::Error = unsafe { glib::translate::from_glib_full(error) };
    Some(anyhow::Error::new(error))
}

#[cfg(test)]
mod tests {
    use glib::translate::ToGlibPtr;

    use super::take_gerror;
    use crate::model::event::RefreshFailureKind;

    #[test]
    fn native_error_transfer_preserves_the_typed_cause_through_context() {
        assert!(take_gerror(std::ptr::null_mut()).is_none());
        let native_error = glib::Error::new(gio::IOErrorEnum::NetworkUnreachable, "opaque");
        let error_ptr = unsafe { glib::ffi::g_error_copy(native_error.to_glib_none().0) };
        let error = take_gerror(error_ptr)
            .expect("a native error is present")
            .context("folder operation failed");

        assert_eq!(
            error
                .downcast_ref::<glib::Error>()
                .unwrap()
                .kind::<gio::IOErrorEnum>(),
            Some(gio::IOErrorEnum::NetworkUnreachable)
        );
        assert_eq!(
            crate::failure::classify_failure(&error),
            RefreshFailureKind::Connectivity
        );
    }
}
