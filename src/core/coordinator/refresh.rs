use std::time::Duration;

use crate::core::mail::{AccountMailService, MailService};
use crate::integration::backend::RefreshOutcome;
use crate::model::account::MailAccountId;
use crate::model::event::{MailEvent, RefreshFailureKind};

use super::{
    MailCoordinator,
    foreground::MonitorPolicy,
    notification::load_notification_snapshot,
    scheduler::WriteLease,
};

pub(super) struct AccountRefresh {
    coordinator: MailCoordinator,
    account_id: Option<MailAccountId>,
}

#[derive(Default)]
pub(super) struct ConvergenceWork {
    write_leases: Vec<WriteLease>,
    message_actions: bool,
}

impl ConvergenceWork {
    pub(super) fn leased_write(write_lease: WriteLease) -> Self {
        Self {
            write_leases: vec![write_lease],
            message_actions: false,
        }
    }

    pub(super) fn queued_message_action(write_lease: WriteLease) -> Self {
        drop(write_lease);
        Self {
            write_leases: Vec::new(),
            message_actions: true,
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.write_leases.is_empty() && !self.message_actions
    }

    pub(super) fn complete_message_actions(&mut self) {
        self.message_actions = false;
    }

    fn merge(&mut self, mut newer: Self) {
        self.write_leases.append(&mut newer.write_leases);
        self.message_actions |= newer.message_actions;
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.write_leases.len() + usize::from(self.message_actions)
    }
}

pub(super) struct AccountRefreshJob {
    pub(super) batches: Vec<AccountRefreshBatch>,
}

pub(super) struct AccountRefreshBatch {
    pub(super) source: AccountRefreshSource,
    pub(super) convergence: ConvergenceWork,
    pub(super) retry_after: Option<Duration>,
}

pub(super) enum AccountRefreshSource {
    Leased(AccountMailService),
    Activate {
        service: MailService,
        account_id: MailAccountId,
    },
}

impl AccountRefreshJob {
    pub(super) fn leased(service: AccountMailService) -> Self {
        Self {
            batches: vec![AccountRefreshBatch::ordinary(
                AccountRefreshSource::Leased(service),
            )],
        }
    }

    pub(super) fn activating(service: MailService, account_id: MailAccountId) -> Self {
        Self {
            batches: vec![AccountRefreshBatch::ordinary(
                AccountRefreshSource::Activate {
                    service,
                    account_id,
                },
            )],
        }
    }

    pub(super) fn convergence(service: AccountMailService, work: ConvergenceWork) -> Self {
        Self {
            batches: vec![AccountRefreshBatch {
                source: AccountRefreshSource::Leased(service),
                convergence: work,
                retry_after: None,
            }],
        }
    }

    pub(super) fn retry(batches: Vec<AccountRefreshBatch>) -> Self {
        Self { batches }
    }

    pub(super) fn account_id(&self) -> &MailAccountId {
        self.batches
            .first()
            .expect("an account refresh must contain work")
            .source
            .account_id()
    }

    pub(super) fn merge(&mut self, newer: Self) {
        for newer_batch in newer.batches {
            let matching = self
                .batches
                .iter()
                .position(|existing| existing.same_source(&newer_batch));
            match matching {
                Some(index) if !newer_batch.convergence.is_empty() => {
                    if self.batches[index].convergence.is_empty() {
                        self.batches[index] = newer_batch;
                    } else {
                        self.batches[index].merge_convergence(newer_batch);
                    }
                }
                Some(index) if !self.batches[index].convergence.is_empty() => {
                    self.batches[index].retry_after = None;
                }
                Some(index) => self.batches[index] = newer_batch,
                None if newer_batch.convergence.is_empty() => {
                    self.batches.insert(0, newer_batch);
                }
                None => self.batches.push(newer_batch),
            }
        }
    }

    #[cfg(test)]
    pub(super) fn accepted_work_count(&self) -> usize {
        self.batches
            .iter()
            .map(|batch| batch.convergence.len())
            .sum()
    }

    #[cfg(test)]
    pub(super) fn has_retry_delay(&self) -> bool {
        self.batches
            .iter()
            .any(|batch| batch.retry_after.is_some())
    }

