use std::rc::Rc;

use adw::prelude::*;
use gtk::glib::value::ToValue;

use crate::i18n::gettext;

use super::panels::{SidebarWidgets, ThreadPanelWidgets};

#[derive(Clone)]
pub(super) struct MailboxNavigation {
    folder_split: adw::NavigationSplitView,
    pub(super) reader_split: adw::NavigationSplitView,
    pub(super) back_button: gtk::Button,
}

impl MailboxNavigation {
    pub(super) fn new(
        sidebar: &SidebarWidgets,
        thread_panel: &ThreadPanelWidgets,
        preview_panel: &gtk::ScrolledWindow,
    ) -> Self {
        let folder_page =
            adw::NavigationPage::with_tag(&sidebar.container, &gettext("Folders"), "folders");
        let thread_page =
            adw::NavigationPage::with_tag(
                &thread_panel.container,
                &gettext("Messages"),
                "messages",
            );
        let folder_split = adw::NavigationSplitView::builder()
            .sidebar(&folder_page)
            .content(&thread_page)
            .show_content(true)
            .min_sidebar_width(200.0)
            .max_sidebar_width(300.0)
            .sidebar_width_fraction(0.40)
            .build();

        let list_page =
            adw::NavigationPage::with_tag(
                &folder_split,
                &gettext("Mailbox"),
                "mailbox-lists",
            );
        let preview_page =
            adw::NavigationPage::with_tag(
                preview_panel,
                &gettext("Message"),
                "message-preview",
            );
        let reader_split = adw::NavigationSplitView::builder()
            .sidebar(&list_page)
            .content(&preview_page)
            .min_sidebar_width(480.0)
            .max_sidebar_width(600.0)
            .sidebar_width_fraction(0.43)
            .build();
        let back_button = gtk::Button::builder()
            .icon_name("go-previous-symbolic")
            .tooltip_text(gettext("Back"))
            .build();

        Self {
            folder_split,
            reader_split,
            back_button,
        }
    }

    pub(super) fn show_folders(&self) {
        self.reader_split.set_show_content(false);
        self.folder_split.set_show_content(false);
    }

    pub(super) fn show_threads(&self) {
        self.reader_split.set_show_content(false);
        self.folder_split.set_show_content(true);
    }

    pub(super) fn show_preview(&self) {
        self.reader_split.set_show_content(true);
    }

    pub(super) fn navigate_back(&self) -> bool {
        if self.reader_split.is_collapsed() && self.reader_split.shows_content() {
            self.reader_split.set_show_content(false);
            return true;
        }
        if self.folder_split.is_collapsed() && self.folder_split.shows_content() {
            self.folder_split.set_show_content(false);
            return true;
        }
        false
    }

    fn can_navigate_back(&self) -> bool {
        (self.reader_split.is_collapsed() && self.reader_split.shows_content())
            || (self.folder_split.is_collapsed() && self.folder_split.shows_content())
    }
}

pub(super) fn install_adaptive_mailbox_layout(
    window: &adw::ApplicationWindow,
    page_stack: &gtk::Stack,
    navigation: &MailboxNavigation,
    compose_button: &gtk::Button,
    compact_compose_button: &gtk::Button,
    search_button: &gtk::Button,
    title_stack: &gtk::Stack,
) {
    window.set_size_request(480, 360);

    // Application-window breakpoints are alternative layouts. Each narrower
    // layout repeats the properties that must remain active from wider ones.
    let medium = adw::Breakpoint::new(
        adw::BreakpointCondition::parse("max-width: 740sp")
            .expect("valid medium mailbox breakpoint"),
    );
    let collapsed = true.to_value();
    let two_column_folder_min_width = 184.0_f64.to_value();
    medium.add_setter(
        &navigation.reader_split,
        "collapsed",
        Some(&collapsed),
    );
    medium.add_setter(
        &navigation.folder_split,
        "min-sidebar-width",
        Some(&two_column_folder_min_width),
    );
    window.add_breakpoint(medium);

    let compact_header_state = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    compact_header_state.set_visible(false);
    let compact_header = adw::Breakpoint::new(
        adw::BreakpointCondition::parse("max-width: 660sp")
            .expect("valid compact header breakpoint"),
    );
    let visible = true.to_value();
    compact_header.add_setter(
        &navigation.reader_split,
        "collapsed",
        Some(&collapsed),
    );
    compact_header.add_setter(
        &navigation.folder_split,
        "min-sidebar-width",
        Some(&two_column_folder_min_width),
    );
    compact_header.add_setter(&compact_header_state, "visible", Some(&visible));
    window.add_breakpoint(compact_header);

    let narrow = adw::Breakpoint::new(
        adw::BreakpointCondition::parse("max-width: 500sp")
            .expect("valid narrow mailbox breakpoint"),
    );
    narrow.add_setter(
        &navigation.reader_split,
        "collapsed",
        Some(&collapsed),
    );
    narrow.add_setter(
        &navigation.folder_split,
        "min-sidebar-width",
        Some(&two_column_folder_min_width),
    );
    narrow.add_setter(&compact_header_state, "visible", Some(&visible));
    narrow.add_setter(
        &navigation.folder_split,
        "collapsed",
        Some(&collapsed),
    );
    window.add_breakpoint(narrow);

    let page_stack_for_sync = page_stack.clone();
    let navigation_for_sync = navigation.clone();
    let compose_button = compose_button.clone();
    let compact_compose_button = compact_compose_button.clone();
    let search_button = search_button.clone();
    let title_stack = title_stack.clone();
    let compact_header_state_for_sync = compact_header_state.clone();
    let sync_header: Rc<dyn Fn()> = Rc::new(move || {
        let composing = page_stack_for_sync.visible_child_name().as_deref() == Some("compose");
        let compact = compact_header_state_for_sync.is_visible();
        compose_button.set_visible(!composing && !compact);
        compact_compose_button.set_visible(!composing && compact);
        search_button.set_visible(!composing);
        title_stack.set_visible(!compact);
        navigation_for_sync
            .back_button
            .set_visible(!composing && navigation_for_sync.can_navigate_back());
    });

    let sync = Rc::clone(&sync_header);
    page_stack.connect_visible_child_name_notify(move |_| sync());
    let sync = Rc::clone(&sync_header);
    compact_header_state.connect_visible_notify(move |_| sync());
    let sync = Rc::clone(&sync_header);
    navigation
        .reader_split
        .connect_collapsed_notify(move |_| sync());
    let sync = Rc::clone(&sync_header);
    navigation
        .reader_split
        .connect_show_content_notify(move |_| sync());
    let sync = Rc::clone(&sync_header);
    navigation
        .folder_split
        .connect_collapsed_notify(move |_| sync());
    let sync = Rc::clone(&sync_header);
    navigation
        .folder_split
        .connect_show_content_notify(move |_| sync());
    sync_header();
}
