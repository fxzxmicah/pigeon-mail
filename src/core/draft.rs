use crate::model::account::{MailAccountId, SendingIdentity};
use crate::model::address::normalized_mailbox_address;
use crate::model::mail::plain_text_to_html;
use crate::model::mail::{DraftMessage, MailtoRequest, MessageDetail};

pub fn create_draft(account_id: MailAccountId, identity: &SendingIdentity) -> DraftMessage {
    draft_for_identity(account_id, identity)
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
    if !request.body.is_empty() {
        draft.text_body = join_authored_body(&request.body, &identity.signature_text, "\n\n");
        let escaped_body = plain_text_to_html(&request.body);
        draft.html_body = join_authored_body(&escaped_body, &identity.signature_html, "<br><br>");
    }
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
    draft.text_body = quoted_reply_text(message, &identity.signature_text);
    draft.html_body = quoted_reply_html(message, &identity.signature_html);
    draft
}

pub fn create_reply_all_draft(
    account_id: MailAccountId,
    identity: &SendingIdentity,
    message: &MessageDetail,
) -> DraftMessage {
    let mut draft = create_reply_draft(account_id, identity, message);
    let identity_address = normalized_mailbox_address(&identity.address);
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
            (!address.is_empty() && address != identity_address && seen.insert(address))
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
    draft.text_body = forwarded_text(message, &identity.signature_text);
    draft.html_body = forwarded_html(message, &identity.signature_html);
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
    draft.text_body = message.body.text_part().unwrap_or_default().to_string();
    draft.html_body = message.body.html_part().unwrap_or_default().to_string();
    draft
}

fn join_authored_body(body: &str, signature: &str, separator: &str) -> String {
    if signature.is_empty() {
        body.to_owned()
    } else {
        format!("{body}{separator}{signature}")
    }
}

