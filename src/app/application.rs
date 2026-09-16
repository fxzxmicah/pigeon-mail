use adw::prelude::*;
use futures::channel::oneshot;
use gtk::glib;
use gtk::{CssProvider, gdk, gio};
use std::cell::RefCell;
use std::rc::Rc;

use crate::app::accounts::{AccountBootstrap, AccountRuntime};
use crate::app::registry::{RegistryInvalidations, watch_account_registry};
use crate::core::coordinator::MailCoordinator;
use crate::model::account::MailAccountId;
use crate::model::mail::{FolderId, MailtoRequest};
use crate::ui::mailbox::{MailboxViewModel, MainWindow};

struct ApplicationSession {
    runtime: AccountRuntime,
    coordinator: MailCoordinator,
    window: RefCell<Option<MainWindow>>,
}

impl ApplicationSession {
    fn new() -> Self {
        let runtime = AccountRuntime::new();
        Self {
            coordinator: MailCoordinator::with_router(runtime.backend_router()),
            runtime,
            window: RefCell::new(None),
        }
    }

    fn window(&self, app: &adw::Application) -> MainWindow {
        ensure_main_window(app, &self.window, &self.runtime, &self.coordinator)
    }

    fn present(&self, app: &adw::Application) -> MainWindow {
        let window = self.window(app);
        window.present();
        window
    }

    fn compose(&self, app: &adw::Application, request: MailtoRequest) {
        self.present(app).open_mailto(request);
    }

    fn show_folder(&self, app: &adw::Application, account_id: MailAccountId, folder_id: FolderId) {
        let window = self.window(app);
        window.show_folder(account_id, folder_id);
        window.present();
    }

    fn save(&self) {
        let window = self.window.borrow().clone();
        if let Some(window) = window {
            if let Some(settings) = window.settings_for_persistence() {
                self.runtime.save_preferences(&settings);
            }
        }
    }
}

pub fn run() -> glib::ExitCode {
    build_application().run()
}

fn build_application() -> adw::Application {
    let app = adw::Application::builder()
        .application_id(crate::config::APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();
    app.set_accels_for_action("win.compose", &["<Primary>n"]);
    app.set_accels_for_action("win.search", &["<Primary>f"]);
    app.set_accels_for_action("win.preferences", &["<Primary>comma"]);
    app.set_accels_for_action("win.back", &["<Alt>Left"]);
    let session = Rc::new(ApplicationSession::new());
    install_actions(&app, &session);

    app.connect_startup(|_| {
        load_app_css();
    });

    let session_for_activate = Rc::clone(&session);
    app.connect_activate(move |app| {
        session_for_activate.present(app);
    });

    let session_for_open = Rc::clone(&session);
    app.connect_open(move |app, files, _| {
        let mut opened = false;
        for file in files {
            match parse_mailto_open_uri(file.uri().as_str()) {
                Ok(request) => {
                    session_for_open.compose(app, request);
                    opened = true;
                }
                Err(error) => crate::logging::report_invalid_input("mailto-open", &error),
            }
        }
        if !opened {
            session_for_open.present(app);
        }
    });

    let session_for_shutdown = session;
    app.connect_shutdown(move |_| {
        session_for_shutdown.save();
    });

    app
}

fn install_actions(app: &adw::Application, session: &Rc<ApplicationSession>) {
    let app_for_compose = app.downgrade();
    let session_for_compose = Rc::clone(session);
    let compose_action = gio::SimpleAction::new(crate::config::ACTION_COMPOSE, None);
    compose_action.connect_activate(move |_, _| {
        if let Some(app) = app_for_compose.upgrade() {
            session_for_compose.compose(&app, Default::default());
        }
    });
    app.add_action(&compose_action);

    let app_for_show_folder = app.downgrade();
    let session_for_show_folder = Rc::clone(session);
    let show_folder_action = gio::SimpleAction::new(
        crate::config::ACTION_SHOW_FOLDER,
        Some(glib::VariantTy::new("(ss)").expect("valid folder action parameter type")),
    );
    show_folder_action.connect_activate(move |_, parameter| {
        let Some((account_id, folder_id)) =
            parameter.and_then(|value| value.get::<(String, String)>())
        else {
            return;
        };
        if let Some(app) = app_for_show_folder.upgrade() {
            session_for_show_folder.show_folder(
                &app,
                MailAccountId(account_id),
                FolderId(folder_id),
            );
        }
    });
    app.add_action(&show_folder_action);
}

fn parse_mailto_open_uri(uri: &str) -> anyhow::Result<MailtoRequest> {
    // GApplication exposes Open requests as GFiles, which render an opaque
    // `mailto:recipient` URI as `mailto:///recipient`.
    let canonical = uri
        .split_once(':')
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("mailto"))
        .and_then(|(_, remainder)| remainder.strip_prefix("///"))
        .map(|recipient| format!("mailto:{recipient}"));
    crate::app::mailto::parse(canonical.as_deref().unwrap_or(uri))
}

