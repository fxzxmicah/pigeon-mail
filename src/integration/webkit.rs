use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use serde::Deserialize;
use webkit::prelude::*;

const COMPOSER_CHANNEL: &str = "mailEditor";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ComposerContent {
    pub html: String,
    pub text: String,
}

#[derive(Debug, Deserialize)]
struct ComposerUpdate {
    generation: u64,
    #[serde(default)]
    request_id: Option<u64>,
    html: String,
    text: String,
}

type SnapshotHandler = Box<dyn FnOnce(Result<ComposerContent, String>)>;

pub struct WebKitComposer {
    view: webkit::WebView,
    content: Rc<RefCell<ComposerContent>>,
    generation: Rc<Cell<u64>>,
    next_request_id: Cell<u64>,
    changed_handlers: Rc<RefCell<Vec<Box<dyn Fn()>>>>,
    snapshot_handlers: Rc<RefCell<HashMap<u64, SnapshotHandler>>>,
}

impl WebKitComposer {
    pub fn new() -> Self {
        let view = webkit::WebView::new();
        let content = Rc::new(RefCell::new(ComposerContent::default()));
        let generation = Rc::new(Cell::new(0));
        let changed_handlers: Rc<RefCell<Vec<Box<dyn Fn()>>>> = Rc::new(RefCell::new(Vec::new()));
        let snapshot_handlers: Rc<RefCell<HashMap<u64, SnapshotHandler>>> =
            Rc::new(RefCell::new(HashMap::new()));

        let manager = view
            .user_content_manager()
            .expect("WebKit composer must have a user-content manager");
        let content_for_message = Rc::clone(&content);
        let generation_for_message = Rc::clone(&generation);
        let handlers_for_message = Rc::clone(&changed_handlers);
        let snapshots_for_message = Rc::clone(&snapshot_handlers);
        manager.connect_script_message_received(Some(COMPOSER_CHANNEL), move |_, value| {
            if !value.is_string() {
                tracing::debug!("ignored a non-text rich composer update");
                return;
            }
            let message = value.to_str();
            let Some(update) = parse_composer_update(&message) else {
                tracing::debug!("ignored a malformed rich composer update");
                return;
            };
            let request_id = update.request_id;
            let accepted_generation = update.generation == generation_for_message.get();
            let changed =
                apply_composer_update(&content_for_message, generation_for_message.get(), update);
            if changed {
                for handler in handlers_for_message.borrow().iter() {
                    handler();
                }
            }
            let snapshot_handler = if accepted_generation {
                request_id
                    .and_then(|request_id| snapshots_for_message.borrow_mut().remove(&request_id))
            } else {
                None
            };
            if let Some(handler) = snapshot_handler {
                handler(Ok(content_for_message.borrow().clone()));
            }
        });
        assert!(
            manager.register_script_message_handler(COMPOSER_CHANNEL, None),
            "rich composer message channel must be unique"
        );

        configure_mail_view(&view);
        view.set_editable(true);

        let composer = Self {
            view,
            content,
            generation,
            next_request_id: Cell::new(0),
            changed_handlers,
            snapshot_handlers,
        };
        composer.load_document();
        composer
    }

    pub fn build_view(&self) -> webkit::WebView {
        self.view.clone()
    }

    pub fn connect_changed<F: Fn() + 'static>(&self, handler: F) {
        self.changed_handlers.borrow_mut().push(Box::new(handler));
    }

    pub fn execute_command(&self, command: &str) {
        self.view.execute_editing_command(command);
        self.view.grab_focus();
    }

    pub fn capture_content<F: FnOnce(Result<ComposerContent, String>) + 'static>(
        &self,
        handler: F,
    ) {
        let request_id = self.next_request_id.get().wrapping_add(1);
        self.next_request_id.set(request_id);
        self.snapshot_handlers
            .borrow_mut()
            .insert(request_id, Box::new(handler));

        let snapshot_handlers = Rc::clone(&self.snapshot_handlers);
        self.view.evaluate_javascript(
            &format!("window.mailEditorPublish({request_id})"),
            None,
            None,
            None::<&gio::Cancellable>,
            move |result| {
                if let Err(error) = result {
                    let handler = snapshot_handlers.borrow_mut().remove(&request_id);
                    let Some(handler) = handler else {
                        return;
                    };
                    crate::logging::report_failure("compose-snapshot", &error);
                    handler(Err(
                        "The current message could not be read from the editor.".into(),
                    ));
                }
            },
        );
    }

    fn load_document(&self) {
        let content = self.content.borrow();
        self.view.stop_loading();
        self.view.load_html(
            &wrap_composer_document(&content.html, self.generation.get()),
            None,
        );
    }
}

