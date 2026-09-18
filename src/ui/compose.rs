use adw::prelude::*;
use gtk::gio;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use crate::core::coordinator::MailCoordinator;
use crate::core::draft;
use crate::i18n::gettext;
use crate::integration::webkit::{ComposerContent, WebKitComposer};
use crate::model::account::{MailAccount, MailAccountId, SendingIdentity};
use crate::model::address::{normalized_mailbox_address, split_mailbox_list};
use crate::model::mail::{
    AttachmentInfo, AttachmentLocation, AttachmentOperation, AttachmentSource, DraftMessage,
    MailtoRequest, TextRange,
};
use crate::ui::attachment;

use super::editor::DualFormatEditor;
use super::mailbox::MailboxViewModel;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComposeKind {
    New,
    EditDraft,
    Reply,
    ReplyAll,
    Forward,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ComposeOperation {
    SaveDraft,
    Send,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ComposeActivity {
    Idle,
    ConvertingBody,
    PreparingWrite(ComposeOperation),
    Writing(ComposeOperation),
}

impl ComposeOperation {
    fn progress_status(self) -> String {
        match self {
            Self::SaveDraft => gettext("Saving…"),
            Self::Send => gettext("Sending…"),
        }
    }

    fn failure_status(self) -> String {
        match self {
            Self::SaveDraft => gettext("Not saved"),
            Self::Send => gettext("Not sent"),
        }
    }

    fn capture_failure(self) -> String {
        match self {
            Self::SaveDraft => gettext("Draft not saved."),
            Self::Send => gettext("Message not sent."),
        }
    }
}

#[derive(Clone)]
pub struct ComposeViewModel {
    pub kind: ComposeKind,
    pub draft: DraftMessage,
    pub available_identities: Vec<SendingIdentity>,
    pub dirty: bool,
}

impl ComposeViewModel {
    pub fn for_action(mailbox: &MailboxViewModel, kind: ComposeKind) -> Option<Self> {
        let account = mailbox.current_account()?;
        let message = mailbox.message_detail();
        let identity = if kind == ComposeKind::EditDraft {
            message
                .and_then(|message| identity_for_from(account.aliases(), &message.from))
                .unwrap_or_else(|| account.default_identity())
        } else {
            account.default_identity()
        };
        let draft = match kind {
            ComposeKind::New => draft::create_draft(account.id.clone(), identity),
            ComposeKind::EditDraft => draft::create_edit_draft(
                account.id.clone(),
                identity,
                message?,
            ),
            ComposeKind::Reply => draft::create_reply_draft(
                account.id.clone(),
                identity,
                message?,
            ),
            ComposeKind::ReplyAll => draft::create_reply_all_draft(
                account.id.clone(),
                identity,
                account.aliases(),
                message?,
            ),
            ComposeKind::Forward => draft::create_forward_draft(
                account.id.clone(),
                identity,
                message?,
            ),
        };

        Some(Self {
            kind,
            draft,
            available_identities: account.aliases().to_vec(),
            dirty: false,
        })
    }

    pub fn for_mailto(mailbox: &MailboxViewModel, request: &MailtoRequest) -> Option<Self> {
        let account = mailbox.current_account()?;
        let identity = account.default_identity();
        Some(Self {
            kind: ComposeKind::New,
            draft: draft::create_mailto_draft(account.id.clone(), identity, request),
            available_identities: account.aliases().to_vec(),
            dirty: !request.is_empty(),
        })
    }

    pub fn selected_identity_index(&self) -> u32 {
        let selected_address = normalized_mailbox_address(&self.draft.from);
        self.available_identities
            .iter()
            .position(|identity| normalized_mailbox_address(&identity.address)
                == selected_address)
            .expect("a compose draft sender belongs to its identity catalog") as u32
    }

    fn rebind_account(&mut self, account: &MailAccount) -> (SendingIdentity, bool) {
        let address = normalized_mailbox_address(&self.draft.from);
        let identity = identity_for_rebind(account, Some(&self.draft.account_id), Some(&address))
            .clone();
        let modified = rebind_draft_account(&mut self.draft, account.id.clone(), &identity);
        self.available_identities = account.aliases().to_vec();
        (identity, modified)
    }

    pub fn title(&self) -> String {
        match self.kind {
            ComposeKind::New => gettext("New Message"),
            ComposeKind::EditDraft => gettext("Edit Draft"),
            ComposeKind::Reply => gettext("Reply"),
            ComposeKind::ReplyAll => gettext("Reply All"),
            ComposeKind::Forward => gettext("Forward"),
        }
    }

    pub fn initial_status(&self) -> String {
        match self.kind {
            ComposeKind::New => gettext("Draft ready"),
            ComposeKind::EditDraft => gettext("Draft loaded"),
            ComposeKind::Reply => gettext("Reply draft ready"),
            ComposeKind::ReplyAll => gettext("Reply-all draft ready"),
            ComposeKind::Forward => gettext("Forward draft ready"),
        }
    }

    pub fn saved_status(&self) -> String {
        match self.kind {
            ComposeKind::New | ComposeKind::EditDraft => gettext("Draft saved"),
            ComposeKind::Reply => gettext("Reply draft saved"),
            ComposeKind::ReplyAll => gettext("Reply-all draft saved"),
            ComposeKind::Forward => gettext("Forward draft saved"),
        }
    }
}

enum NavigationRequest {
    Mailbox(Option<Box<dyn FnOnce()>>),
    Compose(ComposeViewModel),
    Close {
        proceed: Box<dyn FnOnce()>,
        cancel: Box<dyn FnOnce()>,
    },
}

#[derive(Clone)]
pub(super) struct ComposePage {
    inner: Rc<ComposePageInner>,
}

struct ComposePageInner {
    parent: adw::ApplicationWindow,
    page_stack: gtk::Stack,
    root: gtk::ScrolledWindow,
    title: adw::WindowTitle,
    back_button: gtk::Button,
    save_button: gtk::Button,
    send_button: gtk::Button,
    toast_overlay: adw::ToastOverlay,
    mailbox: Rc<RefCell<MailboxViewModel>>,
    coordinator: MailCoordinator,
    composer: Rc<WebKitComposer>,
    body_stack: adw::ViewStack,
    text_editor: gtk::TextView,
    signature_marks: RefCell<Option<(gtk::TextMark, gtk::TextMark)>>,
    convert_body_button: gtk::Button,
    model: RefCell<Option<ComposeViewModel>>,
    activity: Cell<ComposeActivity>,
    navigation_queue: RefCell<VecDeque<NavigationRequest>>,
    navigation_dialog_open: Cell<bool>,
    account_rebind_pending: Cell<bool>,
    updating_widgets: Cell<bool>,
    identity_dropdown: gtk::DropDown,
    to_entry: gtk::Entry,
    cc_entry: gtk::Entry,
    bcc_entry: gtk::Entry,
    subject_entry: gtk::Entry,
    optional_recipients_button: gtk::ToggleButton,
    attachment_list: gtk::ListBox,
}

impl ComposePage {
    pub fn new(
        parent: &adw::ApplicationWindow,
        page_stack: &gtk::Stack,
        mailbox: Rc<RefCell<MailboxViewModel>>,
        coordinator: MailCoordinator,
        toast_overlay: &adw::ToastOverlay,
    ) -> Self {
        let DualFormatEditor {
            composer,
            text_editor,
            stack: body_stack,
            convert_button: convert_body_button,
            mode_controls: body_mode_controls,
            frame: body_frame,
        } = DualFormatEditor::new(&gettext("Message"));
        body_frame.set_height_request(360);
        let body_section = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .vexpand(true)
            .build();
        body_section.append(&body_mode_controls);
        body_section.append(&body_frame);
        let title = adw::WindowTitle::builder()
            .title(gettext("New Message"))
            .subtitle(gettext("Draft ready"))
            .build();
        let back_button = gtk::Button::builder()
            .icon_name("go-previous-symbolic")
            .tooltip_text(gettext("Back to mailbox"))
            .build();
        let save_button = gtk::Button::builder().label(gettext("Save Draft")).build();
        let send_button = gtk::Button::builder()
            .label(gettext("Send"))
            .css_classes(["suggested-action"])
            .build();
        let identity_dropdown =
            gtk::DropDown::new(None::<gtk::StringList>, None::<gtk::Expression>);
        let to_entry = entry(&gettext("Recipients"));
        let cc_entry = entry(&gettext("Cc recipients"));
        let bcc_entry = entry(&gettext("Bcc recipients"));
        let subject_entry = entry(&gettext("Subject"));
        let optional_recipients_button = gtk::ToggleButton::builder()
            .label(gettext("Cc/Bcc"))
            .valign(gtk::Align::Center)
            .build();

        let form_grid = gtk::Grid::builder()
            .hexpand(true)
            .row_spacing(8)
            .column_spacing(12)
            .build();
        form_grid.attach(&form_label(&gettext("_From"), &identity_dropdown), 0, 0, 1, 1);
        form_grid.attach(&identity_dropdown, 1, 0, 1, 1);
        form_grid.attach(&form_label(&gettext("_To"), &to_entry), 0, 1, 1, 1);
        form_grid.attach(&to_entry, 1, 1, 1, 1);
        form_grid.attach(&optional_recipients_button, 2, 1, 1, 1);

        let optional_fields = gtk::Grid::builder()
            .hexpand(true)
            .row_spacing(8)
            .column_spacing(12)
            .build();
        optional_fields.attach(&form_label(&gettext("_Cc"), &cc_entry), 0, 0, 1, 1);
        optional_fields.attach(&cc_entry, 1, 0, 1, 1);
        optional_fields.attach(&form_label(&gettext("_Bcc"), &bcc_entry), 0, 1, 1, 1);
        optional_fields.attach(&bcc_entry, 1, 1, 1, 1);
        let optional_recipients = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .child(&optional_fields)
            .build();
        let revealer = optional_recipients.clone();
        optional_recipients_button.connect_toggled(move |button| {
            revealer.set_reveal_child(button.is_active());
        });

        let subject_row = gtk::Box::builder().spacing(12).build();
        subject_row.append(&form_label(&gettext("_Subject"), &subject_entry));
        subject_row.append(&subject_entry);
        let attachment_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .show_separators(true)
            .build();
        let attachment_viewport = attachment::viewport(&attachment_list);
        let attachment_frame = gtk::Frame::builder()
            .label(gettext("Attachments"))
            .css_classes(["compact-list-frame"])
            .child(&attachment_viewport)
            .build();

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        content.append(&form_grid);
        content.append(&optional_recipients);
        content.append(&subject_row);
        content.append(&body_section);
        content.append(&attachment_frame);
        let root = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&content)
            .build();

        let page = Self {
            inner: Rc::new(ComposePageInner {
                parent: parent.clone(),
                page_stack: page_stack.clone(),
                root,
                title,
                back_button,
                save_button,
                send_button,
                toast_overlay: toast_overlay.clone(),
                mailbox,
                coordinator,
                composer,
                body_stack,
                text_editor,
                signature_marks: RefCell::new(None),
                convert_body_button,
                model: RefCell::new(None),
                activity: Cell::new(ComposeActivity::Idle),
                navigation_queue: RefCell::new(VecDeque::new()),
                navigation_dialog_open: Cell::new(false),
                account_rebind_pending: Cell::new(false),
                updating_widgets: Cell::new(false),
                identity_dropdown,
                to_entry,
                cc_entry,
                bcc_entry,
                subject_entry,
                optional_recipients_button,
                attachment_list,
            }),
        };
        page.connect_signals();
        page
    }

    pub fn root(&self) -> &gtk::ScrolledWindow {
        &self.inner.root
    }

    pub fn title(&self) -> &adw::WindowTitle {
        &self.inner.title
    }
    pub fn back_button(&self) -> &gtk::Button {
        &self.inner.back_button
    }

    pub fn save_button(&self) -> &gtk::Button {
        &self.inner.save_button
    }

    pub fn send_button(&self) -> &gtk::Button {
        &self.inner.send_button
    }

    pub fn is_visible(&self) -> bool {
        self.inner.page_stack.visible_child_name().as_deref() == Some("compose")
    }

    pub fn has_unaccepted_write(&self) -> bool {
        matches!(self.inner.activity.get(), ComposeActivity::PreparingWrite(_))
    }

    pub fn request_open(&self, model: ComposeViewModel) {
        self.request_navigation(NavigationRequest::Compose(model));
    }

    pub fn request_back(&self) {
        if self.is_visible() && self.inner.back_button.is_sensitive() {
            self.request_navigation(NavigationRequest::Mailbox(None));
        }
    }

    pub fn request_mailbox(&self, completed: impl FnOnce() + 'static) {
        self.request_navigation(NavigationRequest::Mailbox(Some(Box::new(completed))));
    }

    pub fn request_close(
        &self,
        proceed: impl FnOnce() + 'static,
        cancel: impl FnOnce() + 'static,
    ) {
        self.request_navigation(NavigationRequest::Close {
            proceed: Box::new(proceed),
            cancel: Box::new(cancel),
        });
    }

    pub fn account_activation_started(&self) {
        if self.is_visible() {
            self.sync_editor_content();
            self.inner.account_rebind_pending.set(true);
            self.set_busy(true);
            if !matches!(
                self.inner.activity.get(),
                ComposeActivity::PreparingWrite(_) | ComposeActivity::Writing(_)
            ) {
                self.inner.title.set_subtitle(&gettext("Switching account…"));
            }
        }
    }

    pub fn mailbox_changed(&self) {
        if !self.is_visible() {
            return;
        }
        if self.inner.activity.get() != ComposeActivity::Idle {
            self.inner.account_rebind_pending.set(true);
            return;
        }
        self.rebind_to_current_account();
        self.request_next_navigation();
    }

    fn connect_signals(&self) {
        let page = self.clone();
        self.inner
            .back_button
            .connect_clicked(move |_| page.request_navigation(NavigationRequest::Mailbox(None)));
        let page = self.clone();
        self.inner
            .save_button
            .connect_clicked(move |_| page.start_save());
        let page = self.clone();
        self.inner
            .send_button
            .connect_clicked(move |_| page.start_send());
        for entry in [
            self.inner.to_entry.clone(),
            self.inner.cc_entry.clone(),
            self.inner.bcc_entry.clone(),
            self.inner.subject_entry.clone(),
        ] {
            let page = self.clone();
            entry.connect_changed(move |_| page.entries_changed());
        }
        let page = self.clone();
        self.inner
            .composer
            .connect_changed(move || page.html_body_changed());
        let page = self.clone();
        self.inner
            .text_editor
            .buffer()
            .connect_changed(move |_| page.text_body_changed());
        let page = self.clone();
        self.inner
            .convert_body_button
            .connect_clicked(move |_| page.convert_body());
        let page = self.clone();
        self.inner
            .identity_dropdown
            .connect_selected_notify(move |dropdown| page.identity_changed(dropdown.selected()));
    }

    fn show_model(&self, model: ComposeViewModel) {
        self.inner.updating_widgets.set(true);
        self.inner.account_rebind_pending.set(false);
        self.inner.title.set_title(&model.title());
        self.inner.title.set_subtitle(&if model.dirty {
            gettext("Modified")
        } else {
            model.initial_status()
        });
        self.inner.to_entry.set_text(&model.draft.to.join(", "));
        self.inner.cc_entry.set_text(&model.draft.cc.join(", "));
        self.inner.bcc_entry.set_text(&model.draft.bcc.join(", "));
        self.inner.subject_entry.set_text(&model.draft.subject);
        self.inner
            .optional_recipients_button
            .set_active(has_optional_recipients(&model.draft));
        self.inner
            .composer
            .set_content(model.draft.body.html(), model.draft.body.text());
        self.set_text_body(model.draft.body.text(), model.draft.body.text_signature());
        self.inner.body_stack.set_visible_child_name(
            if model.draft.body.html().trim().is_empty()
                && !model.draft.body.text().trim().is_empty()
            {
                "text"
            } else {
                "html"
            },
        );
        *self.inner.model.borrow_mut() = Some(model);
        self.set_identity_model();
        self.refresh_attachments(None);
        self.set_busy(false);
        self.inner.updating_widgets.set(false);
        self.inner.page_stack.set_visible_child_name("compose");
        self.inner.to_entry.grab_focus();
    }

    fn request_navigation(&self, navigation: NavigationRequest) {
        if self.navigation_blocked() {
            self.inner
                .navigation_queue
                .borrow_mut()
                .push_back(navigation);
            return;
        }
        let dirty = self.inner.model.borrow().as_ref().is_some_and(|model| model.dirty);
        if !self.is_visible() || !dirty {
            self.perform_navigation(navigation);
            return;
        }
        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Unsaved draft"))
            .body(gettext("The current message has unsaved changes."))
            .build();
        dialog.add_response("continue", &gettext("Continue Editing"));
        dialog.add_response("discard", &gettext("Discard"));
        dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("continue"));
        dialog.set_close_response("continue");
        self.inner.navigation_dialog_open.set(true);
        let page = self.clone();
        dialog.choose(
            Some(&self.inner.parent),
            None::<&gio::Cancellable>,
            move |response| {
                page.inner.navigation_dialog_open.set(false);
                match response.as_str() {
                    "discard" => page.perform_navigation(navigation),
                    _ => page.cancel_navigation(navigation),
                }
            },
        );
    }

    fn request_next_navigation(&self) {
        if self.navigation_blocked() {
            return;
        }
        let next = self.inner.navigation_queue.borrow_mut().pop_front();
        if let Some(next) = next {
            self.request_navigation(next);
        }
    }

    fn navigation_blocked(&self) -> bool {
        self.inner.navigation_dialog_open.get()
            || self.inner.activity.get() != ComposeActivity::Idle
            || self.inner.account_rebind_pending.get()
    }

    fn perform_navigation(&self, navigation: NavigationRequest) {
        match navigation {
            NavigationRequest::Mailbox(completed) => {
                if let Some(model) = self.inner.model.borrow_mut().as_mut() {
                    model.dirty = false;
                }
                self.inner.account_rebind_pending.set(false);
                self.inner.page_stack.set_visible_child_name("mailbox");
                if let Some(completed) = completed {
                    completed();
                }
            }
            NavigationRequest::Compose(model) => self.show_model(model),
            NavigationRequest::Close { proceed, .. } => {
                proceed();
                return;
            }
        }
        self.request_next_navigation();
    }

    fn cancel_navigation(&self, navigation: NavigationRequest) {
        if let NavigationRequest::Close { cancel, .. } = navigation {
            cancel();
        }
        self.request_next_navigation();
    }

    fn mark_modified(&self) {
        if self.inner.updating_widgets.get() {
            return;
        }
        {
            let mut current = self.inner.model.borrow_mut();
            let Some(model) = current.as_mut() else {
                return;
            };
            model.dirty = true;
        }
        self.inner.title.set_subtitle(&gettext("Modified"));
    }

    fn entries_changed(&self) {
        if self.inner.updating_widgets.get() {
            return;
        }
        let to = recipients(&self.inner.to_entry);
        let cc = recipients(&self.inner.cc_entry);
        let bcc = recipients(&self.inner.bcc_entry);
        let subject = self.inner.subject_entry.text().to_string();
        {
            let mut current = self.inner.model.borrow_mut();
            let Some(model) = current.as_mut() else {
                return;
            };
            let draft = &mut model.draft;
            draft.to = to;
            draft.cc = cc;
            draft.bcc = bcc;
            draft.subject = subject;
        }
        self.mark_modified();
    }

    fn html_body_changed(&self) {
        if self.inner.updating_widgets.get() {
            return;
        }
        let content = self.inner.composer.current_content();
        let recovered_signature = (self.signature_text_range().is_none()
            && self.text_body() == content.text)
            .then(|| content_signature_range(&content))
            .flatten();
        if let Some(signature) = recovered_signature {
            self.set_signature_marks(Some(signature));
        }
        let changed = {
            let mut current = self.inner.model.borrow_mut();
            let Some(model) = current.as_mut() else {
                return;
            };
            let draft = &mut model.draft;
            let changed = draft.body.html() != content.html;
            draft.body.set_html(content.html);
            if let Some(signature) = recovered_signature {
                draft.body.set_text(content.text, Some(signature));
            }
            changed
        };
        if changed {
            self.mark_modified();
        }
    }

    fn text_body_changed(&self) {
        if self.inner.updating_widgets.get() {
            return;
        }
        let text = self.text_body();
        let signature = self.signature_text_range();
        {
            let mut current = self.inner.model.borrow_mut();
            let Some(model) = current.as_mut() else {
                return;
            };
            model.draft.body.set_text(text, signature);
        }
        self.mark_modified();
    }

    fn text_body(&self) -> String {
        let buffer = self.inner.text_editor.buffer();
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), true)
            .to_string()
    }

    fn set_text_body(&self, text: &str, signature_range: Option<TextRange>) {
        let buffer = self.inner.text_editor.buffer();
        let old_marks = self.inner.signature_marks.borrow_mut().take();
        if let Some((start, end)) = old_marks {
            buffer.delete_mark(&start);
            buffer.delete_mark(&end);
        }
        buffer.set_text(text);
        self.set_signature_marks(signature_range);
    }

    fn set_signature_marks(&self, signature_range: Option<TextRange>) {
        let buffer = self.inner.text_editor.buffer();
        let marks = signature_range.and_then(|range| {
            let text = buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), true)
                .to_string();
            if !range.is_valid_for(&text) {
                return None;
            }
            let mut start = buffer.start_iter();
            start.forward_chars(range.start as i32);
            let mut end = buffer.start_iter();
            end.forward_chars(range.end as i32);
            let empty = range.start == range.end;
            Some((
                buffer.create_mark(None, &start, !empty),
                buffer.create_mark(None, &end, false),
            ))
        });
        *self.inner.signature_marks.borrow_mut() = marks;
    }

    fn signature_text_range(&self) -> Option<TextRange> {
        let marks = self.inner.signature_marks.borrow();
        let (start, end) = marks.as_ref()?;
        let buffer = self.inner.text_editor.buffer();
        Some(TextRange {
            start: buffer.iter_at_mark(start).offset() as usize,
            end: buffer.iter_at_mark(end).offset() as usize,
        })
    }

    fn replace_text_signature(&self, signature: &str) {
        let Some(range) = self.signature_text_range() else {
            return;
        };
        let buffer = self.inner.text_editor.buffer();
        let marks = self.inner.signature_marks.borrow_mut().take();
        let Some((start_mark, end_mark)) = marks else {
            return;
        };
        let mut start = buffer.iter_at_mark(&start_mark);
        let mut end = buffer.iter_at_mark(&end_mark);
        buffer.delete_mark(&start_mark);
        buffer.delete_mark(&end_mark);
        buffer.delete(&mut start, &mut end);
        let replacement = if signature.is_empty() {
            String::new()
        } else {
            format!("\n\n{signature}")
        };
        if !replacement.is_empty() {
            buffer.insert(&mut start, &replacement);
        }
        self.set_signature_marks(Some(TextRange {
            start: range.start,
            end: range.start + replacement.chars().count(),
        }));
    }

    fn convert_body(&self) {
        if self.inner.activity.get() != ComposeActivity::Idle {
            return;
        }
        let html_active = self.inner.body_stack.visible_child_name().as_deref() == Some("html");
        self.inner.activity.set(ComposeActivity::ConvertingBody);
        self.set_busy(true);
        let page = self.clone();
        self.inner.composer.capture_content(move |snapshot| {
            page.inner.activity.set(ComposeActivity::Idle);
            let Ok(snapshot) = snapshot else {
                page.inner
                    .toast_overlay
                    .add_toast(adw::Toast::new(&gettext("Message not converted")));
                page.finish_account_rebind_or_enable();
                page.request_next_navigation();
                return;
            };
            if html_active {
                let (text, signature_range) = page.text_with_preserved_signature(&snapshot);
                let changed = page.text_body() != text;
                page.inner.updating_widgets.set(true);
                page.set_text_body(&text, signature_range);
                {
                    let mut current = page.inner.model.borrow_mut();
                    if let Some(model) = current.as_mut() {
                        model.draft.body.replace(snapshot.html, text, signature_range);
                    }
                }
                page.inner.body_stack.set_visible_child_name("text");
                page.inner.updating_widgets.set(false);
                if changed {
                    page.mark_modified();
                }
            } else {
                let text = page.text_body();
                let html = page.html_from_text_editor(&snapshot);
                let changed = snapshot.html != html;
                let signature_range = page.signature_text_range();
                page.inner.updating_widgets.set(true);
                page.inner.composer.set_content(&html, &text);
                {
                    let mut current = page.inner.model.borrow_mut();
                    if let Some(model) = current.as_mut() {
                        model.draft.body.replace(html, text, signature_range);
                    }
                }
                page.inner.body_stack.set_visible_child_name("html");
                page.inner.updating_widgets.set(false);
                if changed {
                    page.mark_modified();
                }
            }
            page.finish_account_rebind_or_enable();
            page.request_next_navigation();
        });
    }

    fn identity_changed(&self, selected: u32) {
        if self.inner.updating_widgets.get() {
            return;
        }
        let Some(identity) = self
            .inner
            .model
            .borrow()
            .as_ref()
            .and_then(|model| model.available_identities.get(selected as usize))
            .cloned()
        else {
            return;
        };
        self.sync_editor_content();
        self.inner.composer.replace_signature(&identity.signature.html);
        self.inner.updating_widgets.set(true);
        self.replace_text_signature(&identity.signature.text);
        self.inner.updating_widgets.set(false);
        let text = self.text_body();
        let signature = self.signature_text_range();
        {
            let mut current = self.inner.model.borrow_mut();
            let Some(model) = current.as_mut() else {
                return;
            };
            apply_identity_headers(&mut model.draft, &identity);
            model.draft.body.set_text(text, signature);
        }
        self.mark_modified();
    }

    fn sync_editor_content(&self) {
        let html = self.inner.composer.current_content().html;
        let text = self.text_body();
        let signature = self.signature_text_range();
        let mut current = self.inner.model.borrow_mut();
        let Some(model) = current.as_mut() else {
            return;
        };
        model.draft.body.replace(html, text, signature);
    }

    fn html_from_text_editor(&self, html_content: &ComposerContent) -> String {
        let text = self.text_body();
        html_from_text_preserving_signature(
            &text,
            self.signature_text_range(),
            html_content
                .signature
                .as_ref()
                .map(|signature| signature.html.as_str()),
        )
    }

    fn text_with_preserved_signature(&self, content: &ComposerContent) -> (String, Option<TextRange>) {
        text_from_html_preserving_signature(
            content,
            &self.text_body(),
            self.signature_text_range(),
        )
    }

    fn rebind_to_current_account(&self) {
        self.sync_editor_content();
        let account = self.inner.mailbox.borrow().current_account().cloned();
        let Some(account) = account else {
            self.inner.title.set_subtitle(&gettext("Identity unavailable"));
            self.inner.account_rebind_pending.set(false);
            self.set_busy(false);
            return;
        };
        let (identity, mut modified) = {
            let mut current = self.inner.model.borrow_mut();
            let Some(model) = current.as_mut() else {
                return;
            };
            model.rebind_account(&account)
        };
        self.inner.updating_widgets.set(true);
        self.set_identity_model();
        self.inner.composer.replace_signature(&identity.signature.html);
        self.replace_text_signature(&identity.signature.text);
        let text = self.text_body();
        let signature = self.signature_text_range();
        {
            let mut current = self.inner.model.borrow_mut();
            let draft = &mut current
                .as_mut()
                .expect("an account rebind retains its compose model")
                .draft;
            modified |= draft.body.text() != text;
            draft.body.set_text(text, signature);
        }
        self.inner.account_rebind_pending.set(false);
        self.inner.updating_widgets.set(false);
        self.set_busy(false);
        if modified {
            self.mark_modified();
        }
    }

    fn set_identity_model(&self) {
        let (selected, labels) = {
            let current = self.inner.model.borrow();
            let model = current
                .as_ref()
                .expect("identity controls require a compose model");
            let labels = model
                .available_identities
                .iter()
                .map(|identity| {
                    format!("{} <{}>", identity.display_name_or_address(), identity.address)
                })
                .collect::<Vec<_>>();
            (model.selected_identity_index(), labels)
        };
        let refs = labels.iter().map(String::as_str).collect::<Vec<_>>();
        self.inner
            .identity_dropdown
            .set_model(Some(&gtk::StringList::new(&refs)));
        self.inner.identity_dropdown.set_selected(selected);
    }

    fn begin_operation(&self, operation: ComposeOperation) {
        assert_eq!(
            self.inner.activity.get(),
            ComposeActivity::Idle,
            "compose operation already active"
        );
        self.inner
            .activity
            .set(ComposeActivity::PreparingWrite(operation));
        self.set_busy(true);
        self.inner.title.set_subtitle(&operation.progress_status());
    }

    fn start_save(&self) {
        self.start_operation(ComposeOperation::SaveDraft);
    }

    fn start_send(&self) {
        self.start_operation(ComposeOperation::Send);
    }

    fn start_operation(&self, operation: ComposeOperation) {
        self.begin_operation(operation);
        let text = self.text_body();
        let signature_range = self.signature_text_range();
        let page = self.clone();
        self.inner
            .composer
            .capture_content(move |snapshot| match snapshot {
                Ok(snapshot) => {
                    page.dispatch_operation(operation, snapshot.html, text, signature_range)
                }
                Err(()) => page.finish_operation_failure(
                    operation.failure_status(),
                    operation.capture_failure(),
                ),
            });
    }

    fn dispatch_operation(
        &self,
        operation: ComposeOperation,
        html_body: String,
        text_body: String,
        signature_text_range: Option<TextRange>,
    ) {
        assert_eq!(
            self.inner.activity.get(),
            ComposeActivity::PreparingWrite(operation),
            "only a prepared compose operation can be dispatched"
        );
        let mut draft = self
            .inner
            .model
            .borrow()
            .as_ref()
            .expect("an active compose operation must own a draft")
            .draft
            .clone();
        draft
            .body
            .replace(html_body, text_body, signature_text_range);
        self.inner.activity.set(ComposeActivity::Writing(operation));
        match operation {
            ComposeOperation::SaveDraft => self.inner.coordinator.request_save_draft(draft),
            ComposeOperation::Send => self.inner.coordinator.request_send_draft(draft),
        }
    }

    pub fn finish_save(
        &self,
        result: Result<Option<crate::model::mail::StoredMessageRef>, String>,
    ) {
        assert_eq!(
            self.inner.activity.get(),
            ComposeActivity::Writing(ComposeOperation::SaveDraft),
            "draft-save completion does not match the active operation"
        );
        match result {
            Ok(stored) => {
                self.inner.activity.set(ComposeActivity::Idle);
                let saved_status = {
                    let mut current = self.inner.model.borrow_mut();
                    let model = current
                        .as_mut()
                        .expect("a completed compose operation must still own its draft");
                    let draft = &mut model.draft;
                    if let Some(stored) = stored {
                        if draft.has_cached_attachments() {
                            draft.set_attachment_source(Some(AttachmentSource {
                                account_id: draft.account_id.clone(),
                                conversation_id: stored.conversation_id.clone(),
                            }));
                        } else {
                            draft.set_attachment_source(None);
                        }
                        draft.conversation_id = Some(stored.conversation_id);
                        draft.message_id = Some(stored.message_id);
                    }
                    model.dirty = false;
                    model.saved_status()
                };
                self.inner.title.set_subtitle(&saved_status);
                self.finish_account_rebind_or_enable();
                self.request_next_navigation();
            }
            Err(error) => self.finish_operation_failure(
                ComposeOperation::SaveDraft.failure_status(),
                error,
            ),
        }
    }

    pub fn finish_send(&self, result: Result<(), String>) {
        assert_eq!(
            self.inner.activity.get(),
            ComposeActivity::Writing(ComposeOperation::Send),
            "send completion does not match the active operation"
        );
        match result {
            Ok(_) => {
                self.inner.activity.set(ComposeActivity::Idle);
                self.inner.model.borrow_mut().as_mut()
                    .expect("a completed send retains its compose model")
                    .dirty = false;
                self.inner.title.set_subtitle(&gettext("Queued"));
                {
                    let mut queue = self.inner.navigation_queue.borrow_mut();
                    if queue.is_empty() {
                        queue.push_back(NavigationRequest::Mailbox(None));
                    }
                }
                self.finish_account_rebind_or_enable();
                self.request_next_navigation();
            }
            Err(error) => {
                self.finish_operation_failure(ComposeOperation::Send.failure_status(), error)
            }
        }
    }

    fn finish_operation_failure(&self, status: String, error: String) {
        self.inner.activity.set(ComposeActivity::Idle);
        self.inner.title.set_subtitle(&status);
        self.inner.toast_overlay.add_toast(adw::Toast::new(&error));
        self.resume_navigation_after_failure();
    }

    fn resume_navigation_after_failure(&self) {
        self.finish_account_rebind_or_enable();
        self.request_next_navigation();
    }

    fn finish_account_rebind_or_enable(&self) {
        if self.inner.account_rebind_pending.get() {
            let activation_finished = !self.inner.mailbox.borrow().is_loading();
            if activation_finished {
                self.rebind_to_current_account();
            }
        } else {
            self.set_busy(false);
        }
    }

    fn set_busy(&self, busy: bool) {
        self.inner.root.set_sensitive(!busy);
        self.inner.back_button.set_sensitive(!busy);
        let write_available = !busy && self.write_available();
        self.inner.save_button.set_sensitive(write_available);
        self.inner.send_button.set_sensitive(write_available);
    }

    fn write_available(&self) -> bool {
        self.inner.model.borrow().is_some()
    }

    fn add_attachments(&self) {
        let dialog = gtk::FileDialog::builder()
            .title(gettext("Add attachments"))
            .accept_label(gettext("Attach"))
            .build();
        let page = self.clone();
        dialog.open_multiple(
            Some(&self.inner.parent),
            None::<&gio::Cancellable>,
            move |result| {
                let Ok(files) = result else {
                    return;
                };
                let attachments = (0..files.n_items())
                    .filter_map(|index| files.item(index))
                    .filter_map(|item| item.downcast::<gio::File>().ok())
                    .map(|file| AttachmentInfo {
                        display_name: file
                            .basename()
                            .map(|name| name.to_string_lossy().into_owned())
                            .filter(|name| !name.is_empty())
                            .unwrap_or_else(|| gettext("Attachment")),
                        location: AttachmentLocation::ExternalUri(file.uri().to_string()),
                    })
                    .collect::<Vec<_>>();
                let added_index = {
                    let mut current = page.inner.model.borrow_mut();
                    let Some(model) = current.as_mut() else {
                        return;
                    };
                    let draft = &mut model.draft;
                    let previous_len = draft.attachments.len();
                    for attachment in attachments {
                        if draft.attachments.iter().any(|item| {
                            item.location == attachment.location
                        }) {
                            continue;
                        }
                        draft.attachments.push(attachment);
                    }
                    (draft.attachments.len() > previous_len)
                        .then_some(draft.attachments.len() - 1)
                };
                if let Some(added_index) = added_index {
                    page.refresh_attachments(Some(added_index));
                    page.mark_modified();
                }
            },
        );
    }

    fn open_attachment(&self, index: usize) {
        let (attachment, source) = {
            let current = self.inner.model.borrow();
            let Some(model) = current.as_ref() else {
                return;
            };
            let draft = &model.draft;
            let Some(attachment) = draft.attachments.get(index).cloned() else {
                return;
            };
            (attachment, draft.attachment_source().cloned())
        };
        self.inner.coordinator.request_attachment(
            source,
            AttachmentOperation::Open,
            attachment,
        );
    }

    fn remove_attachment(&self, index: usize) {
        let removed = self.inner.model.borrow_mut().as_mut().is_some_and(|model| {
            let draft = &mut model.draft;
            if index < draft.attachments.len() {
                draft.attachments.remove(index);
                if !draft.has_cached_attachments() {
                    draft.set_attachment_source(None);
                }
                true
            } else {
                false
            }
        });
        if removed {
            self.refresh_attachments(Some(index.saturating_sub(1)));
            self.mark_modified();
        }
    }

    fn refresh_attachments(&self, focus_index: Option<usize>) {
        let attachments = self
            .inner
            .model
            .borrow()
            .as_ref()
            .map(|model| model.draft.attachments.clone())
            .unwrap_or_default();
        let focus_target = rebuild_attachment_list(
            &self.inner.attachment_list,
            &attachments,
            Rc::downgrade(&self.inner),
            focus_index,
        );
        if let Some(target) = focus_target {
            target.grab_focus();
        }
    }
}