    #[cfg(test)]
    pub(super) fn requires_activation(&self) -> bool {
        self.batches
            .iter()
            .any(|batch| matches!(&batch.source, AccountRefreshSource::Activate { .. }))
    }
}

impl AccountRefreshBatch {
    fn ordinary(source: AccountRefreshSource) -> Self {
        Self {
            source,
            convergence: ConvergenceWork::default(),
            retry_after: None,
        }
    }

    pub(super) fn convergence_retry(
        service: AccountMailService,
        convergence: ConvergenceWork,
    ) -> Self {
        assert!(
            !convergence.is_empty(),
            "a convergence retry must own accepted work"
        );
        Self {
            source: AccountRefreshSource::Leased(service),
            convergence,
            retry_after: Some(Duration::from_secs(5)),
        }
    }

    pub(super) fn resolve(self) -> anyhow::Result<ResolvedRefreshBatch> {
        let service = match self.source {
            AccountRefreshSource::Leased(service) => service,
            AccountRefreshSource::Activate {
                service,
                account_id,
            } => service.activate_account_service(&account_id)?,
        };
        Ok(ResolvedRefreshBatch {
            service,
            convergence: self.convergence,
        })
    }

    fn same_source(&self, other: &Self) -> bool {
        match (&self.source, &other.source) {
            (AccountRefreshSource::Leased(left), AccountRefreshSource::Leased(right)) => {
                left.same_backend(right)
            }
            (
                AccountRefreshSource::Activate {
                    service: left_service,
                    account_id: left_account,
                },
                AccountRefreshSource::Activate {
                    service: right_service,
                    account_id: right_account,
                },
            ) => left_account == right_account && left_service.same_router(right_service),
            _ => false,
        }
    }

    fn merge_convergence(&mut self, newer: Self) {
        self.convergence.merge(newer.convergence);
        self.retry_after = match (self.retry_after, newer.retry_after) {
            (Some(current), Some(newer)) => Some(current.min(newer)),
            _ => None,
        };
    }
}

impl AccountRefreshSource {
    fn account_id(&self) -> &MailAccountId {
        match self {
            Self::Leased(service) => service.account_id(),
            Self::Activate { account_id, .. } => account_id,
        }
    }
}

pub(super) struct ResolvedRefreshBatch {
    pub(super) service: AccountMailService,
    pub(super) convergence: ConvergenceWork,
}

impl MailCoordinator {
    pub fn request_foreground_refresh(&self) {
        let Some(account_id) = self.foreground_account() else {
            return;
        };
        self.request_account_refresh(account_id);
    }

    pub fn request_foreground_reconnect(&self) {
        let Some(account_id) = self.foreground_account() else {
            return;
        };
        self.request_account_reconnect(account_id);
    }

    pub(crate) fn reconnect_changed_foreground_route(&self, changed: &[MailAccountId]) {
        let Some(account_id) = self.foreground_account() else {
            return;
        };
        if changed.contains(&account_id) {
            self.request_account_reconnect(account_id);
        }
    }

    pub(super) fn request_account_refresh(&self, account_id: MailAccountId) {
        self.request_scoped_foreground_refresh(account_id, MonitorPolicy::Keep);
    }

    fn request_account_reconnect(&self, account_id: MailAccountId) {
        self.request_scoped_foreground_refresh(account_id, MonitorPolicy::Restart);
    }

    fn request_scoped_foreground_refresh(
        &self,
        account_id: MailAccountId,
        monitor: MonitorPolicy,
    ) {
        if !self
            .state
            .foreground
            .prepare_refresh(&account_id, monitor)
        {
            return;
        }
        let job = self.account_refresh_job(&account_id);
        self.request_account_refresh_job(job);
    }

    pub(super) fn account_refresh_job(&self, account_id: &MailAccountId) -> AccountRefreshJob {
        let service = self.service();
        match service.lease_account(account_id) {
            Some(service) => AccountRefreshJob::leased(service),
            None => AccountRefreshJob::activating(service, account_id.clone()),
        }
    }

    pub(super) fn request_leased_account_refresh(&self, service: AccountMailService) {
        self.request_account_refresh_job(AccountRefreshJob::leased(service));
    }

    pub(super) fn request_account_convergence(
        &self,
        service: AccountMailService,
        work: ConvergenceWork,
    ) {
        self.request_account_refresh_job(AccountRefreshJob::convergence(service, work));
    }

