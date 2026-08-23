use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use crate::core::cache::CacheManager;
use crate::core::draft;
use crate::integration::webkit::{ComposerContent, WebKitComposer};
use crate::model::account::{AliasId, SendingIdentity};
use crate::model::address::{normalized_mailbox_address, split_mailbox_list};
use crate::model::mail::{AttachmentInfo, DraftMessage, MailtoRequest};

use super::mailbox::MailboxViewModel;

const MAILBOX_PAGE: &str = "mailbox";
const COMPOSE_PAGE: &str = "compose";

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

impl ComposeOperation {
    fn progress_status(self) -> &'static str {
        match self {
            Self::SaveDraft => "Saving…",
            Self::Send => "Sending…",
        }
    }

    fn failure_status(self) -> &'static str {
        match self {
            Self::SaveDraft => "Not saved",
            Self::Send => "Not sent",
        }
    }
}

#[derive(Clone)]
pub struct ComposeViewModel {
    pub kind: ComposeKind,
    pub draft: DraftMessage,
    pub available_identities: Vec<SendingIdentity>,
    selected_alias_id: AliasId,
    pub initially_dirty: bool,
}

impl ComposeViewModel {
    pub fn for_action(mailbox: &MailboxViewModel, kind: ComposeKind) -> Option<Self> {
        if !mailbox.can_compose() {
            return None;
        }
        let account = mailbox.current_account()?;
        let message = mailbox.message_detail.as_ref();
        let identity = if kind == ComposeKind::EditDraft {
            message
                .and_then(|message| identity_for_from(&account.aliases, &message.from))
                .or_else(|| account.default_identity())?
        } else {
            account.default_identity()?
        };
        let draft = match kind {
            ComposeKind::New => draft::create_draft(account.id.clone(), identity),
            ComposeKind::EditDraft => draft::create_edit_draft(
                account.id.clone(),
                identity,
                mailbox.message_detail.as_ref()?,
            ),
            ComposeKind::Reply => draft::create_reply_draft(
                account.id.clone(),
                identity,
                mailbox.message_detail.as_ref()?,
            ),
            ComposeKind::ReplyAll => draft::create_reply_all_draft(
                account.id.clone(),
                identity,
                mailbox.message_detail.as_ref()?,
            ),
            ComposeKind::Forward => draft::create_forward_draft(
                account.id.clone(),
                identity,
                mailbox.message_detail.as_ref()?,
            ),
        };

        Some(Self {
            kind,
            draft,
            available_identities: account.aliases.clone(),
            selected_alias_id: identity.id.clone(),
            initially_dirty: false,
        })
    }

    pub fn for_mailto(mailbox: &MailboxViewModel, request: &MailtoRequest) -> Option<Self> {
        let account = mailbox.current_account()?;
        let identity = account.default_identity()?;
        Some(Self {
            kind: ComposeKind::New,
            draft: draft::create_mailto_draft(account.id.clone(), identity, request),
            available_identities: account.aliases.clone(),
            selected_alias_id: identity.id.clone(),
            initially_dirty: !request.is_empty(),
        })
    }

    pub fn selected_identity_index(&self) -> u32 {
        self.available_identities
            .iter()
            .position(|identity| identity.id == self.selected_alias_id)
            .unwrap_or(0) as u32
    }

    pub fn identity_at(&self, index: usize) -> Option<&SendingIdentity> {
        self.available_identities.get(index)
    }

    pub fn title(&self) -> &'static str {
        match self.kind {
            ComposeKind::New => "New Message",
            ComposeKind::EditDraft => "Edit Draft",
            ComposeKind::Reply => "Reply",
            ComposeKind::ReplyAll => "Reply All",
            ComposeKind::Forward => "Forward",
        }
    }

    pub fn initial_status(&self) -> &'static str {
        match self.kind {
            ComposeKind::New => "Draft ready",
            ComposeKind::EditDraft => "Draft loaded",
            ComposeKind::Reply => "Reply draft ready",
            ComposeKind::ReplyAll => "Reply-all draft ready",
            ComposeKind::Forward => "Forward draft ready",
        }
    }

    pub fn saved_status(&self) -> &'static str {
        match self.kind {
            ComposeKind::New | ComposeKind::EditDraft => "Draft saved",
            ComposeKind::Reply => "Reply draft saved",
            ComposeKind::ReplyAll => "Reply-all draft saved",
            ComposeKind::Forward => "Forward draft saved",
        }
    }
}

#[derive(Clone)]
enum PendingNavigation {
    Mailbox,
    Compose(ComposeViewModel),
    Quit,
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
    cache: CacheManager,
    composer: Rc<WebKitComposer>,
    body_stack: gtk::Stack,
    text_editor: gtk::TextView,
    convert_body_button: gtk::Button,
    formatting_bar: adw::WrapBox,
    draft: RefCell<Option<DraftMessage>>,
    identities: RefCell<Vec<SendingIdentity>>,
    signature: RefCell<(String, String)>,
    dirty: Cell<bool>,
    active_operation: Cell<Option<ComposeOperation>>,
    body_conversion_pending: Cell<bool>,
    navigation_queue: RefCell<VecDeque<PendingNavigation>>,
    navigation_dialog_open: Cell<bool>,
    account_rebind_pending: Cell<bool>,
    closing: Cell<bool>,
    updating_widgets: Cell<bool>,
    saved_status: RefCell<&'static str>,
    identity_dropdown: gtk::DropDown,
    to_entry: gtk::Entry,
    cc_entry: gtk::Entry,
    bcc_entry: gtk::Entry,
    subject_entry: gtk::Entry,
    optional_recipients_button: gtk::ToggleButton,
    attachment_list: gtk::ListBox,
    attachment_scroller: gtk::ScrolledWindow,
    attachment_frame: gtk::Frame,
    attachments_label: gtk::Label,
    open_attachment_button: gtk::Button,
    remove_attachment_button: gtk::Button,
}