fn entry(placeholder: &str) -> gtk::Entry {
    gtk::Entry::builder()
        .placeholder_text(placeholder)
        .hexpand(true)
        .build()
}

fn content_signature_range(content: &ComposerContent) -> Option<TextRange> {
    let signature = content.signature.as_ref()?;
    Some(TextRange::for_segment(
        &signature.prefix_text,
        &signature.text,
    ))
}

fn html_from_text_preserving_signature(
    text: &str,
    text_signature_range: Option<TextRange>,
    html_signature: Option<&str>,
) -> String {
    let Some(range) = text_signature_range else {
        return crate::model::mail::plain_text_to_html(text);
    };
    let Some((before, _, after)) = range.split(text) else {
        return crate::model::mail::plain_text_to_html(text);
    };
    let before = if before.is_empty() && html_signature.is_some() {
        "<div><br></div>".to_string()
    } else {
        crate::model::mail::plain_text_to_html(before)
    };
    let after = crate::model::mail::plain_text_to_html(after);
    match html_signature {
        Some(signature) => format!(
            "{before}<div {}>{signature}</div>{after}",
            crate::model::mail::SIGNATURE_REGION_ATTRIBUTE,
        ),
        None => format!("{before}{after}"),
    }
}

fn text_from_html_preserving_signature(
    html_content: &ComposerContent,
    text: &str,
    text_signature_range: Option<TextRange>,
) -> (String, Option<TextRange>) {
    let Some(source_range) = content_signature_range(html_content) else {
        return (html_content.text.clone(), None);
    };
    let Some((before, _, after)) = source_range.split(&html_content.text) else {
        return (html_content.text.clone(), None);
    };
    if let Some((_, signature, _)) = text_signature_range.and_then(|range| range.split(text)) {
        let range = TextRange::for_segment(before, signature);
        return (format!("{before}{signature}{after}"), Some(range));
    }
    if text == html_content.text {
        return (text.to_string(), Some(source_range));
    }
    (format!("{before}{after}"), None)
}