    fn request_account_refresh_job(&self, job: AccountRefreshJob) {
        let account_id = job.account_id().clone();
        let Some(job) = self.state.account_refreshes.begin_or_merge(
            account_id.clone(),
            job,
            AccountRefreshJob::merge,
        ) else {
            return;
        };
        let refresh = AccountRefresh {
            coordinator: self.clone(),
            account_id: Some(account_id),
        };
        self.spawn_account_refresh(job, refresh);
    }

    fn spawn_account_refresh(&self, job: AccountRefreshJob, refresh: AccountRefresh) {
        let account_id = job.account_id().clone();
        let coordinator = self.clone();
        std::thread::spawn(move || {
            let retry_batches = job
                .batches
                .into_iter()
                .filter_map(|batch| coordinator.run_account_refresh_batch(&account_id, batch))
                .collect();
            refresh.complete(retry_batches);
        });
    }

    fn run_account_refresh_batch(
        &self,
        account_id: &MailAccountId,
        batch: AccountRefreshBatch,
    ) -> Option<AccountRefreshBatch> {
        if let Some(delay) = batch.retry_after {
            std::thread::sleep(delay);
        }
        let ResolvedRefreshBatch {
            service,
            mut convergence,
        } = match batch.resolve() {
            Ok(resolved) => resolved,
            Err(error) => {
                crate::logging::report_deferred("mail-account-reactivation", &error);
                self.publish_foreground_activation_failure(
                    account_id,
                    crate::failure::classify_failure(&error),
                );
                return None;
            }
        };

        let unresolved_before = service.unresolved_message_action_count();
        let result = service.refresh();
        let unresolved_after = service.unresolved_message_action_count();
        if unresolved_before != unresolved_after {
            self.publish(MailEvent::PendingWorkChanged);
        }
        if unresolved_after == 0 {
            convergence.complete_message_actions();
        }

        match result {
            Ok(outcome) => self.finish_successful_refresh(service, convergence, outcome),
            Err(error) => {
                crate::logging::report_deferred("mail-refresh", &error);
                self.publish_foreground_refresh(
                    &service,
                    Some(crate::failure::classify_failure(&error)),
                );
                (!convergence.is_empty())
                    .then(|| AccountRefreshBatch::convergence_retry(service, convergence))
            }
        }
    }

    fn finish_successful_refresh(
        &self,
        service: AccountMailService,
        convergence: ConvergenceWork,
        outcome: RefreshOutcome,
    ) -> Option<AccountRefreshBatch> {
        let owns_accepted_work = !convergence.is_empty();
        let current_route = self.is_foreground_route(&service);
        let projection_retry = outcome.projection_stale && current_route;
        let convergence_retry = outcome.convergence_incomplete && owns_accepted_work;

        if !outcome.projection_stale && current_route {
            self.publish_foreground_refresh(&service, None);
            self.start_account_change_monitor(service.clone());
            self.observe_new_mail(&service);
        }
        if owns_accepted_work && !convergence_retry {
            self.request_foreground_refresh_after_retired_convergence(&service);
        }
        if projection_retry && !convergence_retry {
            self.request_leased_account_refresh(service.clone());
        }
        if !convergence_retry {
            return None;
        }

        let mut retry = AccountRefreshBatch::convergence_retry(service, convergence);
        if projection_retry {
            retry.retry_after = None;
        }
        Some(retry)
    }

    fn observe_new_mail(&self, service: &AccountMailService) {
        if !self.is_foreground_route(service) {
            return;
        }
        let account_id = service.account_id().clone();
        match load_notification_snapshot(service) {
            Ok(snapshot) if self.is_foreground_route(service) => {
                for notification in self.state.notifications.observe(&account_id, snapshot) {
                    self.publish(MailEvent::NewMailAvailable {
                        account_id: account_id.clone(),
                        folder_id: notification.folder_id,
                        folder_name: notification.folder_name,
                        count: notification.count,
                    });
                }
            }
            Err(error) => crate::logging::report_deferred("new-mail-snapshot", &error),
            Ok(_) => {}
        }
    }

