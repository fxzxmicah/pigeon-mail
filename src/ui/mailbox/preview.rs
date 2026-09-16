use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib, pango};

use crate::integration::webkit::{configure_mail_view, load_html_document, stop_html_loading};
use crate::model::mail::AttachmentOperation;

use super::state;

#[derive(Clone)]
pub(super) struct PreviewWidgets {
    pub(super) container: gtk::ScrolledWindow,
    state_stack: gtk::Stack,
    subject: gtk::Label,
    meta: gtk::Label,
    recipients: gtk::Label,
    cc_recipients: gtk::Label,
    reply_to_value: gtk::Label,
    additional_headers_toggle: gtk::ToggleButton,
    attachments_label: gtk::Label,
    attachments_list: gtk::ListBox,
    loading_spinner: gtk::Spinner,
    error_page: adw::StatusPage,
    pub(super) reply_button: gtk::Button,
    pub(super) reply_all_button: gtk::Button,
    pub(super) forward_button: gtk::Button,
    pub(super) star_button: gtk::Button,
    pub(super) read_button: gtk::Button,
    pub(super) archive_button: gtk::Button,
    pub(super) trash_button: gtk::Button,
    mode_switcher: adw::InlineViewSwitcher,
    pub(super) mode_stack: adw::ViewStack,
    actions: gtk::ToggleButton,
    html_view: webkit::WebView,
    loaded_html: Rc<RefCell<Option<(crate::model::mail::ConversationId, u64)>>>,
    body: gtk::TextView,
    attachment_dispatch:
        Rc<RefCell<Option<Rc<dyn Fn(AttachmentOperation, crate::model::mail::AttachmentInfo)>>>>,
}

