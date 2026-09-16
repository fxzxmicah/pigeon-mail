use crate::core::mail::AccountMailService;
use crate::model::account::MailAccountId;
use crate::model::event::{MailEvent, RequestId};
use crate::model::mail::FolderId;

use super::MailCoordinator;

pub(super) struct SearchJob {
    service: AccountMailService,
    request_id: RequestId,
    query: String,
}

pub(super) struct ThreadPageJob {
    service: AccountMailService,
    request_id: RequestId,
    folder_id: FolderId,
    offset: usize,
    limit: usize,
}

pub(super) struct MailboxReloadJob {
    service: AccountMailService,
    request_id: RequestId,
    selected_folder_id: Option<FolderId>,
    conversation_limit: usize,
}

pub(super) enum MailboxReadJob {
    ThreadPage(ThreadPageJob),
    Reload(MailboxReloadJob),
}

impl MailboxReadJob {
    fn account_id(&self) -> &MailAccountId {
        match self {
            Self::ThreadPage(job) => job.service.account_id(),
            Self::Reload(job) => job.service.account_id(),
        }
    }
}

impl MailCoordinator {
    pub fn request_thread_page(
        &self,
        request_id: RequestId,
        account_id: MailAccountId,
        folder_id: FolderId,
        offset: usize,
        limit: usize,
    ) {
        self.state.account_searches.cancel_pending(&account_id);
        let Some(service) = self.service().lease_account(&account_id) else {
            self.state.mailbox_reads.cancel_pending(&account_id);
            self.publish(MailEvent::ThreadPageLoaded {
                request_id,
                result: Err("Messages unavailable.".into()),
            });
            return;
        };
        self.queue_mailbox_read(MailboxReadJob::ThreadPage(ThreadPageJob {
            service,
            request_id,
            folder_id,
            offset,
            limit,
        }));
    }

    fn queue_mailbox_read(&self, job: MailboxReadJob) {
        let account_id = job.account_id().clone();
        let Some(job) = self.state.mailbox_reads.begin(account_id, job) else {
            return;
        };
        self.spawn_mailbox_read(job);
    }

    fn spawn_mailbox_read(&self, mut job: MailboxReadJob) {
        let coordinator = self.clone();
        std::thread::spawn(move || loop {
            let account_id = job.account_id().clone();
            match job {
                MailboxReadJob::ThreadPage(job) => coordinator.run_thread_page(job),
                MailboxReadJob::Reload(job) => coordinator.run_mailbox_reload(job),
            }
            let Some(next) = coordinator.state.mailbox_reads.finish(&account_id) else {
                break;
            };
            job = next;
        });
    }

    fn run_thread_page(&self, job: ThreadPageJob) {
        let result = job
            .service
            .list_conversations(&job.folder_id, job.offset, job.limit)
            .map_err(|error| {
                crate::logging::report_failure("message-page-load", &error);
                "Messages unavailable.".to_string()
            });
        self.publish(MailEvent::ThreadPageLoaded {
            request_id: job.request_id,
            result,
        });
    }

    pub fn request_mailbox_reload(
        &self,
        request_id: RequestId,
        account_id: MailAccountId,
        selected_folder_id: Option<FolderId>,
        conversation_limit: usize,
    ) {
        let Some(service) = self.service().lease_account(&account_id) else {
            self.state.mailbox_reads.cancel_pending(&account_id);
            self.publish(MailEvent::MailboxReloaded {
                request_id,
                result: Err("Cache reload failed.".into()),
            });
            return;
        };
        self.queue_mailbox_read(MailboxReadJob::Reload(MailboxReloadJob {
            service,
            request_id,
            selected_folder_id,
            conversation_limit,
        }));
    }

    fn run_mailbox_reload(&self, job: MailboxReloadJob) {
        let MailboxReloadJob {
            service,
            request_id,
            selected_folder_id,
            conversation_limit,
        } = job;
        let result = service
            .load_mailbox_content(selected_folder_id, conversation_limit)
            .map_err(|error| {
                crate::logging::report_failure("mailbox-cache-reload", &error);
                "Cache reload failed.".to_string()
            });
        self.publish(MailEvent::MailboxReloaded {
            request_id,
            result,
        });
    }

    pub fn request_search(
        &self,
        request_id: RequestId,
        account_id: MailAccountId,
        query: String,
    ) {
        self.state.mailbox_reads.cancel_pending(&account_id);
        let Some(service) = self.service().lease_account(&account_id) else {
            self.publish(MailEvent::SearchCompleted {
                request_id,
                result: Err("Search unavailable.".into()),
            });
            return;
        };
        let job = SearchJob {
            service,
            request_id,
            query,
        };
        let Some(job) = self.begin_search(job) else {
            return;
        };
        self.spawn_search(job);
    }

    pub fn cancel_pending_search(&self, account_id: &MailAccountId) {
        self.state.account_searches.cancel_pending(account_id);
    }

    fn begin_search(&self, job: SearchJob) -> Option<SearchJob> {
        self.state
            .account_searches
            .begin(job.service.account_id().clone(), job)
    }