fn form_label(text: &str, mnemonic_widget: &impl IsA<gtk::Widget>) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(text)
        .use_underline(true)
        .xalign(0.0)
        .css_classes(["mail-form-label"])
        .build();
    label.set_mnemonic_widget(Some(mnemonic_widget));
    label
}

fn recipients(entry: &gtk::Entry) -> Vec<String> {
    split_mailbox_list(entry.text().as_str())
}

fn apply_identity_headers(draft: &mut DraftMessage, identity: &SendingIdentity) {
    draft.from = identity.mailbox();
    draft.reply_to = identity.reply_to.clone();
}

fn identity_for_from<'a>(
    identities: &'a [SendingIdentity],
    from: &str,
) -> Option<&'a SendingIdentity> {
    if let Some(identity) = identities
        .iter()
        .find(|identity| identity.mailbox().eq_ignore_ascii_case(from.trim()))
    {
        return Some(identity);
    }

    let address = normalized_mailbox_address(from);
    let display_name = mailbox_display_name(from);
    if !display_name.is_empty()
        && let Some(identity) = identities.iter().find(|identity| {
            normalized_mailbox_address(&identity.address) == address
                && identity.display_name.trim() == display_name
        })
    {
        return Some(identity);
    }

    let mut matching_address = identities
        .iter()
        .filter(|identity| normalized_mailbox_address(&identity.address) == address);
    let identity = matching_address.next()?;
    matching_address.next().is_none().then_some(identity)
}

