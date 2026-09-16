#include <camel/camel.h>
#include <libedataserver/libedataserver.h>
#include <glib/gstdio.h>
#include <errno.h>
#include <fcntl.h>
#include <string.h>
#include <time.h>

typedef struct {
	gchar *plain;
	gchar *html;
} MessageBodies;

typedef struct {
	GPtrArray *lines;
	guint current_index;
} AttachmentList;

typedef struct {
	guint target_index;
	guint current_index;
	gchar *result_uri;
	gchar *message_dir;
	GError *error;
} AttachmentExport;

typedef void (*FolderChangedFunc) (gpointer user_data);
typedef void (*RegistryChangedFunc) (gpointer user_data);

typedef struct {
	FolderChangedFunc callback;
	gpointer user_data;
	GDestroyNotify destroy;
} FolderChangedHandler;

typedef struct {
	GMainContext *context;
	ESourceRegistry *registry;
	gulong source_added_id;
	gulong source_changed_id;
	gulong source_removed_id;
	gulong source_enabled_id;
	gulong source_disabled_id;
	RegistryChangedFunc callback;
	gpointer user_data;
	GDestroyNotify destroy;
} RegistryWatch;

static gboolean
mail_bridge_source_affects_mail (ESource *source)
{
	return e_source_has_extension (source, E_SOURCE_EXTENSION_GOA) ||
		e_source_has_extension (source, E_SOURCE_EXTENSION_COLLECTION) ||
		e_source_has_extension (source, E_SOURCE_EXTENSION_MAIL_ACCOUNT) ||
		e_source_has_extension (source, E_SOURCE_EXTENSION_MAIL_IDENTITY) ||
		e_source_has_extension (source, E_SOURCE_EXTENSION_MAIL_SIGNATURE) ||
		e_source_has_extension (source, E_SOURCE_EXTENSION_MAIL_COMPOSITION) ||
		e_source_has_extension (source, E_SOURCE_EXTENSION_MAIL_SUBMISSION) ||
		e_source_has_extension (source, E_SOURCE_EXTENSION_MAIL_TRANSPORT);
}

static void
mail_bridge_registry_source_changed_cb (ESourceRegistry *registry,
				   ESource *source,
				   gpointer user_data)
{
	RegistryWatch *watch = user_data;

	(void) registry;
	if (!mail_bridge_source_affects_mail (source))
		return;
	watch->callback (watch->user_data);
}

RegistryWatch *
mail_bridge_eds_registry_watch_new (RegistryChangedFunc callback,
			       gpointer user_data,
			       GDestroyNotify destroy,
			       GError **error)
{
	RegistryWatch *watch;

	g_return_val_if_fail (callback != NULL, NULL);

	watch = g_new0 (RegistryWatch, 1);
	watch->context = g_main_context_new ();
	g_main_context_push_thread_default (watch->context);
	watch->registry = e_source_registry_new_sync (NULL, error);
	if (!watch->registry) {
		g_main_context_pop_thread_default (watch->context);
		g_main_context_unref (watch->context);
		g_free (watch);
		return NULL;
	}
	watch->callback = callback;
	watch->user_data = user_data;
	watch->destroy = destroy;
	watch->source_added_id = g_signal_connect (
		watch->registry, "source-added",
		G_CALLBACK (mail_bridge_registry_source_changed_cb), watch);
	watch->source_changed_id = g_signal_connect (
		watch->registry, "source-changed",
		G_CALLBACK (mail_bridge_registry_source_changed_cb), watch);
	watch->source_removed_id = g_signal_connect (
		watch->registry, "source-removed",
		G_CALLBACK (mail_bridge_registry_source_changed_cb), watch);
	watch->source_enabled_id = g_signal_connect (
		watch->registry, "source-enabled",
		G_CALLBACK (mail_bridge_registry_source_changed_cb), watch);
	watch->source_disabled_id = g_signal_connect (
		watch->registry, "source-disabled",
		G_CALLBACK (mail_bridge_registry_source_changed_cb), watch);

	return watch;
}

void
mail_bridge_eds_registry_watch_iteration (RegistryWatch *watch)
{
	if (watch)
		g_main_context_iteration (watch->context, FALSE);
}

void
mail_bridge_eds_registry_watch_free (RegistryWatch *watch)
{
	if (!watch)
		return;
	if (watch->source_added_id)
		g_signal_handler_disconnect (watch->registry, watch->source_added_id);
	if (watch->source_changed_id)
		g_signal_handler_disconnect (watch->registry, watch->source_changed_id);
	if (watch->source_removed_id)
		g_signal_handler_disconnect (watch->registry, watch->source_removed_id);
	if (watch->source_enabled_id)
		g_signal_handler_disconnect (watch->registry, watch->source_enabled_id);
	if (watch->source_disabled_id)
		g_signal_handler_disconnect (watch->registry, watch->source_disabled_id);
	if (watch->destroy)
		watch->destroy (watch->user_data);
	g_clear_object (&watch->registry);
	g_main_context_pop_thread_default (watch->context);
	g_main_context_unref (watch->context);
	g_free (watch);
}

