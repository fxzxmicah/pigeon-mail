mod app;
mod config;
mod core;
mod failure;
mod integration;
mod logging;
mod model;
mod ui;

fn main() -> glib::ExitCode {
    logging::init();
    app::application::run()
}