impl PreviewWidgets {
    pub(super) fn new(parent: &adw::ApplicationWindow, toast_overlay: &adw::ToastOverlay) -> Self {
        let subject = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["title-2"])
            .build();
        subject.set_hexpand(true);
        subject.set_wrap_mode(pango::WrapMode::WordChar);
        let meta = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label"])
            .build();
        meta.set_hexpand(true);
        meta.set_wrap_mode(pango::WrapMode::WordChar);
        meta.add_css_class("message-metadata");
        let recipients = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label"])
            .build();
        recipients.set_hexpand(true);
        recipients.set_wrap_mode(pango::WrapMode::WordChar);
        let cc_recipients = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label"])
            .build();
        cc_recipients.set_hexpand(true);
        cc_recipients.set_wrap_mode(pango::WrapMode::WordChar);
        let reply_to_value = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label"])
            .build();
        reply_to_value.set_hexpand(true);
        reply_to_value.set_wrap_mode(pango::WrapMode::WordChar);
        let additional_headers = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .build();
        additional_headers.append(&cc_recipients);
        additional_headers.append(&reply_to_value);
        let additional_headers_revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .child(&additional_headers)
            .build();
        let additional_headers_toggle = gtk::ToggleButton::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text("More headers")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        let additional_headers_revealer_for_toggle = additional_headers_revealer.clone();
        additional_headers_toggle.connect_toggled(move |button| {
            additional_headers_revealer_for_toggle.set_reveal_child(button.is_active());
        });
        let recipients_line = gtk::Box::builder().spacing(4).build();
        recipients_line.append(&recipients);
        recipients_line.append(&additional_headers_toggle);
        let attachments_label = gtk::Label::builder()
            .xalign(0.0)
            .css_classes(["heading"])
            .label("Attachments")
            .build();
        let attachments_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        let loading_spinner = gtk::Spinner::new();
        loading_spinner.set_halign(gtk::Align::Center);
        loading_spinner.add_css_class("mailbox-loading-spinner");
        loading_spinner.start();
        let loading_title = gtk::Label::builder()
            .css_classes(["title-3"])
            .label("Loading message")
            .build();
        let loading_description = gtk::Label::builder()
            .css_classes(["dim-label"])
            .label("Opening local cache…")
            .build();
        let loading_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .valign(gtk::Align::Center)
            .vexpand(true)
            .css_classes(["mailbox-empty-state"])
            .build();
        loading_box.append(&loading_spinner);
        loading_box.append(&loading_title);
        loading_box.append(&loading_description);
        let error_page = adw::StatusPage::builder()
            .icon_name("dialog-error-symbolic")
            .title("Message unavailable")
            .vexpand(true)
            .css_classes(["compact", "mailbox-empty-state"])
            .build();
        let empty_page = adw::StatusPage::builder()
            .icon_name("mail-read-symbolic")
            .title("No message selected")
            .description("Select a message to read it.")
            .vexpand(true)
            .css_classes(["compact", "mailbox-empty-state"])
            .build();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .hexpand(true)
            .vexpand(true)
            .build();
        content.append(&subject);
        content.append(&meta);
        content.append(&recipients_line);
        content.append(&additional_headers_revealer);
        content.append(&attachments_label);
        content.append(&attachments_list);
        let state_stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .hhomogeneous(false)
            .hexpand(true)
            .vexpand(true)
            .build();
        state_stack.add_named(&content, Some("message"));
        state_stack.add_named(&loading_box, Some("loading"));
        state_stack.add_named(&error_page, Some("error"));
        state_stack.add_named(&empty_page, Some("empty"));
        let reply_button = gtk::Button::builder()
            .label("Reply")
            .can_shrink(true)
            .build();
        let reply_all_button = gtk::Button::builder()
            .label("Reply All")
            .can_shrink(true)
            .build();
        let forward_button = gtk::Button::builder()
            .label("Forward")
            .can_shrink(true)
            .build();
        let star_button = gtk::Button::builder()
            .label("Star")
            .can_shrink(true)
            .build();
        let read_button = gtk::Button::builder()
            .label("Mark Read")
            .can_shrink(true)
            .build();
        let archive_button = gtk::Button::builder()
            .label("Archive")
            .can_shrink(true)
            .build();
        let trash_button = gtk::Button::builder()
            .label("Move to Trash")
            .can_shrink(true)
            .build();
        let html_view = webkit::WebView::new();
        let parent_for_links = parent.clone();
        let toast_for_links = toast_overlay.clone();
        configure_mail_view(&html_view, move |uri| {
            let launcher = gtk::UriLauncher::new(uri);
            let toast = toast_for_links.clone();
            launcher.launch(
                Some(&parent_for_links),
                None::<&gio::Cancellable>,
                move |result| {
                    if let Err(error) = result {
                        crate::logging::report_failure("message-link-open", &error.into());
                        toast.add_toast(adw::Toast::new("Link not opened."));
                    }
                },
            );
        });
        let body = gtk::TextView::builder()
            .editable(false)
            .cursor_visible(false)
            .accepts_tab(false)
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(18)
            .bottom_margin(18)
            .left_margin(18)
            .right_margin(18)
            .hexpand(true)
            .vexpand(true)
            .build();
        install_read_only_text_menu(&body);
        let text_scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .hexpand(true)
            .vexpand(true)
            .child(&body)
            .build();
        let mode_stack = adw::ViewStack::builder()
            .hhomogeneous(false)
            .vhomogeneous(false)
            .height_request(120)
            .hexpand(true)
            .vexpand(true)
            .build();
        mode_stack.add_titled(&html_view, Some("html"), "HTML");
        mode_stack.add_titled(&text_scroller, Some("text"), "Text");
        mode_stack.set_visible_child_name("html");
        let mode_switcher = adw::InlineViewSwitcher::builder()
            .stack(&mode_stack)
            .homogeneous(false)
            .can_shrink(true)
            .build();

        let action_buttons = adw::WrapBox::builder()
            .child_spacing(6)
            .line_spacing(6)
            .margin_top(8)
            .build();
        action_buttons.append(&reply_button);
        action_buttons.append(&reply_all_button);
        action_buttons.append(&forward_button);
        action_buttons.append(&star_button);
        action_buttons.append(&read_button);
        action_buttons.append(&archive_button);
        action_buttons.append(&trash_button);
        let action_revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .child(&action_buttons)
            .build();
        let actions = gtk::ToggleButton::builder()
            .label("Actions")
            .can_shrink(true)
            .build();
        let action_revealer_for_toggle = action_revealer.clone();
        actions.connect_toggled(move |button| {
            action_revealer_for_toggle.set_reveal_child(button.is_active());
        });
        let preview_controls = gtk::Box::builder()
            .spacing(6)
            .halign(gtk::Align::Start)
            .margin_top(8)
            .build();
        preview_controls.append(&mode_switcher);
        preview_controls.append(&actions);
        content.append(&preview_controls);
        content.append(&action_revealer);
        content.append(&mode_stack);
        let panel_content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .margin_top(18)
            .margin_bottom(18)
            .margin_start(16)
            .margin_end(16)
            .hexpand(true)
            .vexpand(true)
            .build();
        panel_content.append(&state_stack);
        let container = gtk::ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .child(&panel_content)
            .css_classes(["message-preview"])
            .build();
        container.set_min_content_width(0);