static void
mail_bridge_folder_changed_handler_free (gpointer data,
				    GClosure *closure)
{
	FolderChangedHandler *handler = data;

	(void) closure;
	if (handler->destroy)
		handler->destroy (handler->user_data);
	g_free (handler);
}

static void
mail_bridge_folder_changed_cb (CamelFolder *folder,
			  CamelFolderChangeInfo *changes,
			  gpointer user_data)
{
	FolderChangedHandler *handler = user_data;

	(void) folder;
	(void) changes;
	handler->callback (handler->user_data);
}

gulong
mail_bridge_camel_folder_watch_changes (CamelFolder *folder,
				   FolderChangedFunc callback,
				   gpointer user_data,
				   GDestroyNotify destroy)
{
	FolderChangedHandler *handler;

	g_return_val_if_fail (CAMEL_IS_FOLDER (folder), 0);
	g_return_val_if_fail (callback != NULL, 0);

	handler = g_new0 (FolderChangedHandler, 1);
	handler->callback = callback;
	handler->user_data = user_data;
	handler->destroy = destroy;

	return g_signal_connect_data (
		folder,
		"changed",
		G_CALLBACK (mail_bridge_folder_changed_cb),
		handler,
		mail_bridge_folder_changed_handler_free,
		0);
}

void
mail_bridge_camel_folder_unwatch_changes (CamelFolder *folder,
				     gulong handler_id)
{
	g_return_if_fail (CAMEL_IS_FOLDER (folder));
	if (handler_id != 0 && g_signal_handler_is_connected (folder, handler_id))
		g_signal_handler_disconnect (folder, handler_id);
}

typedef struct _EdsCamelSession {
	CamelSession parent;
	ESourceRegistry *registry;
	ESourceCredentialsProvider *credentials_provider;
} EdsCamelSession;

typedef struct _EdsCamelSessionClass {
	CamelSessionClass parent_class;
} EdsCamelSessionClass;

G_DEFINE_TYPE (EdsCamelSession, mail_bridge_eds_session, CAMEL_TYPE_SESSION)

static gchar *
mail_bridge_extract_part_text_utf8 (CamelMimePart *part)
{
	CamelContentType *content_type;
	CamelDataWrapper *content;
	CamelStream *memory_stream;
	CamelStream *decode_stream;
	CamelMimeFilter *charset_filter = NULL;
	GByteArray *buffer;
	const gchar *charset;
	gchar *text = NULL;

	g_return_val_if_fail (CAMEL_IS_MIME_PART (part), NULL);

	content_type = camel_mime_part_get_content_type (part);
	content = camel_medium_get_content (CAMEL_MEDIUM (part));
	if (!content_type || !content)
		return NULL;

	memory_stream = camel_stream_mem_new ();
	decode_stream = g_object_ref (memory_stream);

	charset = camel_content_type_param (content_type, "charset");
	if (charset && *charset && g_ascii_strcasecmp (charset, "utf-8") != 0) {
		CamelStream *filtered_stream;

		charset_filter = camel_mime_filter_charset_new (charset, "UTF-8");
		if (charset_filter) {
			filtered_stream = camel_stream_filter_new (memory_stream);
			camel_stream_filter_add (CAMEL_STREAM_FILTER (filtered_stream), charset_filter);
			g_clear_object (&decode_stream);
			decode_stream = filtered_stream;
		}
	}

	if (camel_data_wrapper_decode_to_stream_sync (content, decode_stream, NULL, NULL) >= 0) {
		camel_stream_flush (decode_stream, NULL, NULL);
		buffer = camel_stream_mem_get_byte_array (CAMEL_STREAM_MEM (memory_stream));
		if (buffer && buffer->len > 0)
			text = g_strndup ((const gchar *) buffer->data, buffer->len);
	}

	g_clear_object (&charset_filter);
	g_clear_object (&decode_stream);
	g_clear_object (&memory_stream);

	return text;
}

static gboolean
mail_bridge_collect_message_body_part (CamelMimeMessage *message,
				  CamelMimePart *part,
				  CamelMimePart *parent_part,
				  gpointer user_data)
{
	MessageBodies *bodies = user_data;
	CamelContentType *content_type;
	const CamelContentDisposition *disposition;
	gboolean is_attachment;
	gchar *text;

	(void) message;
	(void) parent_part;

	g_return_val_if_fail (bodies != NULL, FALSE);

	content_type = camel_mime_part_get_content_type (part);
	if (!content_type)
		return TRUE;

	disposition = camel_mime_part_get_content_disposition (part);
	is_attachment = camel_content_disposition_is_attachment (disposition, content_type) ||
		(camel_mime_part_get_filename (part) != NULL);
	if (is_attachment)
		return TRUE;

	if (!bodies->html && camel_content_type_is (content_type, "text", "html")) {
		text = mail_bridge_extract_part_text_utf8 (part);
		if (text && *text)
			bodies->html = text;
		else
			g_free (text);
	}

	if (!bodies->plain && camel_content_type_is (content_type, "text", "plain")) {
		text = mail_bridge_extract_part_text_utf8 (part);
		if (text && *text)
			bodies->plain = text;
		else
			g_free (text);
	}

	return !(bodies->html && bodies->plain);
}