fn parse_composer_update(message: &str) -> Option<ComposerUpdate> {
    serde_json::from_str(message).ok()
}

impl WebKitComposer {
    pub fn set_content(&self, html: &str, text: &str) {
        let superseded_handlers: Vec<_> = self.snapshot_handlers.borrow_mut().drain().collect();
        for (_, handler) in superseded_handlers {
            handler(Err(
                "The editor content changed before it could be captured.".into(),
            ));
        }
        *self.content.borrow_mut() = ComposerContent {
            html: html.to_string(),
            text: text.to_string(),
        };
        self.generation.set(self.generation.get().wrapping_add(1));
        self.load_document();
    }

    pub fn current_content(&self) -> ComposerContent {
        self.content.borrow().clone()
    }
}

fn apply_composer_update(
    content: &RefCell<ComposerContent>,
    current_generation: u64,
    update: ComposerUpdate,
) -> bool {
    if update.generation != current_generation {
        return false;
    }
    let next = ComposerContent {
        html: update.html,
        text: update.text.replace('\u{a0}', " "),
    };
    if *content.borrow() == next {
        return false;
    }
    *content.borrow_mut() = next;
    true
}

fn wrap_composer_document(body: &str, generation: u64) -> String {
    let body_json = serde_json::to_string(body)
        .expect("serializing a Rust string cannot fail")
        .replace('<', "\\u003c");
    format!(
        concat!(
            "<!doctype html><html><head>",
            "<meta charset=\"utf-8\">",
            "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">",
            "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; ",
            "img-src data: cid:; style-src 'unsafe-inline'; script-src 'nonce-mail-editor'\">",
            "<style>",
            "html, body {{ margin: 0; min-height: 100%; max-width: 100%; overflow-x: hidden; }}",
            "body {{ font: 11pt sans-serif; line-height: 1.5; padding: 18px; box-sizing: border-box; ",
            "overflow-wrap: anywhere; word-break: break-word; outline: none; }}",
            "* {{ max-width: 100%; box-sizing: border-box; }}",
            "img, video, iframe, table {{ max-width: 100% !important; height: auto !important; }}",
            "table {{ table-layout: fixed; width: 100% !important; }}",
            "pre, code {{ white-space: pre-wrap; overflow-wrap: anywhere; }}",
            "</style>",
            "<script nonce=\"mail-editor\">",
            "document.addEventListener('DOMContentLoaded', () => {{",
            "document.body.innerHTML = {body_json};",
            "const currentHtml = () => {{",
            "const hasText = document.body.innerText.trim().length > 0;",
            "const hasEmbeddedContent = document.body.querySelector('img,video,audio,table,hr') !== null;",
            "return hasText || hasEmbeddedContent ? document.body.innerHTML : '';",
            "}};",
            "const publish = (requestId = null) => window.webkit.messageHandlers.{channel}.postMessage(JSON.stringify({{",
            "generation: {generation}, request_id: requestId, html: currentHtml(), text: document.body.innerText",
            "}}));",
            "window.mailEditorPublish = publish;",
            "document.addEventListener('input', () => publish());",
            "}});",
            "</script></head><body spellcheck=\"true\"></body></html>"
        ),
        body_json = body_json,
        channel = COMPOSER_CHANNEL,
        generation = generation,
    )
}

