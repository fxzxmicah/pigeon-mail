use serde::{Deserialize, Serialize};

use crate::model::account::MailAccountId;

#[cfg(debug_assertions)]
pub(crate) const CONVERSATION_PAGE_SIZE: usize = 7;

#[cfg(not(debug_assertions))]
pub(crate) const CONVERSATION_PAGE_SIZE: usize = 50;

pub(crate) fn plain_text_to_html(text: &str) -> String {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    glib::markup_escape_text(&normalized)
        .to_string()
        .replace('\n', "<br>")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxMode {
    Loading,
    Live,
    StubNoAccount,
    StubUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FolderId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConversationId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MessageId(pub String);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentInfo {
    pub display_name: String,
    pub uri: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationSummary {
    pub id: ConversationId,
    pub subject: String,
    pub participants: Vec<String>,
    pub message_count: u32,
    pub unread_count: u32,
    pub attachment_count: u32,
    pub starred: bool,
    pub last_updated_unix_ms: i64,
    pub preview: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailFolder {
    pub id: FolderId,
    pub name: String,
    pub unread_count: u32,
    pub kind: FolderKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageDetail {
    pub message_id: MessageId,
    pub conversation_id: ConversationId,
    pub subject: String,
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub reply_to: Option<String>,
    pub date_label: String,
    pub starred: bool,
    pub unread: bool,
    pub attachments: Vec<AttachmentInfo>,
    pub body: MessageBody,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub html_body: String,
    pub text_body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMessageRef {
    pub conversation_id: ConversationId,
    pub message_id: MessageId,
}

impl From<&MessageDetail> for StoredMessageRef {
    fn from(detail: &MessageDetail) -> Self {
        Self {
            conversation_id: detail.conversation_id.clone(),
            message_id: detail.message_id.clone(),
        }
    }
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
            html_body: String::new(),
            text_body: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MailtoRequest, MessageBody, plain_text_to_html};

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