fn mailbox_display_name(mailbox: &str) -> String {
    let display = mailbox
        .split_once('<')
        .map(|(display, _)| display.trim())
        .unwrap_or_default();
    let display = display
        .strip_prefix('"')
        .and_then(|display| display.strip_suffix('"'))
        .unwrap_or(display);
    let mut normalized = String::new();
    let mut escaped = false;
    for ch in display.chars() {
        if escaped {
            normalized.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else {
            normalized.push(ch);
        }
    }
    if escaped {
        normalized.push('\\');
    }
    normalized
}

fn rebind_draft_account(
    draft: &mut DraftMessage,
    account_id: MailAccountId,
    identity: &SendingIdentity,
) -> bool {
    let account_changed = draft.account_id != account_id;
    let previous_from = draft.from.clone();
    let previous_reply_to = draft.reply_to.clone();
    draft.account_id = account_id;
    apply_identity_headers(draft, identity);
    if account_changed {
        draft.conversation_id = None;
        draft.message_id = None;
    }
    account_changed
        || draft.from != previous_from
        || draft.reply_to != previous_reply_to
}

fn identity_for_rebind<'a>(
    account: &'a MailAccount,
    draft_account_id: Option<&MailAccountId>,
    selected_address: Option<&str>,
) -> &'a SendingIdentity {
    if draft_account_id == Some(&account.id)
        && let Some(selected_address) = selected_address
        && let Some(identity) = account.identity(selected_address)
    {
        return identity;
    }
    account.default_identity()
}