static gboolean
mail_bridge_collect_attachment_part (CamelMimeMessage *message,
				CamelMimePart *part,
				CamelMimePart *parent_part,
				gpointer user_data)
{
	AttachmentList *attachments = user_data;
	CamelContentType *content_type;
	const CamelContentDisposition *disposition;
	const gchar *filename;
	const gchar *content_id;
	const gchar *content_location;
	gboolean is_attachment;
	gchar *display_name = NULL;
	gchar *encoded_display_name = NULL;
	gchar *token = NULL;
	gchar *line;

	(void) message;
	(void) parent_part;

	g_return_val_if_fail (attachments != NULL, TRUE);

	content_type = camel_mime_part_get_content_type (part);
	disposition = camel_mime_part_get_content_disposition (part);
	filename = camel_mime_part_get_filename (part);
	content_id = camel_mime_part_get_content_id (part);
	content_location = camel_mime_part_get_content_location (part);

	is_attachment =
		(content_type && camel_content_disposition_is_attachment (disposition, content_type)) ||
		(filename && *filename);
	if (!is_attachment)
		return TRUE;

	attachments->current_index++;

	if (filename && *filename)
		display_name = g_strdup (filename);
	else if (content_location && *content_location)
		display_name = g_path_get_basename (content_location);
	else if (content_id && *content_id)
		display_name = g_strdup (content_id);
	else
		display_name = g_strdup ("Attachment");

	token = g_strdup_printf ("%u", attachments->current_index);
	encoded_display_name = g_uri_escape_string (display_name, NULL, TRUE);

	line = g_strconcat (encoded_display_name, "\t", token, NULL);
	g_ptr_array_add (attachments->lines, line);

	g_free (display_name);
	g_free (encoded_display_name);
	g_free (token);

	return TRUE;
}

static gchar *
mail_bridge_safe_filename (const gchar *filename)
{
	gchar *safe;

	if (!filename || !*filename)
		return g_strdup ("attachment.bin");

	safe = g_strdup (filename);
	g_strdelimit (safe, "\\/:*?\"<>|\r\n\t", '_');
	if (!*safe || g_str_equal (safe, ".") || g_str_equal (safe, "..")) {
		g_free (safe);
		return g_strdup ("attachment.bin");
	}

	return safe;
}

static gchar *
mail_bridge_build_attachment_cache_dir (const gchar *cache_root,
				   const gchar *cache_key)
{
	gchar *cache_digest;
	gchar *message_dir;

	cache_digest = g_compute_checksum_for_string (
		G_CHECKSUM_SHA256,
		cache_key && *cache_key ? cache_key : "message",
		-1);
	message_dir = g_build_filename (cache_root, "attachments", cache_digest, NULL);
	g_free (cache_digest);

	return message_dir;
}

static gboolean
mail_bridge_export_attachment_part (CamelMimeMessage *message,
			       CamelMimePart *part,
			       CamelMimePart *parent_part,
			       gpointer user_data)
{
	AttachmentExport *export_data = user_data;
	CamelContentType *content_type;
	const CamelContentDisposition *disposition;
	const gchar *filename;
	gboolean is_attachment;

	(void) message;
	(void) parent_part;

	g_return_val_if_fail (export_data != NULL, FALSE);

	content_type = camel_mime_part_get_content_type (part);
	disposition = camel_mime_part_get_content_disposition (part);
	filename = camel_mime_part_get_filename (part);

	is_attachment =
		(content_type && camel_content_disposition_is_attachment (disposition, content_type)) ||
		(filename && *filename);
	if (!is_attachment)
		return TRUE;

	export_data->current_index++;
	if (export_data->current_index != export_data->target_index)
		return TRUE;

	if (camel_medium_get_content (CAMEL_MEDIUM (part))) {
		CamelStream *stream;
		CamelDataWrapper *content;
		gchar *safe_name;
		gchar *part_name;
		gchar *part_dir;
		gchar *path;
		gchar *existing_uri;

		safe_name = mail_bridge_safe_filename (filename);
		part_name = g_strdup_printf ("%u", export_data->target_index);
		part_dir = g_build_filename (export_data->message_dir, part_name, NULL);
		path = g_build_filename (part_dir, safe_name, NULL);
		g_free (safe_name);
		g_free (part_name);

		if (g_file_test (path, G_FILE_TEST_EXISTS)) {
			existing_uri = g_filename_to_uri (path, NULL, &export_data->error);
			if (existing_uri) {
				export_data->result_uri = existing_uri;
				g_free (part_dir);
				g_free (path);
				return FALSE;
			}
		}

		if (g_mkdir_with_parents (part_dir, 0700) != 0) {
			g_set_error (
				&export_data->error,
				G_IO_ERROR,
				g_io_error_from_errno (errno),
				"Could not create attachment cache directory");
			g_free (part_dir);
			g_free (path);
			return FALSE;
		}

		stream = camel_stream_fs_new_with_name (path, O_CREAT | O_TRUNC | O_WRONLY, 0600, &export_data->error);
		if (!stream) {
			g_free (part_dir);
			g_free (path);
			return FALSE;
		}

		content = camel_medium_get_content (CAMEL_MEDIUM (part));
		if (camel_data_wrapper_decode_to_stream_sync (content, stream, NULL, &export_data->error) == -1) {
			camel_stream_close (stream, NULL, NULL);
			g_object_unref (stream);
			g_free (part_dir);
			g_free (path);
			return FALSE;
		}

		camel_stream_flush (stream, NULL, NULL);
		camel_stream_close (stream, NULL, NULL);
		g_object_unref (stream);

		export_data->result_uri = g_filename_to_uri (path, NULL, &export_data->error);
		g_free (part_dir);
		g_free (path);
	}

	return FALSE;
}

