use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::ptr;

use anyhow::{Result, anyhow};
use percent_encoding::percent_decode_str;

use crate::model::mail::{
    AttachmentInfo, AttachmentLocation, ConversationId, FolderId, MessageBody, MessageDetail,
    MessageId, StoredMessageRef,
};

use super::{
    CAMEL_MESSAGE_DELETED, CAMEL_MESSAGE_FLAGGED, CAMEL_MESSAGE_SEEN, CamelFolder, CamelMessageInfo,
    CamelService, CamelStore, OwnedGObject, OwnedGlibString, camel_message_info_get_flags,
    camel_message_info_get_from, camel_message_info_get_subject, compose_conversation_id,
    cstr_to_string, get_folder, get_message_info, message_timestamp, take_gerror,
    with_message_info_properties,
};

pub(crate) struct AppendMessageRequest<'a> {
    pub message_id: Option<&'a str>,
    pub folder_id: &'a FolderId,
    pub from: &'a str,
    pub reply_to: Option<&'a str>,
    pub to: &'a [String],
    pub cc: &'a [String],
    pub bcc: &'a [String],
    pub subject: &'a str,
    pub html_body: &'a str,
    pub plain_body: &'a str,
    pub attachment_uris: &'a [String],
    pub is_draft: bool,
}

pub(super) enum CamelMimeMessage {}
enum CamelInternetAddress {}

const CAMEL_RECIPIENT_TYPE_TO: &[u8] = b"To\0";
const CAMEL_RECIPIENT_TYPE_CC: &[u8] = b"Cc\0";
const CAMEL_RECIPIENT_TYPE_BCC: &[u8] = b"Bcc\0";

#[link(name = "camel-1.2")]
unsafe extern "C" {
    fn camel_folder_get_message_cached(
        folder: *mut CamelFolder,
        message_uid: *const c_char,
        cancellable: *mut gio::ffi::GCancellable,
    ) -> *mut CamelMimeMessage;
    fn camel_folder_get_message_sync(
        folder: *mut CamelFolder,
        message_uid: *const c_char,
        cancellable: *mut gio::ffi::GCancellable,
        error: *mut *mut glib::ffi::GError,
    ) -> *mut CamelMimeMessage;
    fn camel_service_get_user_cache_dir(service: *mut CamelService) -> *const c_char;
    fn camel_mime_message_get_subject(message: *mut CamelMimeMessage) -> *const c_char;
    fn camel_mime_message_get_message_id(message: *mut CamelMimeMessage) -> *const c_char;
    fn camel_mime_message_get_from(message: *mut CamelMimeMessage) -> *mut CamelInternetAddress;
    fn camel_mime_message_get_reply_to(message: *mut CamelMimeMessage)
    -> *mut CamelInternetAddress;
    fn camel_mime_message_get_recipients(
        message: *mut CamelMimeMessage,
        recipient_type: *const c_char,
    ) -> *mut CamelInternetAddress;
    fn camel_mime_message_get_date(message: *mut CamelMimeMessage, offset: *mut c_int) -> i64;
    fn camel_mime_message_get_date_received(
        message: *mut CamelMimeMessage,
        offset: *mut c_int,
    ) -> i64;
    fn camel_internet_address_get(
        addr: *mut CamelInternetAddress,
        index: c_int,
        namep: *mut *const c_char,
        addressp: *mut *const c_char,
    ) -> glib::ffi::gboolean;
    fn camel_internet_address_new() -> *mut CamelInternetAddress;
    fn camel_address_decode(address: *mut c_void, raw: *const c_char) -> c_int;
    fn mail_bridge_eds_extract_message_bodies(
        message: *mut CamelMimeMessage,
        out_html: *mut *mut c_char,
        out_plain: *mut *mut c_char,
    ) -> glib::ffi::gboolean;
    fn mail_bridge_eds_extract_message_attachments(message: *mut CamelMimeMessage) -> *mut c_char;
    fn mail_bridge_eds_extract_attachment_to_file(
        message: *mut CamelMimeMessage,
        cache_root: *const c_char,
        cache_key: *const c_char,
        attachment_token: *const c_char,
        error: *mut *mut glib::ffi::GError,
    ) -> *mut c_char;
    fn mail_bridge_eds_append_text_message(
        folder: *mut CamelFolder,
        source_uid: *const c_char,
        message_id: *const c_char,
        from: *const c_char,
        reply_to: *const c_char,
        to_serialized: *const c_char,
        cc_serialized: *const c_char,
        bcc_serialized: *const c_char,
        subject: *const c_char,
        html_body: *const c_char,
        plain_body: *const c_char,
        attachment_uris_serialized: *const c_char,
        is_draft: glib::ffi::gboolean,
        out_appended_uid: *mut *mut c_char,
        out_message_id: *mut *mut c_char,
        error: *mut *mut glib::ffi::GError,
    ) -> glib::ffi::gboolean;
    fn mail_bridge_eds_append_cached_message(
        source_folder: *mut CamelFolder,
        message_uid: *const c_char,
        destination_folder: *mut CamelFolder,
        is_draft: glib::ffi::gboolean,
        out_appended_uid: *mut *mut c_char,
        error: *mut *mut glib::ffi::GError,
    ) -> glib::ffi::gboolean;
}