impl ComposePage {
    pub fn new(
        parent: &adw::ApplicationWindow,
        page_stack: &gtk::Stack,
        mailbox: Rc<RefCell<MailboxViewModel>>,
        cache: CacheManager,
        toast_overlay: &adw::ToastOverlay,
    ) -> Self {
        let composer = Rc::new(WebKitComposer::new());
        let rich_editor = composer.build_view();
        let title = adw::WindowTitle::builder()
            .title("New Message")
            .subtitle("Draft ready")
            .build();
        let back_button = gtk::Button::builder()
            .icon_name("go-previous-symbolic")
            .tooltip_text("Back to mailbox")
            .build();
        let save_button = gtk::Button::builder().label("Save Draft").build();
        let send_button = gtk::Button::builder()
            .label("Send")
            .css_classes(["suggested-action"])
            .build();
        let identity_dropdown =
            gtk::DropDown::new(None::<gtk::StringList>, None::<gtk::Expression>);
        let to_entry = entry("Recipients");
        let cc_entry = entry("Cc recipients");
        let bcc_entry = entry("Bcc recipients");
        let subject_entry = entry("Subject");
        let optional_recipients_button = gtk::ToggleButton::builder()
            .label("Cc/Bcc")
            .valign(gtk::Align::Center)
            .build();

        let form_grid = gtk::Grid::builder()
            .hexpand(true)
            .row_spacing(8)
            .column_spacing(12)
            .build();
        form_grid.attach(&form_label("From"), 0, 0, 1, 1);
        form_grid.attach(&identity_dropdown, 1, 0, 1, 1);
        form_grid.attach(&form_label("To"), 0, 1, 1, 1);
        form_grid.attach(&to_entry, 1, 1, 1, 1);
        form_grid.attach(&optional_recipients_button, 2, 1, 1, 1);

        let optional_fields = gtk::Grid::builder()
            .hexpand(true)
            .row_spacing(8)
            .column_spacing(12)
            .build();
        optional_fields.attach(&form_label("Cc"), 0, 0, 1, 1);
        optional_fields.attach(&cc_entry, 1, 0, 1, 1);
        optional_fields.attach(&form_label("Bcc"), 0, 1, 1, 1);
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
        subject_row.append(&form_label("Subject"));
        subject_row.append(&subject_entry);
        let formatting_bar = adw::WrapBox::builder()
            .child_spacing(4)
            .line_spacing(4)
            .build();
        let body_stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .vexpand(true)
            .build();
        let text_editor = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .vexpand(true)
            .build();
        text_editor.set_top_margin(18);
        text_editor.set_bottom_margin(18);
        text_editor.set_left_margin(18);
        text_editor.set_right_margin(18);
        let text_scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&text_editor)
            .build();
        body_stack.add_titled(&rich_editor, Some("html"), "HTML");
        body_stack.add_titled(&text_scroller, Some("text"), "Text");
        body_stack.set_visible_child_name("html");
        let body_switcher = gtk::StackSwitcher::builder()
            .stack(&body_stack)
            .halign(gtk::Align::Start)
            .build();
        let convert_body_button = gtk::Button::builder().label("Convert to Text").build();
        let body_mode_bar = gtk::Box::builder().spacing(6).build();
        body_mode_bar.append(&body_switcher);
        body_mode_bar.append(&convert_body_button);
        for (icon, tooltip, command) in [
            ("edit-undo-symbolic", "Undo", "Undo"),
            ("edit-redo-symbolic", "Redo", "Redo"),
            ("format-text-bold-symbolic", "Bold", "Bold"),
            ("format-text-italic-symbolic", "Italic", "Italic"),
            ("format-text-underline-symbolic", "Underline", "Underline"),
            (
                "format-list-unordered-symbolic",
                "Bulleted list",
                "InsertUnorderedList",
            ),
            (
                "format-list-ordered-symbolic",
                "Numbered list",
                "InsertOrderedList",
            ),
        ] {
            let button = gtk::Button::builder()
                .icon_name(icon)
                .tooltip_text(tooltip)
                .css_classes(["flat"])
                .build();
            let composer = Rc::clone(&composer);
            button.connect_clicked(move |_| composer.execute_command(command));
            formatting_bar.append(&button);
        }
        let body_frame = gtk::Frame::builder()
            .label("Message")
            .css_classes(["compact-list-frame"])
            .child(&body_stack)
            .height_request(360)
            .vexpand(true)
            .build();