static gboolean
mail_bridge_address_add_serialized (CamelInternetAddress *address,
			       const gchar *serialized,
			       GError **error)
{
	gchar **lines;
	guint ii;
	gboolean added = FALSE;

	g_return_val_if_fail (CAMEL_IS_INTERNET_ADDRESS (address), FALSE);

	if (!serialized || !*serialized)
		return TRUE;

	lines = g_strsplit (serialized, "\n", -1);
	for (ii = 0; lines[ii] != NULL; ii++) {
		gint decoded;

		if (!lines[ii][0])
			continue;

		decoded = camel_address_decode (CAMEL_ADDRESS (address), lines[ii]);
		if (decoded <= 0)
			decoded = camel_address_unformat (CAMEL_ADDRESS (address), lines[ii]);
		if (decoded <= 0) {
			g_set_error (
				error,
				G_IO_ERROR,
				G_IO_ERROR_INVALID_ARGUMENT,
				"Could not parse recipient/address '%s'",
				lines[ii]);
			g_strfreev (lines);
			return FALSE;
		}

		added = TRUE;
	}

	g_strfreev (lines);
	return added;
}

static gboolean
mail_bridge_message_set_address_header (CamelMimeMessage *message,
				   const gchar *serialized,
				   void (*setter) (CamelMimeMessage *, CamelInternetAddress *),
				   GError **error)
{
	CamelInternetAddress *address;
	gboolean success;

	g_return_val_if_fail (CAMEL_IS_MIME_MESSAGE (message), FALSE);
	g_return_val_if_fail (setter != NULL, FALSE);

	if (!serialized || !*serialized)
		return TRUE;

	address = camel_internet_address_new ();
	success = mail_bridge_address_add_serialized (address, serialized, error);
	if (success && camel_address_length (CAMEL_ADDRESS (address)) > 0)
		setter (message, address);
	g_object_unref (address);

	return success;
}

static gboolean
mail_bridge_message_set_recipients_header (CamelMimeMessage *message,
				      const gchar *type,
				      const gchar *serialized,
				      GError **error)
{
	CamelInternetAddress *address;
	gboolean success;

	g_return_val_if_fail (CAMEL_IS_MIME_MESSAGE (message), FALSE);
	g_return_val_if_fail (type != NULL, FALSE);

	if (!serialized || !*serialized)
		return TRUE;

	address = camel_internet_address_new ();
	success = mail_bridge_address_add_serialized (address, serialized, error);
	if (success && camel_address_length (CAMEL_ADDRESS (address)) > 0)
		camel_mime_message_set_recipients (message, type, address);
	g_object_unref (address);

	return success;
}

static CamelMimePart *
mail_bridge_message_new_body_part (const gchar *html_body,
			      const gchar *plain_body)
{
	CamelMimePart *part;

	part = camel_mime_part_new ();
	if (html_body && *html_body && plain_body && *plain_body) {
		CamelMultipart *alternative;
		CamelMimePart *alternative_part;

		alternative = camel_multipart_new ();
		camel_data_wrapper_set_mime_type (
			CAMEL_DATA_WRAPPER (alternative), "multipart/alternative");
		camel_multipart_set_boundary (alternative, NULL);

		alternative_part = camel_mime_part_new ();
		camel_mime_part_set_content (
			alternative_part, plain_body, strlen (plain_body), "text/plain; charset=UTF-8");
		camel_multipart_add_part (alternative, alternative_part);
		g_object_unref (alternative_part);

		alternative_part = camel_mime_part_new ();
		camel_mime_part_set_content (
			alternative_part, html_body, strlen (html_body), "text/html; charset=UTF-8");
		camel_multipart_add_part (alternative, alternative_part);
		g_object_unref (alternative_part);

		camel_medium_set_content (CAMEL_MEDIUM (part), CAMEL_DATA_WRAPPER (alternative));
		g_object_unref (alternative);
	} else if (html_body && *html_body) {
		camel_mime_part_set_content (
			part, html_body, strlen (html_body), "text/html; charset=UTF-8");
	} else {
		const gchar *body = plain_body ? plain_body : "";
		camel_mime_part_set_content (
			part, body, strlen (body), "text/plain; charset=UTF-8");
	}

	return part;
}

