//! EDS account discovery and identity persistence.
//!
//! EDS owns the identity address set. The application extension only adds the
//! fields which the standard mail-identity extension cannot represent.

use std::collections::{HashMap, HashSet};

use anyhow::anyhow;

use crate::integration::registry::{
    IdentityExtension, IdentityExtensionState, IdentityMetadata, IdentityRegistry, MailTriplet,
    SignatureMetadata, SignatureSources, Snapshot, Source,
};
use crate::model::account::{MailAccount, MailAccountId, SendingIdentity, Signature};
use crate::model::address::normalized_mailbox_address;

pub(crate) struct AccountCatalog {
    accounts: Vec<MailAccount>,
    bindings: Vec<EdsAccountBinding>,
}

pub(crate) fn discover() -> anyhow::Result<AccountCatalog> {
    let snapshot = crate::integration::registry::load_snapshot_via_ffi()?;
    let catalog = from_registry(&snapshot)?;
    tracing::debug!(
        triplets = snapshot.triplets.len(),
        bindings = catalog.bindings.len(),
        "EDS registry topology resolved"
    );
    Ok(catalog)
}

impl AccountCatalog {
    pub(crate) fn into_parts(self) -> (Vec<MailAccount>, Vec<EdsAccountBinding>) {
        (self.accounts, self.bindings)
    }
}

fn from_registry(snapshot: &Snapshot) -> anyhow::Result<AccountCatalog> {
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for triplet in &snapshot.triplets {
        let Some(entry) = account_from_triplet(triplet)? else {
            continue;
        };
        if !seen.insert(entry.0.id.clone()) {
            return Err(anyhow!(
                "EDS exposes more than one mail route for account '{}'",
                entry.0.id.0
            ));
        }
        entries.push(entry);
    }
    entries.sort_by(|(left, _), (right, _)| {
        left.display_name
            .cmp(&right.display_name)
            .then_with(|| left.id.0.cmp(&right.id.0))
    });
    let (accounts, bindings) = entries.into_iter().unzip();
    Ok(AccountCatalog { accounts, bindings })
}

fn account_from_triplet(
    triplet: &MailTriplet,
) -> anyhow::Result<Option<(MailAccount, EdsAccountBinding)>> {
    let Some(route) = usable_mail_triplet(triplet) else {
        return Ok(None);
    };
    let identity = route.identity;
    let transport = route.transport;
    let Some(primary_address) = identity.identity_address.as_deref() else {
        return Ok(None);
    };
    let primary_address = primary_address.trim();
    if primary_address.is_empty() {
        return Ok(None);
    }
    let primary_name = identity.identity_name.as_deref().unwrap_or_default().trim();
    let display_name = (!primary_name.is_empty())
        .then_some(primary_name)
        .or_else(|| triplet.goa_name.as_deref().map(str::trim).filter(|name| !name.is_empty()))
        .unwrap_or(primary_address);
    let primary_identity = SendingIdentity::new(
        primary_address.to_string(),
        primary_name.to_string(),
        identity.identity_reply_to.clone(),
        signature_parts(identity.identity_signature.as_ref()),
    );
    let mut account = MailAccount::new(
        MailAccountId(route.account_id.to_string()),
        display_name.to_string(),
        primary_identity,
    );
    crate::integration::identity::import_eds_aliases(
        &mut account,
        identity.identity_aliases.as_deref(),
    )?;
    match &identity.identity_extension {
        IdentityExtensionState::Absent => {}
        IdentityExtensionState::Invalid => {
            return Err(anyhow!(
                "EDS mail identity '{}' contains an invalid identity extension",
                identity.uid
            ));
        }
        IdentityExtensionState::Valid(loaded) => {
            apply_identity_extension(&mut account, &loaded.extension)
        }
    }
    let binding = EdsAccountBinding {
        account_id: account.id.clone(),
        account_uid: triplet.account.uid.clone(),
        account_parent_uid: route.account_parent_uid.to_string(),
        account_backend_name: route.account_backend_name.to_string(),
        account_auth_method: triplet.account.auth_method.clone(),
        identity_uid: identity.uid.clone(),
        transport_uid: transport.uid.clone(),
        transport_backend_name: route.transport_backend_name.to_string(),
        transport_auth_method: transport.auth_method.clone(),
        route_fingerprint: RouteFingerprint {
            account: triplet.account.configuration_hash,
            transport: transport.configuration_hash,
            mailbox: identity.mailbox_configuration_hash,
        },
    };
    Ok(Some((account, binding)))
}

