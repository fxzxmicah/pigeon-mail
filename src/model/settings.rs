use crate::model::account::MailAccountId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppSettings {
    pub selected_account_id: Option<MailAccountId>,
    pub prefer_html_view: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            selected_account_id: None,
            prefer_html_view: true,
        }
    }
}