pub(super) unsafe fn load_mime_message(
    folder: *mut CamelFolder,
    uid: &CStr,
    allow_message_load: bool,
) -> Result<Option<OwnedGObject<CamelMimeMessage>>> {
    if !allow_message_load {
        return Ok(unsafe { get_cached_message(folder, uid) });
    }
    let mut error = ptr::null_mut();
    let message = unsafe {
        camel_folder_get_message_sync(folder, uid.as_ptr(), ptr::null_mut(), &mut error)
    };
    unsafe { OwnedGObject::from_ptr(message) }
        .map(Some)
        .ok_or_else(|| {
            take_gerror(error)
                .unwrap_or_else(|| anyhow!("camel_folder_get_message_sync returned NULL"))
        })
}

unsafe fn get_cached_message(
    folder: *mut CamelFolder,
    uid: &CStr,
) -> Option<OwnedGObject<CamelMimeMessage>> {
    unsafe {
        OwnedGObject::from_ptr(camel_folder_get_message_cached(
            folder,
            uid.as_ptr(),
            ptr::null_mut(),
        ))
    }
}

pub(super) unsafe fn load_message_detail(
    store: *mut CamelStore,
    folder_name: &str,
    uid: &str,
    conversation_id: &ConversationId,
    allow_message_load: bool,
) -> Result<Option<MessageDetail>> {
    let uid_c = CString::new(uid).map_err(|_| anyhow!("message uid contains interior NUL"))?;
    let folder = unsafe { get_folder(store, folder_name) }?;

    unsafe {
        let message_info = get_message_info(folder.as_ptr(), uid)?;
        if camel_message_info_get_flags(message_info.as_ptr()) & CAMEL_MESSAGE_DELETED != 0 {
            return Err(anyhow!("Camel message is marked deleted"));
        }
        let Some(message) = load_mime_message(folder.as_ptr(), &uid_c, allow_message_load)? else {
            return Ok(None);
        };
        build_message_detail(
            uid,
            conversation_id,
            message_info.as_ptr(),
            message.as_ptr(),
        )
        .map(Some)
    }
}

