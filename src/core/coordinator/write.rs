use crate::core::mail::AccountMailService;
use crate::model::account::{MailAccount, MailAccountId};
use crate::model::event::{MailEvent, RequestId};
use crate::model::mail::{
    AttachmentLocation, ConversationId, DraftMessage, MessageAction, PreparedMessage,
};

use super::{MailCoordinator, refresh::ConvergenceWork, scheduler::WriteLease};

#[derive(Clone, Copy)]
enum DraftWrite {
    Save,
    Send,
}

pub(super) struct MessageActionJob {
    service: AccountMailService,
    request_id: RequestId,
    conversation_id: ConversationId,
    action: MessageAction,
    write_lease: WriteLease,
}

pub(super) struct DraftWriteJob {
    service: AccountMailService,
    attachment_service: Option<AccountMailService>,
    draft: DraftMessage,
    operation: DraftWrite,
    write_lease: WriteLease,
}

pub(super) enum MailboxWriteJob {
    MessageAction(MessageActionJob),
    Draft(DraftWriteJob),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum MailboxWriteKey {
    Message(MailAccountId, ConversationId),
    Draft(MailAccountId),
}

impl MailboxWriteJob {
    fn key(&self) -> MailboxWriteKey {
        match self {
            Self::MessageAction(job) => MailboxWriteKey::Message(
                job.service.account_id().clone(),
                job.conversation_id.clone(),
            ),
            Self::Draft(job) => MailboxWriteKey::Draft(job.service.account_id().clone()),
        }
    }
}

impl MailCoordinator {
    pub fn request_message_action(
        &self,
        request_id: RequestId,
        account_id: MailAccountId,
        conversation_id: ConversationId,
        action: MessageAction,
    ) {
        let Some(service) = self.service().lease_account(&account_id) else {
            self.publish(MailEvent::MessageActionCompleted {
                request_id,
                result: Err("Change not saved.".into()),
            });
            return;
        };
        self.queue_mailbox_write(MailboxWriteJob::MessageAction(MessageActionJob {
            service,
            request_id,
            conversation_id,
            action,
            write_lease: self.begin_write(),
        }));
    }

    pub fn request_account_identities_save(
        &self,
        request_id: RequestId,
        accounts: Vec<MailAccount>,
    ) {
        let service = self.service();
        let coordinator = self.clone();
        let write_lease = self.begin_write();
        std::thread::spawn(move || {
            let _write_lease = write_lease;
            let result = service
                .save_account_identities(&accounts)
                .map_err(|error| {
                    crate::logging::report_failure("eds-identity-save", &error);
                    "Changes not saved.".to_string()
                });
            coordinator.publish(MailEvent::AccountIdentitiesSaveCompleted {
                request_id,
                result,
            });
        });
    }

    pub fn request_save_draft(&self, draft: DraftMessage) {
        self.request_draft_write(draft, DraftWrite::Save);
    }

    pub fn request_send_draft(&self, draft: DraftMessage) {
        self.request_draft_write(draft, DraftWrite::Send);
    }

    fn request_draft_write(&self, draft: DraftMessage, operation: DraftWrite) {
        let service_router = self.service();
        let Some(service) = service_router.lease_account(&draft.account_id) else {
            match operation {
                DraftWrite::Save => self.publish(MailEvent::DraftSaveCompleted {
                    result: Err("Draft not saved.".into()),
                }),
                DraftWrite::Send => self.publish(MailEvent::SendCompleted {
                    result: Err("Message not sent.".into()),
                }),
            }
            return;
        };
        let attachment_service = draft
            .attachment_source()
            .and_then(|source| service_router.lease_account(&source.account_id));
        self.queue_mailbox_write(MailboxWriteJob::Draft(DraftWriteJob {
            service,
            attachment_service,
            draft,
            operation,
            write_lease: self.begin_write(),
        }));
    }

    fn queue_mailbox_write(&self, job: MailboxWriteJob) {
        let key = job.key();
        let Some(job) = self.state.mailbox_writes.begin(key, job) else {
            return;
        };
        self.spawn_mailbox_write(job);
    }