    fn spawn_search(&self, mut job: SearchJob) {
        let coordinator = self.clone();
        std::thread::spawn(move || loop {
            let account_id = job.service.account_id().clone();
            let result = job.service.search(&job.query).map_err(|error| {
                    crate::logging::report_failure("mail-search", &error);
                    "Search unavailable.".to_string()
                });
            coordinator.publish(MailEvent::SearchCompleted {
                request_id: job.request_id,
                result,
            });
            let Some(next) = coordinator.state.account_searches.finish(&account_id) else {
                break;
            };
            job = next;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailbox_summary_reads_are_single_flight_and_keep_only_the_newest_follow_up() {
        let coordinator = MailCoordinator::new();
        let backend = crate::integration::stub::mail_backend();
        let account_id = crate::integration::stub::stub_account().id;
        let reload = |request_id: u64, selected_folder: &str| {
            MailboxReadJob::Reload(MailboxReloadJob {
                service: AccountMailService::new(backend.clone()),
                request_id: request_id.into(),
                selected_folder_id: Some(FolderId(selected_folder.into())),
                conversation_limit: request_id as usize,
            })
        };
        let page = |request_id: u64, folder_id: &str| {
            MailboxReadJob::ThreadPage(ThreadPageJob {
                service: AccountMailService::new(backend.clone()),
                request_id: request_id.into(),
                folder_id: FolderId(folder_id.into()),
                offset: 0,
                limit: crate::model::mail::CONVERSATION_PAGE_SIZE,
            })
        };

        assert!(coordinator
            .state
            .mailbox_reads
            .begin(account_id.clone(), reload(1, "inbox"))
            .is_some());
        assert!(coordinator
            .state
            .mailbox_reads
            .begin(account_id.clone(), page(2, "archive"))
            .is_none());
        assert!(coordinator
            .state
            .mailbox_reads
            .begin(account_id.clone(), reload(3, "sent"))
            .is_none());

        let follow_up = coordinator
            .state
            .mailbox_reads
            .finish(&account_id)
            .expect("newest summary read should be retained");
        assert!(matches!(
            follow_up,
            MailboxReadJob::Reload(MailboxReloadJob {
                request_id,
                selected_folder_id: Some(FolderId(folder_id)),
                conversation_limit: 3,
                ..
            }) if request_id == RequestId::from(3) && folder_id == "sent"
        ));
        assert!(coordinator.state.mailbox_reads.finish(&account_id).is_none());

        assert!(coordinator
            .state
            .mailbox_reads
            .begin(account_id.clone(), page(4, "inbox"))
            .is_some());
        assert!(coordinator
            .state
            .mailbox_reads
            .begin(account_id.clone(), reload(5, "archive"))
            .is_none());
        coordinator.cancel_transient_reads(&account_id);
        assert!(coordinator.state.mailbox_reads.finish(&account_id).is_none());
    }

    #[test]
    fn searches_are_single_flight_and_keep_only_the_newest_follow_up() {
        let coordinator = MailCoordinator::new();
        let job = |request_id: u64, query: &str| SearchJob {
            service: AccountMailService::new(
                crate::integration::stub::test_mail_backend(MailAccountId("account-1".into())),
            ),
            request_id: request_id.into(),
            query: query.into(),
        };

        assert!(coordinator.begin_search(job(1, "first")).is_some());
        assert!(coordinator.begin_search(job(2, "second")).is_none());
        assert!(coordinator.begin_search(job(3, "third")).is_none());

        let follow_up = coordinator
            .state
            .account_searches
            .finish(&MailAccountId("account-1".into()))
            .expect("newest search should be retained");
        assert_eq!(follow_up.request_id, RequestId::from(3));
        assert_eq!(follow_up.query, "third");
        assert!(
            coordinator
                .state
                .account_searches
                .finish(&MailAccountId("account-1".into()))
                .is_none()
        );
        assert!(coordinator.begin_search(job(4, "fourth")).is_some());
    }

    #[test]
    fn canceling_one_accounts_summary_reads_preserves_another_accounts_follow_up() {
        let coordinator = MailCoordinator::new();
        let first = MailAccountId("first-account".into());
        let second = MailAccountId("second-account".into());
        let job = |account_id: &MailAccountId, request_id: u64| {
            MailboxReadJob::Reload(MailboxReloadJob {
                service: AccountMailService::new(
                    crate::integration::stub::test_mail_backend(account_id.clone()),
                ),
                request_id: request_id.into(),
                selected_folder_id: None,
                conversation_limit: crate::model::mail::CONVERSATION_PAGE_SIZE,
            })
        };
        for account_id in [&first, &second] {
            assert!(coordinator
                .state
                .mailbox_reads
                .begin(account_id.clone(), job(account_id, 1))
                .is_some());
            assert!(coordinator
                .state
                .mailbox_reads
                .begin(account_id.clone(), job(account_id, 2))
                .is_none());
        }

        coordinator.cancel_transient_reads(&first);
        assert!(coordinator.state.mailbox_reads.finish(&first).is_none());
        let follow_up = coordinator.state.mailbox_reads.finish(&second).unwrap();
        assert_eq!(follow_up.account_id(), &second);
        assert!(matches!(
            follow_up,
            MailboxReadJob::Reload(MailboxReloadJob { request_id, .. })
                if request_id == RequestId::from(2)
        ));
        assert!(coordinator.state.mailbox_reads.finish(&second).is_none());
    }

    #[test]
    fn clearing_a_query_discards_its_queued_follow_up() {
        let coordinator = MailCoordinator::new();
        let account_id = MailAccountId("account-1".into());
        let job = |request_id: u64, query: &str| SearchJob {
            service: AccountMailService::new(
                crate::integration::stub::test_mail_backend(account_id.clone()),
            ),
            request_id: request_id.into(),
            query: query.into(),
        };

        assert!(coordinator.begin_search(job(1, "first")).is_some());
        assert!(coordinator.begin_search(job(2, "second")).is_none());
        coordinator.cancel_pending_search(&account_id);
        assert!(coordinator.state.account_searches.finish(&account_id).is_none());
        assert!(coordinator.begin_search(job(3, "third")).is_some());
    }
}