unsafe fn build_message_detail(
    uid: &str,
    conversation_id: &ConversationId,
    message_info: *mut CamelMessageInfo,
    message: *mut CamelMimeMessage,
) -> Result<MessageDetail> {
    debug_assert!(!message.is_null());
    debug_assert!(!message_info.is_null());
    let (info_flags, info_subject, info_from) = unsafe {
        with_message_info_properties(message_info, || {
            (
                camel_message_info_get_flags(message_info),
                cstr_to_string(camel_message_info_get_subject(message_info)),
                cstr_to_string(camel_message_info_get_from(message_info)),
            )
        })
    };
    let subject = cstr_to_string(unsafe { camel_mime_message_get_subject(message) })
        .filter(|value| !value.trim().is_empty())
        .or(info_subject)
        .unwrap_or_default();
    let from = unsafe { first_address_string(camel_mime_message_get_from(message)) }
        .or(info_from)
        .unwrap_or_default();
    let reply_to = unsafe { first_address_string(camel_mime_message_get_reply_to(message)) };
    let to = unsafe {
        address_list_strings(camel_mime_message_get_recipients(
            message,
            CAMEL_RECIPIENT_TYPE_TO.as_ptr().cast(),
        ))
    };
    let cc = unsafe {
        address_list_strings(camel_mime_message_get_recipients(
            message,
            CAMEL_RECIPIENT_TYPE_CC.as_ptr().cast(),
        ))
    };
    let bcc = unsafe {
        address_list_strings(camel_mime_message_get_recipients(
            message,
            CAMEL_RECIPIENT_TYPE_BCC.as_ptr().cast(),
        ))
    };
    let message_id = cstr_to_string(unsafe { camel_mime_message_get_message_id(message) })
        .unwrap_or_else(|| uid.to_string());
    let (body_html, body_text) = unsafe { extract_message_bodies(message) };
    let body_html = body_html.unwrap_or_default();
    let body_text = body_text.unwrap_or_default();
    let attachments = unsafe { extract_message_attachments(message) }?;
    let mut offset = 0;
    let date_sent = unsafe { camel_mime_message_get_date(message, &mut offset) };
    let date_received = unsafe { camel_mime_message_get_date_received(message, &mut offset) };
    let date_secs = message_timestamp(date_sent, date_received);

    tracing::debug!(
        target: "pigeon::eds",
        body_html_len = body_html.len(),
        body_text_len = body_text.len(),
        has_attachment = !attachments.is_empty(),
        "loaded message detail"
    );
    development_probe_log!(
        "message detail: conversation={} body_html_len={} body_text_len={} attachments={}",
        conversation_id.0,
        body_html.len(),
        body_text.len(),
        attachments.len()
    );

    Ok(MessageDetail {
        message_id: MessageId(message_id),
        conversation_id: conversation_id.clone(),
        subject,
        from,
        to,
        cc,
        bcc,
        reply_to,
        date_unix_secs: date_secs,
        starred: (info_flags & CAMEL_MESSAGE_FLAGGED) != 0,
        unread: (info_flags & CAMEL_MESSAGE_SEEN) == 0,
        attachments,
        body: MessageBody::from_parts(body_html, body_text),
    })
}

pub(super) unsafe fn append_cached_message(
    source_store: *mut CamelStore,
    source_folder_name: &str,
    message_uid: &str,
    destination_store: *mut CamelStore,
    destination_folder_id: &FolderId,
    is_draft: bool,
) -> Result<()> {
    let source_folder = unsafe { get_folder(source_store, source_folder_name) }?;
    let destination_folder =
        unsafe { get_folder(destination_store, &destination_folder_id.0) }?;
    unsafe {
        let message_uid =
            CString::new(message_uid).map_err(|_| anyhow!("message uid contains interior NUL"))?;
        let mut error = ptr::null_mut();
        let success = mail_bridge_eds_append_cached_message(
            source_folder.as_ptr(),
            message_uid.as_ptr(),
            destination_folder.as_ptr(),
            if is_draft { 1 } else { 0 },
            ptr::null_mut(),
            &mut error,
        );
        if success == 0 {
            return Err(take_gerror(error).unwrap_or_else(|| {
                anyhow!("mail_bridge_eds_append_cached_message returned FALSE")
            }));
        }
        Ok(())
    }
}

