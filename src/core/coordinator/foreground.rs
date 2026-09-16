use std::sync::{Arc, Mutex, Weak};

use crate::core::mail::AccountMailService;
use crate::integration::backend::BackendChangeMonitor;
use crate::model::account::MailAccountId;
use crate::model::event::{MailEvent, RequestId};

use super::{MailCoordinator, MailCoordinatorState};

#[derive(Clone)]
pub(super) struct RemoteChangeTrigger {
    state: Weak<MailCoordinatorState>,
}

pub(super) struct ForegroundAccount {
    state: Mutex<ForegroundAccountState>,
}

struct ForegroundAccountState {
    generation: u64,
    account_id: Option<MailAccountId>,
    activation_request: Option<RequestId>,
    monitor: ChangeMonitorPhase,
}

enum ChangeMonitorPhase {
    Stopped,
    Starting,
    Active { _monitor: BackendChangeMonitor },
}

#[derive(Clone, Copy)]
pub(super) enum MonitorPolicy {
    Keep,
    Restart,
}

impl ForegroundAccount {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(ForegroundAccountState {
                generation: 0,
                account_id: None,
                activation_request: None,
                monitor: ChangeMonitorPhase::Stopped,
            }),
        }
    }

    pub(super) fn select(&self, account_id: MailAccountId) {
        let mut state = self.lock();
        state.select(account_id);
        state.activation_request = None;
    }

    pub(super) fn begin_activation(&self, account_id: MailAccountId, request_id: RequestId) {
        let mut state = self.lock();
        state.select(account_id);
        state.activation_request = Some(request_id);
    }

    pub(super) fn finish_activation(
        &self,
        account_id: &MailAccountId,
        request_id: RequestId,
    ) {
        let mut state = self.lock();
        if state.account_id.as_ref() == Some(account_id)
            && state.activation_request == Some(request_id)
        {
            state.activation_request = None;
        }
    }

    pub(super) fn account_id(&self) -> Option<MailAccountId> {
        self.lock().account_id.clone()
    }

    pub(super) fn is_selected(&self, account_id: &MailAccountId) -> bool {
        self.lock().account_id.as_ref() == Some(account_id)
    }

    pub(super) fn prepare_refresh(
        &self,
        account_id: &MailAccountId,
        monitor: MonitorPolicy,
    ) -> bool {
        let mut state = self.lock();
        if state.account_id.as_ref() != Some(account_id) || state.activation_request.is_some() {
            return false;
        }
        if matches!(monitor, MonitorPolicy::Restart) {
            state.stop_monitor();
        }
        true
    }

    pub(super) fn begin_monitor(&self, account_id: &MailAccountId) -> Option<u64> {
        let mut state = self.lock();
        if state.account_id.as_ref() != Some(account_id)
            || state.activation_request.is_some()
            || !matches!(&state.monitor, ChangeMonitorPhase::Stopped)
        {
            return None;
        }
        state.stop_monitor();
        state.monitor = ChangeMonitorPhase::Starting;
        Some(state.generation)
    }

    pub(super) fn finish_monitor(
        &self,
        account_id: &MailAccountId,
        generation: u64,
        result: anyhow::Result<BackendChangeMonitor>,
    ) -> anyhow::Result<()> {
        let mut state = self.lock();
        if state.generation != generation || state.account_id.as_ref() != Some(account_id) {
            return Ok(());
        }
        match result {
            Ok(monitor) => {
                state.monitor = ChangeMonitorPhase::Active { _monitor: monitor };
                Ok(())
            }
            Err(error) => {
                state.monitor = ChangeMonitorPhase::Stopped;
                Err(error)
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ForegroundAccountState> {
        self.state.lock().expect("foreground account lock poisoned")
    }
}

impl ForegroundAccountState {
    fn select(&mut self, account_id: MailAccountId) {
        if self.account_id.as_ref() != Some(&account_id) {
            self.stop_monitor();
            self.account_id = Some(account_id);
        }
    }

    fn stop_monitor(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.monitor = ChangeMonitorPhase::Stopped;
    }
}

impl MailCoordinator {
    pub fn request_account_activation(
        &self,
        request_id: RequestId,
        account_id: MailAccountId,
        conversation_limit: usize,
    ) {
        self.begin_foreground_activation(account_id.clone(), request_id);
        let mail_service = self.service();
        let coordinator = self.clone();
        std::thread::spawn(move || {
            let load = mail_service
                .activate_mailbox(&account_id, conversation_limit)
                .unwrap_or_else(|error| {
                    crate::logging::report_failure("mail-account-activation", &error);
                    crate::model::event::AccountMailboxLoad {
                        mode: crate::model::mail::MailboxMode::Unavailable,
                        content: crate::model::event::MailboxContentSnapshot {
                            folders: Vec::new(),
                            selected_folder_id: None,
                            conversations: Vec::new(),
                        },
                        failure: Some(crate::failure::classify_failure(&error)),
                    }
                });
            let refresh_live_account = load.mode == crate::model::mail::MailboxMode::Live;
            coordinator.finish_foreground_activation(&account_id, request_id);
            coordinator.publish(MailEvent::AccountActivated {
                request_id,
                load,
            });
            if refresh_live_account {
                coordinator.request_account_refresh(account_id);
            }
        });
    }

    pub fn select_foreground_account(&self, account_id: MailAccountId) {
        self.state.foreground.select(account_id);
    }

    fn begin_foreground_activation(&self, account_id: MailAccountId, request_id: RequestId) {
        self.state
            .foreground
            .begin_activation(account_id, request_id);
    }

    pub(super) fn finish_foreground_activation(
        &self,
        account_id: &MailAccountId,
        request_id: RequestId,
    ) {
        self.state
            .foreground
            .finish_activation(account_id, request_id);
    }

    pub(super) fn foreground_account(&self) -> Option<MailAccountId> {
        self.state.foreground.account_id()
    }

    pub(super) fn start_account_change_monitor(&self, service: AccountMailService) {
        if !self.is_foreground_route(&service) {
            return;
        }
        let account_id = service.account_id().clone();
        let Some(generation) = self.state.foreground.begin_monitor(&account_id) else {
            return;
        };
        let change_trigger = self.remote_change_trigger();
        let coordinator = self.clone();
        std::thread::spawn(move || {
            let callback_account_id = account_id.clone();
            let result = service.open_change_monitor(move || {
                change_trigger.trigger(&callback_account_id);
            });
            match coordinator
                .state
                .foreground
                .finish_monitor(&account_id, generation, result)
            {
                Ok(()) => {}
                Err(_error) => {
                    tracing::debug!(
                        target: "pigeon::eds",
                        "foreground account change monitor unavailable"
                    );
                    #[cfg(debug_assertions)]
                    tracing::debug!(
                        target: "pigeon::development::eds",
                        error = %_error,
                        "change monitor setup failed"
                    );
                }
            }
        });
    }

    pub(super) fn remote_change_trigger(&self) -> RemoteChangeTrigger {
        RemoteChangeTrigger {
            state: Arc::downgrade(&self.state),
        }
    }

    pub(super) fn is_foreground_account(&self, account_id: &MailAccountId) -> bool {
        self.state.foreground.is_selected(account_id)
    }

    pub(super) fn is_foreground_route(&self, service: &AccountMailService) -> bool {
        self.is_foreground_account(service.account_id())
            && self
                .service()
                .lease_account(service.account_id())
                .is_some_and(|current| current.same_backend(service))
    }
}

impl RemoteChangeTrigger {
    pub(super) fn trigger(&self, account_id: &MailAccountId) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        MailCoordinator { state }.request_account_refresh(account_id.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitor_restart_invalidates_an_in_flight_installation() {
        let foreground = ForegroundAccount::new();
        let account_id = MailAccountId("account-1".into());
        foreground.select(account_id.clone());
        let generation = foreground
            .begin_monitor(&account_id)
            .expect("first monitor should start");

        assert!(foreground.prepare_refresh(&account_id, MonitorPolicy::Restart));
        foreground
            .finish_monitor(
                &account_id,
                generation,
                Ok(Box::new(()) as BackendChangeMonitor),
            )
            .expect("stale monitor completion should be discarded");
        assert!(foreground.begin_monitor(&account_id).is_some());
    }

    #[test]
    fn stale_monitor_start_cannot_retarget_the_selected_account() {
        let foreground = ForegroundAccount::new();
        let selected = MailAccountId("account-2".into());
        foreground.select(selected.clone());

        assert!(foreground
            .begin_monitor(&MailAccountId("account-1".into()))
            .is_none());
        assert_eq!(foreground.account_id(), Some(selected));
    }

    #[test]
    fn refresh_waits_for_the_matching_activation_completion() {
        let foreground = ForegroundAccount::new();
        let account_id = MailAccountId("account-1".into());
        foreground.begin_activation(account_id.clone(), 7.into());

        assert!(!foreground.prepare_refresh(&account_id, MonitorPolicy::Keep));
        foreground.finish_activation(&account_id, 6.into());
        assert!(!foreground.prepare_refresh(&account_id, MonitorPolicy::Keep));
        foreground.finish_activation(&account_id, 7.into());
        assert!(foreground.prepare_refresh(&account_id, MonitorPolicy::Keep));
    }

    #[test]
    fn monitor_setup_rejects_a_retired_lease_for_the_foreground_account() {
        let coordinator = MailCoordinator::with_router(
            crate::integration::backend::mail_backend_router(),
        );
        let account_id = crate::integration::stub::stub_account_id();
        coordinator.select_foreground_account(account_id.clone());
        let retired = AccountMailService::new(
            crate::integration::stub::test_mail_backend(account_id.clone()),
        );

        coordinator.start_account_change_monitor(retired);

        assert!(coordinator.state.foreground.begin_monitor(&account_id).is_some());
    }
}
