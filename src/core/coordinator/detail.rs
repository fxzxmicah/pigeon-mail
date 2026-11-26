use std::collections::HashMap;
use std::sync::Mutex;

use crate::core::mail::AccountMailService;
use crate::i18n::gettext;
use crate::model::account::MailAccountId;
use crate::model::event::{MailEvent, RequestId};
use crate::model::mail::{ConversationId, MessageDetail};

use super::MailCoordinator;

struct MessageDetailJob {
    service: AccountMailService,
    request_id: RequestId,
    conversation_id: ConversationId,
}

pub(super) struct MessageDetailScheduler {
    requests: Mutex<HashMap<MailAccountId, DetailRequests>>,
}

#[derive(Default)]
struct DetailRequests {
    current: Option<RequestId>,
    cache: DetailFlight,
    remote: DetailFlight,
}

#[derive(Default)]
enum DetailFlight {
    #[default]
    Idle,
    Running { pending: Option<MessageDetailJob> },
}

impl DetailFlight {
    fn enqueue(&mut self, job: MessageDetailJob) -> Option<MessageDetailJob> {
        match self {
            Self::Idle => {
                *self = Self::Running { pending: None };
                Some(job)
            }
            Self::Running { pending } => {
                *pending = Some(job);
                None
            }
        }
    }

    fn finish(&mut self) -> Option<MessageDetailJob> {
        if let Self::Running { pending } = self
            && let Some(next) = pending.take()
        {
            return Some(next);
        }
        *self = Self::Idle;
        None
    }

    fn clear_pending(&mut self) {
        if let Self::Running { pending } = self {
            *pending = None;
        }
    }
}

impl DetailRequests {
    fn is_idle(&self) -> bool {
        self.current.is_none()
            && matches!(self.cache, DetailFlight::Idle)
            && matches!(self.remote, DetailFlight::Idle)
    }
}

impl MessageDetailScheduler {
    pub(super) fn new() -> Self {
        Self {
            requests: Mutex::new(HashMap::new()),
        }
    }

    fn begin_cache(&self, job: MessageDetailJob) -> Option<MessageDetailJob> {
        let mut requests = self
            .requests
            .lock()
            .expect("message detail scheduler lock poisoned");
        let account = requests.entry(job.service.account_id().clone()).or_default();
        account.current = Some(job.request_id);
        account.remote.clear_pending();
        account.cache.enqueue(job)
    }

    fn begin_remote(&self, job: MessageDetailJob) -> Option<MessageDetailJob> {
        let account_id = job.service.account_id().clone();
        self.update(&account_id, |account| {
            if account.current == Some(job.request_id) {
                account.remote.enqueue(job)
            } else {
                None
            }
        })
        .flatten()
    }

    fn is_current(&self, job: &MessageDetailJob) -> bool {
        self.requests
            .lock()
            .expect("message detail scheduler lock poisoned")
            .get(job.service.account_id())
            .is_some_and(|account| account.current == Some(job.request_id))
    }

    fn complete_current(&self, job: &MessageDetailJob) -> bool {
        self.update(job.service.account_id(), |account| {
            if account.current != Some(job.request_id) {
                return false;
            }
            account.current = None;
            true
        })
        .unwrap_or(false)
    }

    fn finish_cache(&self, account_id: &MailAccountId) -> Option<MessageDetailJob> {
        self.update(account_id, |account| account.cache.finish()).flatten()
    }

    fn finish_remote(&self, account_id: &MailAccountId) -> Option<MessageDetailJob> {
        self.update(account_id, |account| account.remote.finish()).flatten()
    }

    fn cancel(&self, account_id: &MailAccountId) {
        let _ = self.update(account_id, |account| {
            account.current = None;
            account.cache.clear_pending();
            account.remote.clear_pending();
        });
    }

    fn update<T>(
        &self,
        account_id: &MailAccountId,
        change: impl FnOnce(&mut DetailRequests) -> T,
    ) -> Option<T> {
        let mut requests = self
            .requests
            .lock()
            .expect("message detail scheduler lock poisoned");
        let account = requests.get_mut(account_id)?;
        let result = change(account);
        if account.is_idle() {
            requests.remove(account_id);
        }
        Some(result)
    }

    #[cfg(test)]
    fn has_current(&self, account_id: &MailAccountId) -> bool {
        self.requests
            .lock()
            .expect("message detail scheduler lock poisoned")
            .get(account_id)
            .is_some_and(|account| account.current.is_some())
    }
}

impl MailCoordinator {
    pub fn request_message_detail(
        &self,
        request_id: RequestId,
        account_id: MailAccountId,
        conversation_id: ConversationId,
    ) {
        let Some(service) = self.service().lease_account(&account_id) else {
            self.state.message_details.cancel(&account_id);
            self.publish(MailEvent::MessageDetailLoaded {
                request_id,
                result: Err(gettext("Message unavailable.")),
            });
            return;
        };
        let job = MessageDetailJob {
            service,
            request_id,
            conversation_id,
        };
        let Some(job) = self.state.message_details.begin_cache(job) else {
            return;
        };
        self.spawn_message_detail_cache_probe(job);
    }

