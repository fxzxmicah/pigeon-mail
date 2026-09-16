use crate::model::account::{MailAccountId, SendingIdentity, Signature};
use crate::model::address::normalized_mailbox_address;
use crate::model::mail::{escape_html_text, plain_text_to_html};
use crate::model::mail::{
    AttachmentSource, DraftMessage, MailtoRequest, MessageDetail, SIGNATURE_REGION_ATTRIBUTE,
    TextRange,
};

pub fn create_draft(account_id: MailAccountId, identity: &SendingIdentity) -> DraftMessage {
    let mut draft = draft_for_identity(account_id, identity);
    set_body_with_signature(&mut draft, "", "", &identity.signature, "", "");
    draft
}

pub fn create_mailto_draft(
    account_id: MailAccountId,
    identity: &SendingIdentity,
    request: &MailtoRequest,
) -> DraftMessage {
    let mut draft = draft_for_identity(account_id, identity);
    draft.to = request.to.clone();
    draft.cc = request.cc.clone();
    draft.bcc = request.bcc.clone();
    draft.subject = request.subject.clone();
    set_body_with_signature(
        &mut draft,
        &plain_text_to_html(&request.body),
        &request.body,
        &identity.signature,
        "",
        "",
    );
    draft
}

pub fn create_reply_draft(
    account_id: MailAccountId,
    identity: &SendingIdentity,
    message: &MessageDetail,
) -> DraftMessage {
    let mut draft = draft_for_identity(account_id, identity);
    draft.to = vec![
        message
            .reply_to
            .as_ref()
            .filter(|address| !address.trim().is_empty())
            .unwrap_or(&message.from)
            .clone(),
    ];
    draft.subject = prefixed_subject("Re:", &message.subject);
    set_body_with_signature(
        &mut draft,
        "",
        "",
        &identity.signature,
        &quoted_reply_html(message),
        &quoted_reply_text(message),
    );
    draft
}

pub fn create_reply_all_draft(
    account_id: MailAccountId,
    identity: &SendingIdentity,
    account_identities: &[SendingIdentity],
    message: &MessageDetail,
) -> DraftMessage {
    let mut draft = create_reply_draft(account_id, identity, message);
    let own_addresses = account_identities
        .iter()
        .map(|identity| normalized_mailbox_address(&identity.address))
        .filter(|address| !address.is_empty())
        .collect::<std::collections::HashSet<_>>();
    let mut seen = draft
        .to
        .iter()
        .map(|recipient| normalized_mailbox_address(recipient))
        .collect::<std::collections::HashSet<_>>();
    draft.cc = message
        .to
        .iter()
        .chain(message.cc.iter())
        .filter_map(|recipient| {
            let address = normalized_mailbox_address(recipient);
            (!address.is_empty() && !own_addresses.contains(&address) && seen.insert(address))
                .then(|| recipient.clone())
        })
        .collect();
    draft
}

pub fn create_forward_draft(
    account_id: MailAccountId,
    identity: &SendingIdentity,
    message: &MessageDetail,
) -> DraftMessage {
    let mut draft = draft_for_identity(account_id, identity);
    draft.subject = prefixed_subject("Fwd:", &message.subject);
    draft.attachments = message.attachments.clone();
    draft.set_attachment_source(inherited_attachment_source(&draft, message));
    set_body_with_signature(
        &mut draft,
        "",
        "",
        &identity.signature,
        &forwarded_html(message),
        &forwarded_text(message),
    );
    draft
}

pub fn create_edit_draft(
    account_id: MailAccountId,
    identity: &SendingIdentity,
    message: &MessageDetail,
) -> DraftMessage {
    let mut draft = draft_for_identity(account_id, identity);
    draft.conversation_id = Some(message.conversation_id.clone());
    draft.message_id = Some(message.message_id.clone());
    draft.to = message.to.clone();
    draft.cc = message.cc.clone();
    draft.bcc = message.bcc.clone();
    draft.subject = message.subject.clone();
    draft.attachments = message.attachments.clone();
    draft.set_attachment_source(inherited_attachment_source(&draft, message));
    draft.body.replace(
        message.body.html_part().unwrap_or_default().to_string(),
        message.body.text_part().unwrap_or_default().to_string(),
        None,
    );
    draft
}

fn inherited_attachment_source(
    draft: &DraftMessage,
    message: &MessageDetail,
) -> Option<AttachmentSource> {
    draft.has_cached_attachments().then(|| AttachmentSource {
        account_id: draft.account_id.clone(),
        conversation_id: message.conversation_id.clone(),
    })
}