        Self {
            container,
            state_stack,
            subject,
            meta,
            recipients,
            cc_recipients,
            reply_to_value,
            additional_headers_toggle,
            attachments_label,
            attachments_list,
            loading_spinner,
            error_page,
            reply_button,
            reply_all_button,
            forward_button,
            star_button,
            read_button,
            archive_button,
            trash_button,
            mode_switcher,
            mode_stack,
            actions,
            html_view,
            loaded_html: Rc::new(RefCell::new(None)),
            body,
            attachment_dispatch: Rc::new(RefCell::new(None)),
        }
    }

    pub(super) fn set_attachment_dispatch<F>(&self, handler: F)
    where
        F: Fn(AttachmentOperation, crate::model::mail::AttachmentInfo) + 'static,
    {
        *self.attachment_dispatch.borrow_mut() = Some(Rc::new(handler));
    }

    pub(super) fn set_prefer_html_view(&self, prefer_html_view: bool) {
        self.mode_stack
            .set_visible_child_name(if prefer_html_view { "html" } else { "text" });
    }

    pub(super) fn refresh_content(&self, content: &state::PreviewContent) {
        let detail = match content {
            state::PreviewContent::Loaded(detail) => Some(detail),
            _ => None,
        };
        let current_conversation = self
            .loaded_html
            .borrow()
            .as_ref()
            .map(|(conversation_id, _)| conversation_id.clone());
        let next_conversation = detail.map(|detail| detail.conversation_id.clone());
        if current_conversation != next_conversation {
            self.additional_headers_toggle.set_active(false);
        }
        if let Some(detail) = detail {
            self.star_button
                .set_label(if detail.starred { "Unstar" } else { "Star" });
            self.read_button.set_label(if detail.unread {
                "Mark Read"
            } else {
                "Mark Unread"
            });
        } else {
            self.star_button.set_label("Star");
            self.read_button.set_label("Mark Read");
        }
        if let Some(detail) = detail {
            self.meta
                .set_label(&format!("From: {}    {}", detail.from, detail.date_label));
        }
        self.render_current(content);
    }

    pub(super) fn sync_action_state(&self, actions: &state::PreviewActions) {
        let state::PreviewActions::Available {
            folder_kind,
            pending,
            has_archive,
            has_trash,
        } = actions
        else {
            self.set_compose_actions(false, false);
            self.star_button.set_sensitive(false);
            self.read_button.set_sensitive(false);
            self.archive_button.set_sensitive(false);
            self.archive_button.set_label("Archive");
            self.trash_button.set_sensitive(false);
            self.trash_button.set_label("Move to Trash");
            self.actions.set_active(false);
            return;
        };

        let action_ready = !*pending;
        let is_drafts = matches!(*folder_kind, crate::model::mail::FolderKind::Drafts);
        let is_archive = matches!(*folder_kind, crate::model::mail::FolderKind::Archive);
        let is_trash = matches!(*folder_kind, crate::model::mail::FolderKind::Trash);

        self.set_compose_actions(action_ready, is_drafts);
        self.read_button.set_sensitive(action_ready && !is_drafts);
        self.star_button.set_sensitive(action_ready);
        self.archive_button
            .set_sensitive(action_ready && *has_archive && !is_archive && !is_trash);
        self.archive_button
            .set_label(if is_archive { "Archived" } else { "Archive" });
        self.trash_button
            .set_sensitive(action_ready && *has_trash && !is_trash);
        self.trash_button.set_label(if is_trash {
            "In Trash"
        } else {
            "Move to Trash"
        });
    }

    fn set_compose_actions(&self, enabled: bool, opens_draft: bool) {
        self.reply_button.set_sensitive(enabled);
        self.reply_button
            .set_label(if opens_draft { "Open Draft" } else { "Reply" });
        self.reply_all_button
            .set_sensitive(enabled && !opens_draft);
        self.forward_button.set_sensitive(enabled);
    }

    fn render_current(&self, content: &state::PreviewContent) {
        if let state::PreviewContent::Loaded(detail) = content {
            self.subject.set_visible(true);
            self.meta.set_visible(true);
            self.recipients.set_visible(true);
            self.subject.set_label(&detail.subject);
            self.recipients
                .set_label(&format!("To: {}", detail.to.join(", ")));
            self.cc_recipients
                .set_label(&format!("Cc: {}", detail.cc.join(", ")));
            let has_cc = !detail.cc.is_empty();
            self.cc_recipients.set_visible(has_cc);
            let has_reply_to = detail.reply_to.is_some();
            if let Some(reply_to) = detail.reply_to.as_deref() {
                self.reply_to_value
                    .set_label(&format!("Reply-To: {reply_to}"));
                self.reply_to_value.set_visible(true);
            } else {
                self.reply_to_value.set_visible(false);
            }
            self.additional_headers_toggle
                .set_visible(has_cc || has_reply_to);
            let attachment_dispatch = self.attachment_dispatch.borrow().clone();
            rebuild_preview_attachment_list(
                &self.attachments_list,
                &detail.attachments,
                attachment_dispatch,
            );
            self.attachments_label
                .set_label(&format!("Attachments ({})", detail.attachments.len()));
            self.attachments_label
                .set_visible(!detail.attachments.is_empty());
            self.attachments_list
                .set_visible(!detail.attachments.is_empty());
            self.mode_switcher.set_visible(true);
            self.mode_stack.set_visible(true);
            let html_body = detail.body.presentation_html();
            let content_key = (detail.conversation_id.clone(), html_fingerprint(html_body));
            let (conversation_changed, html_changed) = {
                let loaded_html = self.loaded_html.borrow();
                (
                    loaded_html.as_ref().is_none_or(|(conversation_id, _)| {
                        conversation_id != &detail.conversation_id
                    }),
                    loaded_html.as_ref() != Some(&content_key),
                )
            };
            let text_body = detail.body.presentation_text();
            let buffer = self.body.buffer();
            if conversation_changed
                || buffer
                    .text(&buffer.start_iter(), &buffer.end_iter(), false)
                    .as_str()
                    != text_body
            {
                buffer.set_text(text_body);
            }
            if html_changed {
                load_html_document(&self.html_view, html_body);
                *self.loaded_html.borrow_mut() = Some(content_key);
            }
        } else {
            self.subject.set_label("");
            self.subject.set_visible(false);
            self.meta.set_label("");
            self.meta.set_visible(false);
            self.recipients.set_label("");
            self.recipients.set_visible(false);
            self.cc_recipients.set_label("");
            self.cc_recipients.set_visible(false);
            self.reply_to_value.set_label("");
            self.reply_to_value.set_visible(false);
            self.additional_headers_toggle.set_active(false);
            self.additional_headers_toggle.set_visible(false);
            self.attachments_label.set_visible(false);
            self.attachments_list.set_visible(false);
            rebuild_preview_attachment_list(&self.attachments_list, &[], None);
            self.mode_switcher.set_visible(false);
            self.mode_stack.set_visible(false);
            stop_html_loading(&self.html_view);
            *self.loaded_html.borrow_mut() = None;
            self.body.buffer().set_text("");
            let error = match content {
                state::PreviewContent::Failed(error) => Some(error.as_str()),
                _ => None,
            };
            self.error_page.set_description(error);
        }
        match content {
            state::PreviewContent::Loading => {
                self.loading_spinner.start();
                self.state_stack.set_visible_child_name("loading");
            }
            state::PreviewContent::Loaded(_) => {
                self.loading_spinner.stop();
                self.state_stack.set_visible_child_name("message");
            }
            state::PreviewContent::Failed(_) => {
                self.loading_spinner.stop();
                self.state_stack.set_visible_child_name("error");
            }
            state::PreviewContent::Empty => {
                self.loading_spinner.stop();
                self.state_stack.set_visible_child_name("empty");
            }
        }
    }
}

