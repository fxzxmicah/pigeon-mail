#[cfg(debug_assertions)]
use std::path::Path;

use gio::prelude::*;

use crate::model::account::MailAccountId;
use crate::model::settings::AppSettings;

const KEY_SELECTED_ACCOUNT_ID: &str = "selected-account-id";
const KEY_PREFER_HTML_VIEW: &str = "prefer-html-view";

#[derive(Clone)]
pub struct SettingsStore;

impl SettingsStore {
    pub fn new() -> Self {
        Self
    }

    pub fn load(&self) -> AppSettings {
        let gsettings = application_settings();
        let selected_account_id = match gsettings.string(KEY_SELECTED_ACCOUNT_ID).as_str() {
            "" => None,
            value => Some(MailAccountId(value.to_string())),
        };
        AppSettings {
            selected_account_id,
            prefer_html_view: gsettings.boolean(KEY_PREFER_HTML_VIEW),
        }
    }

    pub fn save_preferences(
        &self,
        settings: &AppSettings,
    ) -> anyhow::Result<()> {
        let gsettings = application_settings();
        let selected_account = gsettings.set_string(
            KEY_SELECTED_ACCOUNT_ID,
            settings
                .selected_account_id
                .as_ref()
                .map(|account_id| account_id.0.as_str())
                .unwrap_or(""),
        );
        let body_view = gsettings.set_boolean(KEY_PREFER_HTML_VIEW, settings.prefer_html_view);

        selected_account?;
        body_view?;
        Ok(())
    }
}

fn application_settings() -> gio::Settings {
    #[cfg(debug_assertions)]
    {
        development_settings()
    }

    #[cfg(not(debug_assertions))]
    {
        gio::Settings::new(crate::config::APP_ID)
    }
}

#[cfg(debug_assertions)]
fn development_settings() -> gio::Settings {
    let schema_directory = Path::new(env!("OUT_DIR"));
    let source = gio::SettingsSchemaSource::from_directory(
        schema_directory,
        gio::SettingsSchemaSource::default().as_ref(),
        false,
    )
    .expect("the build script must compile the development GSettings schema");
    let schema = source
        .lookup(crate::config::APP_ID, false)
        .expect("the development GSettings schema must contain the application ID");

    gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None)
}