fn has_optional_recipients(draft: &DraftMessage) -> bool {
    !draft.cc.is_empty() || !draft.bcc.is_empty()
}

fn rebuild_attachment_list(
    list: &gtk::ListBox,
    attachments: &[AttachmentInfo],
    page: std::rc::Weak<ComposePageInner>,
    focus_index: Option<usize>,
) -> Option<gtk::Button> {
    list.remove_all();
    let mut focus_target = None;
    for (index, attachment_info) in attachments.iter().enumerate() {
        let remove = gtk::Button::builder()
            .label(gettext("Remove"))
            .can_shrink(true)
            .valign(gtk::Align::Center)
            .build();
        let (row, open) = attachment::row(
            "mail-attachment-symbolic",
            &attachment_info.display_name,
            Some(remove.upcast_ref()),
        );
        let page_for_open = page.clone();
        open.connect_clicked(move |_| {
            if let Some(inner) = page_for_open.upgrade() {
                ComposePage { inner }.open_attachment(index);
            }
        });
        let page_for_remove = page.clone();
        remove.connect_clicked(move |_| {
            if let Some(inner) = page_for_remove.upgrade() {
                ComposePage { inner }.remove_attachment(index);
            }
        });
        if focus_index == Some(index) {
            focus_target = Some(open.clone());
        }
        list.append(&row);
    }

    let (add_row, add) = attachment::row(
        "list-add-symbolic",
        &gettext("Add attachments"),
        None,
    );
    add.connect_clicked(move |_| {
        if let Some(inner) = page.upgrade() {
            ComposePage { inner }.add_attachments();
        }
    });
    if focus_index == Some(attachments.len()) {
        focus_target = Some(add.clone());
    }
    list.append(&add_row);
    focus_target
}

