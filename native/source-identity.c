#include <libedataserver/libedataserver.h>

#define MAIL_BRIDGE_SOURCE_EXTENSION_IDENTITY "org.gnome.pigeon.Identity"

typedef struct _MailBridgeSourceIdentity MailBridgeSourceIdentity;
typedef struct _MailBridgeSourceIdentityClass MailBridgeSourceIdentityClass;
typedef struct _MailBridgeSourceIdentityPrivate MailBridgeSourceIdentityPrivate;

struct _MailBridgeSourceIdentity {
	ESourceExtension parent;
	MailBridgeSourceIdentityPrivate *priv;
};

struct _MailBridgeSourceIdentityClass {
	ESourceExtensionClass parent_class;
};

struct _MailBridgeSourceIdentityPrivate {
	gchar *account_label;
	gchar *default_address;
	gchar **identity_records;
};

enum {
	PROP_0,
	PROP_ACCOUNT_LABEL,
	PROP_DEFAULT_ADDRESS,
	PROP_IDENTITY_RECORDS,
	N_PROPS
};

static GParamSpec *properties[N_PROPS];

GType mail_bridge_source_identity_get_type (void);

G_DEFINE_TYPE_WITH_PRIVATE (
	MailBridgeSourceIdentity,
	mail_bridge_source_identity,
	E_TYPE_SOURCE_EXTENSION)

static void
mail_bridge_source_identity_set_string (MailBridgeSourceIdentity *extension,
					gchar **field,
					const gchar *value,
					GParamSpec *property)
{
	gboolean changed;

	e_source_extension_property_lock (E_SOURCE_EXTENSION (extension));
	changed = g_strcmp0 (*field, value) != 0;
	if (changed) {
		g_free (*field);
		*field = g_strdup (value);
	}
	e_source_extension_property_unlock (E_SOURCE_EXTENSION (extension));

	if (changed)
		g_object_notify_by_pspec (G_OBJECT (extension), property);
}

static void
mail_bridge_source_identity_set_records (MailBridgeSourceIdentity *extension,
					 const gchar * const *records)
{
	gboolean changed;

	e_source_extension_property_lock (E_SOURCE_EXTENSION (extension));
	changed = (extension->priv->identity_records == NULL) != (records == NULL) ||
		(extension->priv->identity_records != NULL &&
		 !g_strv_equal ((const gchar * const *) extension->priv->identity_records,
			       records));
	if (changed) {
		g_strfreev (extension->priv->identity_records);
		extension->priv->identity_records = g_strdupv ((gchar **) records);
	}
	e_source_extension_property_unlock (E_SOURCE_EXTENSION (extension));

	if (changed)
		g_object_notify_by_pspec (G_OBJECT (extension), properties[PROP_IDENTITY_RECORDS]);
}

static void
mail_bridge_source_identity_set_property (GObject *object,
					 guint property_id,
					 const GValue *value,
					 GParamSpec *pspec)
{
	MailBridgeSourceIdentity *extension = (MailBridgeSourceIdentity *) object;

	switch (property_id) {
	case PROP_ACCOUNT_LABEL:
		mail_bridge_source_identity_set_string (
			extension, &extension->priv->account_label,
			g_value_get_string (value), properties[PROP_ACCOUNT_LABEL]);
		break;
	case PROP_DEFAULT_ADDRESS:
		mail_bridge_source_identity_set_string (
			extension, &extension->priv->default_address,
			g_value_get_string (value), properties[PROP_DEFAULT_ADDRESS]);
		break;
	case PROP_IDENTITY_RECORDS:
		mail_bridge_source_identity_set_records (
			extension, g_value_get_boxed (value));
		break;
	default:
		G_OBJECT_WARN_INVALID_PROPERTY_ID (object, property_id, pspec);
	}
}

static void
mail_bridge_source_identity_get_property (GObject *object,
					 guint property_id,
					 GValue *value,
					 GParamSpec *pspec)
{
	MailBridgeSourceIdentity *extension = (MailBridgeSourceIdentity *) object;

	e_source_extension_property_lock (E_SOURCE_EXTENSION (extension));
	switch (property_id) {
	case PROP_ACCOUNT_LABEL:
		g_value_set_string (value, extension->priv->account_label);
		break;
	case PROP_DEFAULT_ADDRESS:
		g_value_set_string (value, extension->priv->default_address);
		break;
	case PROP_IDENTITY_RECORDS:
		g_value_set_boxed (value, extension->priv->identity_records);
		break;
	default:
		G_OBJECT_WARN_INVALID_PROPERTY_ID (object, property_id, pspec);
	}
	e_source_extension_property_unlock (E_SOURCE_EXTENSION (extension));
}