    fn spawn_mailbox_write(&self, mut job: MailboxWriteJob) {
        let coordinator = self.clone();
        std::thread::spawn(move || loop {
            let key = job.key();
            match job {
                MailboxWriteJob::MessageAction(job) => coordinator.run_message_action(job),
                MailboxWriteJob::Draft(job) => coordinator.run_draft_write(job),
            }
            let Some(next) = coordinator.state.mailbox_writes.finish(&key) else {
                break;
            };
            job = next;
        });
    }

    fn run_message_action(&self, job: MessageActionJob) {
        let MessageActionJob {
            service,
            request_id,
            conversation_id,
            action,
            write_lease,
        } = job;
        let result = service
            .apply_message_action(&conversation_id, &action)
            .map_err(|error| {
                crate::logging::report_failure("mail-action-cache-commit", &error);
                "Change not saved.".to_string()
            });
        let requires_convergence = result
            .as_ref()
            .is_ok_and(|outcome| outcome.requires_convergence());
        let convergence = requires_convergence
            .then(|| ConvergenceWork::queued_message_action(write_lease));
        self.publish(MailEvent::MessageActionCompleted {
            request_id,
            result,
        });
        if let Some(convergence) = convergence {
            self.request_account_convergence(service, convergence);
        }
    }

    fn run_draft_write(&self, job: DraftWriteJob) {
        let DraftWriteJob {
            service,
            attachment_service,
            draft,
            operation,
            write_lease,
        } = job;
        let account_id = service.account_id().clone();
        let prepared = prepare_message(attachment_service.as_ref(), draft);
        let (cache_changed, requires_convergence) = match operation {
            DraftWrite::Save => {
                let result = prepared
                    .and_then(|message| service.save_draft(&message))
                    .map_err(|error| {
                        crate::logging::report_failure("draft-cache-save", &error);
                        "Draft not saved.".to_string()
                    });
                let cache_changed = matches!(&result, Ok(Some(_)));
                self.publish(MailEvent::DraftSaveCompleted { result });
                (cache_changed, cache_changed)
            }
            DraftWrite::Send => {
                let result = prepared
                    .and_then(|message| service.queue_delivery(&message))
                    .map_err(|error| {
                        crate::logging::report_failure("message-send", &error);
                        "Message not sent.".to_string()
                    });
                let cache_changed = result
                    .as_ref()
                    .is_ok_and(|outcome| outcome.changed());
                let requires_convergence = result
                    .as_ref()
                    .is_ok_and(|outcome| outcome.requires_convergence());
                self.publish(MailEvent::SendCompleted {
                    result: result.map(|_| ()),
                });
                (cache_changed, requires_convergence)
            }
        };
        if cache_changed {
            self.publish(MailEvent::MailboxCacheChanged {
                account_id: account_id.clone(),
            });
        }
        if requires_convergence {
            self.request_account_convergence(service, ConvergenceWork::leased_write(write_lease));
        }
    }
}

pub(super) fn prepare_message(
    service: Option<&AccountMailService>,
    mut draft: DraftMessage,
) -> anyhow::Result<PreparedMessage> {
    if draft
        .attachments
        .iter()
        .all(|attachment| attachment.location.cached_token().is_none())
    {
        return draft.into_prepared().map_err(anyhow::Error::msg);
    }
    let source = draft
        .attachment_source()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("cached draft attachment has no source conversation"))?;
    let service = service.ok_or_else(|| anyhow::anyhow!("attachment source is unavailable"))?;
    for attachment in &mut draft.attachments {
        let Some(source_token) = attachment.location.cached_token().map(str::to_owned) else {
            continue;
        };
        let uri = service
            .materialize_attachment(&source.conversation_id, &source_token)?
            .ok_or_else(|| anyhow::anyhow!("cached draft attachment is unavailable"))?;
        attachment.location = AttachmentLocation::ExternalUri(uri);
    }
    draft.into_prepared().map_err(anyhow::Error::msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailbox_write_keys_isolate_only_conflicting_operations() {
        let account = MailAccountId("account".into());
        let message = |id: &str| {
            MailboxWriteKey::Message(
                account.clone(),
                ConversationId(id.into()),
            )
        };
        let first = message("inbox\u{1f}first");
        let same = message("inbox\u{1f}first");
        let other_message = message("inbox\u{1f}second");
        let draft = MailboxWriteKey::Draft(account);

        assert_eq!(first, same);
        assert_ne!(first, other_message);
        assert_ne!(first, draft);
    }
}
