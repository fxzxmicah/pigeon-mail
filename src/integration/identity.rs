//! Import of the standard EDS alias address set.

use crate::model::account::{MailAccount, SendingIdentity};
use crate::model::address::normalized_mailbox_address;

use super::camel::decode_addresses;

pub(super) fn import_eds_aliases(
    account: &mut MailAccount,
    aliases: Option<&str>,
) -> anyhow::Result<()> {
    let primary_address = account.primary_identity().address.clone();
    let normalized_primary = normalized_mailbox_address(&primary_address);
    let mut identities = vec![account.primary_identity().clone()];
    for (eds_address, eds_display_name) in aliases
        .map(decode_addresses)
        .transpose()?
        .unwrap_or_default()
    {
        let eds_display_name = eds_display_name.unwrap_or_default();
        let normalized = normalized_mailbox_address(&eds_address);
        if normalized == normalized_primary {
            continue;
        }
        if let Some(existing) = identities.iter_mut().skip(1).find(|identity| {
            normalized_mailbox_address(&identity.address) == normalized
        }) {
            existing.address = eds_address;
            existing.display_name = eds_display_name;
            continue;
        }
        identities.push(SendingIdentity::new(
            eds_address,
            eds_display_name,
            None,
            crate::model::account::Signature::default(),
        ));
    }
    account.replace_aliases(identities, primary_address);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{decode_addresses, import_eds_aliases};
    use crate::model::account::{MailAccount, MailAccountId, SendingIdentity};

    #[test]
    fn quoted_alias_names_reverse_mailbox_escaping() {
        let aliases = decode_addresses(
            r#""Doe, Jane \"Nickname\" C:\\Mail" <jane@example.com>, bare@example.net"#,
        ).unwrap();

        assert_eq!(aliases.len(), 2);
        assert_eq!(
            aliases[0].1.as_deref(),
            Some(r#"Doe, Jane "Nickname" C:\Mail"#)
        );
        assert_eq!(aliases[0].0, "jane@example.com");
        assert_eq!(aliases[1].1, None);
        assert_eq!(aliases[1].0, "bare@example.net");
    }

    #[test]
    fn native_alias_decoding_handles_encoded_names_and_rejects_invalid_bridge_input() {
        let aliases = decode_addresses(
            "=?UTF-8?Q?Jos=C3=A9?= <jose@example.test>, bare@example.net, comment@example.test (Comment)",
        ).unwrap();

        assert_eq!(aliases.len(), 3);
        assert_eq!(aliases[0].0, "jose@example.test");
        assert_eq!(aliases[0].1.as_deref(), Some("José"));
        assert_eq!(aliases[1].0, "bare@example.net");
        assert_eq!(aliases[2].0, "comment@example.test");
        assert_eq!(aliases[2].1.as_deref(), Some("Comment"));
        assert!(decode_addresses("person@example.test\0other@example.net").is_err());
        assert!(decode_addresses("").unwrap().is_empty());
    }

    #[test]
    fn extensionless_eds_aliases_use_the_native_address_set() {
        let mut account = MailAccount::new(
            MailAccountId("account-1".into()),
            "Account".into(),
            SendingIdentity::new(
                "owner@example.com".into(),
                "Owner".into(),
                None,
                Default::default(),
            ),
        );
        import_eds_aliases(
            &mut account,
            Some(
                "Old <same@example.com>, Owner Alias <owner@example.com>, New <SAME@example.com>",
            ),
        ).unwrap();

        let primary = account.primary_identity();
        assert_eq!(primary.address, "owner@example.com");
        assert_eq!(account.aliases().len(), 2);
        assert_eq!(account.aliases()[1].address, "SAME@example.com");
        assert_eq!(account.aliases()[1].display_name, "New");
    }

    #[test]
    fn extensionless_eds_alias_import_is_repeatable() {
        fn load() -> MailAccount {
            let mut account = MailAccount::new(
                MailAccountId("account-1".into()),
                "Account".into(),
                SendingIdentity::new(
                    "owner@example.com".into(),
                    "Owner".into(),
                    None,
                    Default::default(),
                ),
            );
            import_eds_aliases(
                &mut account,
                Some("Same <same@example.com>, Same <same@example.com>"),
            ).unwrap();
            account
        }

        let first = load();
        let second = load();
        assert_eq!(first.aliases().len(), 2);
        assert_eq!(second.aliases().len(), 2);
        assert_eq!(first, second);
    }
}
