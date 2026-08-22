mod app;
mod config;
mod core;
mod failure;
mod integration;
mod logging;
mod model;
mod ui;

use app::application::Application;

fn main() -> glib::ExitCode {
    logging::init();

    let app = Application::new();
    app.run()
}
