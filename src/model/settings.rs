use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub selected_account_id: Option<String>,
    pub prefer_html_view: bool,
    pub account_profiles: Vec<AccountProfile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountProfile {
    pub account_id: String,
    pub account_name: String,
    pub aliases: Vec<AliasProfile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AliasProfile {
    pub alias_id: String,
    pub username: String,
    pub address: String,
    pub reply_to: Option<String>,
    pub signature_text: String,
    pub is_default: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            selected_account_id: None,
            prefer_html_view: true,
            account_profiles: Vec::new(),
        }
    }
}
