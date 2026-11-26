use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::pango;

use crate::core::coordinator::MailCoordinator;
use crate::i18n::{gettext, gettext_f};

use super::{MailboxViewModel, preview::PreviewWidgets, state};

#[derive(Clone)]
pub(super) struct SidebarWidgets {
    pub(super) container: gtk::ScrolledWindow,
    account_title: gtk::Label,
    pub(super) folder_list: gtk::ListBox,
    content_stack: gtk::Stack,
}

#[derive(Clone)]
pub(super) struct ThreadPanelWidgets {
    pub(super) container: gtk::Box,
    heading: gtk::Label,
    pub(super) thread_list: gtk::ListBox,
    content_stack: gtk::Stack,
    loading_title: gtk::Label,
    loading_description: gtk::Label,
    empty_page: adw::StatusPage,
    error_page: adw::StatusPage,
    pub(super) preview_widgets: PreviewWidgets,
}

impl ThreadPanelWidgets {
    fn show_loading(&self, title: &str, description: &str) {
        self.loading_title.set_label(title);
        self.loading_description.set_label(description);
        self.content_stack.set_visible_child_name("loading");
    }

    fn show_messages(&self) {
        self.content_stack.set_visible_child_name("messages");
    }

    fn show_empty(&self, title: &str, description: &str) {
        self.empty_page.set_title(title);
        self.empty_page.set_description(Some(description));
        self.content_stack.set_visible_child_name("empty");
    }

    fn show_error(&self, description: &str) {
        self.error_page.set_description(Some(description));
        self.content_stack.set_visible_child_name("error");
    }
}

pub(super) fn build_sidebar(
    state: &Rc<RefCell<MailboxViewModel>>,
    thread_panel: ThreadPanelWidgets,
    coordinator: MailCoordinator,
    search_entry: gtk::SearchEntry,
    search_bar: gtk::SearchBar,
) -> SidebarWidgets {
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(8)
        .margin_end(8)
        .build();
    let container = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&content)
        .css_classes(["mailbox-sidebar"])
        .build();

    let account_title = gtk::Label::builder()
        .xalign(0.0)
        .margin_start(4)
        .margin_end(4)
        .css_classes(["title-4"])
        .build();
    content.append(&account_title);

    let folder_list = gtk::ListBox::builder()
        .css_classes(["navigation-sidebar", "mail-folder-list"])
        .build();
    let loading_spinner = adw::Spinner::new();
    let loading_label = gtk::Label::builder()
        .xalign(0.0)
        .css_classes(["dim-label"])
        .label(gettext("Loading folders…"))
        .build();
    let loading_page = gtk::Box::builder()
        .spacing(10)
        .margin_top(10)
        .margin_bottom(10)
        .margin_start(10)
        .margin_end(10)
        .build();
    loading_page.append(&loading_spinner);
    loading_page.append(&loading_label);
    let content_stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .hhomogeneous(false)
        .vhomogeneous(false)
        .build();
    content_stack.add_named(&folder_list, Some("folders"));
    content_stack.add_named(&loading_page, Some("loading"));
    content.append(&content_stack);

    let widgets = SidebarWidgets {
        container,
        account_title,
        folder_list,
        content_stack,
    };
    rebuild_sidebar(state, &widgets);

    let state_for_folder = Rc::clone(state);
    let thread_panel_for_folder = thread_panel.clone();
    widgets.folder_list.connect_row_selected(move |_, row| {
        let Some(row) = row else {
            return;
        };

        let selection_changes = state_for_folder
            .borrow()
            .folder_selection_changes(row.index() as usize);
        if !selection_changes {
            return;
        }

        let account_id = state_for_folder.borrow().current_account_id();
        if let Some(account_id) = account_id {
            coordinator.cancel_message_detail_requests(&account_id);
        }
        let request = state_for_folder.borrow_mut().select_folder(row.index() as usize);
        if !search_entry.text().is_empty() {
            search_entry.set_text("");
            search_bar.set_search_mode(false);
        }
        rebuild_thread_panel(&state_for_folder, &thread_panel_for_folder);
        if let Some(request) = request {
            coordinator.request_thread_page(
                request.request_id,
                request.account_id,
                request.folder_id,
                request.offset,
                request.limit,
            );
        }
    });

    widgets
}