        let attachment_list = gtk::ListBox::builder().css_classes(["boxed-list"]).build();
        let attachment_scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(160)
            .child(&attachment_list)
            .build();
        let attachment_frame = gtk::Frame::builder()
            .css_classes(["compact-list-frame"])
            .child(&attachment_scroller)
            .visible(false)
            .build();
        let attachments_label = gtk::Label::builder()
            .xalign(0.0)
            .css_classes(["heading"])
            .label("Attachments")
            .build();
        let add_attachment_button = gtk::Button::builder().label("Add").build();
        let open_attachment_button = gtk::Button::builder()
            .label("Open")
            .sensitive(false)
            .build();
        let remove_attachment_button = gtk::Button::builder()
            .label("Remove")
            .sensitive(false)
            .build();
        let attachment_actions = gtk::Box::builder().spacing(12).build();
        attachment_actions.append(&add_attachment_button);
        attachment_actions.append(&open_attachment_button);
        attachment_actions.append(&remove_attachment_button);

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
        content.append(&body_mode_bar);
        content.append(&formatting_bar);
        content.append(&body_frame);
        content.append(&attachments_label);
        content.append(&attachment_frame);
        content.append(&attachment_actions);
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
                cache,
                composer,
                body_stack,
                text_editor,
                convert_body_button,
                formatting_bar,
                draft: RefCell::new(None),
                identities: RefCell::new(Vec::new()),
                signature: RefCell::new((String::new(), String::new())),
                dirty: Cell::new(false),
                active_operation: Cell::new(None),
                body_conversion_pending: Cell::new(false),
                navigation_queue: RefCell::new(VecDeque::new()),
                navigation_dialog_open: Cell::new(false),
                account_rebind_pending: Cell::new(false),
                closing: Cell::new(false),
                updating_widgets: Cell::new(false),
                saved_status: RefCell::new("Draft saved"),
                identity_dropdown,
                to_entry,
                cc_entry,
                bcc_entry,
                subject_entry,
                optional_recipients_button,
                attachment_list,
                attachment_scroller,
                attachment_frame,
                attachments_label,
                open_attachment_button,
                remove_attachment_button,
            }),
        };
        page.connect_signals(add_attachment_button);
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
        self.inner.page_stack.visible_child_name().as_deref() == Some(COMPOSE_PAGE)
    }

    pub fn request_open(&self, model: ComposeViewModel) {
        self.request_navigation(PendingNavigation::Compose(model));
    }

    pub fn request_back(&self) {
        if self.is_visible() && self.inner.back_button.is_sensitive() {
            self.request_navigation(PendingNavigation::Mailbox);
        }
    }

    pub fn handle_close_request(&self) -> glib::Propagation {
        if self.inner.closing.get() || !self.is_visible() {
            return glib::Propagation::Proceed;
        }
        if self.inner.active_operation.get().is_none()
            && !self.inner.body_conversion_pending.get()
            && !self.inner.dirty.get()
        {
            return glib::Propagation::Proceed;
        }
        self.request_navigation(PendingNavigation::Quit);
        glib::Propagation::Stop
    }

    pub fn account_activation_started(&self) {
        if self.is_visible() {
            self.sync_editor_content();
            self.inner.account_rebind_pending.set(true);
            self.set_busy(true);
            if self.inner.active_operation.get().is_none() {
                self.inner.title.set_subtitle("Switching account…");
            }
        }
    }

    pub fn account_activation_finished(&self) {
        if !self.is_visible() {
            return;
        }
        if self.inner.active_operation.get().is_some() || self.inner.body_conversion_pending.get() {
            self.inner.account_rebind_pending.set(true);
        } else {
            self.rebind_to_current_account();
        }
    }

    pub fn mailbox_replaced(&self) {
        if !self.is_visible()
            || self.inner.active_operation.get().is_some()
            || self.inner.body_conversion_pending.get()
        {
            return;
        }
        self.sync_editor_content();
        self.set_busy(false);
    }

    fn connect_signals(&self, add_attachment_button: gtk::Button) {
        let page = self.clone();
        self.inner
            .back_button
            .connect_clicked(move |_| page.request_navigation(PendingNavigation::Mailbox));
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
            .body_stack
            .connect_visible_child_name_notify(move |_| page.update_body_controls());
        let page = self.clone();
        self.inner
            .identity_dropdown
            .connect_selected_notify(move |dropdown| page.identity_changed(dropdown.selected()));
        let open = self.inner.open_attachment_button.clone();
        let remove = self.inner.remove_attachment_button.clone();
        self.inner
            .attachment_list
            .connect_row_selected(move |_, row| {
                open.set_sensitive(row.is_some());
                remove.set_sensitive(row.is_some());
            });
        let page = self.clone();
        add_attachment_button.connect_clicked(move |_| page.add_attachments());
        let page = self.clone();
        self.inner
            .open_attachment_button
            .connect_clicked(move |_| page.open_selected_attachment());
        let page = self.clone();
        self.inner
            .remove_attachment_button
            .connect_clicked(move |_| page.remove_selected_attachment());
    }

    fn show_model(&self, model: ComposeViewModel) {
        self.inner.updating_widgets.set(true);
        self.inner.account_rebind_pending.set(false);
        self.inner.dirty.set(model.initially_dirty);
        *self.inner.saved_status.borrow_mut() = model.saved_status();
        self.inner.title.set_title(model.title());
        self.inner.title.set_subtitle(if model.initially_dirty {
            "Modified"
        } else {
            model.initial_status()
        });
        *self.inner.signature.borrow_mut() = model
            .identity_at(model.selected_identity_index() as usize)
            .map(identity_signature)
            .unwrap_or_default();
        *self.inner.identities.borrow_mut() = model.available_identities.clone();
        self.set_identity_model(model.selected_identity_index());
        self.inner.to_entry.set_text(&model.draft.to.join(", "));
        self.inner.cc_entry.set_text(&model.draft.cc.join(", "));
        self.inner.bcc_entry.set_text(&model.draft.bcc.join(", "));
        self.inner.subject_entry.set_text(&model.draft.subject);
        self.inner
            .optional_recipients_button
            .set_active(has_optional_recipients(&model.draft));
        self.inner
            .composer
            .set_content(&model.draft.html_body, &model.draft.text_body);
        self.set_text_body(&model.draft.text_body);
        self.inner.body_stack.set_visible_child_name(
            if model.draft.html_body.trim().is_empty() && !model.draft.text_body.trim().is_empty() {
                "text"
            } else {
                "html"
            },
        );
        *self.inner.draft.borrow_mut() = Some(model.draft);
        self.refresh_attachments();
        self.update_body_controls();
        self.set_busy(false);
        self.inner.updating_widgets.set(false);
        self.inner.page_stack.set_visible_child_name(COMPOSE_PAGE);
        self.inner.to_entry.grab_focus();
    }

    fn request_navigation(&self, navigation: PendingNavigation) {
        if self.inner.navigation_dialog_open.get()
            || self.inner.active_operation.get().is_some()
            || self.inner.body_conversion_pending.get()
        {
            self.inner
                .navigation_queue
                .borrow_mut()
                .push_back(navigation);
            return;
        }
        if !self.is_visible() || !self.inner.dirty.get() {
            self.perform_navigation(navigation);
            return;
        }
        let dialog = gtk::AlertDialog::builder()
            .modal(true)
            .message("Unsaved draft")
            .detail("The current message has unsaved changes.")
            .build();
        let can_save = self.write_available();
        if can_save {
            dialog.set_buttons(&["Continue Editing", "Discard", "Save Draft"]);
            dialog.set_default_button(2);
        } else {
            dialog.set_buttons(&["Continue Editing", "Discard"]);
            dialog.set_default_button(0);
        }
        dialog.set_cancel_button(0);
        self.inner.navigation_dialog_open.set(true);
        let page = self.clone();
        dialog.choose(
            Some(&self.inner.parent),
            None::<&gio::Cancellable>,
            move |result| {
                page.inner.navigation_dialog_open.set(false);
                match result {
                    Ok(1) => page.perform_navigation(navigation.clone()),
                    Ok(2) if can_save && page.write_available() => {
                        page.inner
                            .navigation_queue
                            .borrow_mut()
                            .push_front(navigation.clone());
                        page.start_save();
                    }
                    _ => page.request_next_navigation(),
                }
            },
        );
    }

    fn request_next_navigation(&self) {
        if self.inner.navigation_dialog_open.get()
            || self.inner.active_operation.get().is_some()
            || self.inner.body_conversion_pending.get()
        {
            return;
        }
        let next = self.inner.navigation_queue.borrow_mut().pop_front();
        if let Some(next) = next {
            self.request_navigation(next);
        }
    }

    fn perform_navigation(&self, navigation: PendingNavigation) {
        let quitting = matches!(&navigation, PendingNavigation::Quit);
        match navigation {
            PendingNavigation::Mailbox => {
                self.inner.dirty.set(false);
                self.inner.account_rebind_pending.set(false);
                self.inner.page_stack.set_visible_child_name(MAILBOX_PAGE);
            }
            PendingNavigation::Compose(model) => self.show_model(model),
            PendingNavigation::Quit => {
                self.inner.dirty.set(false);
                self.inner.account_rebind_pending.set(false);
                self.inner.closing.set(true);
                self.inner.parent.close();
            }
        }
        if !quitting {
            self.request_next_navigation();
        }
    }

    fn mark_modified(&self) {
        if self.inner.updating_widgets.get() || self.inner.draft.borrow().is_none() {
            return;
        }
        self.inner.dirty.set(true);
        self.inner.title.set_subtitle("Modified");
    }

    fn entries_changed(&self) {
        if self.inner.updating_widgets.get() {
            return;
        }
        if let Some(draft) = self.inner.draft.borrow_mut().as_mut() {
            draft.to = recipients(&self.inner.to_entry);
            draft.cc = recipients(&self.inner.cc_entry);
            draft.bcc = recipients(&self.inner.bcc_entry);
            draft.subject = self.inner.subject_entry.text().to_string();
        }
        self.mark_modified();
    }

    fn html_body_changed(&self) {
        if self.inner.updating_widgets.get() {
            return;
        }
        if let Some(draft) = self.inner.draft.borrow_mut().as_mut() {
            draft.html_body = self.inner.composer.current_content().html;
        }
        self.mark_modified();
    }

    fn text_body_changed(&self) {
        if self.inner.updating_widgets.get() {
            return;
        }
        if let Some(draft) = self.inner.draft.borrow_mut().as_mut() {
            draft.text_body = self.text_body();
        }
        self.mark_modified();
    }

    fn text_body(&self) -> String {
        let buffer = self.inner.text_editor.buffer();
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), true)
            .to_string()
    }

    fn set_text_body(&self, text: &str) {
        self.inner.text_editor.buffer().set_text(text);
    }

    fn update_body_controls(&self) {
        let html_active = self.inner.body_stack.visible_child_name().as_deref() == Some("html");
        self.inner.convert_body_button.set_label(if html_active {
            "Convert to Text"
        } else {
            "Convert to HTML"
        });
        self.inner.formatting_bar.set_visible(html_active);
    }

    fn convert_body(&self) {
        if self.inner.body_stack.visible_child_name().as_deref() == Some("html") {
            if self.inner.body_conversion_pending.replace(true) {
                return;
            }
            self.set_busy(true);
            let page = self.clone();
            self.inner.composer.capture_content(move |snapshot| {
                page.inner.body_conversion_pending.set(false);
                let Ok(snapshot) = snapshot else {
                    page.inner
                        .toast_overlay
                        .add_toast(adw::Toast::new("Message not converted"));
                    page.finish_account_rebind_or_enable();
                    page.request_next_navigation();
                    return;
                };
                let changed = page.text_body() != snapshot.text;
                page.inner.updating_widgets.set(true);
                page.set_text_body(&snapshot.text);
                if let Some(draft) = page.inner.draft.borrow_mut().as_mut() {
                    draft.html_body = snapshot.html;
                    draft.text_body = snapshot.text;
                }
                page.inner.body_stack.set_visible_child_name("text");
                page.inner.updating_widgets.set(false);
                page.update_body_controls();
                if changed {
                    page.mark_modified();
                }
                page.finish_account_rebind_or_enable();
                page.request_next_navigation();
            });
            return;
        }

        let text = self.text_body();
        let html = crate::model::mail::plain_text_to_html(&text);
        let changed = self.inner.composer.current_content().html != html;
        self.inner.updating_widgets.set(true);
        self.inner.composer.set_content(&html, &text);
        if let Some(draft) = self.inner.draft.borrow_mut().as_mut() {
            draft.html_body = html;
            draft.text_body = text;
        }
        self.inner.body_stack.set_visible_child_name("html");
        self.inner.updating_widgets.set(false);
        self.update_body_controls();
        if changed {
            self.mark_modified();
        }
    }

    fn identity_changed(&self, selected: u32) {
        if self.inner.updating_widgets.get() {
            return;
        }
        let Some(identity) = self
            .inner
            .identities
            .borrow()
            .get(selected as usize)
            .cloned()
        else {
            return;
        };
        self.sync_editor_content();
        let previous = self.inner.signature.borrow().clone();
        let next = identity_signature(&identity);
        let content = self.draft_body();
        let html = replace_signature_content_with_separator(
            &content.html,
            &previous.0,
            &next.0,
            "<br><br>",
        );
        let text = replace_signature_content(&content.text, &previous.1, &next.1);
        if let Some(draft) = self.inner.draft.borrow_mut().as_mut() {
            apply_identity(draft, &identity);
            draft.html_body = html.clone();
            draft.text_body = text.clone();
        }
        self.inner.composer.set_content(&html, &text);
        self.set_text_body(&text);
        *self.inner.signature.borrow_mut() = next;
        self.mark_modified();
    }

    fn sync_editor_content(&self) {
        if let Some(draft) = self.inner.draft.borrow_mut().as_mut() {
            draft.html_body = self.inner.composer.current_content().html;
            draft.text_body = self.text_body();
        }
    }

    fn draft_body(&self) -> ComposerContent {
        self.inner
            .draft
            .borrow()
            .as_ref()
            .map(|draft| ComposerContent {
                html: draft.html_body.clone(),
                text: draft.text_body.clone(),
            })
            .unwrap_or_default()
    }

    fn rebind_to_current_account(&self) {
        self.sync_editor_content();
        let Some((account_id, identities, identity)) = self
            .inner
            .mailbox
            .borrow()
            .current_account()
            .and_then(|account| {
                account
                    .default_identity()
                    .cloned()
                    .map(|identity| (account.id.clone(), account.aliases.clone(), identity))
            })
        else {
            self.inner.title.set_subtitle("Identity unavailable");
            self.inner.account_rebind_pending.set(false);
            self.set_busy(false);
            return;
        };
        let previous = self.inner.signature.borrow().clone();
        let next = identity_signature(&identity);
        let content = self.draft_body();
        self.inner.updating_widgets.set(true);
        *self.inner.identities.borrow_mut() = identities;
        let selected = self
            .inner
            .identities
            .borrow()
            .iter()
            .position(|item| item.id == identity.id)
            .unwrap_or(0) as u32;
        self.set_identity_model(selected);
        let rebound =
            self.inner.draft.borrow_mut().as_mut().map(|draft| {
                rebind_draft_account(draft, account_id, &identity, &previous, content)
            });
        if let Some(content) = rebound {
            self.inner
                .composer
                .set_content(&content.html, &content.text);
            self.set_text_body(&content.text);
        }
        *self.inner.signature.borrow_mut() = next;
        self.inner.account_rebind_pending.set(false);
        self.inner.updating_widgets.set(false);
        self.set_busy(false);
        self.mark_modified();
    }

    fn set_identity_model(&self, selected: u32) {
        let labels = self
            .inner
            .identities
            .borrow()
            .iter()
            .map(|identity| {
                format!(
                    "{} <{}>",
                    identity.display_name_or_address(),
                    identity.address
                )
            })
            .collect::<Vec<_>>();
        let refs = labels.iter().map(String::as_str).collect::<Vec<_>>();
        self.inner
            .identity_dropdown
            .set_model(Some(&gtk::StringList::new(&refs)));
        self.inner.identity_dropdown.set_selected(selected);
    }

    fn begin_operation(&self, operation: ComposeOperation) {
        assert_eq!(
            self.inner.active_operation.replace(Some(operation)),
            None,
            "compose operation already active"
        );
        self.set_busy(true);
        self.inner.title.set_subtitle(operation.progress_status());
    }

    fn start_save(&self) {
        self.start_operation(ComposeOperation::SaveDraft);
    }

    fn start_send(&self) {
        self.start_operation(ComposeOperation::Send);
    }

    fn start_operation(&self, operation: ComposeOperation) {
        self.begin_operation(operation);
        let service = self.inner.mailbox.borrow().mail_service();
        let page = self.clone();
        self.inner
            .composer
            .capture_content(move |snapshot| match snapshot {
                Ok(snapshot) => page.dispatch_operation(
                    operation,
                    ComposerContent {
                        html: snapshot.html,
                        text: page.text_body(),
                    },
                    service,
                ),
                Err(error) => page.finish_operation_failure(operation.failure_status(), error),
            });
    }

    fn dispatch_operation(
        &self,
        operation: ComposeOperation,
        content: ComposerContent,
        service: crate::core::mail::MailService,
    ) {
        let mut draft = self
            .inner
            .draft
            .borrow()
            .clone()
            .expect("an active compose operation must own a draft");
        draft.html_body = content.html;
        draft.text_body = content.text;
        match operation {
            ComposeOperation::SaveDraft => self.inner.cache.request_save_draft(service, draft),
            ComposeOperation::Send => self.inner.cache.request_send_draft(service, draft),
        }
    }

    pub fn finish_save(
        &self,
        result: Result<Option<crate::model::mail::StoredMessageRef>, String>,
    ) {
        assert_eq!(
            self.inner.active_operation.get(),
            Some(ComposeOperation::SaveDraft),
            "draft-save completion does not match the active operation"
        );
        match result {
            Ok(stored) => {
                self.inner.active_operation.set(None);
                {
                    let mut draft = self.inner.draft.borrow_mut();
                    let draft = draft
                        .as_mut()
                        .expect("a completed compose operation must still own its draft");
                    if let Some(stored) = stored {
                        draft.conversation_id = Some(stored.conversation_id);
                        draft.message_id = Some(stored.message_id);
                    }
                }
                self.inner.dirty.set(false);
                self.inner
                    .title
                    .set_subtitle(*self.inner.saved_status.borrow());
                let navigation = self.inner.navigation_queue.borrow_mut().pop_front();
                if let Some(navigation) = navigation {
                    self.perform_navigation(navigation);
                    return;
                }
                self.finish_account_rebind_or_enable();
                self.request_next_navigation();
            }
            Err(error) => self.finish_operation_failure("Not saved", error),
        }
    }

    pub fn finish_send(&self, result: Result<(), String>) {
        assert_eq!(
            self.inner.active_operation.get(),
            Some(ComposeOperation::Send),
            "send completion does not match the active operation"
        );
        match result {
            Ok(_) => {
                self.inner.active_operation.set(None);
                self.inner.dirty.set(false);
                self.inner.title.set_subtitle("Queued");
                let navigation = self
                    .inner
                    .navigation_queue
                    .borrow_mut()
                    .pop_front()
                    .unwrap_or(PendingNavigation::Mailbox);
                self.perform_navigation(navigation);
            }
            Err(error) => self.finish_operation_failure("Not sent", error),
        }
    }

    fn finish_operation_failure(&self, status: &str, error: String) {
        self.inner.active_operation.set(None);
        self.inner.title.set_subtitle(status);
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
        self.inner.draft.borrow().is_some()
    }

    fn add_attachments(&self) {
        let dialog = gtk::FileDialog::builder()
            .title("Add attachments")
            .modal(true)
            .accept_label("Attach")
            .build();
        let page = self.clone();
        dialog.open_multiple(
            Some(&self.inner.parent),
            None::<&gio::Cancellable>,
            move |result| {
                let Ok(files) = result else {
                    return;
                };
                let mut changed = false;
                if let Some(draft) = page.inner.draft.borrow_mut().as_mut() {
                    for index in 0..files.n_items() {
                        let Some(item) = files.item(index) else {
                            continue;
                        };
                        let Ok(file) = item.downcast::<gio::File>() else {
                            continue;
                        };
                        let uri = file.uri().to_string();
                        if draft.attachments.iter().any(|item| item.uri == uri) {
                            continue;
                        }
                        let display_name = file
                            .path()
                            .and_then(|path| {
                                path.file_name()
                                    .map(|name| name.to_string_lossy().to_string())
                            })
                            .unwrap_or_else(|| "Attachment".into());
                        draft.attachments.push(AttachmentInfo { display_name, uri });
                        changed = true;
                    }
                }
                if changed {
                    page.refresh_attachments();
                    page.mark_modified();
                }
            },
        );
    }

    fn open_selected_attachment(&self) {
        let Some(row) = self.inner.attachment_list.selected_row() else {
            return;
        };
        let index = row.index() as usize;
        let Some(attachment) = self
            .inner
            .draft
            .borrow()
            .as_ref()
            .and_then(|draft| draft.attachments.get(index))
            .cloned()
        else {
            return;
        };
        let launcher = gtk::FileLauncher::new(Some(&gio::File::for_uri(&attachment.uri)));
        let toast = self.inner.toast_overlay.clone();
        launcher.launch(
            Some(&self.inner.parent),
            None::<&gio::Cancellable>,
            move |result| {
                let message = match result {
                    Ok(_) => format!("Opening {}", attachment.display_name),
                    Err(error) => {
                        crate::logging::report_failure("compose-attachment-open", &error);
                        format!("Not opened: {}", attachment.display_name)
                    }
                };
                toast.add_toast(adw::Toast::new(&message));
            },
        );
    }

    fn remove_selected_attachment(&self) {
        let Some(row) = self.inner.attachment_list.selected_row() else {
            return;
        };
        let index = row.index() as usize;
        let removed = self.inner.draft.borrow_mut().as_mut().is_some_and(|draft| {
            if index < draft.attachments.len() {
                draft.attachments.remove(index);
                true
            } else {
                false
            }
        });
        if removed {
            self.refresh_attachments();
            self.mark_modified();
        }
    }

    fn refresh_attachments(&self) {
        let attachments = self
            .inner
            .draft
            .borrow()
            .as_ref()
            .map(|draft| draft.attachments.clone())
            .unwrap_or_default();
        rebuild_attachment_list(&self.inner.attachment_list, &attachments);
        self.inner
            .attachment_scroller
            .set_min_content_height(attachment_viewport_height(attachments.len()));
        self.inner
            .attachments_label
            .set_label(&attachment_heading(attachments.len()));
        self.inner
            .attachment_frame
            .set_visible(!attachments.is_empty());
        if let Some(row) = self.inner.attachment_list.row_at_index(0) {
            self.inner.attachment_list.select_row(Some(&row));
        } else {
            self.inner.open_attachment_button.set_sensitive(false);
            self.inner.remove_attachment_button.set_sensitive(false);
        }
    }
}