static gboolean
mail_bridge_message_set_body_and_attachments (CamelMimeMessage *message,
					 const gchar *html_body,
					 const gchar *plain_body,
					 const gchar *attachment_uris_serialized,
					 GError **error)
{
	CamelMimePart *body_part;
	gchar **attachment_uris;
	guint ii;

	g_return_val_if_fail (CAMEL_IS_MIME_MESSAGE (message), FALSE);

	body_part = mail_bridge_message_new_body_part (html_body, plain_body);
	if (!attachment_uris_serialized || !*attachment_uris_serialized) {
		CamelDataWrapper *content = camel_medium_get_content (CAMEL_MEDIUM (body_part));
		camel_medium_set_content (CAMEL_MEDIUM (message), content);
		g_object_unref (body_part);
		return TRUE;
	}

	attachment_uris = g_strsplit (attachment_uris_serialized, "\n", -1);
	{
		CamelMultipart *mixed = camel_multipart_new ();
		camel_data_wrapper_set_mime_type (CAMEL_DATA_WRAPPER (mixed), "multipart/mixed");
		camel_multipart_set_boundary (mixed, NULL);
		camel_multipart_add_part (mixed, body_part);
		g_object_unref (body_part);

		for (ii = 0; attachment_uris[ii]; ii++) {
			GFile *file;
			gchar *contents = NULL;
			gsize length = 0;
			gchar *basename;
			gchar *content_type;
			gchar *mime_type;
			CamelMimePart *attachment;

			if (!*attachment_uris[ii])
				continue;
			file = g_file_new_for_uri (attachment_uris[ii]);
			if (!g_file_load_contents (file, NULL, &contents, &length, NULL, error)) {
				g_object_unref (file);
				g_object_unref (mixed);
				g_strfreev (attachment_uris);
				return FALSE;
			}
			if (length > G_MAXINT) {
				g_set_error_literal (error, G_IO_ERROR, G_IO_ERROR_FAILED,
					"Attachment is too large for Camel MIME encoding");
				g_free (contents);
				g_object_unref (file);
				g_object_unref (mixed);
				g_strfreev (attachment_uris);
				return FALSE;
			}

			basename = g_file_get_basename (file);
			content_type = g_content_type_guess (basename, (const guchar *) contents, length, NULL);
			mime_type = content_type ? g_content_type_get_mime_type (content_type) : NULL;
			attachment = camel_mime_part_new ();
			camel_mime_part_set_content (
				attachment, contents, (gint) length,
				mime_type ? mime_type : "application/octet-stream");
			camel_mime_part_set_disposition (attachment, "attachment");
			camel_mime_part_set_filename (
				attachment, basename && *basename ? basename : "attachment.bin");
			camel_multipart_add_part (mixed, attachment);

			g_object_unref (attachment);
			g_free (mime_type);
			g_free (content_type);
			g_free (basename);
			g_free (contents);
			g_object_unref (file);
		}

		camel_medium_set_content (CAMEL_MEDIUM (message), CAMEL_DATA_WRAPPER (mixed));
		g_object_unref (mixed);
	}
	g_strfreev (attachment_uris);
	return TRUE;
}

static ESource *
mail_bridge_eds_session_ref_source_for_service (EdsCamelSession *session,
					   CamelService *service)
{
	const gchar *uid;

	g_return_val_if_fail (session != NULL, NULL);
	g_return_val_if_fail (CAMEL_IS_SERVICE (service), NULL);

	uid = camel_service_get_uid (service);
	if (!uid || !*uid)
		return NULL;

	return e_source_registry_ref_source (session->registry, uid);
}

static gboolean
mail_bridge_eds_session_authenticate_sync (CamelSession *camel_session,
				      CamelService *service,
				      const gchar *mechanism,
				      GCancellable *cancellable,
				      GError **error)
{
	EdsCamelSession *session;
	ESource *source = NULL;
	ENamedParameters *credentials = NULL;
	const gchar *password = NULL;
	CamelAuthenticationResult result;

	session = (EdsCamelSession *) camel_session;

	source = mail_bridge_eds_session_ref_source_for_service (session, service);
	if (!source) {
		g_set_error (
			error, G_IO_ERROR, G_IO_ERROR_NOT_FOUND,
			"Cannot resolve ESource for Camel service '%s'",
			camel_service_get_uid (service));
		return FALSE;
	}

	if (!e_source_credentials_provider_lookup_sync (
		session->credentials_provider,
		source,
		cancellable,
		&credentials,
		error)) {
		g_clear_object (&source);
		return FALSE;
	}

	if (credentials) {
		password = e_named_parameters_get (
			credentials, E_SOURCE_CREDENTIAL_PASSWORD);

		if (password && *password)
			camel_service_set_password (service, password);
	}

	result = camel_service_authenticate_sync (
		service, mechanism, cancellable, error);

	e_named_parameters_free (credentials);
	g_clear_object (&source);

	return result == CAMEL_AUTHENTICATION_ACCEPTED;
}

