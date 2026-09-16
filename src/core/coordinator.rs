mod attachment;
mod detail;
mod foreground;
mod notification;
mod refresh;
mod scheduler;
mod summary;
mod write;

use std::sync::{Arc, Mutex};

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};

#[cfg(test)]
use crate::core::mail::AccountMailService;
use crate::core::mail::MailService;
use crate::integration::backend::SharedMailBackendRouter;
use crate::model::account::MailAccountId;
use crate::model::event::MailEvent;
#[cfg(test)]
use crate::model::event::{RefreshFailureKind, RequestId};
#[cfg(test)]
use crate::model::mail::{
    AttachmentInfo, AttachmentLocation, AttachmentOperation, ConversationId, DraftMessage,
};
#[cfg(test)]
use detail::MessageDetailJob;
use detail::MessageDetailScheduler;
use foreground::ForegroundAccount;
use notification::NotificationTracker;
#[cfg(test)]
use refresh::{AccountRefreshBatch, ConvergenceWork};
use refresh::AccountRefreshJob;
use scheduler::{LatestJobQueue, SerialJobQueue, WriteLease, WriteTracker};
use summary::{MailboxReadJob, SearchJob};
#[cfg(test)]
use write::prepare_message;
use write::{MailboxWriteJob, MailboxWriteKey};

#[derive(Clone)]
pub struct MailCoordinator {
    state: Arc<MailCoordinatorState>,
}

struct MailCoordinatorState {
    service: MailService,
    sender: UnboundedSender<MailEvent>,
    receiver: Mutex<Option<UnboundedReceiver<MailEvent>>>,
    account_refreshes: LatestJobQueue<MailAccountId, AccountRefreshJob>,
    mailbox_reads: LatestJobQueue<MailAccountId, MailboxReadJob>,
    account_searches: LatestJobQueue<MailAccountId, SearchJob>,
    mailbox_writes: SerialJobQueue<MailboxWriteKey, MailboxWriteJob>,
    message_details: MessageDetailScheduler,
    notifications: NotificationTracker,
    foreground: ForegroundAccount,
    writes: Arc<WriteTracker>,
}

impl MailCoordinator {
    #[cfg(test)]
    fn new() -> Self {
        Self::with_router(crate::integration::backend::mail_backend_router())
    }

    pub fn with_router(router: SharedMailBackendRouter) -> Self {
        let (sender, receiver) = unbounded();
        let work_sender = sender.clone();
        Self {
            state: Arc::new(MailCoordinatorState {
                service: MailService::new(router),
                sender,
                receiver: Mutex::new(Some(receiver)),
                account_refreshes: LatestJobQueue::new(),
                mailbox_reads: LatestJobQueue::new(),
                account_searches: LatestJobQueue::new(),
                mailbox_writes: SerialJobQueue::new(),
                message_details: MessageDetailScheduler::new(),
                notifications: NotificationTracker::new(),
                foreground: ForegroundAccount::new(),
                writes: Arc::new(WriteTracker::new(move || {
                    let _ = work_sender.unbounded_send(MailEvent::PendingWorkChanged);
                })),
            }),
        }
    }

    fn service(&self) -> MailService {
        self.state.service.clone()
    }

    fn publish(&self, event: MailEvent) {
        let _ = self.state.sender.unbounded_send(event);
    }

    fn begin_write(&self) -> WriteLease {
        self.state.writes.begin()
    }

    #[cfg(test)]
    fn wait_for_writes(&self) {
        self.state.writes.wait();
    }

    pub(crate) fn pending_work_count(&self) -> usize {
        let accepted = self.state.writes.count();
        let unresolved = self.unresolved_message_action_count();
        accepted + unresolved
    }

    fn unresolved_message_action_count(&self) -> usize {
        self.state.service.unresolved_message_action_count()
    }

    pub(crate) fn take_event_stream(&self) -> UnboundedReceiver<MailEvent> {
        self.state
            .receiver
            .lock()
            .expect("mail event receiver lock poisoned")
            .take()
            .expect("mail event stream may only be attached once")
    }