fn draft_for_identity(account_id: MailAccountId, identity: &SendingIdentity) -> DraftMessage {
    let mut draft = DraftMessage::empty(account_id, identity.mailbox());
    draft.reply_to = identity.reply_to.clone();
    draft
}

fn set_body_with_signature(
    draft: &mut DraftMessage,
    authored_html: &str,
    authored_text: &str,
    signature: &Signature,
    trailing_html: &str,
    trailing_text: &str,
) {
    let authored_html = if authored_html.is_empty() {
        "<div><br></div>"
    } else {
        authored_html
    };
    let signature_html = if signature.html.is_empty() {
        String::new()
    } else {
        format!("<div><br></div>{}", signature.html)
    };
    let after_html = if trailing_html.is_empty() {
        String::new()
    } else {
        format!("<div><br></div>{trailing_html}")
    };
    let html = format!(
        "{authored_html}<div {SIGNATURE_REGION_ATTRIBUTE}>{signature_html}</div>{after_html}"
    );

    let signature_text = if signature.text.is_empty() {
        String::new()
    } else {
        format!("\n\n{}", signature.text)
    };
    let signature_text_range = TextRange::for_segment(authored_text, &signature_text);
    let after_text = if trailing_text.is_empty() {
        String::new()
    } else {
        format!("\n\n{trailing_text}")
    };
    let text = format!("{authored_text}{signature_text}{after_text}");
    draft.body.replace(html, text, Some(signature_text_range));
}

fn prefixed_subject(prefix: &str, subject: &str) -> String {
    let trimmed = subject.trim_start();
    if trimmed
        .get(..prefix.len())
        .map(|candidate| candidate.eq_ignore_ascii_case(prefix))
        .unwrap_or(false)
    {
        subject.to_string()
    } else {
        format!("{prefix} {}", subject)
    }
}

fn quoted_reply_text(message: &MessageDetail) -> String {
    format!(
        "On {}, {} wrote:\n> {}",
        message.date_label,
        message.from,
        message.body.presentation_text().replace('\n', "\n> ")
    )
}

fn quoted_reply_html(message: &MessageDetail) -> String {
    format!(
        "<blockquote><p><b>On {}</b>, {} wrote:</p>{}</blockquote>",
        escape_html_text(&message.date_label),
        escape_html_text(&message.from),
        message.body.presentation_html()
    )
}

fn forwarded_text(message: &MessageDetail) -> String {
    format!(
        "---------- Forwarded message ----------\nFrom: {}\nDate: {}\nTo: {}\nSubject: {}\n\n{}",
        message.from,
        message.date_label,
        message.to.join(", "),
        message.subject,
        message.body.presentation_text()
    )
}

