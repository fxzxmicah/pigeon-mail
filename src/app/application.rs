use adw::prelude::*;
use gtk::glib;
use gtk::{CssProvider, gdk, gio};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::app::accounts::AccountRuntime;
use crate::integration::backend::stub_backend;
use crate::model::account::MailAccountId;
use crate::model::mail::{FolderId, MailtoRequest};
use crate::ui::mailbox::{MailboxViewModel, MainWindow};

const REGISTRY_CHANGE_DEBOUNCE: Duration = Duration::from_millis(400);
const REGISTRY_FAILURE_RETRY: Duration = Duration::from_secs(30);

struct RegistryChanges {
    receiver: mpsc::Receiver<MailAccountId>,
    _monitor_guard: crate::integration::registry::Monitor,
}

struct PendingRediscovery {
    affected_accounts: Vec<MailAccountId>,
    receiver: mpsc::Receiver<anyhow::Result<MailboxViewModel>>,
}

#[derive(Default)]
struct RediscoverySchedule {
    accounts: HashSet<MailAccountId>,
    due: Option<Instant>,
}

impl RediscoverySchedule {
    fn record_change(&mut self, account_id: MailAccountId, now: Instant) {
        self.accounts.insert(account_id);
        self.due = Some(now + REGISTRY_CHANGE_DEBOUNCE);
    }

    fn retry(&mut self, accounts: &[MailAccountId], now: Instant) {
        self.accounts.extend(accounts.iter().cloned());
        self.due.get_or_insert(now + REGISTRY_FAILURE_RETRY);
    }

    fn take_due(&mut self, now: Instant) -> Option<Vec<MailAccountId>> {
        if !self.due.is_some_and(|due| now >= due) {
            return None;
        }
        self.due = None;
        Some(self.accounts.drain().collect())
    }
}

impl RegistryChanges {
    fn open() -> Self {
        let (sender, receiver) = mpsc::channel();
        let monitor = start_registry_monitor(&sender);
        Self {
            receiver,
            _monitor_guard: monitor,
        }
    }
}

struct ApplicationSession {
    runtime: AccountRuntime,
    window: RefCell<Option<MainWindow>>,
}

impl ApplicationSession {
    fn new() -> Self {
        Self {
            runtime: AccountRuntime::new(),
            window: RefCell::new(None),
        }
    }