    #[cfg(test)]
    fn drain(&self) -> Vec<MailEvent> {
        let mut receiver = self
            .state
            .receiver
            .lock()
            .expect("mail event receiver lock poisoned");
        let receiver = receiver
            .as_mut()
            .expect("tests must not take the mail event stream");
        let mut events = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            events.push(event);
        }
        events
    }

    pub fn cancel_transient_reads(&self, account_id: &MailAccountId) {
        self.cancel_pending_search(account_id);
        self.cancel_message_detail_requests(account_id);
        self.state.mailbox_reads.cancel_pending(account_id);
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use super::refresh::AccountRefreshSource;

    fn account_service(account_id: MailAccountId) -> AccountMailService {
        AccountMailService::new(crate::integration::stub::test_mail_backend(account_id))
    }

    fn stub_account_service() -> AccountMailService {
        account_service(crate::integration::stub::stub_account_id())
    }

    fn coordinator_with_stub_route() -> MailCoordinator {
        MailCoordinator::with_router(crate::integration::backend::mail_backend_router())
    }

    #[test]
    fn account_refresh_is_single_flight_until_completion() {
        let coordinator = MailCoordinator::new();
        let account_id = MailAccountId("account-1".into());
        let refresh = coordinator
            .begin_account_refresh(&account_id)
            .expect("first refresh should start");
        assert!(coordinator.account_refresh_running(&account_id));

        refresh.complete(Vec::new());

        assert!(!coordinator.account_refresh_running(&account_id));
        assert!(coordinator.drain().is_empty());
        assert!(coordinator.begin_account_refresh(&account_id).is_some());
    }

    #[test]
    fn duplicate_refresh_request_is_coalesced_as_a_follow_up() {
        let coordinator = MailCoordinator::new();
        let account_id = MailAccountId("account-1".into());
        coordinator.select_foreground_account(account_id.clone());
        let refresh = coordinator
            .begin_account_refresh(&account_id)
            .expect("first refresh should start");
        coordinator.request_leased_account_refresh(account_service(account_id.clone()));
        assert!(coordinator.finish_account_refresh(&account_id).is_some());

        drop(refresh);
    }

    #[test]
    fn immediate_same_route_follow_up_coalesces_and_skips_retry_delay() {
        let coordinator = MailCoordinator::new();
        let service = stub_account_service();
        let mut queued = AccountRefreshJob::retry(vec![
            AccountRefreshBatch::convergence_retry(
                service.clone(),
                ConvergenceWork::leased_write(coordinator.begin_write()),
            ),
        ]);

        queued.merge(AccountRefreshJob::convergence(
            service,
            ConvergenceWork::leased_write(coordinator.begin_write()),
        ));

        assert_eq!(queued.accepted_work_count(), 2);
        assert_eq!(queued.batches.len(), 1);
        assert!(!queued.has_retry_delay());
        drop(queued);
        coordinator.wait_for_writes();
    }

    #[test]
    fn one_route_uses_one_convergence_owner_for_all_queued_message_actions() {
        let coordinator = MailCoordinator::new();
        let service = stub_account_service();
        let mut queued = AccountRefreshJob::convergence(
            service.clone(),
            ConvergenceWork::queued_message_action(coordinator.begin_write()),
        );

        queued.merge(AccountRefreshJob::convergence(
            service,
            ConvergenceWork::queued_message_action(coordinator.begin_write()),
        ));

        assert_eq!(queued.accepted_work_count(), 1);
        assert_eq!(queued.batches.len(), 1);
        assert_eq!(coordinator.state.writes.count(), 0);
    }

    #[test]
    fn convergence_keeps_distinct_leased_routes_in_separate_batches() {
        let coordinator = MailCoordinator::new();
        let account_id = MailAccountId("account".into());
        let mut queued = AccountRefreshJob::convergence(
            account_service(account_id.clone()),
            ConvergenceWork::leased_write(coordinator.begin_write()),
        );
        queued.merge(AccountRefreshJob::convergence(
            account_service(account_id),
            ConvergenceWork::leased_write(coordinator.begin_write()),
        ));

        assert_eq!(queued.accepted_work_count(), 2);
        assert_eq!(queued.batches.len(), 2);
        drop(queued);
        coordinator.wait_for_writes();
    }

    #[test]
    fn ordinary_refresh_accelerates_but_cannot_replace_the_same_writes_route() {
        let coordinator = MailCoordinator::new();
        let service = stub_account_service();
        let mut queued = AccountRefreshJob::retry(vec![
            AccountRefreshBatch::convergence_retry(
                service.clone(),
                ConvergenceWork::leased_write(coordinator.begin_write()),
            ),
        ]);

        queued.merge(AccountRefreshJob::leased(service));

        assert!(!queued.requires_activation());
        assert_eq!(queued.batches.len(), 1);
        assert!(!queued.has_retry_delay());
        drop(queued);
        coordinator.wait_for_writes();
    }

    #[test]
    fn changed_route_activation_precedes_a_retired_route_retry() {
        let coordinator = MailCoordinator::new();
        let mut queued = AccountRefreshJob::retry(vec![
            AccountRefreshBatch::convergence_retry(
                stub_account_service(),
                ConvergenceWork::leased_write(coordinator.begin_write()),
            ),
        ]);

        queued.merge(AccountRefreshJob::activating(
            coordinator.service(),
            crate::integration::stub::stub_account_id(),
        ));

        assert!(queued.requires_activation());
        assert_eq!(queued.batches.len(), 2);
        assert!(matches!(
            &queued.batches[0].source,
            AccountRefreshSource::Activate { .. }
        ));
        assert!(queued.batches[1].retry_after.is_some());
        drop(queued);
        coordinator.wait_for_writes();
    }

    #[test]
    fn retired_route_convergence_cannot_replace_a_queued_activation() {
        let coordinator = MailCoordinator::new();
        let mut queued = AccountRefreshJob::activating(
            coordinator.service(),
            crate::integration::stub::stub_account_id(),
        );

        queued.merge(AccountRefreshJob::convergence(
            stub_account_service(),
            ConvergenceWork::leased_write(coordinator.begin_write()),
        ));

        assert!(queued.requires_activation());
        assert_eq!(queued.batches.len(), 2);
        assert!(matches!(
            &queued.batches[0].source,
            AccountRefreshSource::Activate { .. }
        ));
        assert_eq!(queued.accepted_work_count(), 1);
        drop(queued);
        coordinator.wait_for_writes();
    }

    #[test]
    fn retired_convergence_queues_an_observation_on_the_current_route() {
        let coordinator = coordinator_with_stub_route();
        let account_id = crate::integration::stub::stub_account().id;
        coordinator.select_foreground_account(account_id.clone());
        let current = coordinator
            .service()
            .lease_account(&account_id)
            .expect("stub route should be available");
        let running = coordinator
            .begin_account_refresh(&account_id)
            .expect("refresh should reserve the account queue");

        coordinator.request_foreground_refresh_after_retired_convergence(&account_service(
            account_id.clone(),
        ));

        let follow_up = coordinator
            .finish_account_refresh(&account_id)
            .expect("current route observation should be queued");
        assert_eq!(follow_up.batches.len(), 1);
        assert!(matches!(
            &follow_up.batches[0].source,
            AccountRefreshSource::Leased(service) if service.same_backend(&current)
        ));
        drop(running);
    }

    #[test]
    fn missing_refresh_lease_becomes_a_reactivation_job() {
        let coordinator = MailCoordinator::new();
        let account_id = MailAccountId("unbound-account".into());

        assert!(coordinator.account_refresh_job(&account_id).requires_activation());
    }

    #[test]
    fn missing_account_lease_completes_a_started_read_with_failure() {
        let coordinator = MailCoordinator::new();
        coordinator.request_message_detail(
            7.into(),
            MailAccountId("unbound-account".into()),
            ConversationId("message".into()),
        );

        assert!(matches!(
            coordinator.drain().as_slice(),
            [MailEvent::MessageDetailLoaded {
                request_id,
                result: Err(_),
            }] if *request_id == RequestId::from(7)
        ));
    }

    #[test]
    fn public_refresh_does_not_start_new_work_for_a_background_account() {
        let coordinator = MailCoordinator::new();
        let background = crate::integration::stub::stub_account().id;
        coordinator.select_foreground_account(MailAccountId("foreground-account".into()));

        coordinator.request_account_refresh(background.clone());

        assert!(!coordinator.account_refresh_running(&background));
    }

    #[test]
    fn duplicate_refresh_requests_keep_only_one_follow_up() {
        let coordinator = MailCoordinator::new();
        let account_id = MailAccountId("account-1".into());
        coordinator.select_foreground_account(account_id.clone());
        let refresh = coordinator
            .begin_account_refresh(&account_id)
            .expect("first refresh should start");
        for _ in 0..3 {
            coordinator.request_leased_account_refresh(account_service(account_id.clone()));
        }
        assert!(coordinator.finish_account_refresh(&account_id).is_some());
        assert!(coordinator.finish_account_refresh(&account_id).is_none());

        drop(refresh);
    }

    #[test]
    fn claimed_refresh_follow_up_keeps_the_single_flight_slot_reserved() {
        let coordinator = MailCoordinator::new();
        let account_id = MailAccountId("account-1".into());
        let refresh = coordinator
            .begin_account_refresh(&account_id)
            .expect("first refresh should start");
        coordinator.request_leased_account_refresh(account_service(account_id.clone()));

        assert!(coordinator.finish_account_refresh(&account_id).is_some());
        coordinator.request_leased_account_refresh(account_service(account_id.clone()));
        assert!(coordinator.finish_account_refresh(&account_id).is_some());

        drop(refresh);
    }

    #[test]
    fn draining_account_keeps_its_queued_follow_up() {
        let coordinator = MailCoordinator::new();
        let first = MailAccountId("account-1".into());
        let second = MailAccountId("account-2".into());
        coordinator.select_foreground_account(first.clone());
        let refresh = coordinator
            .begin_account_refresh(&first)
            .expect("foreground account refresh should start");
        coordinator.request_leased_account_refresh(account_service(first.clone()));
        coordinator.select_foreground_account(second.clone());

        assert!(coordinator.finish_account_refresh(&first).is_some());
        assert!(coordinator.is_foreground_account(&second));
        assert!(!coordinator.is_foreground_account(&first));
        drop(refresh);
    }

    #[test]
    fn remote_change_callback_does_not_keep_the_coordinator_alive() {
        let coordinator = MailCoordinator::new();
        let account_id = MailAccountId("account-1".into());
        coordinator.select_foreground_account(account_id.clone());
        let state = Arc::downgrade(&coordinator.state);
        let trigger = coordinator.remote_change_trigger();

        drop(coordinator);

        assert!(state.upgrade().is_none());
        trigger.trigger(&account_id);
    }

    #[test]
    fn foreground_route_publishes_an_account_refresh_event() {
        let coordinator = coordinator_with_stub_route();
        let account_id = crate::integration::stub::stub_account().id;
        coordinator.select_foreground_account(account_id.clone());
        let service = coordinator
            .service()
            .lease_account(&account_id)
            .expect("stub route should be available");
        coordinator.publish_foreground_refresh(&service, None);

        let events = coordinator.drain();
        assert!(matches!(
            events.as_slice(),
            [MailEvent::AccountRefreshCompleted {
                account_id,
                failure: None,
            }]
                if account_id == &crate::integration::stub::stub_account().id
        ));
        assert!(coordinator.drain().is_empty());
    }

    #[test]
    fn foreground_route_preserves_a_privacy_safe_failure_kind() {
        let coordinator = coordinator_with_stub_route();
        let account_id = crate::integration::stub::stub_account().id;
        coordinator.select_foreground_account(account_id.clone());
        let service = coordinator
            .service()
            .lease_account(&account_id)
            .expect("stub route should be available");
        coordinator.publish_foreground_refresh(
            &service,
            Some(RefreshFailureKind::Storage),
        );

        assert!(matches!(
            coordinator.drain().as_slice(),
            [MailEvent::AccountRefreshCompleted {
                account_id,
                failure: Some(RefreshFailureKind::Storage),
            }] if account_id == &crate::integration::stub::stub_account().id
        ));
    }

    #[test]
    fn retired_route_does_not_publish_foreground_account_state() {
        let coordinator = coordinator_with_stub_route();
        let account_id = crate::integration::stub::stub_account().id;
        coordinator.select_foreground_account(account_id.clone());
        coordinator.publish_foreground_refresh(
            &account_service(account_id),
            Some(RefreshFailureKind::Connectivity),
        );

        assert!(coordinator.drain().is_empty());
    }

    #[test]
    fn activation_failure_is_relevant_only_while_the_current_route_is_missing() {
        let coordinator = coordinator_with_stub_route();
        let missing = MailAccountId("missing-account".into());
        coordinator.select_foreground_account(missing.clone());
        coordinator.publish_foreground_activation_failure(&missing, RefreshFailureKind::Storage);
        assert!(matches!(
            coordinator.drain().as_slice(),
            [MailEvent::AccountRefreshCompleted {
                account_id,
                failure: Some(RefreshFailureKind::Storage),
            }] if account_id == &missing
        ));

        let materialized = crate::integration::stub::stub_account().id;
        coordinator.select_foreground_account(materialized.clone());
        coordinator.publish_foreground_activation_failure(
            &materialized,
            RefreshFailureKind::Storage,
        );
        assert!(coordinator.drain().is_empty());
    }

    #[test]
    fn cloned_coordinators_share_events_and_refresh_state() {
        let coordinator = MailCoordinator::new();
        let worker = coordinator.clone();
        let account_id = MailAccountId("account-1".into());
        let refresh = coordinator
            .begin_account_refresh(&account_id)
            .expect("refresh should start");
        assert!(worker.begin_account_refresh(&account_id).is_none());

        worker.publish(MailEvent::MailboxCacheChanged {
            account_id: MailAccountId("account-1".into()),
        });

        let events = coordinator.drain();
        assert!(matches!(
            events.as_slice(),
            [MailEvent::MailboxCacheChanged { account_id }]
                if account_id.0 == "account-1"
        ));
        drop(refresh);
    }

    #[test]
    fn write_leases_count_each_active_worker_scope() {
        let coordinator = MailCoordinator::new();
        let first = coordinator.begin_write();
        let second = coordinator.begin_write();
        assert_eq!(coordinator.state.writes.count(), 2);

        drop(first);
        assert_eq!(coordinator.state.writes.count(), 1);

        drop(second);
        assert_eq!(coordinator.state.writes.count(), 0);
        coordinator.wait_for_writes();
    }

    #[test]
    fn queued_message_action_releases_generic_count_before_convergence() {
        let coordinator = MailCoordinator::new();
        let _work = ConvergenceWork::queued_message_action(coordinator.begin_write());

        assert_eq!(coordinator.state.writes.count(), 0);
    }

    #[test]
    fn identity_batch_save_completes_through_one_shared_event() {
        let coordinator = MailCoordinator::new();
        let account = crate::integration::stub::stub_account();

        coordinator.request_account_identities_save(42.into(), vec![account.clone(), account]);
        coordinator.wait_for_writes();

        assert!(matches!(
            coordinator.drain().as_slice(),
            [
                MailEvent::PendingWorkChanged,
                MailEvent::AccountIdentitiesSaveCompleted {
                    request_id,
                    result: Ok(()),
                },
                MailEvent::PendingWorkChanged,
            ] if *request_id == RequestId::from(42)
        ));
    }

    #[test]
    fn dropping_an_unfinished_refresh_releases_it_without_an_event() {
        let coordinator = MailCoordinator::new();
        let account_id = MailAccountId("account-1".into());
        let refresh = coordinator
            .begin_account_refresh(&account_id)
            .expect("refresh should start");

        drop(refresh);

        assert!(coordinator.drain().is_empty());
        assert!(coordinator.begin_account_refresh(&account_id).is_some());
    }

    #[test]
    fn drain_preserves_event_order_from_a_single_publisher() {
        let coordinator = MailCoordinator::new();
        coordinator.publish(MailEvent::MailboxCacheChanged {
            account_id: MailAccountId("account-1".into()),
        });
        coordinator.publish(MailEvent::AccountRefreshCompleted {
            account_id: MailAccountId("account-2".into()),
            failure: Some(RefreshFailureKind::Connectivity),
        });

        let events = coordinator.drain();
        assert!(matches!(
            events.as_slice(),
            [
                MailEvent::MailboxCacheChanged {
                    account_id: first_account_id,
                },
                MailEvent::AccountRefreshCompleted {
                    account_id: second_account_id,
                    failure: Some(RefreshFailureKind::Connectivity),
                },
            ] if first_account_id.0 == "account-1" && second_account_id.0 == "account-2"
        ));
    }

    #[test]
    fn external_attachment_without_a_mail_request_is_published_immediately() {
        let coordinator = MailCoordinator::new();

        coordinator.request_attachment(
            None,
            AttachmentOperation::Open,
            AttachmentInfo {
                display_name: "report.pdf".into(),
                location: AttachmentLocation::ExternalUri("file:///tmp/report.pdf".into()),
            },
        );

        let events = coordinator.drain();
        assert!(matches!(
            events.as_slice(),
            [MailEvent::AttachmentPrepared {
                operation: AttachmentOperation::Open,
                display_name,
                result: Ok(uri),
            }] if display_name == "report.pdf" && uri == "file:///tmp/report.pdf"
        ));
    }

    #[test]
    fn unresolved_cached_attachment_completes_through_the_normal_failure_event() {
        let coordinator = MailCoordinator::new();

        coordinator.request_attachment(
            None,
            AttachmentOperation::SaveAs,
            AttachmentInfo {
                display_name: "report.pdf".into(),
                location: AttachmentLocation::CachedToken(
                    "1".into(),
                ),
            },
        );
        assert!(matches!(
            coordinator.drain().as_slice(),
            [MailEvent::AttachmentPrepared {
                operation: AttachmentOperation::SaveAs,
                display_name,
                result: Err(_),
            }] if display_name == "report.pdf"
        ));
    }

    #[test]
    fn cached_draft_attachments_require_their_source_scope_until_materialized() {
        let mut draft = DraftMessage::empty(
            MailAccountId("account-1".into()),
            "sender@example.test".into(),
        );
        draft.attachments.push(crate::model::mail::AttachmentInfo {
            display_name: "report.pdf".into(),
            location: AttachmentLocation::CachedToken("1".into()),
        });

        assert!(prepare_message(None, draft.clone()).is_err());

        draft.set_attachment_source(Some(crate::model::mail::AttachmentSource {
            account_id: MailAccountId("source-account".into()),
            conversation_id: ConversationId("drafts\u{1f}42".into()),
        }));
        draft.attachments[0].location =
            AttachmentLocation::ExternalUri("file:///tmp/report.pdf".into());
        assert!(prepare_message(None, draft).is_ok());
    }

    #[test]
    fn multiple_attachment_results_keep_their_own_user_intent() {
        let coordinator = MailCoordinator::new();
        coordinator.publish(MailEvent::AttachmentPrepared {
            operation: AttachmentOperation::Open,
            display_name: "first.pdf".into(),
            result: Ok("file:///cache/first.pdf".into()),
        });
        coordinator.publish(MailEvent::AttachmentPrepared {
            operation: AttachmentOperation::SaveAs,
            display_name: "second.png".into(),
            result: Ok("file:///cache/second.png".into()),
        });

        let events = coordinator.drain();
        assert!(matches!(
            events.as_slice(),
            [
                MailEvent::AttachmentPrepared {
                    operation: AttachmentOperation::Open,
                    display_name: first_name,
                    result: Ok(first_uri),
                },
                MailEvent::AttachmentPrepared {
                    operation: AttachmentOperation::SaveAs,
                    display_name: second_name,
                    result: Ok(second_uri),
                },
            ] if first_name == "first.pdf"
                && first_uri == "file:///cache/first.pdf"
                && second_name == "second.png"
                && second_uri == "file:///cache/second.png"
        ));
    }

    #[test]
    fn remote_message_detail_loads_are_single_flight_and_keep_only_the_newest_follow_up() {
        let coordinator = MailCoordinator::new();
        let scheduler = &coordinator.state.message_details;
        let backend = crate::integration::stub::mail_backend();
        let account_id = crate::integration::stub::stub_account().id;
        let job = |request_id: u64, conversation_id: &str| MessageDetailJob {
            service: AccountMailService::new(backend.clone()),
            request_id: request_id.into(),
            conversation_id: ConversationId(conversation_id.into()),
        };

        assert!(scheduler.begin_remote(job(1, "conversation")).is_some());
        assert!(scheduler.begin_remote(job(2, "conversation")).is_none());
        assert!(scheduler.begin_remote(job(3, "conversation")).is_none());

        let follow_up = scheduler
            .finish_remote(&account_id)
            .expect("newest detail request should be retained");
        assert_eq!(follow_up.request_id, RequestId::from(3));
        assert_eq!(
            follow_up.conversation_id,
            ConversationId("conversation".into())
        );
        assert!(scheduler.finish_remote(&account_id).is_none());
        assert!(scheduler.begin_remote(job(4, "conversation")).is_some());
    }

    #[test]
    fn cached_detail_probes_are_single_flight_and_keep_only_the_newest_follow_up() {
        let coordinator = MailCoordinator::new();
        let scheduler = &coordinator.state.message_details;
        let backend = crate::integration::stub::mail_backend();
        let account_id = crate::integration::stub::stub_account().id;
        let job = |request_id: u64, conversation_id: &str| MessageDetailJob {
            service: AccountMailService::new(backend.clone()),
            request_id: request_id.into(),
            conversation_id: ConversationId(conversation_id.into()),
        };

        assert!(scheduler.begin_cache(job(1, "first")).is_some());
        assert!(scheduler.begin_cache(job(2, "second")).is_none());
        assert!(scheduler.begin_cache(job(3, "third")).is_none());

        let follow_up = scheduler
            .finish_cache(&account_id)
            .expect("newest cache probe should be retained");
        assert_eq!(follow_up.request_id, RequestId::from(3));
        assert_eq!(follow_up.conversation_id, ConversationId("third".into()));
        assert!(scheduler.finish_cache(&account_id).is_none());
    }

    #[test]
    fn cached_detail_probe_bypasses_an_existing_remote_flight() {
        let coordinator = MailCoordinator::new();
        let scheduler = &coordinator.state.message_details;
        let backend = crate::integration::stub::mail_backend();
        let account_id = crate::integration::stub::stub_account().id;
        let job = |request_id: u64, conversation_id: &str| MessageDetailJob {
            service: AccountMailService::new(backend.clone()),
            request_id: request_id.into(),
            conversation_id: ConversationId(conversation_id.into()),
        };

        let active = job(1, "uncached");
        let cached = job(2, "stub-account");
        let detail = cached
            .service
            .cached_message_detail(&cached.conversation_id)
        .expect("stub cache probe should succeed")
        .expect("stub detail should be cached");

        assert!(scheduler.begin_remote(active).is_some());
        scheduler.select(&cached);
        coordinator.apply_message_detail_cache_probe(cached, Ok(Some(detail)));

        assert!(matches!(
            coordinator.drain().as_slice(),
            [MailEvent::MessageDetailLoaded {
                request_id,
                result: Ok(Some(_)),
            }] if *request_id == RequestId::from(2)
        ));
        assert!(scheduler.finish_remote(&account_id).is_none());
    }

    #[test]
    fn failed_cache_probe_is_not_reclassified_as_a_remote_cache_miss() {
        let coordinator = MailCoordinator::new();
        let scheduler = &coordinator.state.message_details;
        let account_id = MailAccountId("account-1".into());
        let job = MessageDetailJob {
            service: AccountMailService::new(
                crate::integration::stub::test_mail_backend(account_id.clone()),
            ),
            request_id: 7.into(),
            conversation_id: ConversationId("conversation-1".into()),
        };
        scheduler.select(&job);

        coordinator.apply_message_detail_cache_probe(job, Err(anyhow::anyhow!("cache failed")));

        assert!(matches!(
            coordinator.drain().as_slice(),
            [MailEvent::MessageDetailLoaded {
                request_id,
                result: Err(_),
            }] if *request_id == RequestId::from(7)
        ));
        assert!(!scheduler.has_current(&account_id));
        assert!(scheduler.finish_remote(&account_id).is_none());
    }

    #[test]
    fn canceled_selection_ignores_a_late_cached_hit() {
        let coordinator = MailCoordinator::new();
        let scheduler = &coordinator.state.message_details;
        let backend = crate::integration::stub::mail_backend();
        let account_id = crate::integration::stub::stub_account().id;
        let job = MessageDetailJob {
            service: AccountMailService::new(backend),
            request_id: 1.into(),
            conversation_id: ConversationId("stub-account".into()),
        };
        let detail = job
            .service
            .cached_message_detail(&job.conversation_id)
        .expect("stub cache probe should succeed")
        .expect("stub detail should be cached");

        scheduler.select(&job);
        scheduler.cancel(&account_id);
        coordinator.apply_message_detail_cache_probe(job, Ok(Some(detail)));

        assert!(coordinator.drain().is_empty());
        assert!(scheduler.finish_remote(&account_id).is_none());
    }

    #[test]
    fn out_of_order_cache_misses_cannot_replace_the_newest_remote_follow_up() {
        let coordinator = MailCoordinator::new();
        let scheduler = &coordinator.state.message_details;
        let account_id = MailAccountId("account-1".into());
        let job = |request_id: u64, conversation_id: &str| MessageDetailJob {
            service: AccountMailService::new(
                crate::integration::stub::test_mail_backend(account_id.clone()),
            ),
            request_id: request_id.into(),
            conversation_id: ConversationId(conversation_id.into()),
        };
        let active = job(1, "active");
        let stale = job(2, "stale");
        let newest = job(3, "newest");

        assert!(scheduler.begin_remote(active).is_some());
        scheduler.select(&stale);
        scheduler.select(&newest);
        coordinator.apply_message_detail_cache_probe(stale, Ok(None));
        coordinator.apply_message_detail_cache_probe(newest, Ok(None));

        let follow_up = scheduler
            .finish_remote(&account_id)
            .expect("newest cache miss should be the only remote follow-up");
        assert_eq!(follow_up.request_id, RequestId::from(3));
        assert_eq!(follow_up.conversation_id, ConversationId("newest".into()));
    }

    #[test]
    fn message_detail_flights_are_scoped_per_account() {
        let coordinator = MailCoordinator::new();
        let scheduler = &coordinator.state.message_details;
        let job = |account_id: &str, request_id: u64| MessageDetailJob {
            service: AccountMailService::new(
                crate::integration::stub::test_mail_backend(MailAccountId(account_id.into())),
            ),
            request_id: request_id.into(),
            conversation_id: ConversationId("conversation".into()),
        };

        assert!(scheduler.begin_cache(job("account-1", 1)).is_some());
        assert!(scheduler.begin_cache(job("account-2", 2)).is_some());
        assert!(scheduler.begin_cache(job("account-1", 3)).is_none());

        let first_cache_follow_up = scheduler
            .finish_cache(&MailAccountId("account-1".into()))
            .expect("first account should retain its cache follow-up");
        assert_eq!(first_cache_follow_up.request_id, RequestId::from(3));
        assert!(scheduler
            .finish_cache(&MailAccountId("account-2".into()))
            .is_none());

        assert!(scheduler.begin_remote(job("account-1", 4)).is_some());
        assert!(scheduler.begin_remote(job("account-2", 5)).is_some());
        assert!(scheduler.begin_remote(job("account-1", 6)).is_none());

        let first_follow_up = scheduler
            .finish_remote(&MailAccountId("account-1".into()))
            .expect("first account should retain its follow-up");
        assert_eq!(first_follow_up.request_id, RequestId::from(6));
        assert!(
            scheduler
                .finish_remote(&MailAccountId("account-2".into()))
                .is_none()
        );
    }

    #[test]
    fn clearing_message_selection_discards_its_queued_detail() {
        let coordinator = MailCoordinator::new();
        let scheduler = &coordinator.state.message_details;
        let account_id = MailAccountId("account-1".into());
        let job = |request_id: u64| MessageDetailJob {
            service: AccountMailService::new(
                crate::integration::stub::test_mail_backend(account_id.clone()),
            ),
            request_id: request_id.into(),
            conversation_id: ConversationId("conversation".into()),
        };

        assert!(scheduler.begin_cache(job(1)).is_some());
        assert!(scheduler.begin_cache(job(2)).is_none());
        assert!(scheduler.begin_remote(job(3)).is_some());
        assert!(scheduler.begin_remote(job(4)).is_none());
        coordinator.cancel_message_detail_requests(&account_id);

        assert!(scheduler.finish_cache(&account_id).is_none());
        assert!(scheduler.finish_remote(&account_id).is_none());
        assert!(scheduler.begin_cache(job(5)).is_some());
        assert!(scheduler.begin_remote(job(6)).is_some());
    }

}