fn entry(placeholder: &str) -> gtk::Entry {
    gtk::Entry::builder()
        .placeholder_text(placeholder)
        .hexpand(true)
        .build()
}

fn form_label(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .css_classes(["mail-form-label"])
        .build()
}

fn recipients(entry: &gtk::Entry) -> Vec<String> {
    split_mailbox_list(entry.text().as_str())
}

fn identity_signature(identity: &SendingIdentity) -> (String, String) {
    (
        identity.signature_html.clone(),
        identity.signature_text.clone(),
    )
}

fn apply_identity(draft: &mut DraftMessage, identity: &SendingIdentity) {
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
    account_id: crate::model::account::MailAccountId,
    identity: &SendingIdentity,
    previous_signature: &(String, String),
    content: ComposerContent,
) -> ComposerContent {
    let next_signature = identity_signature(identity);
    let content = ComposerContent {
        html: replace_signature_content_with_separator(
            &content.html,
            &previous_signature.0,
            &next_signature.0,
            "<br><br>",
        ),
        text: replace_signature_content(&content.text, &previous_signature.1, &next_signature.1),
    };
    draft.account_id = account_id;
    apply_identity(draft, identity);
    draft.html_body = content.html.clone();
    draft.text_body = content.text.clone();
    draft.conversation_id = None;
    draft.message_id = None;
    content
}