struct UsableMailTriplet<'a> {
    account_id: &'a str,
    account_parent_uid: &'a str,
    account_backend_name: &'a str,
    identity: &'a Source,
    transport: &'a Source,
    transport_backend_name: &'a str,
}

fn usable_mail_triplet(triplet: &MailTriplet) -> Option<UsableMailTriplet<'_>> {
    let account = &triplet.account;
    let identity = triplet.identity.as_ref()?;
    let transport = triplet.transport.as_ref()?;
    if triplet.mail_enabled == Some(false)
        || !account.enabled
        || !identity.enabled
        || !transport.enabled
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
    Some(UsableMailTriplet {
        account_id,
        account_parent_uid: account.parent.as_deref()?,
        account_backend_name: account.backend_name.as_deref()?,
        identity,
        transport,
        transport_backend_name: transport.backend_name.as_deref()?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct EdsAccountBinding {
    pub(crate) account_id: MailAccountId,
    pub(crate) account_uid: String,
    pub(crate) account_parent_uid: String,
    pub(crate) account_backend_name: String,
    pub(crate) account_auth_method: Option<String>,
    pub(crate) identity_uid: String,
    pub(crate) transport_uid: String,
    pub(crate) transport_backend_name: String,
    pub(crate) transport_auth_method: Option<String>,
    pub(crate) route_fingerprint: RouteFingerprint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct RouteFingerprint {
    pub(crate) account: u64,
    pub(crate) transport: u64,
    pub(crate) mailbox: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredIdentityRecord {
    name: Option<String>,
    reply_to: Option<String>,
    aliases: Option<String>,
    extension: Option<IdentityExtension>,
    extension_needs_rewrite: bool,
}

impl StoredIdentityRecord {
    fn from_source(identity: &Source) -> Self {
        Self {
            name: identity.identity_name.as_deref().and_then(nonempty_trimmed),
            reply_to: identity.identity_reply_to.as_deref().and_then(nonempty_trimmed),
            aliases: identity.identity_aliases.as_deref().and_then(nonempty_trimmed),
            extension: identity.identity_extension.as_valid().cloned(),
            extension_needs_rewrite: identity.identity_extension.needs_rewrite(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DesiredIdentityRecord {
    name: Option<String>,
    reply_to: Option<String>,
    aliases: Option<String>,
    extension: IdentityExtension,
}

impl DesiredIdentityRecord {
    fn from_account(account: &MailAccount, previous: Option<&IdentityExtension>) -> Self {
        let primary = account.primary_identity();
        Self {
            name: nonempty_trimmed(&primary.display_name),
            reply_to: primary.reply_to.as_deref().and_then(nonempty_trimmed),
            aliases: serialize_identity_aliases(account),
            extension: identity_extension_from_account(account, previous),
        }
    }

    fn matches(&self, stored: &StoredIdentityRecord) -> bool {
        !stored.extension_needs_rewrite
            && self.name == stored.name
            && self.reply_to == stored.reply_to
            && self.aliases == stored.aliases
            && stored.extension.as_ref() == Some(&self.extension)
    }
}

fn identity_extension_from_account(
    account: &MailAccount,
    previous: Option<&IdentityExtension>,
) -> IdentityExtension {
    let identities = account
        .aliases()
        .iter()
        .map(|identity| {
            let address = normalized_mailbox_address(&identity.address);
            let previous_identity = previous
                .into_iter()
                .flat_map(|extension| &extension.identities)
                .find(|metadata| normalized_mailbox_address(&metadata.address) == address);
            IdentityMetadata {
                address: identity.address.clone(),
                reply_to: if account.is_primary_identity(&identity.address) {
                    String::new()
                } else {
                    identity.reply_to.clone().unwrap_or_default()
                },
                signatures: SignatureSources {
                    html: signature_source(
                        previous_identity.map(|metadata| &metadata.signatures.html),
                        &identity.address,
                        &identity.signature.html,
                        "text/html",
                    ),
                    text: signature_source(
                        previous_identity.map(|metadata| &metadata.signatures.text),
                        &identity.address,
                        &identity.signature.text,
                        "text/plain",
                    ),
                },
            }
        })
        .collect();
    IdentityExtension {
        account_label: account.display_name.clone(),
        default_address: account.default_identity().address.clone(),
        identities,
    }
}

fn apply_identity_extension(account: &mut MailAccount, extension: &IdentityExtension) {
    let standard_identities = account.aliases().to_vec();
    let primary_address = normalized_mailbox_address(&account.primary_identity().address);
    let metadata = extension
        .identities
        .iter()
        .map(|metadata| (normalized_mailbox_address(&metadata.address), metadata))
        .filter(|(address, _)| !address.is_empty())
        .collect::<HashMap<_, _>>();
    let mut represented = HashSet::new();
    let mut aliases = Vec::with_capacity(standard_identities.len());

    for address in std::iter::once(primary_address.clone()).chain(
        extension
            .identities
            .iter()
            .map(|identity| normalized_mailbox_address(&identity.address))
            .filter(|address| address != &primary_address),
    ) {
        if !represented.insert(address.clone()) {
            continue;
        }
        let Some(identity) = standard_identities
            .iter()
            .find(|identity| normalized_mailbox_address(&identity.address) == address)
            .cloned()
        else {
            continue;
        };
        aliases.push(apply_identity_metadata(
            identity,
            metadata.get(&address).copied(),
            address == primary_address,
        ));
    }
    aliases.extend(standard_identities.into_iter().filter(|identity| {
        represented.insert(normalized_mailbox_address(&identity.address))
    }));
    let default_address = aliases
        .iter()
        .find(|identity| {
            normalized_mailbox_address(&identity.address)
                == normalized_mailbox_address(&extension.default_address)
        })
        .map(|identity| identity.address.clone())
        .unwrap_or_else(|| account.primary_identity().address.clone());
    assert!(
        account.rename(extension.account_label.clone()),
        "a validated identity extension has an account label"
    );
    account.replace_aliases(aliases, default_address);
}

fn apply_identity_metadata(
    mut identity: SendingIdentity,
    metadata: Option<&IdentityMetadata>,
    primary: bool,
) -> SendingIdentity {
    if let Some(metadata) = metadata {
        if !primary {
            identity.reply_to = nonempty_trimmed(&metadata.reply_to);
        }
        identity.signature = Signature {
            html: metadata.signatures.html.contents.clone(),
            text: metadata.signatures.text.contents.clone(),
        };
    }
    identity
}

fn signature_parts(signature: Option<&SignatureMetadata>) -> Signature {
    let Some(signature) = signature else {
        return Signature::default();
    };
    if signature_is_html(signature) {
        Signature {
            html: signature.contents.clone(),
            text: String::new(),
        }
    } else {
        Signature {
            html: String::new(),
            text: signature.contents.clone(),
        }
    }
}

fn valid_signature_uid(uid: &str) -> bool {
    !uid.is_empty() && uid != "none"
}

fn signature_is_html(signature: &SignatureMetadata) -> bool {
    signature
        .mime_type
        .split(';')
        .next()
        .is_some_and(|mime_type| mime_type.trim().eq_ignore_ascii_case("text/html"))
}

fn signature_source(
    previous: Option<&SignatureMetadata>,
    display_name: &str,
    contents: &str,
    mime_type: &str,
) -> SignatureMetadata {
    if contents.is_empty() {
        return SignatureMetadata {
            uid: "none".into(),
            display_name: String::new(),
            contents: String::new(),
            mime_type: mime_type.into(),
        };
    }
    if let Some(previous) = previous.filter(|previous| {
        valid_signature_uid(&previous.uid)
            && previous.display_name == display_name
            && previous.contents == contents
            && previous.mime_type.eq_ignore_ascii_case(mime_type)
    }) {
        return previous.clone();
    }
    SignatureMetadata {
        uid: crate::integration::registry::generate_uid(),
        display_name: display_name.into(),
        contents: contents.into(),
        mime_type: mime_type.into(),
    }
}

fn nonempty_trimmed(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

struct IdentityWrite {
    identity_uid: String,
    desired: DesiredIdentityRecord,
    primary_address: String,
}

pub(crate) fn save_identities(accounts: &[MailAccount]) -> anyhow::Result<()> {
    let accounts = accounts
        .iter()
        .filter(|account| !crate::integration::stub::is_stub_account_id(&account.id))
        .collect::<Vec<_>>();
    if accounts.is_empty() {
        return Ok(());
    }

    let registry = IdentityRegistry::load()?;
    let catalog = from_registry(registry.snapshot())?;
    let writes = accounts
        .into_iter()
        .map(|account| {
            let binding = catalog
                .bindings
                .iter()
                .find(|binding| binding.account_id == account.id)
                .ok_or_else(|| anyhow!("mail account '{}' is absent from EDS", account.id.0))?;
            let stored_identity = registry
                .snapshot()
                .triplets
                .iter()
                .filter_map(|triplet| triplet.identity.as_ref())
                .find(|identity| identity.uid == binding.identity_uid)
                .ok_or_else(|| anyhow!("EDS identity '{}' disappeared", binding.identity_uid))?;
            let stored = StoredIdentityRecord::from_source(stored_identity);
            let desired = DesiredIdentityRecord::from_account(account, stored.extension.as_ref());
            Ok((
                stored,
                IdentityWrite {
                    identity_uid: binding.identity_uid.clone(),
                    desired,
                    primary_address: normalized_mailbox_address(
                        &account.primary_identity().address,
                    ),
                },
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    for (stored, write) in writes {
        if write.desired.matches(&stored) {
            continue;
        }
        let extension = &write.desired.extension;
        let primary_metadata = extension
            .identities
            .iter()
            .find(|identity| {
                normalized_mailbox_address(&identity.address) == write.primary_address
            })
            .expect("runtime identity metadata contains the primary identity");
        let primary_signature_uid = if valid_signature_uid(&primary_metadata.signatures.html.uid) {
            &primary_metadata.signatures.html.uid
        } else {
            &primary_metadata.signatures.text.uid
        };
        registry.write_identity(
            &write.identity_uid,
            write.desired.name.as_deref(),
            write.desired.reply_to.as_deref(),
            write.desired.aliases.as_deref(),
            &extension.account_label,
            &extension.default_address,
            primary_signature_uid,
            &extension.identities,
        )?;
    }
    Ok(())
}

fn serialize_identity_aliases(account: &MailAccount) -> Option<String> {
    let aliases = account
        .aliases()
        .iter()
        .filter(|identity| !account.is_primary_identity(&identity.address))
        .map(SendingIdentity::mailbox)
        .collect::<Vec<_>>();
    (!aliases.is_empty()).then(|| aliases.join(", "))
}

#[cfg(test)]
mod tests {
    use super::{
        apply_identity_extension, from_registry, serialize_identity_aliases, signature_source,
    };
    use crate::integration::registry::{
        IdentityExtension, IdentityExtensionState, IdentityMetadata, MailTriplet, SignatureMetadata,
        SignatureSources, Snapshot, Source,
    };
    use crate::model::account::{MailAccount, MailAccountId, SendingIdentity};

    fn metadata(address: &str, reply_to: &str) -> IdentityMetadata {
        IdentityMetadata {
            address: address.into(),
            reply_to: reply_to.into(),
            signatures: SignatureSources {
                html: SignatureMetadata {
                    uid: "none".into(),
                    display_name: String::new(),
                    contents: format!("<p>{address}</p>"),
                    mime_type: "text/html".into(),
                },
                text: SignatureMetadata {
                    uid: "none".into(),
                    display_name: String::new(),
                    contents: address.into(),
                    mime_type: "text/plain".into(),
                },
            },
        }
    }

    fn triplet(account_id: &str, source_prefix: &str) -> MailTriplet {
        MailTriplet {
            account: Source {
                uid: format!("{source_prefix}-account"),
                parent: Some(format!("{source_prefix}-collection")),
                enabled: true,
                backend_name: Some("imapx".into()),
                ..Source::default()
            },
            identity: Some(Source {
                uid: format!("{source_prefix}-identity"),
                enabled: true,
                identity_name: Some("Owner".into()),
                identity_address: Some(format!("{source_prefix}@example.com")),
                ..Source::default()
            }),
            transport: Some(Source {
                uid: format!("{source_prefix}-transport"),
                enabled: true,
                backend_name: Some("smtp".into()),
                ..Source::default()
            }),
            goa_account_id: Some(account_id.into()),
            goa_name: Some("Account".into()),
            mail_enabled: Some(true),
        }
    }

    #[test]
    fn standard_aliases_are_serialized_from_the_complete_address_set() {
        let mut account = MailAccount::new(
            MailAccountId("account".into()),
            "Account".into(),
            SendingIdentity::new(
                "owner@example.com".into(),
                "Owner".into(),
                None,
                Default::default(),
            ),
        );
        assert!(account.add_identity(SendingIdentity::new(
            "alias@example.com".into(),
            "Alias".into(),
            None,
            Default::default(),
        )));
        assert_eq!(
            serialize_identity_aliases(&account).as_deref(),
            Some("\"Alias\" <alias@example.com>")
        );
    }

    #[test]
    fn standard_alias_roundtrip_preserves_explicit_and_absent_display_names() {
        let mut account = MailAccount::new(
            MailAccountId("account".into()), "Account".into(),
            SendingIdentity::new("owner@example.com".into(), "Owner".into(), None, Default::default()),
        );
        for (address, name) in [
            ("same@example.com", "same@example.com"),
            ("bare@example.net", ""),
            ("quoted@example.test", r#"Doe, Jane "Nickname" C:\Mail"#),
            ("jose@example.test", "José"),
        ] {
            assert!(account.add_identity(SendingIdentity::new(
                address.into(), name.into(), None, Default::default(),
            )));
        }
        let mut source = triplet("account", "owner");
        source.identity.as_mut().unwrap().identity_aliases = serialize_identity_aliases(&account);
        let (accounts, _) = from_registry(&Snapshot { triplets: vec![source] })
            .unwrap().into_parts();

        assert_eq!(accounts[0].aliases(), account.aliases());
    }

    #[test]
    fn account_display_fallback_does_not_become_the_primary_sender_name() {
        for name in [None, Some("  ")] {
            for goa_name in [Some("Account label"), None] {
                let mut source = triplet("account", "owner");
                source.identity.as_mut().unwrap().identity_name = name.map(str::to_string);
                source.goa_name = goa_name.map(str::to_string);
                let (accounts, _) = from_registry(&Snapshot { triplets: vec![source] })
                    .unwrap().into_parts();
                let account = &accounts[0];

                assert!(account.primary_identity().display_name.is_empty());
                assert_eq!(account.primary_identity().mailbox(), "owner@example.com");
                assert_eq!(account.display_name, goa_name.unwrap_or("owner@example.com"));
                assert!(super::DesiredIdentityRecord::from_account(account, None).name.is_none());
            }
        }
    }

    #[test]
    fn duplicate_complete_routes_fail_at_the_account_boundary() {
        let snapshot = Snapshot {
            triplets: vec![triplet("same-account", "first"), triplet("same-account", "second")],
        };

        assert!(from_registry(&snapshot).is_err());
    }

    #[test]
    fn invalid_identity_extension_fails_catalog_normalization() {
        let mut invalid = triplet("account", "invalid");
        invalid
            .identity
            .as_mut()
            .unwrap()
            .identity_extension = IdentityExtensionState::Invalid;

        assert!(
            from_registry(&Snapshot {
                triplets: vec![invalid],
            })
            .is_err()
        );
    }

    #[test]
    fn signature_source_reuse_requires_the_same_complete_eds_value() {
        let previous = SignatureMetadata {
            uid: "existing-signature".into(),
            display_name: "sender@example.com".into(),
            contents: "Regards".into(),
            mime_type: "text/plain".into(),
        };

        assert_eq!(
            signature_source(
                Some(&previous),
                "sender@example.com",
                "Regards",
                "text/plain",
            ),
            previous
        );

        let changed = signature_source(
            Some(&previous),
            "other@example.com",
            "Regards",
            "text/plain",
        );
        assert_ne!(changed.uid, previous.uid);
        assert_eq!(changed.display_name, "other@example.com");
    }

    #[test]
    fn extension_orders_native_members_and_appends_unrepresented_aliases() {
        let mut account = MailAccount::new(
            MailAccountId("account".into()),
            "Native account".into(),
            SendingIdentity::new(
                "owner@example.com".into(),
                "Owner".into(),
                None,
                Default::default(),
            ),
        );
        for address in ["later@example.com", "first@example.com", "new@example.com"] {
            assert!(account.add_identity(SendingIdentity::new(
                address.into(),
                address.into(),
                None,
                Default::default(),
            )));
        }
        let extension = IdentityExtension {
            account_label: "Account".into(),
            default_address: "first@example.com".into(),
            identities: vec![
                metadata("owner@example.com", "ignored@example.com"),
                metadata("first@example.com", "reply@example.com"),
                metadata("later@example.com", ""),
            ],
        };

        apply_identity_extension(&mut account, &extension);

        assert_eq!(
            account
                .aliases()
                .iter()
                .map(|identity| identity.address.as_str())
                .collect::<Vec<_>>(),
            [
                "owner@example.com",
                "first@example.com",
                "later@example.com",
                "new@example.com",
            ]
        );
        assert_eq!(account.display_name, "Account");
        assert_eq!(account.default_identity().address, "first@example.com");
        assert_eq!(
            account
                .identity("first@example.com")
                .and_then(|identity| identity.reply_to.as_deref()),
            Some("reply@example.com")
        );
        assert_eq!(account.primary_identity().reply_to, None);
    }
}