fn install_read_only_text_menu(text_view: &gtk::TextView) {
    for action in [
        "menu.popup",
        "clipboard.cut",
        "clipboard.paste",
        "selection.delete",
        "text.undo",
        "text.redo",
        "text.clear",
        "misc.insert-emoji",
    ] {
        text_view.action_set_enabled(action, false);
    }
    text_view.buffer().set_enable_undo(false);

    let actions = gio::SimpleActionGroup::new();
    let copy_action = gio::SimpleAction::new("copy", None);
    let text_view_for_copy = text_view.clone();
    copy_action.connect_activate(move |_, _| {
        let buffer = text_view_for_copy.buffer();
        let Some((start, end)) = buffer.selection_bounds() else {
            return;
        };
        let text = buffer.text(&start, &end, false);
        text_view_for_copy.display().clipboard().set_text(&text);
    });
    actions.add_action(&copy_action);

    let select_all_action = gio::SimpleAction::new("select-all", None);
    let text_view_for_select_all = text_view.clone();
    select_all_action.connect_activate(move |_, _| {
        let buffer = text_view_for_select_all.buffer();
        buffer.select_range(&buffer.start_iter(), &buffer.end_iter());
    });
    actions.add_action(&select_all_action);
    text_view.insert_action_group("reader-text", Some(&actions));

    let menu = gio::Menu::new();
    let popover = gtk::PopoverMenu::from_model(Some(&menu));
    popover.set_has_arrow(false);
    popover.set_halign(gtk::Align::Start);
    popover.set_parent(text_view);

    let click = gtk::GestureClick::new();
    click.set_button(0);
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    let popover_for_click = popover.clone();
    let menu_for_click = menu.clone();
    let text_view_for_click = text_view.clone();
    click.connect_pressed(move |gesture, _, x, y| {
        let Some(event) = gesture.current_event() else {
            return;
        };
        if !event.triggers_context_menu() {
            return;
        }
        popup_text_menu(
            &popover_for_click,
            &menu_for_click,
            &text_view_for_click,
            x,
            y,
        );
        gesture.set_state(gtk::EventSequenceState::Claimed);
    });
    text_view.add_controller(click);

    let key_controller = gtk::EventControllerKey::new();
    let popover_for_key = popover.clone();
    let menu_for_key = menu;
    let text_view_for_key = text_view.clone();
    key_controller.connect_key_pressed(move |_, key, _, modifiers| {
        let context_menu_key = key == gtk::gdk::Key::Menu
            || (key == gtk::gdk::Key::F10
                && modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK));
        if !context_menu_key {
            return glib::Propagation::Proceed;
        }
        popup_text_menu(
            &popover_for_key,
            &menu_for_key,
            &text_view_for_key,
            f64::from(text_view_for_key.width()) / 2.0,
            f64::from(text_view_for_key.height()) / 2.0,
        );
        glib::Propagation::Stop
    });
    text_view.add_controller(key_controller);
}

