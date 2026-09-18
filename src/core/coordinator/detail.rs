use std::collections::HashMap;
use std::sync::Mutex;

use crate::core::mail::AccountMailService;
use crate::i18n::gettext;
use crate::model::account::MailAccountId;
use crate::model::event::{MailEvent, RequestId};
use crate::model::mail::{ConversationId, MessageDetail};

use super::{MailCoordinator, scheduler::LatestJobQueue};

pub(super) struct MessageDetailJob {
    pub(super) service: AccountMailService,
    pub(super) request_id: RequestId,
    pub(super) conversation_id: ConversationId,
}

pub(super) struct MessageDetailScheduler {
    cache_jobs: LatestJobQueue<MailAccountId, MessageDetailJob>,
    remote_jobs: LatestJobQueue<MailAccountId, MessageDetailJob>,
    current_requests: Mutex<HashMap<MailAccountId, RequestId>>,
}

impl MessageDetailScheduler {
    pub(super) fn new() -> Self {
        Self {
            cache_jobs: LatestJobQueue::new(),
            remote_jobs: LatestJobQueue::new(),
            current_requests: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn select(&self, job: &MessageDetailJob) {
        self.current_requests
            .lock()
            .expect("message detail scheduler lock poisoned")
            .insert(job.service.account_id().clone(), job.request_id);
    }

    pub(super) fn is_current(&self, job: &MessageDetailJob) -> bool {
        self.current_requests
            .lock()
            .expect("message detail scheduler lock poisoned")
            .get(job.service.account_id())
            .copied()
            == Some(job.request_id)
    }

    pub(super) fn complete_current(&self, job: &MessageDetailJob) -> bool {
        let mut requests = self
            .current_requests
            .lock()
            .expect("message detail scheduler lock poisoned");
        if requests.get(job.service.account_id()).copied() != Some(job.request_id) {
            return false;
        }
        requests.remove(job.service.account_id());
        true
    }

    pub(super) fn begin_remote(&self, job: MessageDetailJob) -> Option<MessageDetailJob> {
        self.remote_jobs
            .begin(job.service.account_id().clone(), job)
    }

    pub(super) fn begin_cache(&self, job: MessageDetailJob) -> Option<MessageDetailJob> {
        self.cache_jobs.begin(job.service.account_id().clone(), job)
    }

    pub(super) fn finish_cache(&self, account_id: &MailAccountId) -> Option<MessageDetailJob> {
        self.cache_jobs.finish(account_id)
    }

    pub(super) fn finish_remote(&self, account_id: &MailAccountId) -> Option<MessageDetailJob> {
        self.remote_jobs.finish(account_id)
    }

    pub(super) fn cancel(&self, account_id: &MailAccountId) {
        self.cache_jobs.cancel_pending(account_id);
        self.remote_jobs.cancel_pending(account_id);
        self.current_requests
            .lock()
            .expect("message detail scheduler lock poisoned")
            .remove(account_id);
    }

    #[cfg(test)]
    pub(super) fn has_current(&self, account_id: &MailAccountId) -> bool {
        self.current_requests
            .lock()
            .expect("message detail scheduler lock poisoned")
            .contains_key(account_id)
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
        self.state.message_details.select(&job);
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

    pub(super) fn apply_message_detail_cache_probe(
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

        if !self.state.message_details.is_current(&job) {
            return;
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
    if !service.fill_message_cache(conversation_id)? {
        return Ok(None);
    }
    service.cached_message_detail(conversation_id)
}