static gboolean
mail_bridge_eds_session_get_oauth2_access_token_sync (CamelSession *camel_session,
						 CamelService *service,
						 gchar **out_access_token,
						 gint *out_expires_in,
						 GCancellable *cancellable,
						 GError **error)
{
	EdsCamelSession *session;
	ESource *source = NULL;
	gboolean success;

	session = (EdsCamelSession *) camel_session;

	source = mail_bridge_eds_session_ref_source_for_service (session, service);
	if (!source) {
		g_set_error (
			error, G_IO_ERROR, G_IO_ERROR_NOT_FOUND,
			"Cannot resolve ESource for Camel service '%s'",
			camel_service_get_uid (service));
		return FALSE;
	}

	success = e_source_get_oauth2_access_token_sync (
		source,
		cancellable,
		out_access_token,
		out_expires_in,
		error);

	g_clear_object (&source);

	return success;
}

static CamelFilterDriver *
mail_bridge_eds_session_get_filter_driver (CamelSession *camel_session,
				      const gchar *type,
				      CamelFolder *for_folder,
				      GError **error)
{
	(void) type;
	(void) for_folder;
	(void) error;

	return camel_filter_driver_new (camel_session);
}

static void
mail_bridge_eds_session_dispose (GObject *object)
{
	EdsCamelSession *session = (EdsCamelSession *) object;

	g_clear_object (&session->credentials_provider);
	g_clear_object (&session->registry);

	G_OBJECT_CLASS (mail_bridge_eds_session_parent_class)->dispose (object);
}

static void
mail_bridge_eds_session_class_init (EdsCamelSessionClass *klass)
{
	GObjectClass *object_class;
	CamelSessionClass *session_class;

	object_class = G_OBJECT_CLASS (klass);
	object_class->dispose = mail_bridge_eds_session_dispose;

	session_class = CAMEL_SESSION_CLASS (klass);
	session_class->authenticate_sync = mail_bridge_eds_session_authenticate_sync;
	session_class->get_filter_driver = mail_bridge_eds_session_get_filter_driver;
	session_class->get_oauth2_access_token_sync =
		mail_bridge_eds_session_get_oauth2_access_token_sync;
}

static void
mail_bridge_eds_session_init (EdsCamelSession *session)
{
	(void) session;
	session->registry = NULL;
	session->credentials_provider = NULL;
}

CamelSession *
mail_bridge_eds_session_new (ESourceRegistry *registry)
{
	EdsCamelSession *session;

	g_return_val_if_fail (E_IS_SOURCE_REGISTRY (registry), NULL);

	session = g_object_new (
		mail_bridge_eds_session_get_type (),
		"user-data-dir", e_get_user_data_dir (),
		"user-cache-dir", e_get_user_cache_dir (),
		"online", TRUE,
		NULL);

	session->registry = g_object_ref (registry);
	session->credentials_provider =
		e_source_credentials_provider_new (registry);

	return CAMEL_SESSION (session);
}

gboolean
mail_bridge_eds_extract_message_bodies (CamelMimeMessage *message,
				   gchar **out_html,
				   gchar **out_plain)
{
	MessageBodies bodies = { NULL, NULL };

	g_return_val_if_fail (CAMEL_IS_MIME_MESSAGE (message), FALSE);

	if (out_html)
		*out_html = NULL;
	if (out_plain)
		*out_plain = NULL;

	camel_mime_message_foreach_part (
		message,
		mail_bridge_collect_message_body_part,
		&bodies);

	if (CAMEL_IS_MIME_PART (message)) {
		CamelContentType *content_type = camel_mime_part_get_content_type (CAMEL_MIME_PART (message));

		if (!bodies.html && content_type && camel_content_type_is (content_type, "text", "html"))
			bodies.html = mail_bridge_extract_part_text_utf8 (CAMEL_MIME_PART (message));
		if (!bodies.plain && content_type && camel_content_type_is (content_type, "text", "plain"))
			bodies.plain = mail_bridge_extract_part_text_utf8 (CAMEL_MIME_PART (message));
	}

	if (out_html)
		*out_html = bodies.html;
	else
		g_free (bodies.html);

	if (out_plain)
		*out_plain = bodies.plain;
	else
		g_free (bodies.plain);

	return bodies.html != NULL || bodies.plain != NULL;
}

gchar *
mail_bridge_eds_extract_message_attachments (CamelMimeMessage *message)
{
	AttachmentList attachments;
	guint ii;
	GString *serialized;

	g_return_val_if_fail (CAMEL_IS_MIME_MESSAGE (message), NULL);

	attachments.lines = g_ptr_array_new_with_free_func (g_free);
	attachments.current_index = 0;
	camel_mime_message_foreach_part (
		message,
		mail_bridge_collect_attachment_part,
		&attachments);

	if (attachments.lines->len == 0) {
		g_ptr_array_free (attachments.lines, TRUE);
		return NULL;
	}

	serialized = g_string_new (NULL);
	for (ii = 0; ii < attachments.lines->len; ii++) {
		if (ii > 0)
			g_string_append_c (serialized, '\n');
		g_string_append (serialized, attachments.lines->pdata[ii]);
	}

	g_ptr_array_free (attachments.lines, TRUE);

	return g_string_free (serialized, FALSE);
}