fn has_optional_recipients(draft: &DraftMessage) -> bool {
    !draft.cc.is_empty() || !draft.bcc.is_empty()
}

fn attachment_heading(count: usize) -> String {
    if count == 0 {
        "Attachments".into()
    } else {
        format!("Attachments ({count})")
    }
}

fn attachment_viewport_height(count: usize) -> i32 {
    count.min(4) as i32 * 40
}

fn replace_signature_content(
    current: &str,
    previous_signature: &str,
    next_signature: &str,
) -> String {
    replace_signature_content_with_separator(current, previous_signature, next_signature, "\n\n")
}

fn replace_signature_content_with_separator(
    current: &str,
    previous_signature: &str,
    next_signature: &str,
    separator: &str,
) -> String {
    if current.trim().is_empty() {
        current.to_string()
    } else if previous_signature.is_empty() {
        current.to_string()
    } else if current == previous_signature {
        next_signature.to_string()
    } else if let Some(base) = current.strip_prefix(previous_signature) {
        let base = base.strip_prefix(separator).unwrap_or(base);
        if next_signature.is_empty() {
            base.to_string()
        } else if base.is_empty() {
            next_signature.to_string()
        } else {
            format!("{next_signature}{separator}{base}")
        }
    } else if let Some(base) = current.strip_suffix(previous_signature) {
        let base = base.strip_suffix(separator).unwrap_or(base);
        if next_signature.is_empty() {
            base.to_string()
        } else if base.is_empty() {
            next_signature.to_string()
        } else {
            format!("{base}{separator}{next_signature}")
        }
    } else {
        current.to_string()
    }
}

