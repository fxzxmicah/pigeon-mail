use std::collections::HashSet;

use crate::model::account::MailAccountId;

pub(crate) const SIGNATURE_REGION_ATTRIBUTE: &str = "data-signature-region";

pub(crate) const CONVERSATION_PAGE_SIZE: usize = 50;

pub(crate) fn escape_html_text(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '\'' => escaped.push_str("&apos;"),
            '"' => escaped.push_str("&quot;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

pub(crate) fn plain_text_to_html(text: &str) -> String {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    escape_html_text(&normalized).replace('\n', "<br>")
}

pub(crate) fn sort_conversations(conversations: &mut [ConversationSummary]) {
    conversations.sort_by(|left, right| {
        right
            .last_updated_unix_ms
            .cmp(&left.last_updated_unix_ms)
            .then_with(|| left.subject.cmp(&right.subject))
    });
}

pub(crate) fn sort_and_deduplicate_conversations(
    conversations: &mut Vec<ConversationSummary>,
) {
    let mut seen = HashSet::new();
    conversations.retain(|summary| seen.insert(summary.id.clone()));
    sort_conversations(conversations);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxMode {
    Loading,
    Live,
    NoAccount,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    Unchanged,
    Applied,
    Queued,
}

impl WriteOutcome {
    pub fn changed(self) -> bool {
        self != Self::Unchanged
    }

    pub fn requires_convergence(self) -> bool {
        self == Self::Queued
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageAction {
    SetStarred(bool),
    SetRead(bool),
    MoveTo(FolderId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentOperation {
    Open,
    SaveAs,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FolderId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConversationId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MessageId(pub String);

#[derive(Debug, Clone)]
pub struct AttachmentInfo {
    pub display_name: String,
    pub location: AttachmentLocation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentLocation {
    CachedToken(String),
    ExternalUri(String),
}

impl AttachmentLocation {
    pub fn cached_token(&self) -> Option<&str> {
        match self {
            Self::CachedToken(token) => Some(token),
            Self::ExternalUri(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentSource {
    pub account_id: MailAccountId,
    pub conversation_id: ConversationId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationSummary {
    pub id: ConversationId,
    pub folder_id: FolderId,
    pub subject: String,
    pub participants: Vec<String>,
    pub message_count: u32,
    pub unread_count: u32,
    pub attachment_count: u32,
    pub starred: bool,
    pub last_updated_unix_ms: i64,
    pub preview: String,
}

#[derive(Debug, Clone)]
pub struct MailFolder {
    pub id: FolderId,
    pub name: String,
    pub unread_count: u32,
    pub kind: FolderKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderKind {
    Inbox,
    Drafts,
    Outbox,
    Sent,
    Archive,
    Trash,
    Spam,
    Custom,
}

#[derive(Debug, Clone)]
pub struct MessageDetail {
    pub message_id: MessageId,
    pub conversation_id: ConversationId,
    pub subject: String,
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub reply_to: Option<String>,
    pub date_unix_secs: i64,
    pub starred: bool,
    pub unread: bool,
    pub attachments: Vec<AttachmentInfo>,
    pub body: MessageBody,
}

#[derive(Debug, Clone)]
pub enum MessageBody {
    Empty,
    Html {
        html: String,
        presentation_text: String,
    },
    Text {
        text: String,
        presentation_html: String,
    },
    Alternative {
        html: String,
        text: String,
    },
}

impl MessageBody {
    pub fn from_parts(html: String, text: String) -> Self {
        match (html.trim().is_empty(), text.trim().is_empty()) {
            (true, true) => Self::Empty,
            (false, true) => Self::Html {
                presentation_text: html2text::from_read(html.as_bytes(), u16::MAX as usize)
                    .expect("reading an in-memory HTML body cannot fail")
                    .trim_end_matches('\n')
                    .to_string(),
                html,
            },
            (true, false) => Self::Text {
                presentation_html: plain_text_to_html(&text),
                text,
            },
            (false, false) => Self::Alternative { html, text },
        }
    }

    pub fn html_part(&self) -> Option<&str> {
        match self {
            Self::Html { html, .. } | Self::Alternative { html, .. } => Some(html),
            Self::Empty | Self::Text { .. } => None,
        }
    }

    pub fn text_part(&self) -> Option<&str> {
        match self {
            Self::Text { text, .. } | Self::Alternative { text, .. } => Some(text),
            Self::Empty | Self::Html { .. } => None,
        }
    }

    pub fn presentation_html(&self) -> &str {
        match self {
            Self::Empty => "",
            Self::Html { html, .. } | Self::Alternative { html, .. } => html,
            Self::Text {
                presentation_html, ..
            } => presentation_html,
        }
    }

    pub fn presentation_text(&self) -> &str {
        match self {
            Self::Empty => "",
            Self::Html {
                presentation_text, ..
            } => presentation_text,
            Self::Text { text, .. } | Self::Alternative { text, .. } => text,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DraftMessage {
    pub conversation_id: Option<ConversationId>,
    pub message_id: Option<MessageId>,
    pub account_id: MailAccountId,
    pub from: String,
    pub reply_to: Option<String>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub attachments: Vec<AttachmentInfo>,
    attachment_source: Option<AttachmentSource>,
    pub body: DraftBody,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedMessage {
    pub(crate) conversation_id: Option<ConversationId>,
    pub(crate) message_id: Option<MessageId>,
    pub(crate) from: String,
    pub(crate) reply_to: Option<String>,
    pub(crate) to: Vec<String>,
    pub(crate) cc: Vec<String>,
    pub(crate) bcc: Vec<String>,
    pub(crate) subject: String,
    pub(crate) attachment_uris: Vec<String>,
    pub(crate) body: DraftBody,
}

#[derive(Debug, Clone, Default)]
pub struct DraftBody {
    html: String,
    text: String,
    text_signature: Option<TextRange>,
}

impl DraftBody {
    pub fn html(&self) -> &str {
        &self.html
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn text_signature(&self) -> Option<TextRange> {
        self.text_signature
    }

    pub fn set_html(&mut self, html: String) {
        self.html = html;
    }

    pub fn set_text(&mut self, text: String, text_signature: Option<TextRange>) {
        self.text_signature = text_signature.filter(|range| range.is_valid_for(&text));
        self.text = text;
    }

    pub fn replace(&mut self, html: String, text: String, text_signature: Option<TextRange>) {
        self.set_html(html);
        self.set_text(text, text_signature);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextRange {
    pub start: usize,
    pub end: usize,
}

impl TextRange {
    pub fn for_segment(prefix: &str, segment: &str) -> Self {
        let start = prefix.chars().count();
        Self {
            start,
            end: start + segment.chars().count(),
        }
    }

    pub fn split<'a>(self, text: &'a str) -> Option<(&'a str, &'a str, &'a str)> {
        if self.start > self.end {
            return None;
        }
        let start = byte_index(text, self.start)?;
        let end = byte_index(text, self.end)?;
        Some((&text[..start], &text[start..end], &text[end..]))
    }

    pub fn is_valid_for(self, text: &str) -> bool {
        self.split(text).is_some()
    }
}

fn byte_index(text: &str, character_offset: usize) -> Option<usize> {
    if character_offset == text.chars().count() {
        Some(text.len())
    } else {
        text.char_indices()
            .nth(character_offset)
            .map(|(index, _)| index)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMessageRef {
    pub conversation_id: ConversationId,
    pub message_id: MessageId,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MailtoRequest {
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub body: String,
}

impl MailtoRequest {
    pub fn is_empty(&self) -> bool {
        self.to.is_empty()
            && self.cc.is_empty()
            && self.bcc.is_empty()
            && self.subject.is_empty()
            && self.body.is_empty()
    }
}

impl DraftMessage {
    pub fn empty(account_id: MailAccountId, from: String) -> Self {
        Self {
            conversation_id: None,
            message_id: None,
            account_id,
            from,
            reply_to: None,
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: String::new(),
            attachments: Vec::new(),
            attachment_source: None,
            body: DraftBody::default(),
        }
    }

    pub(crate) fn has_cached_attachments(&self) -> bool {
        self.attachments
            .iter()
            .any(|attachment| attachment.location.cached_token().is_some())
    }

    pub(crate) fn attachment_source(&self) -> Option<&AttachmentSource> {
        self.attachment_source.as_ref()
    }

    pub(crate) fn set_attachment_source(&mut self, source: Option<AttachmentSource>) {
        self.attachment_source = if self.has_cached_attachments() {
            source
        } else {
            None
        };
    }

    pub(crate) fn into_prepared(self) -> Result<PreparedMessage, &'static str> {
        let attachment_uris = self
            .attachments
            .into_iter()
            .map(|attachment| match attachment.location {
                AttachmentLocation::ExternalUri(uri) => Ok(uri),
                AttachmentLocation::CachedToken(_) => {
                    Err("draft contains an unresolved cached attachment")
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(PreparedMessage {
            conversation_id: self.conversation_id,
            message_id: self.message_id,
            from: self.from,
            reply_to: self.reply_to,
            to: self.to,
            cc: self.cc,
            bcc: self.bcc,
            subject: self.subject,
            attachment_uris,
            body: self.body,
        })
    }
}

impl PreparedMessage {
    pub(crate) fn has_recipient(&self) -> bool {
        [&self.to, &self.cc, &self.bcc]
            .into_iter()
            .flatten()
            .any(|recipient| !recipient.trim().is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AttachmentInfo, AttachmentLocation, ConversationId, ConversationSummary, DraftBody,
        DraftMessage, FolderId, MailtoRequest, MessageBody, TextRange, WriteOutcome,
        escape_html_text, plain_text_to_html, sort_and_deduplicate_conversations,
    };
    use crate::model::account::MailAccountId;

    fn summary(subject: &str, updated: i64) -> ConversationSummary {
        ConversationSummary {
            id: ConversationId("same-message".into()),
            folder_id: FolderId("inbox".into()),
            subject: subject.into(),
            participants: Vec::new(),
            message_count: 1,
            unread_count: 0,
            attachment_count: 0,
            starred: false,
            last_updated_unix_ms: updated,
            preview: String::new(),
        }
    }

    #[test]
    fn conversation_deduplication_preserves_source_precedence_before_sorting() {
        let mut conversations = vec![summary("authoritative", 1), summary("stale", 2)];

        sort_and_deduplicate_conversations(&mut conversations);

        assert_eq!(conversations.len(), 1);
        assert_eq!(conversations[0].subject, "authoritative");
    }

    #[test]
    fn write_outcomes_separate_visible_change_from_network_convergence() {
        assert!(!WriteOutcome::Unchanged.changed());
        assert!(!WriteOutcome::Unchanged.requires_convergence());
        assert!(WriteOutcome::Applied.changed());
        assert!(!WriteOutcome::Applied.requires_convergence());
        assert!(WriteOutcome::Queued.changed());
        assert!(WriteOutcome::Queued.requires_convergence());
    }

    #[test]
    fn prepared_messages_accept_only_materialized_attachments() {
        let mut ready = DraftMessage::empty(
            MailAccountId("account".into()),
            "sender@example.com".into(),
        );
        ready.attachments.push(AttachmentInfo {
            display_name: "document.pdf".into(),
            location: AttachmentLocation::ExternalUri("file:///document.pdf".into()),
        });
        let prepared = ready.into_prepared().unwrap();
        assert_eq!(
            prepared.attachment_uris,
            vec!["file:///document.pdf".to_string()]
        );

        let mut unresolved = DraftMessage::empty(
            MailAccountId("account".into()),
            "sender@example.com".into(),
        );
        unresolved.attachments.push(AttachmentInfo {
            display_name: "cached.pdf".into(),
            location: AttachmentLocation::CachedToken("part-1".into()),
        });
        assert!(unresolved.into_prepared().is_err());
    }

    #[test]
    fn text_ranges_use_character_offsets_and_reject_invalid_boundaries() {
        let range = TextRange::for_segment("前缀", "签名");
        assert_eq!(range, TextRange { start: 2, end: 4 });
        assert_eq!(range.split("前缀签名尾部"), Some(("前缀", "签名", "尾部")));
        assert!(!TextRange { start: 4, end: 3 }.is_valid_for("text"));
        assert!(!TextRange { start: 0, end: 5 }.is_valid_for("text"));
    }

    #[test]
    fn draft_body_never_retains_a_range_outside_its_text() {
        let mut body = DraftBody::default();
        body.set_text("short".into(), Some(TextRange { start: 0, end: 9 }));

        assert_eq!(body.text(), "short");
        assert!(body.text_signature().is_none());
    }

    #[test]
    fn mailto_is_empty_only_when_every_field_is_empty() {
        assert!(MailtoRequest::default().is_empty());
        for request in [
            MailtoRequest {
                to: vec!["to@example.test".into()],
                ..Default::default()
            },
            MailtoRequest {
                cc: vec!["cc@example.test".into()],
                ..Default::default()
            },
            MailtoRequest {
                bcc: vec!["bcc@example.test".into()],
                ..Default::default()
            },
            MailtoRequest {
                subject: "Subject".into(),
                ..Default::default()
            },
            MailtoRequest {
                body: "Body".into(),
                ..Default::default()
            },
        ] {
            assert!(!request.is_empty());
        }
    }

    #[test]
    fn plain_text_html_conversion_escapes_content_and_normalizes_line_endings() {
        assert_eq!(plain_text_to_html(""), "");
        assert_eq!(
            plain_text_to_html("A < B & C\nSecond line"),
            "A &lt; B &amp; C<br>Second line"
        );
        assert_eq!(
            plain_text_to_html("One\r\nTwo\rThree"),
            "One<br>Two<br>Three"
        );
        assert_eq!(plain_text_to_html("Unicode: 鸽子"), "Unicode: 鸽子");
    }

    #[test]
    fn html_text_escaping_covers_markup_and_attribute_delimiters() {
        assert_eq!(
            escape_html_text("<&>'\""),
            "&lt;&amp;&gt;&apos;&quot;"
        );
    }

    #[test]
    fn message_body_preserves_mime_parts_and_fills_only_missing_presentations() {
        let empty = MessageBody::from_parts(String::new(), String::new());
        assert!(empty.html_part().is_none());
        assert!(empty.text_part().is_none());
        assert_eq!(empty.presentation_html(), "");
        assert_eq!(empty.presentation_text(), "");

        let text = MessageBody::from_parts(String::new(), "A < B\nSecond line".into());
        assert!(text.html_part().is_none());
        assert_eq!(text.text_part(), Some("A < B\nSecond line"));
        assert_eq!(text.presentation_html(), "A &lt; B<br>Second line");

        let html = MessageBody::from_parts(
            "<p>First &amp; second</p><p>Third</p>".into(),
            String::new(),
        );
        assert_eq!(
            html.html_part(),
            Some("<p>First &amp; second</p><p>Third</p>")
        );
        assert!(html.text_part().is_none());
        assert!(html.presentation_text().contains("First & second"));
        assert!(html.presentation_text().contains("Third"));

        let alternative = MessageBody::from_parts("<strong>Rich</strong>".into(), "Plain".into());
        assert_eq!(alternative.html_part(), Some("<strong>Rich</strong>"));
        assert_eq!(alternative.text_part(), Some("Plain"));
        assert_eq!(alternative.presentation_html(), "<strong>Rich</strong>");
        assert_eq!(alternative.presentation_text(), "Plain");
    }

    #[test]
    fn message_body_ignores_whitespace_only_parts_without_changing_real_content() {
        let empty = MessageBody::from_parts(" \n".into(), "\t".into());
        assert!(matches!(empty, MessageBody::Empty));

        let text = MessageBody::from_parts("  ".into(), "One\r\nTwo".into());
        assert!(text.html_part().is_none());
        assert_eq!(text.text_part(), Some("One\r\nTwo"));
        assert_eq!(text.presentation_html(), "One<br>Two");

        let html = MessageBody::from_parts("<p>Visible</p>".into(), " \n".into());
        assert!(html.text_part().is_none());
        assert_eq!(html.html_part(), Some("<p>Visible</p>"));
    }

    #[test]
    fn html_only_presentation_text_handles_incomplete_real_world_markup() {
        let body = MessageBody::from_parts(
            "<section>First<br><strong>Second &amp; third".into(),
            String::new(),
        );

        assert!(body.presentation_text().contains("First"));
        assert!(body.presentation_text().contains("Second & third"));
        assert!(body.text_part().is_none());
    }
}