fn forwarded_html(message: &MessageDetail) -> String {
    format!(
        "<p>---------- Forwarded message ----------</p><p><b>From:</b> {}<br><b>Date:</b> {}<br><b>To:</b> {}<br><b>Subject:</b> {}</p>{}",
        escape_html_text(&message.from),
        escape_html_text(&message.date_label),
        escape_html_text(&message.to.join(", ")),
        escape_html_text(&message.subject),
        message.body.presentation_html()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::mail::{
        AttachmentInfo, AttachmentLocation, ConversationId, MailtoRequest, MessageBody,
        MessageId,
    };

    fn identity() -> SendingIdentity {
        SendingIdentity::new(
            "me@example.com".into(),
            "Me".into(),
            None,
            Signature {
                html: "<p>Signature</p>".into(),
                text: "Signature".into(),
            },
        )
    }

    fn message() -> MessageDetail {
        MessageDetail {
            message_id: MessageId("message-1".into()),
            conversation_id: ConversationId("conversation-1".into()),
            subject: "Subject <unsafe>".into(),
            from: "Sender <sender@example.com>".into(),
            to: vec!["ME@EXAMPLE.COM".into(), "other@example.com".into()],
            cc: vec!["other@example.com".into(), "third@example.com".into()],
            bcc: Vec::new(),
            reply_to: None,
            date_label: "Today & tomorrow".into(),
            starred: false,
            unread: false,
            attachments: Vec::new(),
            body: MessageBody::from_parts("<p>Body</p>".into(), "Body".into()),
        }
    }

    #[test]
    fn subject_prefix_detection_is_case_insensitive() {
        assert_eq!(prefixed_subject("Re:", "RE: Existing"), "RE: Existing");
        assert_eq!(prefixed_subject("Re:", "Subject"), "Re: Subject");
    }

    #[test]
    fn new_draft_materializes_a_marked_signature_region() {
        let draft = create_draft(MailAccountId("account-1".into()), &identity());

        assert_eq!(draft.body.text(), "\n\nSignature");
        assert!(
            draft
                .body
                .html()
                .starts_with("<div><br></div><div data-signature-region>")
        );
        assert!(draft.body.html().contains("<p>Signature</p>"));
        assert_eq!(draft.body.text_signature(), Some(TextRange { start: 0, end: 11 }));
    }

    #[test]
    fn every_draft_kind_materializes_the_selected_identity_headers() {
        let mut identity = identity();
        identity.reply_to = Some("Replies <reply@example.test>".into());
        let account_id = MailAccountId("account-1".into());
        let source = message();
        let drafts = [
            create_draft(account_id.clone(), &identity),
            create_mailto_draft(account_id.clone(), &identity, &MailtoRequest::default()),
            create_reply_draft(account_id.clone(), &identity, &source),
            create_reply_all_draft(
                account_id.clone(),
                &identity,
                std::slice::from_ref(&identity),
                &source,
            ),
            create_forward_draft(account_id.clone(), &identity, &source),
            create_edit_draft(account_id, &identity, &source),
        ];

        for draft in drafts {
            assert_eq!(draft.from, "\"Me\" <me@example.com>");
            assert_eq!(
                draft.reply_to.as_deref(),
                Some("Replies <reply@example.test>")
            );
        }
    }

    #[test]
    fn mailto_draft_preserves_recipients_and_materializes_the_signature() {
        let request = MailtoRequest {
            to: vec!["to@example.net".into()],
            cc: vec!["copy@example.org".into()],
            bcc: vec!["hidden@example.com".into()],
            subject: "Subject".into(),
            body: "A < B\nSecond line".into(),
        };
        let draft = create_mailto_draft(MailAccountId("account-1".into()), &identity(), &request);

        assert_eq!(draft.to, request.to);
        assert_eq!(draft.cc, request.cc);
        assert_eq!(draft.bcc, request.bcc);
        assert_eq!(draft.subject, "Subject");
        assert_eq!(draft.body.text(), "A < B\nSecond line\n\nSignature");
        assert!(draft.body.html().starts_with("A &lt; B<br>Second line"));
        assert!(draft.body.html().contains("data-signature-region"));
    }

    #[test]
    fn blank_mailto_body_still_has_an_editable_signature_region() {
        let draft = create_mailto_draft(
            MailAccountId("account-1".into()),
            &identity(),
            &MailtoRequest::default(),
        );

        assert_eq!(draft.body.text(), "\n\nSignature");
        assert!(draft.body.html().contains("<p>Signature</p>"));
        assert!(draft.body.text_signature().is_some());
    }

    #[test]
    fn mailto_body_without_a_signature_does_not_reserve_blank_lines() {
        let mut identity = identity();
        identity.signature.text.clear();
        identity.signature.html.clear();
        let draft = create_mailto_draft(
            MailAccountId("account-1".into()),
            &identity,
            &MailtoRequest {
                body: "Message body".into(),
                ..Default::default()
            },
        );

        assert_eq!(draft.body.text(), "Message body");
        assert_eq!(
            draft.body.text_signature(),
            Some(TextRange { start: 12, end: 12 })
        );
        assert!(draft.body.html().contains("data-signature-region></div>"));
    }

    #[test]
    fn blank_draft_without_a_signature_starts_with_one_empty_editor_line() {
        let mut identity = identity();
        identity.signature = Signature::default();

        let draft = create_draft(MailAccountId("account-1".into()), &identity);

        assert!(draft.body.text().is_empty());
        assert_eq!(
            draft.body.html(),
            "<div><br></div><div data-signature-region></div>"
        );
        assert_eq!(
            draft.body.text_signature(),
            Some(TextRange { start: 0, end: 0 })
        );
    }

    #[test]
    fn editing_a_draft_preserves_its_message_identity_and_html() {
        let draft = create_edit_draft(MailAccountId("account-1".into()), &identity(), &message());

        assert_eq!(draft.message_id.as_ref().unwrap().0, "message-1");
        assert_eq!(draft.conversation_id.as_ref().unwrap().0, "conversation-1");
        assert_eq!(draft.body.html(), "<p>Body</p>");
    }

    #[test]
    fn inherited_attachments_keep_their_source_for_editing_and_forwarding() {
        let mut message = message();
        message.attachments.push(AttachmentInfo {
            display_name: "report.pdf".into(),
            location: AttachmentLocation::CachedToken("1".into()),
        });
        let account_id = MailAccountId("account-1".into());

        let edited = create_edit_draft(account_id.clone(), &identity(), &message);
        let forwarded = create_forward_draft(account_id.clone(), &identity(), &message);

        for draft in [edited, forwarded] {
            assert_eq!(draft.attachments.len(), 1);
            let source = draft
                .attachment_source()
                .expect("inherited attachments require a materialization source");
            assert_eq!(source.account_id, account_id);
            assert_eq!(source.conversation_id, message.conversation_id);
        }
    }

    #[test]
    fn editing_plain_text_keeps_the_html_representation_absent() {
        let mut message = message();
        message.body = MessageBody::from_parts(String::new(), "A < B\nSecond line".into());

        let draft = create_edit_draft(MailAccountId("account-1".into()), &identity(), &message);

        assert_eq!(draft.body.text(), "A < B\nSecond line");
        assert!(draft.body.html().is_empty());
    }

    #[test]
    fn editing_html_keeps_the_text_representation_absent() {
        let mut message = message();
        message.body = MessageBody::from_parts("<p>Rich only</p>".into(), String::new());

        let draft = create_edit_draft(MailAccountId("account-1".into()), &identity(), &message);

        assert_eq!(draft.body.html(), "<p>Rich only</p>");
        assert!(draft.body.text().is_empty());
    }

    #[test]
    fn editing_an_empty_external_body_keeps_both_representations_empty() {
        let mut message = message();
        message.body = MessageBody::Empty;

        let draft = create_edit_draft(MailAccountId("account-1".into()), &identity(), &message);

        assert!(draft.body.text().is_empty());
        assert!(draft.body.html().is_empty());
    }

    #[test]
    fn reply_all_excludes_the_sender_identity_and_deduplicates_recipients() {
        let identity = identity();
        let draft = create_reply_all_draft(
            MailAccountId("account-1".into()),
            &identity,
            std::slice::from_ref(&identity),
            &message(),
        );

        assert_eq!(draft.to, vec!["Sender <sender@example.com>"]);
        assert_eq!(draft.cc, vec!["other@example.com", "third@example.com"]);
        assert_eq!(draft.from, "\"Me\" <me@example.com>");
    }

    #[test]
    fn reply_all_excludes_every_identity_owned_by_the_account() {
        let identity = identity();
        let alias = SendingIdentity::new(
            "alias@example.net".into(),
            "Alias".into(),
            None,
            Signature::default(),
        );
        let mut message = message();
        message.to.push("Alias <ALIAS@example.net>".into());
        message.cc.push("alias@example.net".into());

        let draft = create_reply_all_draft(
            MailAccountId("account-1".into()),
            &identity,
            &[identity.clone(), alias],
            &message,
        );

        assert_eq!(draft.cc, ["other@example.com", "third@example.com"]);
    }

    #[test]
    fn reply_uses_reply_to_and_reply_all_compares_exact_normalized_addresses() {
        let mut message = message();
        message.reply_to = Some("Replies <reply@example.test>".into());
        message.to = vec![
            "Me <ME@example.com>".into(),
            "Not Me <notme@example.com>".into(),
            "Duplicate <duplicate@example.test>".into(),
        ];
        message.cc = vec![
            "duplicate@example.test".into(),
            "reply@example.test".into(),
            "  ".into(),
            "<>".into(),
        ];

        let reply = create_reply_draft(MailAccountId("account-1".into()), &identity(), &message);
        let identity = identity();
        let reply_all = create_reply_all_draft(
            MailAccountId("account-1".into()),
            &identity,
            std::slice::from_ref(&identity),
            &message,
        );

        assert_eq!(reply.to, vec!["Replies <reply@example.test>"]);
        assert_eq!(
            reply_all.cc,
            vec![
                "Not Me <notme@example.com>",
                "Duplicate <duplicate@example.test>",
            ]
        );
    }

    #[test]
    fn generated_html_escapes_header_metadata_but_preserves_message_html() {
        let reply = quoted_reply_html(&message());
        let forwarded = forwarded_html(&message());

        assert!(reply.contains("Today &amp; tomorrow"));
        assert!(forwarded.contains("Subject &lt;unsafe&gt;"));
        assert!(reply.contains("<p>Body</p>"));
        assert!(forwarded.contains("<p>Body</p>"));
    }
}