#[cfg(test)]
mod tests {
    use super::{
        ComposeKind, ComposeViewModel, apply_identity_headers, has_optional_recipients,
        html_from_text_preserving_signature, identity_for_rebind,
        rebind_draft_account, text_from_html_preserving_signature,
    };
    use crate::integration::stub::stub_account;
    use crate::integration::webkit::{ComposerContent, ComposerSignatureRegion};
    use crate::model::account::{MailAccount, MailAccountId, SendingIdentity};
    use crate::model::event::MailboxContentSnapshot;
    use crate::model::mail::{
        AttachmentInfo, AttachmentLocation, AttachmentSource, ConversationId, MailboxMode,
        MailtoRequest, MessageId, TextRange,
    };
    use crate::model::settings::AppSettings;
    use crate::ui::mailbox::MailboxViewModel;

    #[test]
    fn stub_mailto_uses_the_visible_stub_identity_without_an_eds_binding() {
        let mailbox = MailboxViewModel::from_content(
            vec![stub_account()],
            AppSettings::default(),
            MailboxMode::NoAccount,
            MailboxContentSnapshot {
                folders: Vec::new(),
                selected_folder_id: None,
                conversations: Vec::new(),
            },
        );
        let request = MailtoRequest {
            to: vec!["recipient@example.test".into()],
            subject: "Stub compose".into(),
            ..Default::default()
        };

        let model = ComposeViewModel::for_mailto(&mailbox, &request).unwrap();

        assert_eq!(model.draft.account_id.0, "local-stub");
        assert_eq!(model.draft.to, ["recipient@example.test"]);
        assert_eq!(model.draft.subject, "Stub compose");
        assert!(model.dirty);
    }

