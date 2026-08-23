use glib;
use serde::{Deserialize, Serialize};

use crate::model::mail::plain_text_to_html;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MailAccountId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AliasId(pub String);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailAccount {
    pub id: MailAccountId,
    pub display_name: String,
    pub aliases: Vec<SendingIdentity>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendingIdentity {
    pub id: AliasId,
    pub address: String,
    pub display_name: String,
    pub reply_to: Option<String>,
    pub signature_html: String,
    pub signature_text: String,
    pub is_default: bool,
    pub is_primary_address: bool,
}

impl MailAccount {
    pub fn selector_label(&self) -> String {
        self.display_name.clone()
    }

    pub fn default_identity(&self) -> Option<&SendingIdentity> {
        self.aliases
            .iter()
            .find(|identity| identity.is_default)
            .or_else(|| self.aliases.first())
    }

    pub fn primary_identity(&self) -> Option<&SendingIdentity> {
        self.aliases
            .iter()
            .find(|identity| identity.is_primary_address)
    }

    pub fn primary_identity_mut(&mut self) -> Option<&mut SendingIdentity> {
        self.aliases
            .iter_mut()
            .find(|identity| identity.is_primary_address)
    }

    pub fn primary_or_first_identity(&self) -> Option<&SendingIdentity> {
        self.primary_identity().or_else(|| self.aliases.first())
    }
}

impl SendingIdentity {
    pub fn mailbox(&self) -> String {
        let display_name = self.display_name.trim();
        if display_name.is_empty() {
            return self.address.trim().to_string();
        }
        let quoted_name = display_name.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{quoted_name}\" <{}>", self.address.trim())
    }

    pub fn new(
        account_id: &str,
        address: String,
        display_name: String,
        reply_to: Option<String>,
        signature_html: String,
        signature_text: String,
        is_default: bool,
    ) -> Self {
        let signature_html = normalized_signature_html(signature_html, &signature_text);
        Self {
            id: AliasId(format!("{account_id}:alias:{}", glib::uuid_string_random())),
            address,
            display_name,
            reply_to,
            signature_html,
            signature_text,
            is_default,
            is_primary_address: false,
        }
    }

    pub fn with_id(
        id: AliasId,
        address: String,
        display_name: String,
        reply_to: Option<String>,
        signature_html: String,
        signature_text: String,
        is_default: bool,
        is_primary_address: bool,
    ) -> Self {
        let signature_html = normalized_signature_html(signature_html, &signature_text);
        Self {
            id,
            address,
            display_name,
            reply_to,
            signature_html,
            signature_text,
            is_default,
            is_primary_address,
        }
    }

    pub fn display_name_or_address(&self) -> String {
        if self.display_name.trim().is_empty() {
            self.address.clone()
        } else {
            self.display_name.clone()
        }
    }
}

fn normalized_signature_html(signature_html: String, signature_text: &str) -> String {
    if signature_html.trim().is_empty() && !signature_text.is_empty() {
        plain_text_to_html(signature_text)
    } else {
        signature_html
    }
}

#[cfg(test)]
mod tests {
    use super::{AliasId, SendingIdentity};

    fn identity(display_name: &str) -> SendingIdentity {
        SendingIdentity::with_id(
            AliasId("alias-1".into()),
            "a@example.com".into(),
            display_name.into(),
            None,
            String::new(),
            String::new(),
            true,
            false,
        )
    }

    #[test]
    fn mailbox_quotes_provider_display_names_safely() {
        assert_eq!(
            identity("Doe, Jane").mailbox(),
            "\"Doe, Jane\" <a@example.com>"
        );
        assert_eq!(
            identity("A \"B\"").mailbox(),
            "\"A \\\"B\\\"\" <a@example.com>"
        );
        assert_eq!(identity("").mailbox(), "a@example.com");
    }

    #[test]
    fn identity_construction_preserves_rich_signatures_and_normalizes_plain_ones() {
        let rich = SendingIdentity::with_id(
            AliasId("rich".into()),
            "rich@example.test".into(),
            "Rich".into(),
            None,
            "<strong>Rich</strong>".into(),
            "Plain".into(),
            false,
            false,
        );
        assert_eq!(rich.signature_html, "<strong>Rich</strong>");

        let plain = SendingIdentity::with_id(
            AliasId("plain".into()),
            "plain@example.test".into(),
            "Plain".into(),
            None,
            String::new(),
            "A < B\nSecond line".into(),
            false,
            false,
        );
        assert_eq!(plain.signature_html, "A &lt; B<br>Second line");
    }

    #[test]
    fn identity_role_is_explicit_and_independent_of_its_stable_id() {
        let primary = SendingIdentity::with_id(
            AliasId("stable-random-id".into()),
            "primary@example.test".into(),
            "Primary".into(),
            None,
            String::new(),
            String::new(),
            true,
            true,
        );
        let alias = SendingIdentity::with_id(
            AliasId("looks-like:primary".into()),
            "alias@example.test".into(),
            "Alias".into(),
            None,
            String::new(),
            String::new(),
            false,
            false,
        );

        assert!(primary.is_primary_address);
        assert!(!alias.is_primary_address);
    }
}