pub(super) unsafe fn export_attachment(
    store: *mut CamelStore,
    service: *mut CamelService,
    folder_name: &str,
    uid: &str,
    conversation_id: &ConversationId,
    attachment_uri: &str,
    allow_message_load: bool,
) -> Result<Option<String>> {
    let uid_c = CString::new(uid).map_err(|_| anyhow!("message uid contains interior NUL"))?;
    let cache_root = unsafe { service_user_cache_dir(service) }
        .ok_or_else(|| anyhow!("camel_service_get_user_cache_dir returned NULL"))?;
    let cache_root =
        CString::new(cache_root).map_err(|_| anyhow!("cache root contains interior NUL"))?;
    let cache_key = CString::new(conversation_id.0.as_str())
        .map_err(|_| anyhow!("conversation id contains interior NUL"))?;
    let attachment_uri = CString::new(attachment_uri)
        .map_err(|_| anyhow!("attachment uri contains interior NUL"))?;
    let folder = unsafe { get_folder(store, folder_name) }?;

    unsafe {
        let Some(message) = load_mime_message(folder.as_ptr(), &uid_c, allow_message_load)? else {
            return Ok(None);
        };

        let mut error = ptr::null_mut();
        let exported_uri = OwnedGlibString::from_ptr(
            mail_bridge_eds_extract_attachment_to_file(
                message.as_ptr(),
                cache_root.as_ptr(),
                cache_key.as_ptr(),
                attachment_uri.as_ptr(),
                &mut error,
            ),
        )
        .ok_or_else(|| {
            take_gerror(error).unwrap_or_else(|| {
                anyhow!("mail_bridge_eds_extract_attachment_to_file returned NULL")
            })
        })?;
        let uri = exported_uri.to_string_lossy();
        let uri = (!uri.is_empty()).then_some(uri).ok_or_else(|| {
            anyhow!("mail_bridge_eds_extract_attachment_to_file returned an empty URI")
        })?;
        Ok(Some(uri))
    }
}

pub(super) unsafe fn append_message_to_folder(
    store: *mut CamelStore,
    source_uid: &str,
    request: &AppendMessageRequest<'_>,
) -> Result<StoredMessageRef> {
    let folder = unsafe { get_folder(store, &request.folder_id.0) }?;

    unsafe {
        let source_uid_c =
            CString::new(source_uid).map_err(|_| anyhow!("source uid contains interior NUL"))?;
        let message_id = request
            .message_id
            .map(CString::new)
            .transpose()
            .map_err(|_| anyhow!("message id contains interior NUL"))?;
        let from = CString::new(request.from).map_err(|_| anyhow!("from contains interior NUL"))?;
        let reply_to = request
            .reply_to
            .map(CString::new)
            .transpose()
            .map_err(|_| anyhow!("reply-to contains interior NUL"))?;
        let to_serialized = serialize_lines(request.to, "recipient")?;
        let cc_serialized = serialize_lines(request.cc, "recipient")?;
        let bcc_serialized = serialize_lines(request.bcc, "recipient")?;
        let subject =
            CString::new(request.subject).map_err(|_| anyhow!("subject contains interior NUL"))?;
        let html_body = CString::new(request.html_body)
            .map_err(|_| anyhow!("html body contains interior NUL"))?;
        let plain_body = CString::new(request.plain_body)
            .map_err(|_| anyhow!("plain body contains interior NUL"))?;
        let attachment_uris = serialize_lines(request.attachment_uris, "attachment URI")?;
        let attachment_count = request.attachment_uris.len();
        anyhow::ensure!(
            request.attachment_uris.iter().all(|uri| !uri.trim().is_empty()),
            "attachment URI is empty"
        );
        let mut appended_uid = ptr::null_mut();
        let mut stored_message_id = ptr::null_mut();
        let mut error = ptr::null_mut();

        let ok = mail_bridge_eds_append_text_message(
            folder.as_ptr(),
            source_uid_c.as_ptr(),
            message_id
                .as_ref()
                .map_or(ptr::null(), |value| value.as_ptr()),
            from.as_ptr(),
            reply_to
                .as_ref()
                .map_or(ptr::null(), |value| value.as_ptr()),
            to_serialized.as_ptr(),
            cc_serialized.as_ptr(),
            bcc_serialized.as_ptr(),
            subject.as_ptr(),
            html_body.as_ptr(),
            plain_body.as_ptr(),
            attachment_uris.as_ptr(),
            if request.is_draft { 1 } else { 0 },
            &mut appended_uid,
            &mut stored_message_id,
            &mut error,
        );
        let appended_uid = OwnedGlibString::from_ptr(appended_uid);
        let stored_message_id = OwnedGlibString::from_ptr(stored_message_id);
        if ok == 0 {
            return Err(take_gerror(error)
                .unwrap_or_else(|| anyhow!("mail_bridge_eds_append_text_message failed")));
        }

        let appended_uid = appended_uid
            .as_ref()
            .map_or_else(String::new, OwnedGlibString::to_string_lossy);
        let message_id = stored_message_id
            .as_ref()
            .map_or_else(String::new, OwnedGlibString::to_string_lossy);

        anyhow::ensure!(!appended_uid.is_empty(), "Camel local append returned no message UID");
        anyhow::ensure!(!message_id.is_empty(), "Camel local append returned no Message-ID");

        let conversation_id = ConversationId(compose_conversation_id(
            &request.folder_id.0,
            &appended_uid,
        ));
        Ok(StoredMessageRef {
            conversation_id,
            message_id: MessageId(message_id),
            // The native writer emits one attachment part per URI in input order.
            attachment_tokens: (1..=attachment_count).map(|index| index.to_string()).collect(),
        })
    }
}

