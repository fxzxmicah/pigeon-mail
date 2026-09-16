use std::collections::HashSet;

use crate::model::address::normalized_mailbox_address;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MailAccountId(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailAccount {
    pub id: MailAccountId,
    pub display_name: String,
    aliases: Vec<SendingIdentity>,
    primary_address: String,
    default_address: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendingIdentity {
    pub address: String,
    pub display_name: String,
    pub reply_to: Option<String>,
    pub signature: Signature,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Signature {
    pub html: String,
    pub text: String,
}

impl MailAccount {
    pub fn new(id: MailAccountId, display_name: String, primary: SendingIdentity) -> Self {
        let primary = primary.normalized();
        assert!(
            !normalized_mailbox_address(&primary.address).is_empty(),
            "a primary identity requires a mail address"
        );
        let display_name = display_name.trim();
        assert!(!display_name.is_empty(), "a mail account requires a display name");
        let primary_address = primary.address.clone();
        Self {
            id,
            display_name: display_name.to_string(),
            aliases: vec![primary],
            default_address: primary_address.clone(),
            primary_address,
        }
    }

    pub fn aliases(&self) -> &[SendingIdentity] {
        &self.aliases
    }

    pub fn default_identity(&self) -> &SendingIdentity {
        self.identity(&self.default_address)
            .expect("the default identity belongs to its mail account")
    }

    pub fn primary_identity(&self) -> &SendingIdentity {
        self.identity(&self.primary_address)
            .expect("the primary identity belongs to its mail account")
    }

    pub fn identity(&self, address: &str) -> Option<&SendingIdentity> {
        let address = normalized_mailbox_address(address);
        self.aliases
            .iter()
            .find(|identity| normalized_mailbox_address(&identity.address) == address)
    }

    pub fn is_default_identity(&self, address: &str) -> bool {
        normalized_mailbox_address(&self.default_address)
            == normalized_mailbox_address(address)
    }

    pub fn is_primary_identity(&self, address: &str) -> bool {
        normalized_mailbox_address(&self.primary_address)
            == normalized_mailbox_address(address)
    }

    pub(crate) fn replace_aliases(
        &mut self,
        aliases: Vec<SendingIdentity>,
        default_address: String,
    ) {
        let aliases = aliases
            .into_iter()
            .map(SendingIdentity::normalized)
            .collect::<Vec<_>>();
        let mut addresses = HashSet::new();
        assert!(aliases.iter().all(|identity| {
            let address = normalized_mailbox_address(&identity.address);
            !address.is_empty() && addresses.insert(address)
        }));
        let primary_address = normalized_mailbox_address(&self.primary_address);
        assert!(aliases.iter().any(|identity| {
            normalized_mailbox_address(&identity.address) == primary_address
        }));
        let default_address = normalized_mailbox_address(&default_address);
        let default_address = aliases
            .iter()
            .find(|identity| normalized_mailbox_address(&identity.address) == default_address)
            .expect("the default identity belongs to its mail account")
            .address
            .clone();
        self.aliases = aliases;
        self.default_address = default_address;
    }

    pub(crate) fn rename(&mut self, display_name: String) -> bool {
        let display_name = display_name.trim();
        if display_name.is_empty() {
            return false;
        }
        self.display_name = display_name.to_string();
        true
    }

    pub(crate) fn add_identity(&mut self, identity: SendingIdentity) -> bool {
        let identity = identity.normalized();
        let address = normalized_mailbox_address(&identity.address);
        if address.is_empty() || self.identity(&address).is_some() {
            return false;
        }
        self.aliases.push(identity);
        true
    }

    pub(crate) fn update_identity(
        &mut self,
        original_address: &str,
        display_name: String,
        address: String,
        reply_to: Option<String>,
        signature: Signature,
    ) -> bool {
        let Some(index) = self.aliases.iter().position(|identity| {
            normalized_mailbox_address(&identity.address)
                == normalized_mailbox_address(original_address)
        }) else {
            return false;
        };
        let is_primary = self.is_primary_identity(original_address);
        let address = if is_primary {
            self.aliases[index].address.clone()
        } else {
            address.trim().to_string()
        };
        let normalized = normalized_mailbox_address(&address);
        if normalized.is_empty()
            || self.aliases.iter().enumerate().any(|(other_index, identity)| {
                other_index != index
                    && normalized_mailbox_address(&identity.address) == normalized
            })
        {
            return false;
        }

        let old_address = self.aliases[index].address.clone();
        self.aliases[index] =
            SendingIdentity::new(address.clone(), display_name, reply_to, signature);
        if self.is_default_identity(&old_address) {
            self.default_address = address;
        }
        true
    }

    pub(crate) fn remove_identity(&mut self, address: &str) -> bool {
        let Some(index) = self.aliases.iter().position(|identity| {
            normalized_mailbox_address(&identity.address)
                == normalized_mailbox_address(address)
        }) else {
            return false;
        };
        if self.is_primary_identity(address) || self.is_default_identity(address) {
            return false;
        }
        self.aliases.remove(index);
        true
    }

    pub(crate) fn set_default_identity(&mut self, address: &str) -> bool {
        let Some(address) = self.identity(address).map(|identity| identity.address.clone()) else {
            return false;
        };
        self.default_address = address;
        true
    }
}

fn trimmed_optional(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_string())
    })
}

