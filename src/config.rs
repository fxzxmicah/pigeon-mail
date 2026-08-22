use std::path::PathBuf;

pub const APP_ID: &str = "org.gnome.pigeon";
pub const APP_NAME: &str = "Pigeon Mail";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const PROJECT_URL: &str = env!("CARGO_PKG_REPOSITORY");
pub const ISSUE_URL: &str = concat!(env!("CARGO_PKG_REPOSITORY"), "/issues");
const STORAGE_DIRECTORY: &str = "pigeon";

pub fn config_file(name: &str) -> PathBuf {
    glib::user_config_dir().join(STORAGE_DIRECTORY).join(name)
}

#[cfg(not(test))]
pub fn data_file(name: &str) -> PathBuf {
    glib::user_data_dir().join(STORAGE_DIRECTORY).join(name)
}