fn serialize_lines(values: &[String], field: &str) -> Result<CString> {
    let mut serialized = Vec::with_capacity(values.len());
    for value in values {
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        if value.contains('\r') || value.contains('\n') {
            return Err(anyhow!("{field} contains a line break"));
        }
        serialized.push(value);
    }
    CString::new(serialized.join("\n")).map_err(|_| anyhow!("{field} contains interior NUL"))
}

/// Uses the same MIME address decoder as ESourceMailIdentity's alias-set API.
pub(in crate::integration) fn decode_addresses(raw: &str) -> Result<Vec<(String, Option<String>)>> {
    let raw = CString::new(raw).map_err(|_| anyhow!("address list contains interior NUL"))?;
    let addresses = unsafe { camel_internet_address_new() };
    let owner: glib::Object = unsafe {
        glib::translate::from_glib_full(addresses as *mut glib::gobject_ffi::GObject)
    };
    let count = unsafe { camel_address_decode(addresses.cast(), raw.as_ptr()) };
    let mut result = Vec::new();
    for index in 0..count {
        let mut name = ptr::null();
        let mut address = ptr::null();
        if unsafe { camel_internet_address_get(addresses, index, &mut name, &mut address) } == 0 {
            continue;
        }
        if let Some(address) = cstr_to_string(address).filter(|address| !address.is_empty()) {
            result.push((
                address,
                cstr_to_string(name).filter(|name| !name.trim().is_empty()),
            ));
        }
    }
    drop(owner);
    Ok(result)
}

unsafe fn first_address_string(addresses: *mut CamelInternetAddress) -> Option<String> {
    let values = unsafe { address_list_strings(addresses) };
    values.into_iter().next()
}

unsafe fn address_list_strings(addresses: *mut CamelInternetAddress) -> Vec<String> {
    if addresses.is_null() {
        return Vec::new();
    }

    let mut result = Vec::new();
    let mut index = 0;
    loop {
        let mut name_ptr: *const c_char = ptr::null();
        let mut email_ptr: *const c_char = ptr::null();
        let ok =
            unsafe { camel_internet_address_get(addresses, index, &mut name_ptr, &mut email_ptr) };
        if ok == 0 {
            break;
        }

        let name = cstr_to_string(name_ptr).unwrap_or_default();
        let email = cstr_to_string(email_ptr).unwrap_or_default();
        let value = match (name.trim(), email.trim()) {
            ("", "") => {
                None
            }
            ("", email) => {
                Some(email.to_string())
            }
            (name, "") => {
                Some(name.to_string())
            }
            (name, email) => {
                Some(format!("{name} <{email}>"))
            }
        };
        if let Some(value) = value {
            result.push(value);
        }

        index += 1;
    }

    result
}

unsafe fn extract_message_bodies(
    message: *mut CamelMimeMessage,
) -> (Option<String>, Option<String>) {
    let mut html_ptr: *mut c_char = ptr::null_mut();
    let mut plain_ptr: *mut c_char = ptr::null_mut();

    let ok =
        unsafe { mail_bridge_eds_extract_message_bodies(message, &mut html_ptr, &mut plain_ptr) };
    let html = unsafe { OwnedGlibString::from_ptr(html_ptr) };
    let plain = unsafe { OwnedGlibString::from_ptr(plain_ptr) };
    if ok == 0 {
        return (None, None);
    }

    (
        html.as_ref().map(OwnedGlibString::to_string_lossy),
        plain.as_ref().map(OwnedGlibString::to_string_lossy),
    )
}