impl SendingIdentity {
    pub fn new(
        address: String,
        display_name: String,
        reply_to: Option<String>,
        signature: Signature,
    ) -> Self {
        Self {
            address,
            display_name,
            reply_to,
            signature,
        }
        .normalized()
    }

    fn normalized(mut self) -> Self {
        self.address = self.address.trim().to_string();
        self.display_name = self.display_name.trim().to_string();
        self.reply_to = trimmed_optional(self.reply_to);
        self
    }

    pub fn mailbox(&self) -> String {
        let display_name = self.display_name.trim();
        if display_name.is_empty() {
            return self.address.trim().to_string();
        }
        let quoted_name = display_name.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{quoted_name}\" <{}>", self.address.trim())
    }

    pub fn display_name_or_address(&self) -> String {
        if self.display_name.trim().is_empty() {
            self.address.clone()
        } else {
            self.display_name.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MailAccount, MailAccountId, SendingIdentity, Signature};

    fn identity(address: &str, name: &str) -> SendingIdentity {
        SendingIdentity::new(
            address.into(),
            name.into(),
            None,
            Signature::default(),
        )
    }

    fn account() -> MailAccount {
        MailAccount::new(
            MailAccountId("account-1".into()),
            "Account".into(),
            identity("primary@example.com", "Primary"),
        )
    }

    #[test]
    fn mailbox_quotes_provider_display_names_safely() {
        assert_eq!(
            identity("a@example.com", "Doe, Jane").mailbox(),
            "\"Doe, Jane\" <a@example.com>"
        );
        assert_eq!(
            identity("a@example.com", "A \"B\"").mailbox(),
            "\"A \\\"B\\\"\" <a@example.com>"
        );
    }

    #[test]
    fn account_uses_address_as_its_only_identity_key() {
        let mut account = account();
        assert!(account.add_identity(identity("alias@example.com", "Alias")));
        assert!(!account.add_identity(identity("ALIAS@example.com", "Duplicate")));
        assert!(account.set_default_identity("ALIAS@example.com"));
        assert_eq!(account.default_identity().display_name, "Alias");
        assert!(!account.remove_identity("alias@example.com"));
        assert!(account.set_default_identity("primary@example.com"));
        assert!(account.remove_identity("alias@example.com"));
    }

    #[test]
    fn full_model_update_replaces_an_alias_address_without_a_second_key() {
        let mut account = account();
        assert!(account.add_identity(identity("old@example.com", "Alias")));
        assert!(account.set_default_identity("old@example.com"));
        assert!(account.update_identity(
            "old@example.com",
            "Renamed".into(),
            "new@example.com".into(),
            Some("reply@example.com".into()),
            Signature::default(),
        ));
        assert!(account.identity("old@example.com").is_none());
        assert_eq!(account.default_identity().address, "new@example.com");
    }

    #[test]
    fn identity_construction_establishes_shared_text_invariants() {
        let identity = SendingIdentity::new(
            "  alias@example.com  ".into(),
            "  Alias  ".into(),
            Some("   ".into()),
            Signature::default(),
        );

        assert_eq!(identity.address, "alias@example.com");
        assert_eq!(identity.display_name, "Alias");
        assert_eq!(identity.reply_to, None);
    }
}