pub(super) fn build_thread_panel(
    state: &Rc<RefCell<MailboxViewModel>>,
    detail_widgets: PreviewWidgets,
    coordinator: MailCoordinator,
) -> ThreadPanelWidgets {
    let container = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .vexpand(true)
        .css_classes(["conversation-panel"])
        .build();

    let content = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();

    let heading = gtk::Label::builder()
        .xalign(0.0)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(16)
        .margin_end(16)
        .css_classes(["title-4"])
        .build();
    container.append(&heading);

    let thread_list = gtk::ListBox::builder()
        .css_classes(["conversation-list"])
        .vexpand(true)
        .build();
    let loading_spinner = adw::Spinner::new();
    loading_spinner.add_css_class("mailbox-loading-spinner");
    let loading_title = gtk::Label::builder()
        .label(gettext("Loading messages"))
        .css_classes(["title-3"])
        .build();
    let loading_description = gtk::Label::builder()
        .label(gettext("Reading the local mail cache…"))
        .wrap(true)
        .max_width_chars(28)
        .justify(gtk::Justification::Center)
        .css_classes(["dim-label"])
        .build();
    let loading_page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .margin_start(24)
        .margin_end(24)
        .build();
    loading_page.append(&loading_spinner);
    loading_page.append(&loading_title);
    loading_page.append(&loading_description);
    let empty_page = adw::StatusPage::builder()
        .icon_name("mail-unread-symbolic")
        .title(gettext("No messages"))
        .description(gettext("Folder is empty."))
        .css_classes(["compact"])
        .build();
    let error_page = adw::StatusPage::builder()
        .icon_name("dialog-error-symbolic")
        .title(gettext("Messages unavailable"))
        .css_classes(["compact"])
        .build();
    let content_stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .hhomogeneous(false)
        .vexpand(true)
        .build();
    content.set_child(Some(&thread_list));
    content_stack.add_named(&content, Some("messages"));
    content_stack.add_named(&loading_page, Some("loading"));
    content_stack.add_named(&empty_page, Some("empty"));
    content_stack.add_named(&error_page, Some("error"));

    let state_for_selection = Rc::clone(state);
    let preview_for_selection = detail_widgets.clone();
    let coordinator_for_selection = coordinator.clone();
    thread_list.connect_row_activated(move |_, row| {
        let id = row.widget_name();
        {
            let mut state = state_for_selection.borrow_mut();
            state.select_thread(crate::model::mail::ConversationId(id.to_string()));
        }
        let request = state_for_selection.borrow_mut().begin_message_detail_load();
        if let Some(request) = request {
            coordinator_for_selection.request_message_detail(
                request.request_id,
                request.account_id,
                request.conversation_id,
            );
        }
        refresh_preview(&state_for_selection, &preview_for_selection);
    });

    let state_for_scroll = Rc::clone(state);
    let coordinator_for_scroll = coordinator;
    content
        .vadjustment()
        .connect_value_changed(move |adjustment| {
            let threshold = 96.0;
            let at_bottom =
                adjustment.value() + adjustment.page_size() >= adjustment.upper() - threshold;
            if !at_bottom {
                return;
            }

            let request = state_for_scroll.borrow_mut().begin_thread_page_load();
            if let Some(request) = request {
                coordinator_for_scroll.request_thread_page(
                    request.request_id,
                    request.account_id,
                    request.folder_id,
                    request.offset,
                    request.limit,
                );
            }
        });
    container.append(&content_stack);

    let widgets = ThreadPanelWidgets {
        container,
        heading,
        thread_list,
        content_stack,
        loading_title,
        loading_description,
        empty_page,
        error_page,
        preview_widgets: detail_widgets,
    };
    rebuild_thread_panel(state, &widgets);
    widgets
}

pub(super) fn rebuild_sidebar(state: &Rc<RefCell<MailboxViewModel>>, widgets: &SidebarWidgets) {
    let content = state.borrow().sidebar_content();
    widgets.account_title.set_label(&gettext("Folders"));

    match content {
        state::SidebarContent::Loading => {
            widgets.content_stack.set_visible_child_name("loading");
        }
        state::SidebarContent::Folders { folders, selected } => {
            widgets.folder_list.remove_all();
            for folder in folders {
                let row = gtk::ListBoxRow::new();
                let line = gtk::Box::builder()
                    .spacing(10)
                    .margin_top(8)
                    .margin_bottom(8)
                    .margin_start(10)
                    .margin_end(10)
                    .build();
                let icon = gtk::Image::from_icon_name(folder_icon_name(folder.kind));
                icon.add_css_class("dim-label");
                let label = gtk::Label::builder()
                    .xalign(0.0)
                    .hexpand(true)
                    .ellipsize(pango::EllipsizeMode::End)
                    .label(&folder.name)
                    .build();
                line.append(&icon);
                line.append(&label);
                if folder.unread_count > 0 {
                    let unread = gtk::Label::builder()
                        .label(folder.unread_count.to_string())
                        .css_classes(["caption", "mail-count-badge"])
                        .valign(gtk::Align::Center)
                        .build();
                    line.append(&unread);
                }
                row.set_child(Some(&line));
                widgets.folder_list.append(&row);
            }

            if let Some(row) = widgets.folder_list.row_at_index(selected as i32) {
                widgets.folder_list.select_row(Some(&row));
            }

            widgets.content_stack.set_visible_child_name("folders");
        }
    }
}