unsafe fn extract_message_attachments(
    message: *mut CamelMimeMessage,
) -> Result<Vec<AttachmentInfo>> {
    let Some(serialized) = (unsafe {
        OwnedGlibString::from_ptr(mail_bridge_eds_extract_message_attachments(message))
    }) else {
        return Ok(Vec::new());
    };
    let serialized = unsafe { CStr::from_ptr(serialized.as_ptr()) }
        .to_str()
        .map_err(|_| anyhow!("attachment records are not UTF-8"))?;

    parse_attachment_records(serialized)
}

fn parse_attachment_records(serialized: &str) -> Result<Vec<AttachmentInfo>> {
    if serialized.is_empty() {
        return Err(anyhow!("attachment records are empty"));
    }
    serialized
        .lines()
        .map(|line| {
            let (display_name, token) = line
                .split_once('\t')
                .ok_or_else(|| anyhow!("attachment record has no token separator"))?;
            let display_name = percent_decode_str(display_name)
                .decode_utf8()
                .map_err(|_| anyhow!("attachment display name is not UTF-8"))?;
            let index = token
                .parse::<u32>()
                .map_err(|_| anyhow!("attachment record has an invalid token"))?;
            if display_name.is_empty() || index == 0 {
                return Err(anyhow!("attachment record is incomplete"));
            }
            Ok(AttachmentInfo {
                display_name: display_name.into_owned(),
                location: AttachmentLocation::CachedToken(token.to_string()),
            })
        })
        .collect()
}