fn rebuild_attachment_list(list: &gtk::ListBox, attachments: &[AttachmentInfo]) {
    list.remove_all();
    for attachment in attachments {
        let row = gtk::ListBoxRow::new();
        let content = gtk::Box::builder()
            .spacing(12)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(12)
            .margin_end(12)
            .build();
        let icon = gtk::Image::from_icon_name("mail-attachment-symbolic");
        icon.add_css_class("dim-label");
        let title = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .label(&attachment.display_name)
            .build();
        content.append(&icon);
        content.append(&title);
        row.set_child(Some(&content));
        row.set_activatable(false);
        list.append(&row);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ComposeViewModel, apply_identity, attachment_heading, has_optional_recipients,
        identity_for_from, rebind_draft_account, replace_signature_content,
        replace_signature_content_with_separator,
    };
    use crate::integration::backend::stub_backend;
    use crate::integration::stub::stub_account;
    use crate::integration::webkit::ComposerContent;
    use crate::model::account::{AliasId, MailAccountId, SendingIdentity};
    use crate::model::event::AccountMailboxSnapshot;
    use crate::model::mail::{
        AttachmentInfo, ConversationId, MailboxMode, MailtoRequest, MessageId,
    };
    use crate::model::settings::AppSettings;
    use crate::ui::mailbox::MailboxViewModel;

    #[test]
    fn stub_mailto_uses_the_visible_stub_identity_without_an_eds_binding() {
        let mailbox = MailboxViewModel::from_snapshot(
            vec![stub_account()],
            AppSettings::default(),
            stub_backend(),
            AccountMailboxSnapshot {
                mode: MailboxMode::StubNoAccount,
                folders: Vec::new(),
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
        assert!(model.initially_dirty);
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
    fn draft_from_claims_the_exact_alias_before_another_same_address_identity() {
        let identities = vec![
            SendingIdentity::with_id(
                AliasId("account-1:primary".into()),
                "shared@example.test".into(),
                "Primary Name".into(),
                None,
                String::new(),
                String::new(),
                true,
                true,
            ),
            SendingIdentity::with_id(
                AliasId("account-1:alias".into()),
                "shared@example.test".into(),
                "Alias Name".into(),
                None,
                String::new(),
                String::new(),
                false,
                false,
            ),
        ];

        let selected = identity_for_from(&identities, "Alias Name <shared@example.test>").unwrap();

        assert_eq!(selected.id.0, "account-1:alias");
        assert!(identity_for_from(&identities, "shared@example.test").is_none());
    }

    #[test]
    fn changing_identity_materializes_mail_headers_in_the_draft() {
        let mut draft = crate::model::mail::DraftMessage::empty(
            MailAccountId("account-1".into()),
            "Primary <primary@example.test>".into(),
        );
        let alias = SendingIdentity::with_id(
            AliasId("account-1:alias".into()),
            "alias@example.test".into(),
            "Alias Name".into(),
            Some("replies@example.test".into()),
            String::new(),
            String::new(),
            false,
            false,
        );

        apply_identity(&mut draft, &alias);

        assert_eq!(draft.from, "\"Alias Name\" <alias@example.test>");
        assert_eq!(draft.reply_to.as_deref(), Some("replies@example.test"));
    }

    #[test]
    fn attachment_heading_omits_a_zero_count_but_reports_present_items() {
        assert_eq!(attachment_heading(0), "Attachments");
        assert_eq!(attachment_heading(1), "Attachments (1)");
        assert_eq!(attachment_heading(12), "Attachments (12)");
    }

    #[test]
    fn signature_replacement_preserves_absent_mime_representations() {
        assert_eq!(replace_signature_content("", "Old", "New"), "");
        assert_eq!(
            replace_signature_content("Old\n\nQuoted reply", "Old", "New"),
            "New\n\nQuoted reply"
        );
        assert_eq!(
            replace_signature_content("Authored text\n\nOld", "Old", "New"),
            "Authored text\n\nNew"
        );
    }

    #[test]
    fn signature_removal_does_not_leave_its_separator_behind() {
        assert_eq!(
            replace_signature_content("Old\n\nQuoted reply", "Old", ""),
            "Quoted reply"
        );
        assert_eq!(
            replace_signature_content("Authored text\n\nOld", "Old", ""),
            "Authored text"
        );
    }

    #[test]
    fn unrelated_or_absent_previous_signature_does_not_rewrite_user_content() {
        assert_eq!(
            replace_signature_content("User kept Old inside a sentence", "Old", "New"),
            "User kept Old inside a sentence"
        );
        assert_eq!(
            replace_signature_content("Authored text", "", "New"),
            "Authored text"
        );
    }

    #[test]
    fn rich_signature_replacement_preserves_the_remaining_html() {
        assert_eq!(
            replace_signature_content_with_separator(
                "<strong>Old</strong><br><br><blockquote>Reply</blockquote>",
                "<strong>Old</strong>",
                "<em>New</em>",
                "<br><br>",
            ),
            "<em>New</em><br><br><blockquote>Reply</blockquote>"
        );
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
            uri: "file:///tmp/notes.txt".into(),
        }];
        draft.conversation_id = Some(ConversationId("old-draft".into()));
        draft.message_id = Some(MessageId("old-message".into()));
        let identity = SendingIdentity {
            id: AliasId("account-b:alias".into()),
            address: "sender@example.test".into(),
            display_name: "Sender".into(),
            reply_to: Some("reply@example.test".into()),
            signature_text: "New signature".into(),
            signature_html: "<em>New signature</em>".into(),
            is_default: true,
            is_primary_address: false,
        };

        let content = rebind_draft_account(
            &mut draft,
            MailAccountId("account-b".into()),
            &identity,
            &(
                "<strong>Old signature</strong>".into(),
                "Old signature".into(),
            ),
            ComposerContent {
                html: "Message<br><br><strong>Old signature</strong>".into(),
                text: "Message\n\nOld signature".into(),
            },
        );

        assert_eq!(draft.account_id.0, "account-b");
        assert_eq!(draft.from, "\"Sender\" <sender@example.test>");
        assert_eq!(draft.reply_to.as_deref(), Some("reply@example.test"));
        assert_eq!(draft.to, ["recipient@example.test"]);
        assert_eq!(draft.subject, "Preserved subject");
        assert_eq!(draft.attachments.len(), 1);
        assert!(draft.conversation_id.is_none());
        assert!(draft.message_id.is_none());
        assert_eq!(content.text, "Message\n\nNew signature");
        assert_eq!(content.html, "Message<br><br><em>New signature</em>");
    }
}
