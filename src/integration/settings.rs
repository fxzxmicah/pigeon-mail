use std::path::{Path, PathBuf};

use gio::prelude::*;

use crate::model::settings::{AccountProfile, AppSettings};

const KEY_SELECTED_ACCOUNT_ID: &str = "selected-account-id";
const KEY_PREFER_HTML_VIEW: &str = "prefer-html-view";

#[derive(Clone)]
pub struct SettingsStore {
    metadata_path: PathBuf,
}

impl SettingsStore {
    pub fn new() -> Self {
        Self {
            metadata_path: crate::config::config_file("accounts.json"),
        }
    }

    pub fn load(&self) -> anyhow::Result<AppSettings> {
        let gsettings = application_settings();
        let selected_account_id = match gsettings.string(KEY_SELECTED_ACCOUNT_ID).as_str() {
            "" => None,
            value => Some(value.to_string()),
        };

        Ok(AppSettings {
            selected_account_id,
            prefer_html_view: gsettings.boolean(KEY_PREFER_HTML_VIEW),
            account_profiles: crate::integration::json::load_vec(&self.metadata_path)?,
        })
    }

    pub fn save(&self, settings: &AppSettings) -> anyhow::Result<()> {
        ensure_account_profiles_readable(&self.metadata_path)?;
        save_account_profiles(&self.metadata_path, &settings.account_profiles)?;
        let gsettings = application_settings();
        gsettings.set_string(
            KEY_SELECTED_ACCOUNT_ID,
            settings.selected_account_id.as_deref().unwrap_or(""),
        )?;
        gsettings.set_boolean(KEY_PREFER_HTML_VIEW, settings.prefer_html_view)?;
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

fn save_account_profiles(path: &Path, profiles: &[AccountProfile]) -> anyhow::Result<()> {
    crate::integration::json::save_slice(path, profiles)
}

fn ensure_account_profiles_readable(path: &Path) -> anyhow::Result<()> {
    crate::integration::json::load_vec::<AccountProfile>(path).map(|_| ())
}