    #[test]
    fn optional_recipient_fields_open_only_when_the_draft_already_uses_them() {
        let mut draft = crate::model::mail::DraftMessage::empty(
            MailAccountId("account-1".into()),
            "Primary <primary@example.test>".into(),
        );
        assert!(!has_optional_recipients(&draft));
        draft.cc.push("copy@example.test".into());
        assert!(has_optional_recipients(&draft));
        draft.cc.clear();
        draft.bcc.push("hidden@example.test".into());
        assert!(has_optional_recipients(&draft));
    }

    #[test]
    fn changing_identity_materializes_mail_headers_in_the_draft() {
        let mut draft = crate::model::mail::DraftMessage::empty(
            MailAccountId("account-1".into()),
            "Primary <primary@example.test>".into(),
        );
        let alias = SendingIdentity::new(
            "alias@example.test".into(),
            "Alias Name".into(),
            Some("replies@example.test".into()),
            crate::model::account::Signature {
                html: "<p>Alias signature</p>".into(),
                text: "Alias signature".into(),
            },
        );

        apply_identity_headers(&mut draft, &alias);

        assert_eq!(draft.from, "\"Alias Name\" <alias@example.test>");
        assert_eq!(draft.reply_to.as_deref(), Some("replies@example.test"));
    }

    #[test]
    fn compose_identity_selection_is_derived_from_the_draft_sender() {
        let primary = SendingIdentity::new(
            "primary@example.test".into(),
            "Primary".into(),
            None,
            Default::default(),
        );
        let alias = SendingIdentity::new(
            "alias@example.test".into(),
            "Alias".into(),
            None,
            Default::default(),
        );
        let model = ComposeViewModel {
            kind: ComposeKind::New,
            draft: crate::model::mail::DraftMessage::empty(
                MailAccountId("account-1".into()),
                alias.mailbox(),
            ),
            available_identities: vec![primary, alias],
            dirty: false,
        };

        assert_eq!(model.selected_identity_index(), 1);
    }

    #[test]
    fn body_conversion_preserves_the_destination_signature_format() {
        let text = "Authored\n\nPlain signature\n\nQuoted";
        let text_range = TextRange { start: 8, end: 25 };
        let html = html_from_text_preserving_signature(
            text,
            Some(text_range),
            Some("<div><br></div><strong>Rich signature</strong>"),
        );
        assert!(html.contains(
            "<div data-signature-region><div><br></div><strong>Rich signature</strong></div>"
        ));
        assert!(!html.contains("Plain signature"));

        let html_content = ComposerContent {
            text: "Authored\n\nRendered rich signature\n\nQuoted".into(),
            signature: Some(ComposerSignatureRegion {
                prefix_text: "Authored".into(),
                text: "\n\nRendered rich signature".into(),
                html: "<div><br></div><strong>Rich signature</strong>".into(),
            }),
            ..Default::default()
        };
        let (converted, range) = text_from_html_preserving_signature(
            &html_content,
            text,
            Some(text_range),
        );
        assert_eq!(converted, text);
        assert_eq!(range, Some(text_range));
    }

    #[test]
    fn body_conversion_does_not_create_a_missing_destination_signature() {
        let text = "Authored\n\nPlain signature\n\nQuoted";
        let text_range = TextRange { start: 8, end: 25 };
        let html = html_from_text_preserving_signature(text, Some(text_range), None);
        assert!(!html.contains("Plain signature"));
        assert!(!html.contains("data-signature-region"));

        let html_content = ComposerContent {
            text: "Authored\n\nRendered signature\n\nQuoted".into(),
            signature: Some(ComposerSignatureRegion {
                prefix_text: "Authored".into(),
                text: "\n\nRendered signature".into(),
                html: "<div><br></div><strong>Rendered signature</strong>".into(),
            }),
            ..Default::default()
        };
        let (converted, range) =
            text_from_html_preserving_signature(&html_content, "Different text", None);
        assert_eq!(converted, "Authored\n\nQuoted");
        assert!(range.is_none());
    }