unsafe fn service_user_cache_dir(service: *mut CamelService) -> Option<String> {
    cstr_to_string(unsafe { camel_service_get_user_cache_dir(service) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gio::prelude::*;
    use std::path::PathBuf;

    enum CamelStream {}
    enum CamelDataWrapper {}

    unsafe extern "C" {
        fn camel_mime_message_new() -> *mut CamelMimeMessage;
        fn camel_stream_mem_new_with_buffer(buffer: *const c_char, len: usize) -> *mut CamelStream;
        fn camel_data_wrapper_construct_from_stream_sync(
            wrapper: *mut CamelDataWrapper,
            stream: *mut CamelStream,
            cancellable: *mut gio::ffi::GCancellable,
            error: *mut *mut glib::ffi::GError,
        ) -> glib::ffi::gboolean;
    }

    fn parse_mime(raw: &str) -> OwnedGObject<CamelMimeMessage> {
        unsafe {
            let message = OwnedGObject::from_ptr(camel_mime_message_new()).unwrap();
            let stream = OwnedGObject::from_ptr(camel_stream_mem_new_with_buffer(
                raw.as_ptr().cast(),
                raw.len(),
            ))
            .unwrap();
            let mut error = ptr::null_mut();
            let parsed = camel_data_wrapper_construct_from_stream_sync(
                message.as_ptr().cast(),
                stream.as_ptr(),
                ptr::null_mut(),
                &mut error,
            );
            assert_ne!(parsed, 0, "Camel MIME parse failed: {:?}", take_gerror(error));
            message
        }
    }

    #[test]
    fn nested_mime_keeps_attachments_out_of_the_body_and_exports_the_selected_part() {
        let message = parse_mime(concat!(
            "From: sender@example.com\r\n",
            "To: reader@example.net\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: multipart/mixed; boundary=mixed\r\n\r\n",
            "--mixed\r\nContent-Type: text/plain\r\n",
            "Content-Disposition: attachment; filename=report.txt\r\n",
            "Content-Transfer-Encoding: base64\r\n\r\nZmlyc3Q=\r\n",
            "--mixed\r\nContent-Type: multipart/alternative; boundary=body\r\n\r\n",
            "--body\r\nContent-Type: text/plain; charset=iso-8859-1\r\n",
            "Content-Transfer-Encoding: quoted-printable\r\n\r\nCaf=E9\r\n",
            "--body\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>HTML body</p>\r\n",
            "--body--\r\n",
            "--mixed\r\nContent-Type: text/plain\r\n",
            "Content-Disposition: attachment; filename=report.txt\r\n",
            "Content-Transfer-Encoding: base64\r\n\r\nc2Vjb25k\r\n",
            "--mixed--\r\n",
        ));
        let (html, text) = unsafe { extract_message_bodies(message.as_ptr()) };
        assert_eq!(text.as_deref().map(str::trim), Some("Café"));
        assert_eq!(html.as_deref().map(str::trim), Some("<p>HTML body</p>"));

        let attachments = unsafe { extract_message_attachments(message.as_ptr()) }.unwrap();
        assert_eq!(attachments.len(), 2);
        assert!(attachments.iter().all(|part| part.display_name == "report.txt"));

        struct TempCache(PathBuf);
        impl Drop for TempCache {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let mut error = ptr::null_mut();
        let path = unsafe {
            OwnedGlibString::from_ptr(glib::ffi::g_dir_make_tmp(
                c"pigeon-mime-test-XXXXXX".as_ptr(),
                &mut error,
            ))
        }
        .unwrap_or_else(|| panic!("cannot create temporary cache: {:?}", take_gerror(error)));
        let root = TempCache(PathBuf::from(path.to_string_lossy()));
        let export = |part: &AttachmentInfo| {
            let token = CString::new(part.location.cached_token().unwrap()).unwrap();
            let mut error = ptr::null_mut();
            let uri = unsafe {
                OwnedGlibString::from_ptr(mail_bridge_eds_extract_attachment_to_file(
                    message.as_ptr(),
                    path.as_ptr(),
                    c"test-message".as_ptr(),
                    token.as_ptr(),
                    &mut error,
                ))
            }
            .unwrap_or_else(|| panic!("attachment export failed: {:?}", take_gerror(error)));
            let file = gio::File::for_uri(&uri.to_string_lossy()).path().unwrap();
            assert!(file.starts_with(&root.0));
            file
        };

        let second = export(&attachments[1]);
        let first = export(&attachments[0]);
        assert_ne!(first, second);
        assert_eq!(std::fs::read(first).unwrap().as_slice(), b"first");
        assert_eq!(std::fs::read(second).unwrap().as_slice(), b"second");
    }

    #[test]
    fn attachment_uris_preserve_order_and_skip_empty_values() {
        let serialized = serialize_lines(
            &[
                "file:///tmp/first.txt".into(),
                "  ".into(),
                "file:///tmp/second.bin".into(),
            ],
            "attachment URI",
        )
        .unwrap();
        assert_eq!(
            serialized.to_str().unwrap(),
            "file:///tmp/first.txt\nfile:///tmp/second.bin"
        );
    }

    #[test]
    fn attachment_uri_line_breaks_cannot_corrupt_the_wire_format() {
        assert!(serialize_lines(&["file:///tmp/a\nb".into()], "attachment URI").is_err());
        assert!(serialize_lines(&["file:///tmp/a\rb".into()], "attachment URI").is_err());
    }

    #[test]
    fn recipient_line_breaks_cannot_create_extra_address_records() {
        assert!(
            serialize_lines(
                &["person@example.com\nBcc: hidden@example.com".into()],
                "recipient",
            ).is_err()
        );
        assert!(
            serialize_lines(&["person@example.com\rhidden@example.com".into()], "recipient")
                .is_err()
        );
    }

    #[test]
    fn attachment_records_preserve_delimiters_and_duplicate_display_names() {
        let records = parse_attachment_records(
            "same%09name%0A100%25.txt\t1\nsame%09name%0A100%25.txt\t2",
        )
        .unwrap();

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].display_name, "same\tname\n100%.txt");
        assert_eq!(records[1].display_name, records[0].display_name);
        assert_eq!(records[0].location.cached_token(), Some("1"));
        assert_eq!(records[1].location.cached_token(), Some("2"));
    }

    #[test]
    fn malformed_attachment_records_fail_at_the_camel_boundary() {
        for records in ["", "name", "name\t0", "name\tnot-a-token", "%FF\t1"] {
            assert!(parse_attachment_records(records).is_err());
        }
    }
}