fn wrap_html_document(body: &str) -> String {
    format!(
        concat!(
            "<html><head>",
            "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">",
            "<style>",
            "html, body {{ margin: 0; padding: 0; max-width: 100%; overflow-x: hidden; }}",
            "body {{ font: 11pt sans-serif; line-height: 1.5; padding: 18px; box-sizing: border-box; ",
            "overflow-wrap: anywhere; word-break: break-word; }}",
            "* {{ max-width: 100%; box-sizing: border-box; }}",
            "img, video, iframe, table {{ max-width: 100% !important; height: auto !important; }}",
            "table {{ table-layout: fixed; width: 100% !important; }}",
            "pre, code {{ white-space: pre-wrap; overflow-wrap: anywhere; }}",
            "</style></head><body>{}</body></html>"
        ),
        body
    )
}

pub fn configure_mail_view(view: &webkit::WebView) {
    view.set_vexpand(true);
    view.set_hexpand(true);
}

pub fn load_html_document(view: &webkit::WebView, body: &str) {
    view.stop_loading();
    view.load_html(&wrap_html_document(body), None);
}

pub fn stop_html_loading(view: &webkit::WebView) {
    view.stop_loading();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(generation: u64, html: &str, text: &str) -> ComposerUpdate {
        ComposerUpdate {
            generation,
            request_id: None,
            html: html.into(),
            text: text.into(),
        }
    }

    #[test]
    fn composer_update_accepts_current_generation_and_normalizes_non_breaking_spaces() {
        let content = RefCell::new(ComposerContent::default());

        assert!(apply_composer_update(
            &content,
            7,
            update(7, "<b>Hello</b>&nbsp;world", "Hello\u{a0}world"),
        ));
        assert_eq!(content.borrow().html, "<b>Hello</b>&nbsp;world");
        assert_eq!(content.borrow().text, "Hello world");
    }

    #[test]
    fn composer_update_parser_accepts_snapshot_ids_and_rejects_incomplete_or_wrong_types() {
        let parsed = parse_composer_update(
            r#"{"generation":12,"request_id":41,"html":"<p>Draft</p>","text":"Draft"}"#,
        )
        .expect("valid snapshot message");
        assert_eq!(parsed.request_id, Some(41));
        assert_eq!(parsed.generation, 12);

        assert!(parse_composer_update(r#"{"generation":12,"html":"<p>Draft</p>"}"#).is_none());
        assert!(
            parse_composer_update(r#"{"generation":12,"request_id":"wrong","html":"","text":""}"#)
                .is_none()
        );
        assert!(parse_composer_update("not json").is_none());
    }

    #[test]
    fn stale_and_duplicate_composer_updates_do_not_emit_changes() {
        let content = RefCell::new(ComposerContent {
            html: "<p>Current</p>".into(),
            text: "Current".into(),
        });

        assert!(!apply_composer_update(
            &content,
            4,
            update(3, "<p>Stale</p>", "Stale"),
        ));
        assert!(!apply_composer_update(
            &content,
            4,
            update(4, "<p>Current</p>", "Current"),
        ));
        assert_eq!(content.borrow().text, "Current");
    }

    #[test]
    fn composer_document_encodes_untrusted_closing_script_markup() {
        let document = wrap_composer_document(
            "</script><img src='https://example.invalid/tracker' onerror='alert(1)'>",
            9,
        );

        assert!(!document.contains("</script><img"));
        assert!(document.contains("\\u003c/script>"));
        assert!(document.contains("default-src 'none'"));
        assert!(document.contains("generation: 9"));
    }

    #[test]
    fn composer_document_preserves_empty_and_multiline_content_as_json() {
        let empty = wrap_composer_document("", 0);
        let multiline = wrap_composer_document("<p>One</p>\n<p>Two</p>", u64::MAX);

        assert!(empty.contains("document.body.innerHTML = \"\";"));
        assert!(empty.contains("html: currentHtml()"));
        assert!(empty.contains("querySelector('img,video,audio,table,hr')"));
        assert!(multiline.contains("\\u003cp>One\\u003c/p>\\n\\u003cp>Two\\u003c/p>"));
        assert!(multiline.contains(&format!("generation: {}", u64::MAX)));
    }
}