gchar *
mail_bridge_eds_extract_attachment_to_file (CamelMimeMessage *message,
				       const gchar *cache_root,
				       const gchar *cache_key,
				       const gchar *attachment_token,
				       GError **error)
{
	AttachmentExport export_data;
	guint64 target_index;
	gchar *result;

	g_return_val_if_fail (CAMEL_IS_MIME_MESSAGE (message), NULL);
	g_return_val_if_fail (cache_root != NULL, NULL);
	g_return_val_if_fail (cache_key != NULL, NULL);
	g_return_val_if_fail (attachment_token != NULL, NULL);

	target_index = g_ascii_strtoull (attachment_token, NULL, 10);
	if (!*attachment_token ||
	    strspn (attachment_token, "0123456789") != strlen (attachment_token) ||
	    target_index == 0 || target_index > G_MAXUINT) {
		g_set_error_literal (error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT, "Invalid attachment token");
		return NULL;
	}

	export_data.target_index = (guint) target_index;

	export_data.current_index = 0;
	export_data.result_uri = NULL;
	export_data.message_dir = mail_bridge_build_attachment_cache_dir (cache_root, cache_key);
	export_data.error = NULL;

	camel_mime_message_foreach_part (
		message,
		mail_bridge_export_attachment_part,
		&export_data);

	g_free (export_data.message_dir);

	if (export_data.error) {
		g_propagate_error (error, export_data.error);
		return NULL;
	}

	result = export_data.result_uri;
	if (!result)
		g_set_error_literal (error, G_IO_ERROR, G_IO_ERROR_NOT_FOUND, "Attachment not found");

	return result;
}

gboolean
mail_bridge_eds_append_text_message (CamelFolder *folder,
				const gchar *source_uid,
				const gchar *message_id,
				const gchar *from,
				const gchar *reply_to,
				const gchar *to_serialized,
				const gchar *cc_serialized,
				const gchar *bcc_serialized,
				const gchar *subject,
				const gchar *html_body,
				const gchar *plain_body,
				const gchar *attachment_uris_serialized,
				gboolean is_draft,
				gchar **out_appended_uid,
				gchar **out_message_id,
				GError **error)
{
	CamelMimeMessage *message;
	CamelMessageInfo *info;
	gchar *appended_uid = NULL;
	gboolean success;

	g_return_val_if_fail (CAMEL_IS_FOLDER (folder), FALSE);
	g_return_val_if_fail (from != NULL, FALSE);

	if (out_appended_uid)
		*out_appended_uid = NULL;
	if (out_message_id)
		*out_message_id = NULL;

	message = camel_mime_message_new ();
	if (source_uid && *source_uid)
		camel_mime_message_set_source (message, source_uid);
	camel_mime_message_set_subject (message, subject ? subject : "");
	camel_mime_message_set_date (message, CAMEL_MESSAGE_DATE_CURRENT, 0);

	if (!mail_bridge_message_set_address_header (message, from, camel_mime_message_set_from, error)) {
		g_object_unref (message);
		return FALSE;
	}
	/* Generate this before the durable Outbox append.  The same Message-ID then
	 * survives transport and lets the cache reconcile a queued item with the
	 * provider's Sent copy without downloading message bodies. */
	camel_mime_message_set_message_id (message,
		message_id && *message_id ? message_id : NULL);

	if (!mail_bridge_message_set_address_header (message, reply_to, camel_mime_message_set_reply_to, error) ||
	    !mail_bridge_message_set_recipients_header (message, CAMEL_RECIPIENT_TYPE_TO, to_serialized, error) ||
	    !mail_bridge_message_set_recipients_header (message, CAMEL_RECIPIENT_TYPE_CC, cc_serialized, error) ||
	    !mail_bridge_message_set_recipients_header (message, CAMEL_RECIPIENT_TYPE_BCC, bcc_serialized, error)) {
		g_object_unref (message);
		return FALSE;
	}
	if (!is_draft) {
		CamelInternetAddress *to = camel_mime_message_get_recipients (
			message, CAMEL_RECIPIENT_TYPE_TO);
		CamelInternetAddress *cc = camel_mime_message_get_recipients (
			message, CAMEL_RECIPIENT_TYPE_CC);
		CamelInternetAddress *bcc = camel_mime_message_get_recipients (
			message, CAMEL_RECIPIENT_TYPE_BCC);
		gint recipient_count = 0;

		if (to)
			recipient_count += camel_address_length (CAMEL_ADDRESS (to));
		if (cc)
			recipient_count += camel_address_length (CAMEL_ADDRESS (cc));
		if (bcc)
			recipient_count += camel_address_length (CAMEL_ADDRESS (bcc));
		if (recipient_count == 0) {
			g_set_error_literal (error, G_IO_ERROR, G_IO_ERROR_INVALID_DATA,
				"Message has no recipients");
			g_object_unref (message);
			return FALSE;
		}
	}

	if (!mail_bridge_message_set_body_and_attachments (
		message, html_body, plain_body, attachment_uris_serialized, error)) {
		g_object_unref (message);
		return FALSE;
	}

	info = camel_message_info_new_from_message (NULL, message);
	if (!info) {
		g_set_error_literal (error, G_IO_ERROR, G_IO_ERROR_FAILED, "Could not create CamelMessageInfo");
		g_object_unref (message);
		return FALSE;
	}

	if (is_draft)
		camel_message_info_set_flags (info, CAMEL_MESSAGE_DRAFT, CAMEL_MESSAGE_DRAFT);

	success = camel_folder_append_message_sync (
		folder,
		message,
		info,
		&appended_uid,
		NULL,
		error);

	if (success && out_message_id)
		*out_message_id = g_strdup (camel_mime_message_get_message_id (message));

	g_object_unref (info);
	g_object_unref (message);

	if (!success) {
		g_free (appended_uid);
		return FALSE;
	}

	if (out_appended_uid)
		*out_appended_uid = appended_uid;
	else
		g_free (appended_uid);

	return TRUE;
}