    pub(super) fn publish_foreground_refresh(
        &self,
        service: &AccountMailService,
        failure: Option<RefreshFailureKind>,
    ) {
        if self.is_foreground_route(service) {
            self.publish(MailEvent::AccountRefreshCompleted {
                account_id: service.account_id().clone(),
                failure,
            });
        }
    }

    pub(super) fn publish_foreground_activation_failure(
        &self,
        account_id: &MailAccountId,
        failure: RefreshFailureKind,
    ) {
        if self.is_foreground_account(account_id)
            && self.service().lease_account(account_id).is_none()
        {
            self.publish(MailEvent::AccountRefreshCompleted {
                account_id: account_id.clone(),
                failure: Some(failure),
            });
        }
    }

    pub(super) fn request_foreground_refresh_after_retired_convergence(
        &self,
        completed_service: &AccountMailService,
    ) {
        if !self.is_foreground_route(completed_service) {
            self.request_account_refresh(completed_service.account_id().clone());
        }
    }

    #[cfg(test)]
    pub(super) fn begin_account_refresh(
        &self,
        account_id: &MailAccountId,
    ) -> Option<AccountRefresh> {
        self.state.account_refreshes.begin(
            account_id.clone(),
            AccountRefreshJob::leased(AccountMailService::new(
                crate::integration::stub::test_mail_backend(account_id.clone()),
            )),
        )?;
        Some(AccountRefresh {
            coordinator: self.clone(),
            account_id: Some(account_id.clone()),
        })
    }

    #[cfg(test)]
    pub(super) fn account_refresh_running(&self, account_id: &MailAccountId) -> bool {
        self.state.account_refreshes.contains_key(account_id)
    }

    pub(super) fn finish_account_refresh(
        &self,
        account_id: &MailAccountId,
    ) -> Option<AccountRefreshJob> {
        self.state.account_refreshes.finish(account_id)
    }

    fn abort_account_refresh(&self, account_id: &MailAccountId) {
        self.state.account_refreshes.abort(account_id);
    }
}

impl AccountRefresh {
    pub(super) fn complete(mut self, retry_batches: Vec<AccountRefreshBatch>) {
        let Some(account_id) = self.account_id.take() else {
            return;
        };
        if !retry_batches.is_empty() {
            let retry = AccountRefreshJob::retry(retry_batches);
            let started = self.coordinator.state.account_refreshes.begin_or_merge(
                account_id.clone(),
                retry,
                AccountRefreshJob::merge,
            );
            debug_assert!(
                started.is_none(),
                "completed refresh must still own its queue slot"
            );
        }
        let next_job = self.coordinator.finish_account_refresh(&account_id);
        if let Some(job) = next_job {
            let refresh = AccountRefresh {
                coordinator: self.coordinator.clone(),
                account_id: Some(account_id),
            };
            self.coordinator.spawn_account_refresh(job, refresh);
        }
    }
}

impl Drop for AccountRefresh {
    fn drop(&mut self) {
        if let Some(account_id) = self.account_id.take() {
            self.coordinator.abort_account_refresh(&account_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(account_id: &str) -> AccountMailService {
        AccountMailService::new(crate::integration::stub::test_mail_backend(MailAccountId(
            account_id.into(),
        )))
    }

    #[test]
    fn incomplete_convergence_keeps_the_accepted_write_owned_by_its_retry() {
        let coordinator = MailCoordinator::new();
        let convergence = ConvergenceWork::leased_write(coordinator.begin_write());

        let retry = coordinator
            .finish_successful_refresh(
                service("account-1"),
                convergence,
                RefreshOutcome {
                    convergence_incomplete: true,
                    projection_stale: false,
                },
            )
            .expect("unfinished convergence must retain its accepted write");

        assert_eq!(coordinator.pending_work_count(), 1);
        assert!(retry.retry_after.is_some());
        drop(retry);
        coordinator.wait_for_writes();
    }

    #[test]
    fn completed_convergence_releases_its_accepted_write_without_a_retry() {
        let coordinator = MailCoordinator::new();
        let convergence = ConvergenceWork::leased_write(coordinator.begin_write());

        assert!(
            coordinator
                .finish_successful_refresh(
                    service("account-1"),
                    convergence,
                    RefreshOutcome::default(),
                )
                .is_none()
        );
        coordinator.wait_for_writes();
        assert_eq!(coordinator.pending_work_count(), 0);
    }
}