    fn window(&self, app: &adw::Application) -> MainWindow {
        ensure_main_window(app, &self.window, &self.runtime)
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
        if let Some(window) = self.window.borrow().as_ref() {
            let mailbox = window.mailbox();
            if !mailbox.is_bootstrap_placeholder() {
                self.runtime.save_mailbox(&mailbox);
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
) -> MainWindow {
    if let Some(window) = slot.borrow().as_ref() {
        return window.clone();
    }

    let window = MainWindow::new(app, MailboxViewModel::loading_placeholder(stub_backend()));
    *slot.borrow_mut() = Some(window.clone());

    let mut registry_changes = Some(RegistryChanges::open());
    let (sender, receiver) = mpsc::channel();
    let bootstrap_runtime = AccountRuntime::clone(runtime);
    std::thread::spawn(move || {
        let mailbox = bootstrap_runtime.initial_mailbox();
        let _ = sender.send(mailbox);
    });
    let window_for_bootstrap = window.clone();
    let runtime_for_registry = AccountRuntime::clone(runtime);
    glib::timeout_add_local(Duration::from_millis(50), move || {
        match receiver.try_recv() {
            Ok(mailbox) => {
                window_for_bootstrap.replace_mailbox(mailbox);
                watch_account_registry(
                    &window_for_bootstrap,
                    &runtime_for_registry,
                    registry_changes
                        .take()
                        .expect("registry monitor installed once"),
                );
                glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            Err(mpsc::TryRecvError::Disconnected) => {
                tracing::error!("startup mailbox worker ended without a result");
                window_for_bootstrap.replace_mailbox(AccountRuntime::unavailable_mailbox());
                watch_account_registry(
                    &window_for_bootstrap,
                    &runtime_for_registry,
                    registry_changes
                        .take()
                        .expect("registry monitor installed once"),
                );
                glib::ControlFlow::Break
            }
        }
    });

    window
}

fn watch_account_registry(
    window: &MainWindow,
    runtime: &AccountRuntime,
    registry: RegistryChanges,
) {
    let mut schedule = RediscoverySchedule::default();
    let mut rediscovery = None::<PendingRediscovery>;
    let window = window.clone();
    let runtime = runtime.clone();

    glib::timeout_add_local(Duration::from_millis(100), move || {
        while let Ok(account_id) = registry.receiver.try_recv() {
            schedule.record_change(account_id, Instant::now());
        }

        let mut worker_finished = false;
        if let Some(pending) = rediscovery.as_ref() {
            match pending.receiver.try_recv() {
                Ok(Ok(mailbox)) => {
                    window
                        .replace_mailbox_after_registry_change(mailbox, &pending.affected_accounts);
                    worker_finished = true;
                }
                Ok(Err(error)) => {
                    schedule.retry(&pending.affected_accounts, Instant::now());
                    crate::logging::report_deferred("eds-account-rediscovery", &error);
                    worker_finished = true;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    schedule.retry(&pending.affected_accounts, Instant::now());
                    tracing::error!("EDS account rediscovery worker ended without a result");
                    worker_finished = true;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if worker_finished {
            rediscovery = None;
        }

        if rediscovery.is_none()
            && let Some(changed_accounts) = schedule.take_due(Instant::now())
        {
            let (settings, backend) = {
                let mailbox = window.mailbox();
                (
                    runtime.settings_for_mailbox(&mailbox),
                    mailbox.retained_backend(),
                )
            };
            let runtime = runtime.clone();
            let (sender, receiver) = mpsc::channel();
            let affected_accounts = changed_accounts.clone();
            std::thread::spawn(move || {
                let result = runtime.rediscover_mailbox(settings, backend, &changed_accounts);
                let _ = sender.send(result);
            });
            rediscovery = Some(PendingRediscovery {
                affected_accounts,
                receiver,
            });
        }

        glib::ControlFlow::Continue
    });
}

fn start_registry_monitor(
    sender: &mpsc::Sender<MailAccountId>,
) -> crate::integration::registry::Monitor {
    let sender = sender.clone();
    crate::integration::registry::Monitor::start(
        move |account_id| {
            let _ = sender.send(MailAccountId(account_id));
        },
        |error| crate::logging::report_deferred("eds-registry-monitor", &error),
    )
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
    use super::{RediscoverySchedule, parse_mailto_open_uri};
    use crate::model::account::MailAccountId;
    use std::time::{Duration, Instant};

    #[test]
    fn registry_changes_are_debounced_and_claimed_once() {
        let start = Instant::now();
        let mut schedule = RediscoverySchedule::default();
        schedule.record_change(MailAccountId("account-1".into()), start);
        schedule.record_change(
            MailAccountId("account-2".into()),
            start + Duration::from_millis(200),
        );

        assert!(
            schedule
                .take_due(start + Duration::from_millis(599))
                .is_none()
        );
        let mut accounts = schedule
            .take_due(start + Duration::from_millis(600))
            .expect("the last change should release the batch");
        accounts.sort_by(|left, right| left.0.cmp(&right.0));
        assert_eq!(
            accounts,
            [
                MailAccountId("account-1".into()),
                MailAccountId("account-2".into())
            ]
        );
        assert!(schedule.take_due(start + Duration::from_secs(1)).is_none());
    }

    #[test]
    fn failed_rediscovery_preserves_accounts_and_retries_later() {
        let start = Instant::now();
        let mut schedule = RediscoverySchedule::default();
        let accounts = [MailAccountId("account-1".into())];
        schedule.retry(&accounts, start);

        assert!(schedule.take_due(start + Duration::from_secs(29)).is_none());
        assert_eq!(
            schedule.take_due(start + Duration::from_secs(30)),
            Some(Vec::from(accounts))
        );
    }

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

        assert_eq!(
            crate::config::DETAILED_ACTION_SHOW_FOLDER,
            format!("app.{}", crate::config::ACTION_SHOW_FOLDER)
        );

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