    fn spawn_message_detail_cache_probe(&self, mut job: MessageDetailJob) {
        let coordinator = self.clone();
        std::thread::spawn(move || loop {
            let account_id = job.service.account_id().clone();
            if coordinator.state.message_details.is_current(&job) {
                let cached = job.service.cached_message_detail(&job.conversation_id);
                coordinator.apply_message_detail_cache_probe(job, cached);
            }
            let Some(next) = coordinator.state.message_details.finish_cache(&account_id) else {
                break;
            };
            job = next;
        });
    }

    pub fn cancel_message_detail_requests(&self, account_id: &MailAccountId) {
        self.state.message_details.cancel(account_id);
    }

    fn apply_message_detail_cache_probe(
        &self,
        job: MessageDetailJob,
        result: anyhow::Result<Option<MessageDetail>>,
    ) {
        match result {
            Ok(Some(detail)) => {
                if self.state.message_details.complete_current(&job) {
                    self.publish(MailEvent::MessageDetailLoaded {
                        request_id: job.request_id,
                        result: Ok(Some(detail)),
                    });
                }
                return;
            }
            Ok(None) => {}
            Err(error) => {
                crate::logging::report_failure("message-detail-cache-probe", &error);
                if self.state.message_details.complete_current(&job) {
                    self.publish(MailEvent::MessageDetailLoaded {
                        request_id: job.request_id,
                        result: Err(gettext("Message unavailable.")),
                    });
                }
                return;
            }
        }

        if let Some(job) = self.state.message_details.begin_remote(job) {
            self.spawn_message_detail(job);
        }
    }

    fn spawn_message_detail(&self, mut job: MessageDetailJob) {
        let coordinator = self.clone();
        std::thread::spawn(move || loop {
            let account_id = job.service.account_id().clone();
            if coordinator.state.message_details.is_current(&job) {
                let result = load_message_detail(&job.service, &job.conversation_id)
                    .map_err(|error| {
                        crate::logging::report_failure("message-detail-load", &error);
                        gettext("Message unavailable.")
                    });
                if coordinator.state.message_details.complete_current(&job) {
                    coordinator.publish(MailEvent::MessageDetailLoaded {
                        request_id: job.request_id,
                        result,
                    });
                }
            }
            let Some(next) = coordinator.state.message_details.finish_remote(&account_id) else {
                break;
            };
            job = next;
        });
    }
}

fn load_message_detail(
    service: &AccountMailService,
    conversation_id: &ConversationId,
) -> anyhow::Result<Option<MessageDetail>> {
    service.fill_message_cache(conversation_id)?;
    service.cached_message_detail(conversation_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache_miss(
        scheduler: &MessageDetailScheduler,
        job: MessageDetailJob,
    ) -> Option<MessageDetailJob> {
        let account_id = job.service.account_id().clone();
        let job = scheduler.begin_cache(job).expect("no cache probe should be active");
        let remote = scheduler.begin_remote(job);
        assert!(scheduler.finish_cache(&account_id).is_none());
        remote
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

        assert!(cache_miss(scheduler, job(1, "conversation")).is_some());
        assert!(cache_miss(scheduler, job(2, "conversation")).is_none());
        assert!(cache_miss(scheduler, job(3, "conversation")).is_none());

        let follow_up = scheduler
            .finish_remote(&account_id)
            .expect("newest detail request should be retained");
        assert_eq!(follow_up.request_id, RequestId::from(3));
        assert_eq!(
            follow_up.conversation_id,
            ConversationId("conversation".into())
        );
        assert!(scheduler.finish_remote(&account_id).is_none());
        assert!(cache_miss(scheduler, job(4, "conversation")).is_some());
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

        assert!(cache_miss(scheduler, active).is_some());
        let cached = scheduler.begin_cache(cached).unwrap();
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
        let job = scheduler.begin_cache(job).unwrap();

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

        let job = scheduler.begin_cache(job).unwrap();
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

        assert!(cache_miss(scheduler, active).is_some());
        let stale = scheduler.begin_cache(stale).unwrap();
        assert!(scheduler.begin_cache(newest).is_none());
        let newest = scheduler.finish_cache(&account_id).unwrap();
        coordinator.apply_message_detail_cache_probe(newest, Ok(None));
        coordinator.apply_message_detail_cache_probe(stale, Ok(None));

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
        assert!(scheduler.finish_cache(&MailAccountId("account-1".into())).is_none());
        assert!(scheduler
            .finish_cache(&MailAccountId("account-2".into()))
            .is_none());

        assert!(cache_miss(scheduler, job("account-1", 4)).is_some());
        assert!(cache_miss(scheduler, job("account-2", 5)).is_some());
        assert!(cache_miss(scheduler, job("account-1", 6)).is_none());

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

        assert!(cache_miss(scheduler, job(1)).is_some());
        assert!(cache_miss(scheduler, job(2)).is_none());
        coordinator.cancel_message_detail_requests(&account_id);
        assert!(scheduler.finish_remote(&account_id).is_none());

        let active = scheduler.begin_cache(job(3)).unwrap();
        assert!(scheduler.begin_cache(job(4)).is_none());
        coordinator.cancel_message_detail_requests(&account_id);
        assert!(scheduler.begin_remote(active).is_none());
        assert!(scheduler.finish_cache(&account_id).is_none());
        assert!(cache_miss(scheduler, job(5)).is_some());
    }
}