    #[test]
    fn text_conversion_keeps_an_editable_line_before_an_initial_signature() {
        let text = "\n\nSignature";
        let html = html_from_text_preserving_signature(
            text,
            Some(TextRange { start: 0, end: 11 }),
            Some("<div><br></div><strong>Signature</strong>"),
        );

        assert!(html.starts_with("<div><br></div><div data-signature-region>"));
    }

    #[test]
    fn equivalent_body_representations_recover_the_semantic_signature_range() {
        let text = "Authored\n\nSignature\n\nQuoted";
        let html_content = ComposerContent {
            text: text.into(),
            signature: Some(ComposerSignatureRegion {
                prefix_text: "Authored".into(),
                text: "\n\nSignature".into(),
                html: "<div><br></div><strong>Signature</strong>".into(),
            }),
            ..Default::default()
        };

        let (converted, range) = text_from_html_preserving_signature(&html_content, text, None);

        assert_eq!(converted, text);
        assert_eq!(range, Some(TextRange { start: 8, end: 19 }));
    }

    #[test]
    fn account_rebind_preserves_authored_content_but_resets_draft_provenance() {
        let mut draft = crate::model::mail::DraftMessage::empty(
            MailAccountId("account-a".into()),
            "Old Sender <old@example.test>".into(),
        );
        draft.to = vec!["recipient@example.test".into()];
        draft.subject = "Preserved subject".into();
        draft.attachments = vec![AttachmentInfo {
            display_name: "notes.txt".into(),
            location: AttachmentLocation::CachedToken("notes".into()),
        }];
        draft.set_attachment_source(Some(AttachmentSource {
            account_id: MailAccountId("account-a".into()),
            conversation_id: ConversationId("old-draft".into()),
        }));
        draft.conversation_id = Some(ConversationId("old-draft".into()));
        draft.message_id = Some(MessageId("old-message".into()));
        let identity = SendingIdentity {
            address: "sender@example.test".into(),
            display_name: "Sender".into(),
            reply_to: Some("reply@example.test".into()),
            signature: crate::model::account::Signature {
                html: "<em>New signature</em>".into(),
                text: "New signature".into(),
            },
        };

        draft.body.replace("Message body".into(), "Message body".into(), None);

        let changed = rebind_draft_account(
            &mut draft,
            MailAccountId("account-b".into()),
            &identity,
        );

        assert!(changed);
        assert_eq!(draft.account_id.0, "account-b");
        assert_eq!(draft.from, "\"Sender\" <sender@example.test>");
        assert_eq!(draft.reply_to.as_deref(), Some("reply@example.test"));
        assert_eq!(draft.to, ["recipient@example.test"]);
        assert_eq!(draft.subject, "Preserved subject");
        assert_eq!(draft.attachments.len(), 1);
        assert!(draft.conversation_id.is_none());
        assert!(draft.message_id.is_none());
        assert_eq!(
            draft
                .attachment_source()
                .map(|source| source.account_id.0.as_str()),
            Some("account-a")
        );
        assert_eq!(draft.body.text(), "Message body");
        assert_eq!(draft.body.html(), "Message body");
    }

    #[test]
    fn registry_refresh_retains_a_valid_selection_and_draft_provenance() {
        let account_id = MailAccountId("account-a".into());
        let default = SendingIdentity::new(
            "default@example.test".into(),
            "Default".into(),
            None,
            crate::model::account::Signature::default(),
        );
        let selected = SendingIdentity::new(
            "selected@example.test".into(),
            "Selected".into(),
            None,
            crate::model::account::Signature::default(),
        );
        let mut account = MailAccount::new(account_id.clone(), "Account".into(), default.clone());
        account.replace_aliases(
            vec![default.clone(), selected.clone()],
            default.address.clone(),
        );

        let retained = identity_for_rebind(&account, Some(&account_id), Some(&selected.address));
        assert_eq!(&retained.address, &selected.address);
        let switched = identity_for_rebind(
            &account,
            Some(&MailAccountId("account-b".into())),
            Some(&selected.address),
        );
        assert_eq!(&switched.address, &default.address);

        let mut draft = crate::model::mail::DraftMessage::empty(
            account_id.clone(),
            selected.mailbox(),
        );
        draft.conversation_id = Some(ConversationId("draft".into()));
        draft.message_id = Some(MessageId("message".into()));
        let changed = rebind_draft_account(&mut draft, account_id, &selected);

        assert!(!changed);
        assert!(draft.conversation_id.is_some());
        assert!(draft.message_id.is_some());
    }

    #[test]
    fn compose_catalog_rebind_uses_the_authored_sender_not_the_previous_row_index() {
        let account_id = MailAccountId("account-a".into());
        let primary = SendingIdentity::new(
            "primary@example.test".into(), "Primary".into(), None, Default::default(),
        );
        let alias = SendingIdentity::new(
            "alias@example.test".into(), "Alias".into(), None, Default::default(),
        );
        let mut account = MailAccount::new(account_id.clone(), "Account".into(), primary.clone());
        account.replace_aliases(vec![primary.clone(), alias.clone()], primary.address.clone());
        let mut model = ComposeViewModel {
            kind: ComposeKind::EditDraft,
            draft: crate::model::mail::DraftMessage::empty(account_id, alias.mailbox()),
            available_identities: account.aliases().to_vec(),
            dirty: false,
        };
        model.draft.conversation_id = Some(ConversationId("draft".into()));
        model.draft.message_id = Some(MessageId("message".into()));
        model.draft.body.replace("<p>Authored</p>".into(), "Authored".into(), None);
        account.replace_aliases(vec![alias.clone(), primary.clone()], primary.address.clone());

        let (selected, modified) = model.rebind_account(&account);

        assert_eq!(selected.address, alias.address);
        assert!(!modified);
        assert_eq!(
            model.available_identities[model.selected_identity_index() as usize].address,
            alias.address,
        );
        assert!(model.draft.conversation_id.is_some());
        assert!(model.draft.message_id.is_some());
        assert_eq!(model.draft.body.text(), "Authored");

        account.replace_aliases(vec![primary.clone()], primary.address.clone());
        let (selected, modified) = model.rebind_account(&account);
        assert_eq!(selected.address, primary.address);
        assert!(modified);
        assert_eq!(model.selected_identity_index(), 0);
        assert!(model.draft.conversation_id.is_some());

        let replacement = MailAccount::new(
            MailAccountId("account-b".into()), "Replacement".into(), alias.clone(),
        );
        let (selected, modified) = model.rebind_account(&replacement);
        assert_eq!(selected.address, alias.address);
        assert!(modified);
        assert_eq!(model.draft.account_id, replacement.id);
        assert!(model.draft.conversation_id.is_none());
        assert!(model.draft.message_id.is_none());
        assert_eq!(model.draft.body.text(), "Authored");
    }
}