gboolean
mail_bridge_eds_append_cached_message (CamelFolder *source_folder,
				  const gchar *message_uid,
				  CamelFolder *destination_folder,
				  gboolean is_draft,
				  gchar **out_appended_uid,
				  GError **error)
{
	CamelMimeMessage *message;
	CamelMessageInfo *info;
	gchar *appended_uid = NULL;
	gboolean success;

	g_return_val_if_fail (CAMEL_IS_FOLDER (source_folder), FALSE);
	g_return_val_if_fail (message_uid != NULL, FALSE);
	g_return_val_if_fail (CAMEL_IS_FOLDER (destination_folder), FALSE);

	if (out_appended_uid)
		*out_appended_uid = NULL;

	message = camel_folder_get_message_sync (
		source_folder, message_uid, NULL, error);
	if (!message)
		return FALSE;

	info = camel_message_info_new_from_message (NULL, message);
	if (!info) {
		g_set_error_literal (error, G_IO_ERROR, G_IO_ERROR_FAILED,
			"Could not create CamelMessageInfo for cached message");
		g_object_unref (message);
		return FALSE;
	}
	if (is_draft)
		camel_message_info_set_flags (info, CAMEL_MESSAGE_DRAFT, CAMEL_MESSAGE_DRAFT);

	success = camel_folder_append_message_sync (
		destination_folder, message, info, &appended_uid, NULL, error);
	g_object_unref (info);
	g_object_unref (message);

	if (!success) {
		g_free (appended_uid);
		return FALSE;
	}
	if (out_appended_uid)
		*out_appended_uid = appended_uid;
	else
		g_free (appended_uid);
	return TRUE;
}

gboolean
mail_bridge_eds_transport_send_cached_message (CamelTransport *transport,
					  CamelFolder *folder,
					  const gchar *message_uid,
					  gboolean *out_sent_message_saved,
					  GError **error)
{
	CamelMimeMessage *message;
	CamelInternetAddress *from;
	CamelInternetAddress *recipients;
	CamelInternetAddress *header;
	gboolean success;

	g_return_val_if_fail (CAMEL_IS_TRANSPORT (transport), FALSE);
	g_return_val_if_fail (CAMEL_IS_FOLDER (folder), FALSE);
	g_return_val_if_fail (message_uid != NULL, FALSE);
	g_return_val_if_fail (out_sent_message_saved != NULL, FALSE);

	*out_sent_message_saved = FALSE;
	message = camel_folder_get_message_sync (folder, message_uid, NULL, error);
	if (!message) {
		if (!error || !*error)
			g_set_error_literal (error, G_IO_ERROR, G_IO_ERROR_NOT_FOUND,
				"Queued message is not available in the local Camel store");
		return FALSE;
	}

	from = camel_mime_message_get_from (message);
	recipients = camel_internet_address_new ();
	header = camel_mime_message_get_recipients (message, CAMEL_RECIPIENT_TYPE_TO);
	if (header)
		camel_address_cat (CAMEL_ADDRESS (recipients), CAMEL_ADDRESS (header));
	header = camel_mime_message_get_recipients (message, CAMEL_RECIPIENT_TYPE_CC);
	if (header)
		camel_address_cat (CAMEL_ADDRESS (recipients), CAMEL_ADDRESS (header));
	header = camel_mime_message_get_recipients (message, CAMEL_RECIPIENT_TYPE_BCC);
	if (header)
		camel_address_cat (CAMEL_ADDRESS (recipients), CAMEL_ADDRESS (header));

	if (!from || camel_address_length (CAMEL_ADDRESS (from)) == 0) {
		g_set_error_literal (error, G_IO_ERROR, G_IO_ERROR_INVALID_DATA,
			"Queued message has no sender");
		success = FALSE;
	} else if (camel_address_length (CAMEL_ADDRESS (recipients)) == 0) {
		g_set_error_literal (error, G_IO_ERROR, G_IO_ERROR_INVALID_DATA,
			"Queued message has no recipients");
		success = FALSE;
	} else {
		success = camel_transport_send_to_sync (
			transport,
			message,
			CAMEL_ADDRESS (from),
			CAMEL_ADDRESS (recipients),
			out_sent_message_saved,
			NULL,
			error);
	}

	g_object_unref (recipients);
	g_object_unref (message);
	return success;
}