fn ensure_main_window(
    app: &adw::Application,
    slot: &RefCell<Option<MainWindow>>,
    runtime: &AccountRuntime,
    coordinator: &MailCoordinator,
) -> MainWindow {
    let existing_window = slot.borrow().clone();
    if let Some(window) = existing_window {
        return window;
    }

    let window = MainWindow::new(
        app,
        MailboxViewModel::loading_placeholder(),
        coordinator.clone(),
    );
    *slot.borrow_mut() = Some(window.clone());
    install_mail_refresh_triggers(coordinator);

    let (sender, receiver) = oneshot::channel();
    let bootstrap_runtime = AccountRuntime::clone(runtime);
    std::thread::spawn(move || {
        let registry_invalidations = RegistryInvalidations::open();
        let result = bootstrap_runtime.initial_catalog();
        let _ = sender.send((registry_invalidations, result));
    });
    let window_for_bootstrap = window.clone();
    let runtime_for_registry = AccountRuntime::clone(runtime);
    let coordinator_for_registry = coordinator.clone();
    glib::spawn_future_local(async move {
        match receiver.await {
            Ok((registry_invalidations, bootstrap)) => {
                let seed = match bootstrap {
                    AccountBootstrap::Ready(seed) => seed,
                    AccountBootstrap::Unavailable { seed, error } => {
                        crate::logging::report_failure("mailbox-initialization", &error);
                        seed
                    }
                };
                window_for_bootstrap.replace_mailbox(MailboxViewModel::from_account_catalog(
                    seed.accounts,
                    seed.settings,
                    seed.mode,
                ));
                match registry_invalidations {
                    Ok(registry_invalidations) => watch_account_registry(
                        &window_for_bootstrap,
                        &runtime_for_registry,
                        &coordinator_for_registry,
                        registry_invalidations,
                    ),
                    Err(error) => {
                        crate::logging::report_deferred("eds-registry-monitor", &error)
                    }
                }
            }
            Err(_) => {
                tracing::error!("startup mailbox worker ended without a result");
            }
        }
    });

    window
}

fn install_mail_refresh_triggers(coordinator: &MailCoordinator) {
    let periodic = coordinator.clone();
    glib::timeout_add_seconds_local(180, move || {
        periodic.request_foreground_refresh();
        glib::ControlFlow::Continue
    });

    let network = coordinator.clone();
    gio::NetworkMonitor::default().connect_network_changed(move |_, available| {
        if available {
            network.request_foreground_reconnect();
        }
    });
}

fn load_app_css() {
    let provider = CssProvider::new();
    provider.load_from_string(include_str!("../ui/style.css"));

    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        #[cfg(debug_assertions)]
        gtk::IconTheme::for_display(&display)
            .add_search_path(concat!(env!("CARGO_MANIFEST_DIR"), "/data/icons"));
    }
}

#[cfg(test)]
mod tests {
    use super::parse_mailto_open_uri;

    #[test]
    fn accepts_standard_and_gfile_mailto_uris() {
        let standard = parse_mailto_open_uri("mailto:person@example.test").unwrap();
        let transported =
            parse_mailto_open_uri("MAILTO:///person@example.test?subject=Hello").unwrap();

        assert_eq!(standard.to, ["person@example.test"]);
        assert_eq!(transported.to, ["person@example.test"]);
        assert_eq!(transported.subject, "Hello");
        assert_eq!(
            parse_mailto_open_uri("mailto:///").unwrap(),
            Default::default()
        );
    }

    #[test]
    fn rejects_other_schemes_and_transported_hierarchical_mailto_uris() {
        assert!(parse_mailto_open_uri("https:///example.test").is_err());
        assert!(parse_mailto_open_uri("mailto://////person@example.test").is_err());
        assert!(parse_mailto_open_uri("mailto://///person@example.test/").is_err());
    }

    #[test]
    fn desktop_compose_and_dbus_service_match_application_actions() {
        let desktop_entry = include_str!("../../data/org.gnome.pigeon.desktop");
        let service = include_str!("../../data/org.gnome.pigeon.service.in");
        let action_group = format!("[Desktop Action {}]", crate::config::ACTION_COMPOSE);

        assert!(desktop_entry.lines().any(|line| {
            line.strip_prefix("Actions=").is_some_and(|actions| {
                actions
                    .split(';')
                    .any(|name| name == crate::config::ACTION_COMPOSE)
            })
        }));
        assert!(
            desktop_entry
                .lines()
                .any(|line| line == action_group.as_str())
        );
        assert!(
            desktop_entry
                .lines()
                .any(|line| line == "Exec=pigeon mailto:")
        );
        assert!(
            desktop_entry
                .lines()
                .any(|line| line == "DBusActivatable=true")
        );
        let service_name = format!("Name={}", crate::config::APP_ID);
        assert!(service.lines().any(|line| line == service_name.as_str()));
        assert!(
            service
                .lines()
                .any(|line| line == "Exec=@bindir@/pigeon --gapplication-service")
        );
    }
}
