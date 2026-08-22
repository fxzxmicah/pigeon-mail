use adw::prelude::*;
use gtk::glib;
use gtk::{CssProvider, gdk, gio};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::app::accounts::AccountRuntime;
use crate::integration::backend::stub_backend;
use crate::ui::mailbox::{MailboxViewModel, MainWindow};

struct RegistryChanges {
    receiver: std::sync::mpsc::Receiver<String>,
    _monitor: crate::integration::registry::Monitor,
}

impl RegistryChanges {
    fn open() -> Self {
        let (sender, receiver) = std::sync::mpsc::channel();
        let monitor = start_registry_monitor(&sender);
        Self {
            receiver,
            _monitor: monitor,
        }
    }
}

pub struct Application {
    inner: adw::Application,
}

impl Application {
    pub fn new() -> Self {
        let inner = adw::Application::builder()
            .application_id(crate::config::APP_ID)
            .flags(gio::ApplicationFlags::HANDLES_OPEN)
            .build();
        inner.set_accels_for_action("win.compose", &["<Primary>n"]);
        inner.set_accels_for_action("win.preferences", &["<Primary>comma"]);
        inner.set_accels_for_action("win.back", &["<Alt>Left"]);
        let runtime = Rc::new(AccountRuntime::new());
        let window = Rc::new(RefCell::new(None::<MainWindow>));

        inner.connect_startup(|_| {
            load_app_css();
        });

        let window_for_activate = Rc::clone(&window);
        let runtime_for_activate = Rc::clone(&runtime);
        inner.connect_activate(move |app| {
            let window =
                ensure_main_window(app, &window_for_activate, runtime_for_activate.as_ref());
            window.present();
        });

        let window_for_open = Rc::clone(&window);
        let runtime_for_open = Rc::clone(&runtime);
        inner.connect_open(move |app, files, _hint| {
            let window = ensure_main_window(app, &window_for_open, runtime_for_open.as_ref());
            window.present();
            for file in files {
                match crate::app::mailto::parse(file.uri().as_str()) {
                    Ok(request) => {
                        if !window.open_mailto(request) {
                            tracing::warn!(
                                "mailto request could not be opened without a sending identity"
                            );
                        }
                    }
                    Err(error) => crate::logging::report_failure("mailto-open", &error),
                }
            }
        });

        let runtime_for_shutdown = Rc::clone(&runtime);
        let window_for_shutdown = Rc::clone(&window);
        inner.connect_shutdown(move |_| {
            if let Some(window) = window_for_shutdown.borrow().as_ref() {
                let mailbox = window.mailbox();
                if !mailbox.is_bootstrap_placeholder() {
                    runtime_for_shutdown.save_mailbox(&mailbox);
                }
            }
        });

        Self { inner }
    }

    pub fn run(self) -> glib::ExitCode {
        self.inner.run()
    }
}

fn ensure_main_window(
    app: &adw::Application,
    slot: &Rc<RefCell<Option<MainWindow>>>,
    runtime: &AccountRuntime,
) -> MainWindow {
    if let Some(window) = slot.borrow().as_ref() {
        return window.clone();
    }

    let window = MainWindow::new(app, MailboxViewModel::loading_placeholder(stub_backend()));
    *slot.borrow_mut() = Some(window.clone());

    let mut registry_changes = Some(RegistryChanges::open());
    let (sender, receiver) = std::sync::mpsc::channel();
    let bootstrap_runtime = AccountRuntime::clone(runtime);
    std::thread::spawn(move || {
        let mailbox = bootstrap_runtime.initial_mailbox();
        let _ = sender.send(mailbox);
    });
    let window_for_bootstrap = window.clone();
    let runtime_for_registry = AccountRuntime::clone(runtime);
    glib::timeout_add_local(
        std::time::Duration::from_millis(50),
        move || match receiver.try_recv() {
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
            Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                tracing::error!("startup mailbox worker ended without a result");
                window_for_bootstrap.replace_mailbox(MailboxViewModel::from_snapshot(
                    vec![crate::integration::stub::stub_account()],
                    crate::model::settings::AppSettings::default(),
                    stub_backend(),
                    crate::model::event::AccountMailboxSnapshot {
                        mode: crate::model::mail::MailboxMode::StubUnavailable,
                        folders: Vec::new(),
                        conversations: Vec::new(),
                    },
                ));
                watch_account_registry(
                    &window_for_bootstrap,
                    &runtime_for_registry,
                    registry_changes
                        .take()
                        .expect("registry monitor installed once"),
                );
                glib::ControlFlow::Break
            }
        },
    );

    window
}

fn watch_account_registry(
    window: &MainWindow,
    runtime: &AccountRuntime,
    registry: RegistryChanges,
) {
    const CHANGE_DEBOUNCE: Duration = Duration::from_millis(400);

    let mut pending_change = None::<Instant>;
    let mut changed_accounts = HashSet::new();
    let mut rediscovery: Option<
        std::sync::mpsc::Receiver<(
            Vec<crate::model::account::MailAccountId>,
            anyhow::Result<MailboxViewModel>,
        )>,
    > = None;
    let window = window.clone();
    let runtime = runtime.clone();

    glib::timeout_add_local(Duration::from_millis(100), move || {
        while let Ok(account_id) = registry.receiver.try_recv() {
            changed_accounts.insert(crate::model::account::MailAccountId(account_id));
            pending_change = Some(Instant::now());
        }

        let mut worker_finished = false;
        if let Some(receiver) = rediscovery.as_ref() {
            match receiver.try_recv() {
                Ok((affected_accounts, Ok(mailbox))) => {
                    window.replace_mailbox_after_registry_change(mailbox, &affected_accounts);
                    worker_finished = true;
                }
                Ok((affected_accounts, Err(error))) => {
                    changed_accounts.extend(affected_accounts);
                    crate::logging::report_deferred("eds-account-rediscovery", &error);
                    worker_finished = true;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    tracing::error!("EDS account rediscovery worker ended without a result");
                    worker_finished = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if worker_finished {
            rediscovery = None;
        }

        if rediscovery.is_none()
            && pending_change.is_some_and(|changed_at| changed_at.elapsed() >= CHANGE_DEBOUNCE)
        {
            pending_change = None;
            let changed_accounts = changed_accounts.drain().collect::<Vec<_>>();
            let (settings, backend) = {
                let mailbox = window.mailbox();
                (
                    runtime.settings_for_mailbox(&mailbox),
                    mailbox
                        .current_account_id()
                        .filter(|account_id| account_id.0 != "local-stub")
                        .map(|_| mailbox.backend()),
                )
            };
            let runtime = runtime.clone();
            let (sender, receiver) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let result = runtime.rediscover_mailbox(settings, backend, &changed_accounts);
                let _ = sender.send((changed_accounts, result));
            });
            rediscovery = Some(receiver);
        }

        glib::ControlFlow::Continue
    });
}

fn start_registry_monitor(
    sender: &std::sync::mpsc::Sender<String>,
) -> crate::integration::registry::Monitor {
    let sender = sender.clone();
    crate::integration::registry::Monitor::start(
        move |account_id| {
            let _ = sender.send(account_id);
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