pub(super) fn rebuild_thread_panel(state: &Rc<RefCell<MailboxViewModel>>, widgets: &ThreadPanelWidgets) {
    let view = state.borrow().thread_list_view();
    widgets.thread_list.remove_all();
    widgets.heading.set_label(&view.heading);
    match &view.content {
        state::ThreadListContent::LoadingMailbox => {
            widgets.show_loading(&gettext("Loading mail"), &gettext("Opening local cache…"))
        }
        state::ThreadListContent::LoadingMessages => {
            widgets.show_loading(
                &gettext("Loading messages"),
                &gettext("Opening local cache…"),
            )
        }
        state::ThreadListContent::Searching => {
            widgets.show_loading(
                &gettext("Searching mail"),
                &gettext("Searching local cache…"),
            )
        }
        state::ThreadListContent::SearchFailed(error) => {
            widgets.show_error(error);
        }
        state::ThreadListContent::EmptyFolder => {
            widgets.show_empty(&gettext("No messages"), &gettext("Folder is empty."))
        }
        state::ThreadListContent::EmptySearch => {
            widgets.show_empty(
                &gettext("No matches"),
                &gettext("Local cache has no matches."),
            )
        }
        state::ThreadListContent::Messages { threads, selected } => {
            widgets.show_messages();
            for thread in threads {
                append_thread_row(&widgets.thread_list, thread);
            }
            if let Some(index) = selected
                .as_ref()
                .and_then(|selected_id| threads.iter().position(|thread| &thread.id == selected_id))
            {
                widgets
                    .thread_list
                    .select_row(widgets.thread_list.row_at_index(index as i32).as_ref());
            }
        }
    }

    refresh_preview(state, &widgets.preview_widgets);
}

pub(super) fn refresh_preview(state: &Rc<RefCell<MailboxViewModel>>, widgets: &PreviewWidgets) {
    let (content, actions) = {
        let mailbox = state.borrow();
        (mailbox.preview_content(), mailbox.preview_actions())
    };
    widgets.refresh_content(&content);
    widgets.sync_action_state(&actions);
}

pub(super) fn refresh_preview_actions(state: &Rc<RefCell<MailboxViewModel>>, widgets: &PreviewWidgets) {
    let actions = state.borrow().preview_actions();
    widgets.sync_action_state(&actions);
}

pub(super) fn append_thread_row(thread_list: &gtk::ListBox, thread: &crate::model::mail::ConversationSummary) {
    let row = gtk::ListBoxRow::new();
    row.set_widget_name(&thread.id.0);

    let container = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(14)
        .margin_end(14)
        .build();

    let sender_line = gtk::Box::builder().spacing(8).build();
    let participants = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(pango::EllipsizeMode::End)
        .lines(1)
        .css_classes(["heading"])
        .label(if thread.participants.is_empty() {
            gettext("Unknown sender")
        } else {
            thread.participants.join(", ")
        })
        .build();
    if thread.unread_count > 0 {
        participants.add_css_class("conversation-unread");
    }
    sender_line.append(&participants);
    if thread.attachment_count > 0 {
        let attachment = gtk::Image::from_icon_name("mail-attachment-symbolic");
        attachment.set_tooltip_text(Some(&gettext("Attachments")));
        attachment.add_css_class("dim-label");
        sender_line.append(&attachment);
    }
    if thread.starred {
        let star = gtk::Image::from_icon_name("starred-symbolic");
        star.set_tooltip_text(Some(&gettext("Starred")));
        star.add_css_class("accent");
        sender_line.append(&star);
    }

    let subject = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .lines(1)
        .label(thread_subject_label(thread))
        .build();
    if thread.unread_count > 0 {
        subject.add_css_class("conversation-unread");
    }

    let preview = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .lines(2)
        .css_classes(["dim-label"])
        .label(&thread.preview)
        .build();
    preview.set_ellipsize(pango::EllipsizeMode::End);

    container.append(&sender_line);
    container.append(&subject);
    container.append(&preview);
    row.set_child(Some(&container));
    thread_list.append(&row);
}

fn folder_icon_name(kind: crate::model::mail::FolderKind) -> &'static str {
    use crate::model::mail::FolderKind;

    match kind {
        FolderKind::Inbox => {
            "mail-unread-symbolic"
        }
        FolderKind::Drafts => {
            "document-edit-symbolic"
        }
        FolderKind::Outbox => {
            "mail-send-receive-symbolic"
        }
        FolderKind::Sent => {
            "mail-send-symbolic"
        }
        FolderKind::Archive => {
            "folder-documents-symbolic"
        }
        FolderKind::Trash => {
            "user-trash-symbolic"
        }
        FolderKind::Spam => {
            "mail-mark-junk-symbolic"
        }
        FolderKind::Custom => {
            "folder-symbolic"
        }
    }
}

fn thread_subject_label(thread: &crate::model::mail::ConversationSummary) -> String {
    let subject = if thread.subject.trim().is_empty() {
        return if thread.message_count > 1 {
            gettext_f(
                "(No subject) ({count})",
                &[("count", &thread.message_count.to_string())],
            )
        } else {
            gettext("(No subject)")
        };
    } else {
        thread.subject.trim()
    };
    if thread.message_count > 1 {
        gettext_f(
            "{subject} ({count})",
            &[
                ("subject", subject),
                ("count", &thread.message_count.to_string()),
            ],
        )
    } else {
        subject.into()
    }
}