static void
mail_bridge_source_identity_finalize (GObject *object)
{
	MailBridgeSourceIdentity *extension = (MailBridgeSourceIdentity *) object;

	g_free (extension->priv->account_label);
	g_free (extension->priv->default_address);
	g_strfreev (extension->priv->identity_records);
	G_OBJECT_CLASS (mail_bridge_source_identity_parent_class)->finalize (object);
}

static void
mail_bridge_source_identity_class_init (MailBridgeSourceIdentityClass *class)
{
	GObjectClass *object_class = G_OBJECT_CLASS (class);
	ESourceExtensionClass *extension_class = E_SOURCE_EXTENSION_CLASS (class);
	GParamFlags flags = G_PARAM_READWRITE |
		G_PARAM_CONSTRUCT |
		G_PARAM_EXPLICIT_NOTIFY |
		G_PARAM_STATIC_STRINGS |
		E_SOURCE_PARAM_SETTING;

	object_class->set_property = mail_bridge_source_identity_set_property;
	object_class->get_property = mail_bridge_source_identity_get_property;
	object_class->finalize = mail_bridge_source_identity_finalize;
	extension_class->name = MAIL_BRIDGE_SOURCE_EXTENSION_IDENTITY;

	properties[PROP_ACCOUNT_LABEL] = g_param_spec_string (
		"account-label", NULL, NULL, NULL, flags);
	properties[PROP_DEFAULT_ADDRESS] = g_param_spec_string (
		"default-address", NULL, NULL, NULL, flags);
	properties[PROP_IDENTITY_RECORDS] = g_param_spec_boxed (
		"identity-records", NULL, NULL, G_TYPE_STRV, flags);
	g_object_class_install_properties (object_class, N_PROPS, properties);
}

static void
mail_bridge_source_identity_init (MailBridgeSourceIdentity *extension)
{
	extension->priv = mail_bridge_source_identity_get_instance_private (extension);
}

void
mail_bridge_eds_register_source_types (void)
{
	g_type_ensure (mail_bridge_source_identity_get_type ());
}

static MailBridgeSourceIdentity *
mail_bridge_eds_source_get_identity_extension (ESource *source,
					       gboolean create)
{
	g_return_val_if_fail (E_IS_SOURCE (source), NULL);
	mail_bridge_eds_register_source_types ();
	if (!create &&
	    !e_source_has_extension (source, MAIL_BRIDGE_SOURCE_EXTENSION_IDENTITY))
		return NULL;
	return e_source_get_extension (
		source, MAIL_BRIDGE_SOURCE_EXTENSION_IDENTITY);
}

gboolean
mail_bridge_eds_source_has_identity_extension (ESource *source)
{
	return mail_bridge_eds_source_get_identity_extension (source, FALSE) != NULL;
}

gchar *
mail_bridge_eds_source_dup_identity_account_label (ESource *source)
{
	MailBridgeSourceIdentity *extension =
		mail_bridge_eds_source_get_identity_extension (source, FALSE);
	gchar *value;

	if (!extension)
		return NULL;
	e_source_extension_property_lock (E_SOURCE_EXTENSION (extension));
	value = g_strdup (extension->priv->account_label);
	e_source_extension_property_unlock (E_SOURCE_EXTENSION (extension));
	return value;
}

gchar *
mail_bridge_eds_source_dup_identity_default_address (ESource *source)
{
	MailBridgeSourceIdentity *extension =
		mail_bridge_eds_source_get_identity_extension (source, FALSE);
	gchar *value;

	if (!extension)
		return NULL;
	e_source_extension_property_lock (E_SOURCE_EXTENSION (extension));
	value = g_strdup (extension->priv->default_address);
	e_source_extension_property_unlock (E_SOURCE_EXTENSION (extension));
	return value;
}

gchar **
mail_bridge_eds_source_dup_identity_records (ESource *source)
{
	MailBridgeSourceIdentity *extension =
		mail_bridge_eds_source_get_identity_extension (source, FALSE);
	gchar **value;

	if (!extension)
		return NULL;
	e_source_extension_property_lock (E_SOURCE_EXTENSION (extension));
	value = g_strdupv (extension->priv->identity_records);
	e_source_extension_property_unlock (E_SOURCE_EXTENSION (extension));
	return value;
}

void
mail_bridge_eds_source_set_identity_records (ESource *source,
					      const gchar *account_label,
					      const gchar *default_address,
					      const gchar * const *identity_records)
{
	MailBridgeSourceIdentity *extension =
		mail_bridge_eds_source_get_identity_extension (source, TRUE);

	g_return_if_fail (extension != NULL);
	mail_bridge_source_identity_set_string (
		extension, &extension->priv->account_label, account_label,
		properties[PROP_ACCOUNT_LABEL]);
	mail_bridge_source_identity_set_string (
		extension, &extension->priv->default_address, default_address,
		properties[PROP_DEFAULT_ADDRESS]);
	mail_bridge_source_identity_set_records (extension, identity_records);
}