fn popup_text_menu(
    popover: &gtk::PopoverMenu,
    menu: &gio::Menu,
    text_view: &gtk::TextView,
    x: f64,
    y: f64,
) {
    let buffer = text_view.buffer();
    menu.remove_all();
    if buffer.selection_bounds().is_some() {
        menu.append(Some("Copy"), Some("reader-text.copy"));
    }
    menu.append(Some("Select All"), Some("reader-text.select-all"));
    let pointing_to = gtk::gdk::Rectangle::new(x.round() as i32, y.round() as i32, 1, 1);
    popover.set_pointing_to(Some(&pointing_to));
    popover.popup();
}

fn html_fingerprint(html: &str) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    html.hash(&mut hasher);
    hasher.finish()
}

fn rebuild_preview_attachment_list(
    list: &gtk::ListBox,
    attachments: &[crate::model::mail::AttachmentInfo],
    dispatch: Option<Rc<dyn Fn(AttachmentOperation, crate::model::mail::AttachmentInfo)>>,
) {
    list.remove_all();

    for attachment in attachments {
        let row = gtk::ListBoxRow::new();
        let box_row = gtk::Box::builder()
            .spacing(12)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(12)
            .margin_end(12)
            .build();
        let text_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .hexpand(true)
            .build();
        let title_label = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::End)
            .lines(1)
            .label(&attachment.display_name)
            .build();
        let title = gtk::Button::builder()
            .hexpand(true)
            .can_shrink(true)
            .css_classes(["flat"])
            .child(&title_label)
            .build();
        let subtitle_text = attachment.location.external_uri().unwrap_or("");
        let subtitle = gtk::Label::builder()
            .xalign(0.0)
            .css_classes(["dim-label"])
            .label(subtitle_text)
            .build();
        subtitle.set_ellipsize(pango::EllipsizeMode::Middle);
        subtitle.set_single_line_mode(true);
        subtitle.set_visible(!subtitle_text.is_empty());
        text_box.append(&title);
        text_box.append(&subtitle);
        box_row.append(&text_box);

        let actions = gtk::Box::builder()
            .spacing(6)
            .halign(gtk::Align::End)
            .build();
        let save_button = gtk::Button::builder()
            .label("Save As")
            .can_shrink(true)
            .build();
        actions.append(&save_button);
        box_row.append(&actions);

        if let Some(dispatch) = dispatch.clone() {
            let attachment = attachment.clone();
            title.connect_clicked(move |_| {
                dispatch(AttachmentOperation::Open, attachment.clone());
            });
        }
        if let Some(dispatch) = dispatch.clone() {
            let attachment = attachment.clone();
            save_button.connect_clicked(move |_| {
                dispatch(AttachmentOperation::SaveAs, attachment.clone());
            });
        }
        row.set_child(Some(&box_row));
        row.set_activatable(false);
        list.append(&row);
    }
}
