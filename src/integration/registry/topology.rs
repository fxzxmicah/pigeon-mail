use std::collections::{BTreeMap, HashSet};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use super::{MailTriplet, Snapshot, Source, SourceInfo};

type Sources<'a> = BTreeMap<&'a str, &'a SourceInfo>;

pub(super) fn build_snapshot(infos: &[SourceInfo]) -> Snapshot {
    let sources = infos
        .iter()
        .map(|info| (info.uid.as_str(), info))
        .collect::<Sources<'_>>();
    let triplets = infos
        .iter()
        .filter(|info| info.is_account)
        .map(|account| mail_triplet(account, &sources))
        .collect();

    Snapshot { triplets }
}

fn mail_triplet(account: &SourceInfo, sources: &Sources<'_>) -> MailTriplet {
    let identity = account
        .direct_identity_uid
        .as_deref()
        .and_then(|uid| sources.get(uid).copied());
    let transport = identity
        .and_then(|identity| identity.direct_transport_uid.as_deref())
        .and_then(|uid| sources.get(uid).copied());
    let goa_id = |source: &SourceInfo| {
        inherited_source_value(source, sources, |entry| entry.direct_goa_account_id.as_ref())
    };
    let goa_account_id = goa_id(account).filter(|account_id| {
        identity.and_then(goa_id).as_ref() == Some(account_id)
            && transport.and_then(goa_id).as_ref() == Some(account_id)
    });
    let goa_name = [identity, Some(account), transport]
        .into_iter()
        .flatten()
        .find_map(|source| {
            inherited_source_value(source, sources, |entry| entry.direct_goa_name.as_ref())
        });
    MailTriplet {
        account: project_source(account, sources),
        identity: identity
            .filter(|source| is_mail_source(source))
            .map(|source| project_source(source, sources)),
        transport: transport
            .filter(|source| is_mail_source(source))
            .map(|source| project_source(source, sources)),
        goa_account_id,
        goa_name,
        mail_enabled: inherited_source_value(account, sources, |entry| {
            entry.direct_collection_mail_enabled.as_ref()
        }),
    }
}

fn is_mail_source(source: &SourceInfo) -> bool {
    source.is_account || source.is_identity || source.is_transport
}

fn project_source(source: &SourceInfo, sources: &Sources<'_>) -> Source {
    let backend_name = if source.is_account {
        source.direct_mail_account_backend_name.clone()
    } else if source.is_transport {
        source.direct_mail_transport_backend_name.clone()
    } else {
        None
    };
    Source {
        uid: source.uid.clone(),
        parent: source.parent.clone(),
        enabled: source.enabled,
        identity_name: source.direct_name.clone(),
        identity_address: source.direct_address.clone(),
        identity_reply_to: source.direct_reply_to.clone(),
        identity_aliases: source.direct_aliases.clone(),
        identity_signature: source.direct_signature.clone(),
        identity_extension: source.direct_identity_extension.clone(),
        backend_name,
        auth_method: source.direct_auth_method.clone(),
        mailbox_configuration_hash: source.mailbox_configuration_hash,
        configuration_hash: source_configuration_hash(source, sources),
    }
}

fn source_chain<'a>(
    source: &'a SourceInfo,
    sources: &'a Sources<'a>,
) -> impl Iterator<Item = &'a SourceInfo> {
    let mut visited = HashSet::new();
    std::iter::successors(Some(source), move |current| {
        current.parent.as_deref().and_then(|uid| sources.get(uid).copied())
    })
    .take_while(move |source| visited.insert(source.uid.as_str()))
}

fn inherited_source_value<T: Clone>(
    source: &SourceInfo,
    sources: &Sources<'_>,
    value: impl for<'a> Fn(&'a SourceInfo) -> Option<&'a T>,
) -> Option<T> {
    source_chain(source, sources).find_map(value).cloned()
}

fn source_configuration_hash(source: &SourceInfo, sources: &Sources<'_>) -> u64 {
    let mut hasher = DefaultHasher::new();
    for entry in source_chain(source, sources) {
        entry.uid.hash(&mut hasher);
        entry.serialized_configuration.hash(&mut hasher);
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integration::registry::IdentityExtensionState;

    fn source(uid: &str, parent: Option<&str>) -> SourceInfo {
        SourceInfo {
            uid: uid.into(),
            parent: parent.map(str::to_string),
            enabled: true,
            direct_goa_account_id: None,
            direct_goa_name: None,
            direct_collection_mail_enabled: None,
            direct_mail_account_backend_name: None,
            direct_mail_transport_backend_name: None,
            direct_identity_uid: None,
            direct_transport_uid: None,
            direct_name: None,
            direct_address: None,
            direct_reply_to: None,
            direct_aliases: None,
            direct_signature: None,
            direct_identity_extension: IdentityExtensionState::Absent,
            direct_auth_method: None,
            mailbox_configuration_hash: 0,
            serialized_configuration: String::new(),
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

        let snapshot = build_snapshot(&[collection, account, identity, transport]);

        assert_eq!(snapshot.triplets.len(), 1);
        let triplet = &snapshot.triplets[0];
        assert_eq!(triplet.goa_account_id.as_deref(), Some("account-example"));
        assert_eq!(triplet.goa_name.as_deref(), Some("Example Account"));
        assert_eq!(triplet.mail_enabled, Some(true));
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
    fn triplet_rejects_conflicting_inherited_goa_ownership() {
        let mut collection = source("collection", None);
        collection.direct_goa_account_id = Some("account-one".into());

        let mut account = source("account", Some("collection"));
        account.is_account = true;
        account.direct_identity_uid = Some("identity".into());
        let mut identity = source("identity", Some("collection"));
        identity.is_identity = true;
        identity.direct_goa_account_id = Some("account-two".into());
        identity.direct_transport_uid = Some("transport".into());
        let mut transport = source("transport", Some("collection"));
        transport.is_transport = true;

        let snapshot = build_snapshot(&[collection, account, identity, transport]);

        assert_eq!(snapshot.triplets.len(), 1);
        assert_eq!(snapshot.triplets[0].goa_account_id, None);
    }

    #[test]
    fn inherited_values_stop_at_a_parent_cycle() {
        let first = source("first", Some("second"));
        let second = source("second", Some("first"));
        let sources = [&first, &second]
            .into_iter()
            .map(|source| (source.uid.as_str(), source))
            .collect::<BTreeMap<_, _>>();

        let inherited = inherited_source_value(&first, &sources, |source| {
            source.direct_goa_account_id.as_ref()
        });

        assert_eq!(inherited, None);
    }

    #[test]
    fn source_configuration_hash_includes_ancestor_settings() {
        let mut collection = source("collection", None);
        collection.serialized_configuration = "[Collection]\nBackendName=first".into();
        let account = source("account", Some("collection"));
        let first_sources = [&collection, &account]
            .into_iter()
            .map(|source| (source.uid.as_str(), source))
            .collect();
        let first = source_configuration_hash(&account, &first_sources);

        collection.serialized_configuration = "[Collection]\nBackendName=second".into();
        let second_sources = [&collection, &account]
            .into_iter()
            .map(|source| (source.uid.as_str(), source))
            .collect();

        assert_ne!(first, source_configuration_hash(&account, &second_sources));
    }
}
