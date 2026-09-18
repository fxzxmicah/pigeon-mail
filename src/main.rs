mod app;
mod config;
mod core;
mod failure;
mod integration;
mod i18n;
mod logging;
mod model;
mod ui;

fn main() -> glib::ExitCode {
    i18n::init().expect("failed to initialize translations");
    logging::init();
    app::application::run()
}