fn draft_for_identity(account_id: MailAccountId, identity: &SendingIdentity) -> DraftMessage {
    let mut draft = DraftMessage::empty(account_id, identity.mailbox());
    draft.reply_to = identity.reply_to.clone();
    draft.text_body = identity.signature_text.clone();
    draft.html_body = identity.signature_html.clone();
    draft
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

fn quoted_reply_text(message: &MessageDetail, signature: &str) -> String {
    let mut body = String::new();
    if !signature.is_empty() {
        body.push_str(signature);
        body.push_str("\n\n");
    }
    body.push_str(&format!(
        "On {}, {} wrote:\n> {}",
        message.date_label,
        message.from,
        message.body.presentation_text().replace('\n', "\n> ")
    ));
    body
}

fn quoted_reply_html(message: &MessageDetail, signature: &str) -> String {
    let mut body = String::new();
    if !signature.is_empty() {
        body.push_str(signature);
        body.push_str("<br><br>");
    }
    body.push_str(&format!(
        "<blockquote><p><b>On {}</b>, {} wrote:</p>{}</blockquote>",
        escape_html(&message.date_label),
        escape_html(&message.from),
        message.body.presentation_html()
    ));
    body
}

fn forwarded_text(message: &MessageDetail, signature: &str) -> String {
    let mut body = String::new();
    if !signature.is_empty() {
        body.push_str(signature);
        body.push_str("\n\n");
    }
    body.push_str(&format!(
        "---------- Forwarded message ----------\nFrom: {}\nDate: {}\nTo: {}\nSubject: {}\n\n{}",
        message.from,
        message.date_label,
        message.to.join(", "),
        message.subject,
        message.body.presentation_text()
    ));
    body
}

fn forwarded_html(message: &MessageDetail, signature: &str) -> String {
    let mut body = String::new();
    if !signature.is_empty() {
        body.push_str(signature);
        body.push_str("<br><br>");
    }
    body.push_str(&format!(
        "<p>---------- Forwarded message ----------</p><p><b>From:</b> {}<br><b>Date:</b> {}<br><b>To:</b> {}<br><b>Subject:</b> {}</p>{}",
        escape_html(&message.from),
        escape_html(&message.date_label),
        escape_html(&message.to.join(", ")),
        escape_html(&message.subject),
        message.body.presentation_html()
    ));
    body
}

fn escape_html(value: &str) -> String {
    glib::markup_escape_text(value).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::account::AliasId;
    use crate::model::mail::{ConversationId, MailtoRequest, MessageBody, MessageId};

    fn identity() -> SendingIdentity {
        SendingIdentity::with_id(
            AliasId("alias-1".into()),
            "me@example.com".into(),
            "Me".into(),
            None,
            "<p>Signature</p>".into(),
            "Signature".into(),
            true,
            true,
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
    fn new_draft_starts_with_both_identity_signature_representations() {
        let draft = create_draft(MailAccountId("account-1".into()), &identity());

        assert_eq!(draft.text_body, "Signature");
        assert_eq!(draft.html_body, "<p>Signature</p>");
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
            create_reply_all_draft(account_id.clone(), &identity, &source),
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
    fn mailto_draft_preserves_recipients_and_places_signature_after_escaped_body() {
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
        assert_eq!(draft.text_body, "A < B\nSecond line\n\nSignature");
        assert_eq!(
            draft.html_body,
            "A &lt; B<br>Second line<br><br><p>Signature</p>"
        );
    }

    #[test]
    fn blank_mailto_body_keeps_the_normal_signature_only_draft() {
        let draft = create_mailto_draft(
            MailAccountId("account-1".into()),
            &identity(),
            &MailtoRequest::default(),
        );

        assert_eq!(draft.text_body, "Signature");
        assert_eq!(draft.html_body, "<p>Signature</p>");
    }

    #[test]
    fn mailto_body_without_a_signature_has_no_artificial_separator() {
        let mut identity = identity();
        identity.signature_text.clear();
        identity.signature_html.clear();
        let draft = create_mailto_draft(
            MailAccountId("account-1".into()),
            &identity,
            &MailtoRequest {
                body: "Message body".into(),
                ..Default::default()
            },
        );

        assert_eq!(draft.text_body, "Message body");
        assert_eq!(draft.html_body, "Message body");
    }

    #[test]
    fn editing_a_draft_preserves_its_message_identity_and_html() {
        let draft = create_edit_draft(MailAccountId("account-1".into()), &identity(), &message());

        assert_eq!(draft.message_id.as_ref().unwrap().0, "message-1");
        assert_eq!(draft.conversation_id.as_ref().unwrap().0, "conversation-1");
        assert_eq!(draft.html_body, "<p>Body</p>");
    }

    #[test]
    fn editing_plain_text_keeps_the_html_representation_absent() {
        let mut message = message();
        message.body = MessageBody::from_parts(String::new(), "A < B\nSecond line".into());

        let draft = create_edit_draft(MailAccountId("account-1".into()), &identity(), &message);

        assert_eq!(draft.text_body, "A < B\nSecond line");
        assert!(draft.html_body.is_empty());
    }

    #[test]
    fn editing_html_keeps_the_text_representation_absent() {
        let mut message = message();
        message.body = MessageBody::from_parts("<p>Rich only</p>".into(), String::new());

        let draft = create_edit_draft(MailAccountId("account-1".into()), &identity(), &message);

        assert_eq!(draft.html_body, "<p>Rich only</p>");
        assert!(draft.text_body.is_empty());
    }

    #[test]
    fn editing_an_empty_external_body_keeps_both_representations_empty() {
        let mut message = message();
        message.body = MessageBody::Empty;

        let draft = create_edit_draft(MailAccountId("account-1".into()), &identity(), &message);

        assert!(draft.text_body.is_empty());
        assert!(draft.html_body.is_empty());
    }

    #[test]
    fn reply_all_excludes_the_sender_identity_and_deduplicates_recipients() {
        let draft =
            create_reply_all_draft(MailAccountId("account-1".into()), &identity(), &message());

        assert_eq!(draft.to, vec!["Sender <sender@example.com>"]);
        assert_eq!(draft.cc, vec!["other@example.com", "third@example.com"]);
        assert_eq!(draft.from, "\"Me\" <me@example.com>");
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
        let reply_all =
            create_reply_all_draft(MailAccountId("account-1".into()), &identity(), &message);

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
        let reply = quoted_reply_html(&message(), "");
        let forwarded = forwarded_html(&message(), "");

        assert!(reply.contains("Today &amp; tomorrow"));
        assert!(forwarded.contains("Subject &lt;unsafe&gt;"));
        assert!(reply.contains("<p>Body</p>"));
        assert!(forwarded.contains("<p>Body</p>"));
    }
}
