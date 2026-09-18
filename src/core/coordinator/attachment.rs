use crate::i18n::gettext;
use crate::model::event::MailEvent;
use crate::model::mail::{
    AttachmentInfo, AttachmentLocation, AttachmentOperation, AttachmentSource,
};

use super::MailCoordinator;

impl MailCoordinator {
    pub fn request_attachment(
        &self,
        source: Option<AttachmentSource>,
        operation: AttachmentOperation,
        attachment: AttachmentInfo,
    ) {
        let AttachmentInfo {
            display_name,
            location,
        } = attachment;
        let source_token = match location {
            AttachmentLocation::ExternalUri(uri) => {
                self.publish(MailEvent::AttachmentPrepared {
                    operation,
                    display_name,
                    result: Ok(uri),
                });
                return;
            }
            AttachmentLocation::CachedToken(token) => token,
        };
        let Some(source) = source else {
            self.publish(MailEvent::AttachmentPrepared {
                operation,
                display_name,
                result: Err(gettext("Attachment unavailable.")),
            });
            return;
        };
        let Some(mail_service) = self.service().lease_account(&source.account_id) else {
            self.publish(MailEvent::AttachmentPrepared {
                operation,
                display_name,
                result: Err(gettext("Attachment unavailable.")),
            });
            return;
        };

        let coordinator = self.clone();
        std::thread::spawn(move || {
            let result = mail_service.materialize_attachment(
                &source.conversation_id,
                &source_token,
            )
            .map_err(|error| {
                crate::logging::report_failure("attachment-prepare", &error);
                gettext("Attachment unavailable.")
            })
            .and_then(|prepared_uri| {
                prepared_uri.ok_or_else(|| gettext("Attachment unavailable."))
            });
            coordinator.publish(MailEvent::AttachmentPrepared {
                operation,
                display_name,
                result,
            });
        });
    }
}
