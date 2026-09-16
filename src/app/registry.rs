use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures::channel::oneshot;
use gtk::glib;

use crate::app::accounts::{AccountRediscovery, AccountRuntime, RediscoveryInput};
use crate::core::coordinator::MailCoordinator;
use crate::ui::mailbox::{MailboxSessionSnapshot, MailboxViewModel, MainWindow};

pub(super) struct RegistryInvalidations {
    receiver: UnboundedReceiver<()>,
    _monitor_guard: crate::integration::registry::Monitor,
}

#[derive(Default)]
struct RegistryRediscovery {
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
    window: &MainWindow,
    runtime: &AccountRuntime,
    coordinator: &MailCoordinator,
    invalidations: RegistryInvalidations,
) {
    let rediscovery = Rc::new(RefCell::new(RegistryRediscovery::default()));
    let window = window.clone();
    let runtime = runtime.clone();
    let coordinator = coordinator.clone();
    glib::spawn_future_local(async move {
        let RegistryInvalidations {
            mut receiver,
            _monitor_guard,
        } = invalidations;
        while receiver.next().await.is_some() {
            let now = Instant::now();
            rediscovery.borrow_mut().schedule.invalidate(now);
            schedule_registry_rediscovery(&rediscovery, &window, &runtime, &coordinator);
        }
    });
}

fn schedule_registry_rediscovery(
    state: &Rc<RefCell<RegistryRediscovery>>,
    window: &MainWindow,
    runtime: &AccountRuntime,
    coordinator: &MailCoordinator,
) {
    let due = {
        let mut state = state.borrow_mut();
        if let Some(timer) = state.timer.take() {
            timer.remove();
        }
        state.schedule.due
    };
    let Some(due) = due else {
        return;
    };
    let delay = due.saturating_duration_since(Instant::now());
    let state_for_timer = Rc::clone(state);
    let window_for_timer = window.clone();
    let runtime_for_timer = runtime.clone();
    let coordinator_for_timer = coordinator.clone();
    let timer = glib::timeout_add_local_once(delay, move || {
        begin_registry_rediscovery(
            &state_for_timer,
            &window_for_timer,
            &runtime_for_timer,
            &coordinator_for_timer,
        );
    });
    state.borrow_mut().timer = Some(timer);
}

fn begin_registry_rediscovery(
    state: &Rc<RefCell<RegistryRediscovery>>,
    window: &MainWindow,
    runtime: &AccountRuntime,
    coordinator: &MailCoordinator,
) {
    {
        let mut rediscovery = state.borrow_mut();
        rediscovery.timer = None;
        if rediscovery.running {
            return;
        }
        if !rediscovery.schedule.take_due(Instant::now()) {
            drop(rediscovery);
            schedule_registry_rediscovery(state, window, runtime, coordinator);
            return;
        }
        rediscovery.running = true;
    }
    let (sender, receiver) = oneshot::channel();
    std::thread::spawn(move || {
        let result = crate::integration::account::discover();
        let _ = sender.send(result);
    });

    let state_for_completion = Rc::clone(state);
    let window_for_completion = window.clone();
    let runtime_for_completion = runtime.clone();
    let coordinator_for_completion = coordinator.clone();
    glib::spawn_future_local(async move {
        let result = receiver.await;
        let superseded = {
            let mut rediscovery = state_for_completion.borrow_mut();
            rediscovery.running = false;
            rediscovery.schedule.is_pending()
        };
        match result {
            _ if superseded => {}
            Ok(Ok(catalog)) => {
                let input = rediscovery_input(window_for_completion.session_snapshot());
                match runtime_for_completion.apply_discovered_catalog(input, catalog) {
                    AccountRediscovery::Unchanged => {}
                    AccountRediscovery::Catalog {
                        accounts,
                        changed_routes,
                    } => {
                        window_for_completion.update_account_catalog(accounts);
                        coordinator_for_completion
                            .reconnect_changed_foreground_route(&changed_routes);
                    }
                    AccountRediscovery::Replacement { seed } => {
                        window_for_completion.replace_mailbox(
                            MailboxViewModel::from_account_catalog(
                                seed.accounts,
                                seed.settings,
                                seed.mode,
                            ),
                        )
                    }
                }
            }
            Ok(Err(error)) => {
                crate::logging::report_deferred("eds-account-rediscovery", &error);
            }
            Err(_) => {
                tracing::error!("EDS account rediscovery worker ended without a result");
            }
        }
        schedule_registry_rediscovery(
            &state_for_completion,
            &window_for_completion,
            &runtime_for_completion,
            &coordinator_for_completion,
        );
    });
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
