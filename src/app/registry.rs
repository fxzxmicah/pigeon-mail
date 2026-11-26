use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures::channel::oneshot;
use gtk::glib;

use crate::app::accounts::{AccountRediscovery, AccountRuntime, RediscoveryInput};
use crate::core::coordinator::MailCoordinator;
use crate::integration::account::AccountCatalog;
use crate::ui::mailbox::{MailboxSessionSnapshot, MailboxViewModel, MainWindow};

pub(super) struct RegistryInvalidations {
    receiver: UnboundedReceiver<()>,
    _monitor_guard: crate::integration::registry::Monitor,
}

struct RegistryRediscovery {
    window: Weak<MainWindow>,
    runtime: AccountRuntime,
    coordinator: MailCoordinator,
    state: RefCell<RediscoveryState>,
}

#[derive(Default)]
struct RediscoveryState {
    schedule: RediscoverySchedule,
    running: bool,
    timer: Option<glib::SourceId>,
}

#[derive(Default)]
struct RediscoverySchedule {
    due: Option<Instant>,
}

impl RediscoverySchedule {
    fn invalidate(&mut self, now: Instant) {
        self.due = Some(now + Duration::from_millis(400));
    }

    fn take_due(&mut self, now: Instant) -> bool {
        if !self.due.is_some_and(|due| now >= due) {
            return false;
        }
        self.due = None;
        true
    }

    fn is_pending(&self) -> bool {
        self.due.is_some()
    }
}

impl RegistryInvalidations {
    pub(super) fn open() -> anyhow::Result<Self> {
        let (sender, receiver) = unbounded();
        let monitor = start_registry_monitor(&sender)?;
        Ok(Self {
            receiver,
            _monitor_guard: monitor,
        })
    }
}

pub(super) fn watch_account_registry(
    window: &Rc<MainWindow>,
    runtime: &AccountRuntime,
    coordinator: &MailCoordinator,
    invalidations: RegistryInvalidations,
) {
    let rediscovery = Rc::new(RegistryRediscovery {
        window: Rc::downgrade(window),
        runtime: runtime.clone(),
        coordinator: coordinator.clone(),
        state: RefCell::new(RediscoveryState::default()),
    });
    glib::spawn_future_local(async move {
        let RegistryInvalidations {
            mut receiver,
            _monitor_guard,
        } = invalidations;
        while receiver.next().await.is_some() {
            if rediscovery.window.upgrade().is_none() {
                break;
            }
            rediscovery
                .state
                .borrow_mut()
                .schedule
                .invalidate(Instant::now());
            rediscovery.schedule();
        }
    });
}

impl RegistryRediscovery {
    fn schedule(self: &Rc<Self>) {
        let due = {
            let mut state = self.state.borrow_mut();
            if let Some(timer) = state.timer.take() {
                timer.remove();
            }
            state.schedule.due
        };
        let Some(due) = due else {
            return;
        };
        let delay = due.saturating_duration_since(Instant::now());
        let rediscovery = Rc::clone(self);
        let timer = glib::timeout_add_local_once(delay, move || {
            rediscovery.begin();
        });
        self.state.borrow_mut().timer = Some(timer);
    }

    fn begin(self: &Rc<Self>) {
        {
            let mut state = self.state.borrow_mut();
            state.timer = None;
            if state.running || self.window.upgrade().is_none() {
                return;
            }
            if !state.schedule.take_due(Instant::now()) {
                drop(state);
                self.schedule();
                return;
            }
            state.running = true;
        }
        let (sender, receiver) = oneshot::channel();
        std::thread::spawn(move || {
            let result = crate::integration::account::discover();
            let _ = sender.send(result);
        });

        let rediscovery = Rc::clone(self);
        glib::spawn_future_local(async move {
            let result = receiver.await;
            let superseded = {
                let mut state = rediscovery.state.borrow_mut();
                state.running = false;
                state.schedule.is_pending()
            };
            let Some(window) = rediscovery.window.upgrade() else {
                return;
            };
            if !superseded {
                match result {
                    Ok(Ok(catalog)) => {
                        rediscovery.apply_catalog(&window, catalog);
                    }
                    Ok(Err(error)) => {
                        crate::logging::report_deferred("eds-account-rediscovery", &error);
                    }
                    Err(_) => {
                        tracing::error!("EDS account rediscovery worker ended without a result");
                    }
                }
            }
            rediscovery.schedule();
        });
    }

    fn apply_catalog(&self, window: &MainWindow, catalog: AccountCatalog) {
        let input = rediscovery_input(window.session_snapshot());
        match self.runtime.apply_discovered_catalog(input, catalog) {
            AccountRediscovery::Unchanged => {}
            AccountRediscovery::Catalog {
                accounts,
                changed_routes,
            } => {
                window.update_account_catalog(accounts);
                self.coordinator
                    .reconnect_changed_foreground_route(&changed_routes);
            }
            AccountRediscovery::Replacement { seed } => {
                window.replace_mailbox(MailboxViewModel::from_account_catalog(
                    seed.accounts,
                    seed.settings,
                    seed.mode,
                ));
            }
        }
    }
}

fn rediscovery_input(mailbox: MailboxSessionSnapshot) -> RediscoveryInput {
    RediscoveryInput::new(
        mailbox.settings,
        mailbox.current_account_id,
        mailbox.accounts,
    )
}

fn start_registry_monitor(
    sender: &UnboundedSender<()>,
) -> anyhow::Result<crate::integration::registry::Monitor> {
    let changes = sender.clone();
    crate::integration::registry::Monitor::start(move || {
        let _ = changes.unbounded_send(());
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_changes_are_debounced_and_claimed_once() {
        let start = Instant::now();
        let mut schedule = RediscoverySchedule::default();
        schedule.invalidate(start);
        schedule.invalidate(start + Duration::from_millis(200));

        assert!(schedule.is_pending());
        assert!(!schedule.take_due(start + Duration::from_millis(599)));
        assert!(schedule.take_due(start + Duration::from_millis(600)));
        assert!(!schedule.is_pending());
        assert!(!schedule.take_due(start + Duration::from_secs(1)));
    }
}
