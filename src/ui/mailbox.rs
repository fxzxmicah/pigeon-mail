use adw::prelude::*;
use futures::StreamExt;
use gtk::glib::variant::ToVariant;
use gtk::glib::value::ToValue;
use gtk::{gio, glib, pango};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

mod preview;
mod state;

pub(crate) use state::MailboxViewModel;
use preview::PreviewWidgets;

use crate::core::coordinator::MailCoordinator;
use crate::i18n::{gettext, gettext_f, ngettext_f};
use crate::model::account::MailAccount;
use crate::model::event::MailEvent;
use crate::model::mail::{AttachmentOperation, FolderId, MailtoRequest};
use crate::model::settings::AppSettings;

use super::compose::{ComposeKind, ComposePage, ComposeViewModel};
use super::settings::{SettingsDialog, build_settings};

pub(crate) struct MailboxSessionSnapshot {
    pub settings: AppSettings,
    pub current_account_id: Option<crate::model::account::MailAccountId>,
    pub accounts: Vec<MailAccount>,
}

#[derive(Clone)]
pub struct MainWindow {
    inner: adw::ApplicationWindow,
    mailbox: Rc<RefCell<MailboxViewModel>>,
    title: adw::WindowTitle,
    account_dropdown: gtk::DropDown,
    sidebar: SidebarWidgets,
    thread_panel: ThreadPanelWidgets,
    navigation: MailboxNavigation,
    coordinator: MailCoordinator,
    compose_page: ComposePage,
    mailbox_controls: MailboxControls,
    toast_overlay: adw::ToastOverlay,
    mailto_queue: Rc<RefCell<Vec<MailtoRequest>>>,
    deferred_folder: Rc<RefCell<Option<(crate::model::account::MailAccountId, FolderId)>>>,
    settings_dialog: Rc<RefCell<Option<SettingsDialog>>>,
}

#[derive(Clone)]
struct MailboxControls {
    compose_action: gio::SimpleAction,
    refresh_button: gtk::Button,
    search_entry: gtk::SearchEntry,
    search_button: gtk::Button,
    search_bar: gtk::SearchBar,
}

impl MailboxControls {
    fn update(&self, account_ready: bool) {
        self.compose_action.set_enabled(account_ready);
        self.refresh_button.set_sensitive(account_ready);
        self.search_entry.set_sensitive(account_ready);
        self.search_button.set_sensitive(account_ready);
    }
}

#[derive(Clone)]
struct MailboxNavigation {
    folder_split: adw::NavigationSplitView,
    reader_split: adw::NavigationSplitView,
    back_button: gtk::Button,
}

impl MailboxNavigation {
    fn new(
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

    fn show_folders(&self) {
        self.reader_split.set_show_content(false);
        self.folder_split.set_show_content(false);
    }

    fn show_threads(&self) {
        self.reader_split.set_show_content(false);
        self.folder_split.set_show_content(true);
    }

    fn show_preview(&self) {
        self.reader_split.set_show_content(true);
    }

    fn navigate_back(&self) -> bool {
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

impl MainWindow {
    pub fn new(
        app: &adw::Application,
        mailbox: MailboxViewModel,
        coordinator: MailCoordinator,
    ) -> Self {
        let state = Rc::new(RefCell::new(mailbox));
        let mailto_queue = Rc::new(RefCell::new(Vec::new()));
        let deferred_folder = Rc::new(RefCell::new(None));

        let header = adw::HeaderBar::new();
        let status_summary = state.borrow().status_summary();
        let title = adw::WindowTitle::builder()
            .title(crate::config::APP_NAME)
            .subtitle(status_summary)
            .build();

        let compose_button = gtk::Button::builder()
            .label(gettext("Compose"))
            .action_name("win.compose")
            .css_classes(["suggested-action"])
            .build();
        let about_button = gtk::Button::builder()
            .icon_name("help-about-symbolic")
            .tooltip_text(gettext("About"))
            .action_name("win.about")
            .build();
        header.pack_end(&about_button);

        let preferences_button = gtk::Button::builder()
            .icon_name("emblem-system-symbolic")
            .tooltip_text(gettext("Mail Settings"))
            .action_name("win.preferences")
            .build();
        header.pack_end(&preferences_button);

        let refresh_button = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text(gettext("Refresh current account"))
            .build();
        header.pack_end(&refresh_button);

        let inner = adw::ApplicationWindow::builder()
            .application(app)
            .title(crate::config::APP_NAME)
            .default_width(1280)
            .default_height(860)
            .build();

        #[cfg(debug_assertions)]
        inner.add_css_class("devel");

        let toast_overlay = adw::ToastOverlay::new();
        let pending_work_toast = Rc::new(RefCell::new(None::<adw::Toast>));
        let page_stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .hhomogeneous(false)
            .vhomogeneous(false)
            .hexpand(true)
            .vexpand(true)
            .build();
        let compose_page = ComposePage::new(
            &inner,
            &page_stack,
            Rc::clone(&state),
            coordinator.clone(),
            &toast_overlay,
        );
        let title_stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .hhomogeneous(false)
            .vhomogeneous(false)
            .build();
        title_stack.add_named(&title, Some("mailbox"));
        title_stack.add_named(compose_page.title(), Some("compose"));
        header.set_title_widget(Some(&title_stack));

        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text(gettext("Search current account"))
            .halign(gtk::Align::Center)
            .width_chars(30)
            .max_width_chars(40)
            .build();
        let search_bar = gtk::SearchBar::new();
        search_bar.set_child(Some(&search_entry));
        search_bar.connect_entry(&search_entry);
        let search_button = gtk::Button::builder()
            .icon_name("system-search-symbolic")
            .tooltip_text(gettext("Search current account"))
            .action_name("win.search")
            .build();

        let preview_widgets = PreviewWidgets::new(&inner, &toast_overlay);
        let state_for_preview_mode = Rc::clone(&state);
        preview_widgets
            .mode_stack
            .connect_visible_child_name_notify(move |stack| {
                state_for_preview_mode
                    .borrow_mut()
                    .set_prefer_html_view(stack.visible_child_name().as_deref() == Some("html"));
            });
        let thread_panel =
            build_thread_panel(&state, preview_widgets.clone(), coordinator.clone());
        let sidebar = build_sidebar(
            &state,
            thread_panel.clone(),
            coordinator.clone(),
            search_entry.clone(),
            search_bar.clone(),
        );

        bind_preview_actions(
            &state,
            &preview_widgets,
            compose_page.clone(),
            coordinator.clone(),
        );
        let navigation =
            MailboxNavigation::new(&sidebar, &thread_panel, &preview_widgets.container);
        let navigation_for_folder = navigation.clone();
        sidebar
            .folder_list
            .connect_row_activated(move |_, _| navigation_for_folder.show_threads());
        let navigation_for_thread = navigation.clone();
        thread_panel
            .thread_list
            .connect_row_activated(move |_, _| navigation_for_thread.show_preview());

        let search_entry_for_action = search_entry.clone();
        let search_bar_for_action = search_bar.clone();
        let thread_list_for_search_action = thread_panel.thread_list.clone();
        let page_stack_for_search_action = page_stack.clone();
        let search_action = gio::SimpleAction::new("search", None);
        search_action.connect_activate(move |_, _| {
            if page_stack_for_search_action
                .visible_child_name()
                .as_deref()
                == Some("mailbox")
                && search_entry_for_action.is_sensitive()
            {
                if search_bar_for_action.is_search_mode() {
                    search_entry_for_action.set_text("");
                    search_bar_for_action.set_search_mode(false);
                    thread_list_for_search_action.grab_focus();
                } else {
                    search_bar_for_action.set_search_mode(true);
                    search_entry_for_action.grab_focus();
                }
            }
        });
        inner.add_action(&search_action);
        let state_for_compose = Rc::clone(&state);
        let compose_page_for_action = compose_page.clone();
        let compose_action = gio::SimpleAction::new(crate::config::ACTION_COMPOSE, None);
        compose_action.connect_activate(move |_, _| {
            let model = {
                let mailbox = state_for_compose.borrow();
                ComposeViewModel::for_action(&mailbox, ComposeKind::New)
            };
            if let Some(model) = model {
                compose_page_for_action.request_open(model);
            }
        });
        inner.add_action(&compose_action);
        let mailbox_controls = MailboxControls {
            compose_action,
            refresh_button,
            search_entry,
            search_button,
            search_bar,
        };
        let account_ready = {
            let state = state.borrow();
            state.account_ready()
        };
        mailbox_controls.update(account_ready);
        let account_dropdown = build_account_dropdown(
            &state,
            coordinator.clone(),
            sidebar.clone(),
            thread_panel.clone(),
            title.clone(),
            compose_page.clone(),
            mailbox_controls.clone(),
            navigation.clone(),
        );
        account_dropdown.set_sensitive(account_ready);
        let compact_compose_button = gtk::Button::builder()
            .icon_name("mail-message-new-symbolic")
            .tooltip_text(gettext("Compose"))
            .action_name("win.compose")
            .visible(false)
            .build();
        header.pack_start(compose_page.back_button());
        header.pack_start(&navigation.back_button);
        header.pack_start(&account_dropdown);
        header.pack_start(&compose_button);
        header.pack_start(&compact_compose_button);
        header.pack_end(&mailbox_controls.search_button);
        header.pack_end(compose_page.send_button());
        header.pack_end(compose_page.save_button());

        page_stack.add_named(&navigation.reader_split, Some("mailbox"));
        page_stack.add_named(compose_page.root(), Some("compose"));
        page_stack.set_visible_child_name("mailbox");

        compose_page.back_button().set_visible(false);
        navigation.back_button.set_visible(false);
        compose_page.save_button().set_visible(false);
        compose_page.send_button().set_visible(false);
        let refresh_button_for_page = mailbox_controls.refresh_button.clone();
        let search_bar_for_page = mailbox_controls.search_bar.clone();
        let back_button_for_page = compose_page.back_button().clone();
        let save_button_for_page = compose_page.save_button().clone();
        let send_button_for_page = compose_page.send_button().clone();
        let title_stack_for_page = title_stack.clone();
        page_stack.connect_visible_child_name_notify(move |stack| {
            let composing = stack.visible_child_name().as_deref() == Some("compose");
            refresh_button_for_page.set_visible(!composing);
            if composing {
                search_bar_for_page.set_search_mode(false);
            }
            back_button_for_page.set_visible(composing);
            save_button_for_page.set_visible(composing);
            send_button_for_page.set_visible(composing);
            title_stack_for_page.set_visible_child_name(if composing {
                "compose"
            } else {
                "mailbox"
            });
        });

        install_close_guard(&inner, &coordinator, &compose_page);

        let compose_page_for_back = compose_page.clone();
        let page_stack_for_back = page_stack.clone();
        let navigation_for_back = navigation.clone();
        let back_action = gio::SimpleAction::new("back", None);
        back_action.connect_activate(move |_, _| {
            if page_stack_for_back.visible_child_name().as_deref() == Some("compose") {
                compose_page_for_back.request_back();
            } else {
                navigation_for_back.navigate_back();
            }
        });
        inner.add_action(&back_action);
        navigation.back_button.set_action_name(Some("win.back"));

        install_adaptive_mailbox_layout(
            &inner,
            &page_stack,
            &navigation,
            &compose_button,
            &compact_compose_button,
            &mailbox_controls.search_button,
            &title_stack,
        );

        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header);
        toolbar_view.add_top_bar(&mailbox_controls.search_bar);
        toolbar_view.set_content(Some(&page_stack));
        toast_overlay.set_child(Some(&toolbar_view));

        inner.set_content(Some(&toast_overlay));

        let parent_for_preferences = inner.clone();
        let state_for_preferences = Rc::clone(&state);
        let coordinator_for_preferences = coordinator.clone();
        let settings_dialog = Rc::new(RefCell::new(None::<SettingsDialog>));
        let settings_for_action = Rc::clone(&settings_dialog);
        let preferences_action = gio::SimpleAction::new("preferences", None);
        preferences_action.connect_activate(move |_, _| {
            let existing_window = settings_for_action.borrow().clone();
            if let Some(settings_window) = existing_window {
                settings_window.present();
                return;
            }
            let settings_window = build_settings(
                &parent_for_preferences,
                Rc::clone(&state_for_preferences),
                coordinator_for_preferences.clone(),
            );
            let settings_for_close = Rc::clone(&settings_for_action);
            settings_window.connect_close(move || {
                settings_for_close.borrow_mut().take();
            });
            settings_window.present();
            *settings_for_action.borrow_mut() = Some(settings_window);
        });
        inner.add_action(&preferences_action);
        preferences_action.set_enabled(account_dropdown.is_sensitive());
        let preferences_for_account_state = preferences_action.clone();
        account_dropdown.connect_sensitive_notify(move |dropdown| {
            preferences_for_account_state.set_enabled(dropdown.is_sensitive());
        });

        let parent_for_about = inner.clone();
        let about_action = gio::SimpleAction::new("about", None);
        about_action.connect_activate(move |_, _| {
            super::about::present(&parent_for_about);
        });
        inner.add_action(&about_action);

        let state_for_search = Rc::clone(&state);
        let thread_for_search = thread_panel.clone();
        let coordinator_for_search = coordinator.clone();
        let navigation_for_search = navigation.clone();
        mailbox_controls
            .search_entry
            .connect_search_changed(move |entry| {
                let account_id = state_for_search.borrow().current_account_id();
                if let Some(account_id) = account_id {
                    coordinator_for_search.cancel_message_detail_requests(&account_id);
                }
                let transition = state_for_search.borrow_mut().search(entry.text().as_str());
                match transition {
                    state::SearchTransition::Requested(request) => {
                        navigation_for_search.show_threads();
                        coordinator_for_search.request_search(
                            request.request_id,
                            request.account_id,
                            request.query,
                        );
                    }
                    state::SearchTransition::Cleared => {
                        let account_id = state_for_search.borrow().current_account_id();
                        if let Some(account_id) = account_id {
                            coordinator_for_search.cancel_pending_search(&account_id);
                            let reload = state_for_search
                                .borrow_mut()
                                .begin_cache_change(&account_id);
                            if let Some(reload) = reload {
                                request_view_reload(&coordinator_for_search, reload);
                            }
                        }
                    }
                    state::SearchTransition::Unchanged => {}
                }
                rebuild_thread_panel(&state_for_search, &thread_for_search);
            });
        let thread_list_for_stop_search = thread_panel.thread_list.clone();
        let search_bar_for_stop_search = mailbox_controls.search_bar.clone();
        mailbox_controls
            .search_entry
            .connect_stop_search(move |entry| {
                entry.set_text("");
                search_bar_for_stop_search.set_search_mode(false);
                thread_list_for_stop_search.grab_focus();
            });

        let coordinator_for_button = coordinator.clone();
        mailbox_controls.refresh_button.connect_clicked(move |_| {
            coordinator_for_button.request_foreground_refresh();
        });

        let state_for_refresh_events = Rc::clone(&state);
        let sidebar_for_refresh_events = sidebar.clone();
        let thread_for_refresh_events = thread_panel.clone();
        let coordinator_for_events = coordinator.clone();
        let parent_for_events = inner.clone();
        let toast_for_events = toast_overlay.clone();
        let title_for_events = title.clone();
        let account_dropdown_for_events = account_dropdown.clone();
        let mailbox_controls_for_events = mailbox_controls.clone();
        let compose_page_for_events = compose_page.clone();
        let pending_work_for_events = Rc::clone(&pending_work_toast);
        let mailto_queue_for_events = Rc::clone(&mailto_queue);
        let deferred_folder_for_events = Rc::clone(&deferred_folder);
        let settings_for_events = Rc::clone(&settings_dialog);
        let mut mail_events = coordinator_for_events.take_event_stream();
        glib::spawn_future_local(async move {
            while let Some(event) = mail_events.next().await {
                match event {
                    MailEvent::AccountActivated {
                        request_id,
                        load,
                    } => {
                        let activation_applied = state_for_refresh_events
                            .borrow_mut()
                            .finish_account_activation(request_id, load)
                            .is_some();
                        if activation_applied {
                            let (account_ready, status_summary) = {
                                let state = state_for_refresh_events.borrow();
                                (
                                    state.account_ready(),
                                    state.status_summary(),
                                )
                            };
                            account_dropdown_for_events.set_sensitive(account_ready);
                            mailbox_controls_for_events.update(account_ready);
                            compose_page_for_events.mailbox_changed();
                            title_for_events.set_subtitle(&status_summary);
                            rebuild_sidebar(&state_for_refresh_events, &sidebar_for_refresh_events);
                            rebuild_thread_panel(
                                &state_for_refresh_events,
                                &thread_for_refresh_events,
                            );
                            apply_deferred_folder(
                                &deferred_folder_for_events,
                                &state_for_refresh_events,
                                &account_dropdown_for_events,
                                &sidebar_for_refresh_events,
                            );
                            drain_mailto_queue(
                                &mailto_queue_for_events,
                                &state_for_refresh_events,
                                &compose_page_for_events,
                                &toast_for_events,
                            );
                        }
                    }
                    MailEvent::AccountRefreshCompleted {
                        account_id,
                        failure,
                    } => {
                        let reload = state_for_refresh_events
                            .borrow_mut()
                            .finish_account_refresh(&account_id, failure);
                        if state_for_refresh_events
                            .borrow()
                            .current_account_id()
                            .as_ref()
                            == Some(&account_id)
                        {
                            let status_summary =
                                state_for_refresh_events.borrow().status_summary();
                            title_for_events.set_subtitle(&status_summary);
                        }
                        if let Some(reload) = reload {
                            request_view_reload(&coordinator_for_events, reload);
                        }
                    }
                    MailEvent::MailboxReloaded {
                        request_id,
                        result,
                    } => {
                        let outcome = state_for_refresh_events
                            .borrow_mut()
                            .finish_mailbox_reload(request_id, result);
                        match outcome {
                            Some(state::MailboxReloadOutcome::Applied) => {
                                rebuild_sidebar(
                                    &state_for_refresh_events,
                                    &sidebar_for_refresh_events,
                                );
                                rebuild_thread_panel(
                                    &state_for_refresh_events,
                                    &thread_for_refresh_events,
                                );
                                apply_deferred_folder(
                                    &deferred_folder_for_events,
                                    &state_for_refresh_events,
                                    &account_dropdown_for_events,
                                    &sidebar_for_refresh_events,
                                );
                            }
                            Some(state::MailboxReloadOutcome::Failed(error)) => {
                                toast_for_events.add_toast(adw::Toast::new(&error));
                            }
                            None => {}
                        }
                    }
                    MailEvent::NewMailAvailable {
                        account_id,
                        folder_id,
                        folder_name,
                        count,
                    } => {
                        if !notification_belongs_to_current_account(
                            state_for_refresh_events
                                .borrow()
                                .current_account_id()
                                .as_ref(),
                            &account_id,
                        ) {
                            continue;
                        }
                        let account_name = state_for_refresh_events
                            .borrow()
                            .account_display_name(&account_id)
                            .map(str::to_owned)
                            .unwrap_or_else(|| gettext("mail account"));
                        let notification = gio::Notification::new(&gettext("New mail"));
                        notification.set_body(Some(&new_mail_notification_body(
                            count,
                            &folder_name,
                            &account_name,
                        )));
                        let notification_target =
                            (account_id.0.as_str(), folder_id.0.as_str()).to_variant();
                        notification.set_default_action_and_target_value(
                            &format!("app.{}", crate::config::ACTION_SHOW_FOLDER),
                            Some(&notification_target),
                        );
                        if let Some(application) = parent_for_events.application() {
                            application.send_notification(
                                Some(&format!("new-mail-{}-{}", account_id.0, folder_id.0)),
                                &notification,
                            );
                        }
                    }
                    MailEvent::MailboxCacheChanged { account_id } => {
                        let reload = state_for_refresh_events
                            .borrow_mut()
                            .begin_cache_change(&account_id);
                        if let Some(reload) = reload {
                            request_view_reload(&coordinator_for_events, reload);
                        }
                    }
                    MailEvent::ThreadPageLoaded {
                        request_id,
                        result,
                    } => {
                        let error = result.as_ref().err().cloned();
                        let outcome = state_for_refresh_events
                            .borrow_mut()
                            .finish_thread_page_load(request_id, result);
                        if let Some(outcome) = outcome {
                            match outcome {
                                state::ThreadPageOutcome::Initial => rebuild_thread_panel(
                                    &state_for_refresh_events,
                                    &thread_for_refresh_events,
                                ),
                                state::ThreadPageOutcome::Additional(added) => {
                                    for thread in &added {
                                        append_thread_row(
                                            &thread_for_refresh_events.thread_list,
                                            thread,
                                        );
                                    }
                                }
                            }
                            if let Some(error) = error {
                                toast_for_events.add_toast(adw::Toast::new(&error));
                            }
                        }
                    }
                    MailEvent::SearchCompleted {
                        request_id,
                        result,
                    } => {
                        let applied = state_for_refresh_events
                            .borrow_mut()
                            .finish_search(request_id, result);
                        if applied {
                            rebuild_thread_panel(
                                &state_for_refresh_events,
                                &thread_for_refresh_events,
                            );
                        }
                    }
                    MailEvent::MessageDetailLoaded {
                        request_id,
                        result,
                    } => {
                        let mut state = state_for_refresh_events.borrow_mut();
                        let applied = state.finish_message_detail_load(request_id, result);
                        drop(state);
                        if applied {
                            refresh_preview(
                                &state_for_refresh_events,
                                &thread_for_refresh_events.preview_widgets,
                            );
                        }
                    }
                    MailEvent::MessageActionCompleted {
                        request_id,
                        result,
                    } => {
                        let outcome = state_for_refresh_events
                            .borrow_mut()
                            .finish_message_action(request_id, result);
                        match outcome {
                            Some(state::MessageActionOutcome::Failed(error)) => {
                                refresh_preview_actions(
                                    &state_for_refresh_events,
                                    &thread_for_refresh_events.preview_widgets,
                                );
                                toast_for_events.add_toast(adw::Toast::new(&error));
                            }
                            Some(outcome) => {
                                if let state::MessageActionOutcome::SelectionChanged(request) =
                                    outcome
                                {
                                    coordinator_for_events.request_message_detail(
                                        request.request_id,
                                        request.account_id,
                                        request.conversation_id,
                                    );
                                }
                                rebuild_sidebar(
                                    &state_for_refresh_events,
                                    &sidebar_for_refresh_events,
                                );
                                rebuild_thread_panel(
                                    &state_for_refresh_events,
                                    &thread_for_refresh_events,
                                );
                            }
                            None => {}
                        }
                    }
                    MailEvent::AccountIdentitiesSaveCompleted {
                        request_id,
                        result,
                    } => {
                        let settings_dialog = settings_for_events.borrow().clone();
                        let handled = settings_dialog.is_some_and(|dialog| {
                            dialog.finish_save(request_id, result.clone())
                        });
                        if !handled && let Err(message) = result {
                            toast_for_events.add_toast(adw::Toast::new(&message));
                        }
                    }
                    MailEvent::PendingWorkChanged => {
                        let count = coordinator_for_events.pending_work_count();
                        let current_toast = pending_work_for_events.borrow().clone();
                        if count == 0 {
                            let toast = pending_work_for_events.borrow_mut().take();
                            if let Some(toast) = toast {
                                toast.dismiss();
                            }
                        } else if let Some(toast) = current_toast {
                            toast.set_title(&pending_tasks_label(count));
                        } else {
                            let toast = adw::Toast::builder()
                                .title(pending_tasks_label(count))
                                .build();
                            let pending = Rc::downgrade(&pending_work_for_events);
                            toast.connect_dismissed(move |dismissed| {
                                let Some(pending) = pending.upgrade() else {
                                    return;
                                };
                                let mut current = pending.borrow_mut();
                                if current
                                    .as_ref()
                                    .is_some_and(|current| current == dismissed)
                                {
                                    *current = None;
                                }
                            });
                            toast_for_events.add_toast(toast.clone());
                            *pending_work_for_events.borrow_mut() = Some(toast);
                        }
                    }
                    MailEvent::DraftSaveCompleted { result } => {
                        compose_page_for_events.finish_save(result);
                    }
                    MailEvent::SendCompleted { result } => {
                        compose_page_for_events.finish_send(result);
                    }
                    MailEvent::AttachmentPrepared {
                        operation,
                        display_name,
                        result,
                    } => handle_prepared_attachment(
                        &parent_for_events,
                        &toast_for_events,
                        operation,
                        display_name,
                        result,
                    ),
                }
            }
        });

        Self {
            inner,
            mailbox: state,
            title,
            account_dropdown,
            sidebar,
            thread_panel,
            navigation,
            coordinator,
            compose_page,
            mailbox_controls,
            toast_overlay,
            mailto_queue,
            deferred_folder,
            settings_dialog,
        }
    }

    pub fn present(&self) {
        self.inner.present();
    }

    pub fn show_folder(
        &self,
        account_id: crate::model::account::MailAccountId,
        folder_id: FolderId,
    ) {
        let deferred = Rc::clone(&self.deferred_folder);
        let mailbox = Rc::clone(&self.mailbox);
        let account_dropdown = self.account_dropdown.clone();
        let sidebar = self.sidebar.clone();
        let navigation = self.navigation.clone();
        self.compose_page.request_mailbox(move || {
            *deferred.borrow_mut() = Some((account_id, folder_id));
            navigation.show_threads();
            apply_deferred_folder(&deferred, &mailbox, &account_dropdown, &sidebar);
        });
    }

    pub fn replace_mailbox(&self, mailbox: MailboxViewModel) {
        let previous_account_id = self.mailbox.borrow().current_account_id();
        if let Some(account_id) = previous_account_id {
            self.coordinator.cancel_transient_reads(&account_id);
        }
        *self.mailbox.borrow_mut() = mailbox;
        let initial_activation = self.mailbox.borrow_mut().begin_initial_activation();
        self.compose_page.mailbox_changed();
        let prefer_html_view = self.mailbox.borrow().prefer_html_view();
        self.thread_panel
            .preview_widgets
            .set_prefer_html_view(prefer_html_view);
        let (status_summary, accounts, selected_account, account_ready) = {
            let state = self.mailbox.borrow();
            (
                state.status_summary(),
                state.accounts().to_vec(),
                state.selected_account_index(),
                state.account_ready(),
            )
        };
        self.title.set_subtitle(&status_summary);
        refresh_account_dropdown_from_accounts(
            &self.account_dropdown,
            &accounts,
            selected_account,
        );
        self.account_dropdown.set_sensitive(account_ready);
        rebuild_sidebar(&self.mailbox, &self.sidebar);
        rebuild_thread_panel(&self.mailbox, &self.thread_panel);
        let settings_dialog = self.settings_dialog.borrow().clone();
        if let Some(dialog) = settings_dialog {
            dialog.mailbox_changed();
        }
        apply_deferred_folder(
            &self.deferred_folder,
            &self.mailbox,
            &self.account_dropdown,
            &self.sidebar,
        );
        self.mailbox_controls.update(account_ready);
        self.present_mailto_queue();
        if let Some(activation) = initial_activation {
            self.coordinator.request_account_activation(
                activation.request_id,
                activation.account_id,
                activation.conversation_limit,
            );
        } else {
            let account_id = self.mailbox.borrow().current_account_id();
            if let Some(account_id) = account_id {
                self.coordinator.select_foreground_account(account_id);
            }
            self.coordinator.request_foreground_refresh();
        }
    }

    pub fn update_account_catalog(
        &self,
        accounts: Vec<crate::model::account::MailAccount>,
    ) {
        self.mailbox.borrow_mut().update_registry_catalog(accounts);
        self.compose_page.mailbox_changed();
        let settings_dialog = self.settings_dialog.borrow().clone();
        if let Some(dialog) = settings_dialog {
            dialog.mailbox_changed();
        }
        let (accounts, selected_account, account_ready) = {
            let state = self.mailbox.borrow();
            (
                state.accounts().to_vec(),
                state.selected_account_index(),
                state.account_ready(),
            )
        };
        refresh_account_dropdown_from_accounts(
            &self.account_dropdown,
            &accounts,
            selected_account,
        );
        self.account_dropdown.set_sensitive(account_ready);
        self.mailbox_controls.update(account_ready);
        apply_deferred_folder(
            &self.deferred_folder,
            &self.mailbox,
            &self.account_dropdown,
            &self.sidebar,
        );
    }

    pub(crate) fn session_snapshot(&self) -> MailboxSessionSnapshot {
        let mailbox = self.mailbox.borrow();
        MailboxSessionSnapshot {
            settings: mailbox.settings(),
            current_account_id: mailbox.current_account_id(),
            accounts: mailbox.accounts().to_vec(),
        }
    }

    pub(crate) fn settings_for_persistence(&self) -> Option<AppSettings> {
        let mailbox = self.mailbox.borrow();
        (!mailbox.is_bootstrap_placeholder()).then(|| mailbox.settings())
    }

    pub fn open_mailto(&self, request: MailtoRequest) {
        if self.mailbox.borrow().is_loading() {
            self.mailto_queue.borrow_mut().push(request);
            return;
        }
        present_mailto_request(
            &self.mailbox,
            &self.compose_page,
            &self.toast_overlay,
            request,
        );
    }

    fn present_mailto_queue(&self) {
        drain_mailto_queue(
            &self.mailto_queue,
            &self.mailbox,
            &self.compose_page,
            &self.toast_overlay,
        );
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CloseState {
    Idle,
    AwaitingPendingDecision,
    PendingWarningAccepted,
    AwaitingComposeDecision,
    Allowed,
}

fn install_close_guard(
    window: &adw::ApplicationWindow,
    coordinator: &MailCoordinator,
    compose_page: &ComposePage,
) {
    let close_state = Rc::new(Cell::new(CloseState::Idle));
    let state_for_close = Rc::clone(&close_state);
    let coordinator = coordinator.clone();
    let compose_page = compose_page.clone();
    let parent_window = window.clone();
    window.connect_close_request(move |_| {
        let state = state_for_close.get();
        if state == CloseState::Allowed {
            return glib::Propagation::Proceed;
        }
        if matches!(
            state,
            CloseState::AwaitingPendingDecision | CloseState::AwaitingComposeDecision
        ) {
            return glib::Propagation::Stop;
        }

        let work_count = coordinator.pending_work_count()
            + usize::from(compose_page.has_unaccepted_write());
        if work_count > 0 && state != CloseState::PendingWarningAccepted {
            state_for_close.set(CloseState::AwaitingPendingDecision);
            let dialog = adw::AlertDialog::builder()
                .heading(gettext("Tasks still pending"))
                .body(
                    ngettext_f(
                        "Closing now will discard {count} pending task.",
                        "Closing now will discard {count} pending tasks.",
                        work_count as u32,
                        &[("count", &work_count.to_string())],
                    )
                )
                .build();
            dialog.add_response("cancel", &gettext("Cancel"));
            dialog.add_response("close", &gettext("Close"));
            dialog.set_response_appearance("close", adw::ResponseAppearance::Destructive);
            dialog.set_default_response(Some("cancel"));
            dialog.set_close_response("cancel");
            let close_state = Rc::clone(&state_for_close);
            let parent = parent_window.clone();
            dialog.choose(
                Some(&parent_window),
                None::<&gio::Cancellable>,
                move |response| {
                    if response == "close" {
                        close_state.set(CloseState::PendingWarningAccepted);
                        parent.close();
                    } else {
                        close_state.set(CloseState::Idle);
                    }
                },
            );
            return glib::Propagation::Stop;
        }
        state_for_close.set(CloseState::AwaitingComposeDecision);
        let state_after_discard = Rc::clone(&state_for_close);
        let parent = parent_window.clone();
        let state_after_cancel = Rc::clone(&state_for_close);
        compose_page.request_close(
            move || {
                state_after_discard.set(CloseState::Allowed);
                glib::idle_add_local_once(move || parent.close());
            },
            move || state_after_cancel.set(CloseState::Idle),
        );
        glib::Propagation::Stop
    });
}

fn install_adaptive_mailbox_layout(
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

fn drain_mailto_queue(
    queue: &Rc<RefCell<Vec<MailtoRequest>>>,
    mailbox: &Rc<RefCell<MailboxViewModel>>,
    compose_page: &ComposePage,
    toast_overlay: &adw::ToastOverlay,
) {
    if mailbox.borrow().is_loading() {
        return;
    }
    let requests = std::mem::take(&mut *queue.borrow_mut());
    for request in requests {
        present_mailto_request(mailbox, compose_page, toast_overlay, request);
    }
}

fn present_mailto_request(
    mailbox: &Rc<RefCell<MailboxViewModel>>,
    compose_page: &ComposePage,
    toast_overlay: &adw::ToastOverlay,
    request: MailtoRequest,
) {
    let model = {
        let mailbox = mailbox.borrow();
        ComposeViewModel::for_mailto(&mailbox, &request)
    };
    let Some(model) = model else {
        toast_overlay.add_toast(adw::Toast::new(&gettext("Mail link unavailable.")));
        tracing::warn!("mailto request could not be opened without a sending identity");
        return;
    };
    compose_page.request_open(model);
}

fn notification_belongs_to_current_account(
    current_account_id: Option<&crate::model::account::MailAccountId>,
    event_account_id: &crate::model::account::MailAccountId,
) -> bool {
    current_account_id == Some(event_account_id)
}

fn new_mail_notification_body(count: usize, folder_name: &str, account_name: &str) -> String {
    ngettext_f(
        "{count} new conversation in {folder} — {account}",
        "{count} new conversations in {folder} — {account}",
        count as u32,
        &[
            ("count", &count.to_string()),
            ("folder", folder_name),
            ("account", account_name),
        ],
    )
}

fn pending_tasks_label(count: usize) -> String {
    ngettext_f(
        "{count} task pending",
        "{count} tasks pending",
        count as u32,
        &[("count", &count.to_string())],
    )
}

fn dispatch_message_action(
    coordinator: &MailCoordinator,
    request: Option<state::MessageActionRequest>,
) {
    if let Some(request) = request {
        coordinator.request_message_action(
            request.request_id,
            request.account_id,
            request.conversation_id,
            request.action,
        );
    }
}

fn request_mailbox_reload(coordinator: &MailCoordinator, request: state::MailboxReloadRequest) {
    coordinator.request_mailbox_reload(
        request.request_id,
        request.account_id,
        request.selected_folder_id,
        request.conversation_limit,
    );
}

fn request_view_reload(coordinator: &MailCoordinator, request: state::ViewReloadRequest) {
    match request {
        state::ViewReloadRequest::Mailbox(request) => {
            request_mailbox_reload(coordinator, request)
        }
        state::ViewReloadRequest::Search(request) => coordinator.request_search(
            request.request_id,
            request.account_id,
            request.query,
        ),
    }
}

fn build_account_dropdown(
    state: &Rc<RefCell<MailboxViewModel>>,
    coordinator: MailCoordinator,
    sidebar: SidebarWidgets,
    thread_panel: ThreadPanelWidgets,
    title: adw::WindowTitle,
    compose_page: ComposePage,
    controls: MailboxControls,
    navigation: MailboxNavigation,
) -> gtk::DropDown {
    let dropdown = gtk::DropDown::new(None::<gtk::StringList>, None::<gtk::Expression>);
    let (accounts, selected_account) = {
        let state = state.borrow();
        (state.accounts().to_vec(), state.selected_account_index())
    };
    refresh_account_dropdown_from_accounts(&dropdown, &accounts, selected_account);
    let state_for_change = Rc::clone(state);
    dropdown.connect_selected_notify(move |dropdown| {
        let selected = dropdown.selected() as usize;
        if selected == state_for_change.borrow().selected_account_index() {
            return;
        }
        let previous_account_id = state_for_change.borrow().current_account_id();
        if let Some(account_id) = previous_account_id {
            coordinator.cancel_transient_reads(&account_id);
        }
        let Some(activation) = state_for_change
            .borrow_mut()
            .begin_account_activation(selected)
        else {
            return;
        };
        dropdown.set_sensitive(false);
        controls.update(false);
        compose_page.account_activation_started();
        controls.search_entry.set_text("");
        navigation.show_folders();
        let status_summary = state_for_change.borrow().status_summary();
        title.set_subtitle(&status_summary);
        rebuild_sidebar(&state_for_change, &sidebar);
        rebuild_thread_panel(&state_for_change, &thread_panel);
        coordinator.request_account_activation(
            activation.request_id,
            activation.account_id,
            activation.conversation_limit,
        );
    });
    dropdown
}

fn refresh_account_dropdown_from_accounts(
    dropdown: &gtk::DropDown,
    accounts: &[crate::model::account::MailAccount],
    selected_account: usize,
) {
    let labels: Vec<String> = accounts
        .iter()
        .map(|account| account.display_name.clone())
        .collect();
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let model = gtk::StringList::new(&refs);
    let notifications = dropdown.freeze_notify();
    dropdown.set_model(Some(&model));
    dropdown.set_selected(selected_account as u32);
    drop(notifications);
}

fn apply_deferred_folder(
    deferred: &Rc<RefCell<Option<(crate::model::account::MailAccountId, FolderId)>>>,
    state: &Rc<RefCell<MailboxViewModel>>,
    account_dropdown: &gtk::DropDown,
    sidebar: &SidebarWidgets,
) {
    let Some((account_id, folder_id)) = deferred.borrow().clone() else {
        return;
    };

    let (current_account_id, account_index, loading) = {
        let mailbox = state.borrow();
        (
            mailbox.current_account_id(),
            mailbox.account_index(&account_id),
            mailbox.is_loading(),
        )
    };
    if current_account_id.as_ref() != Some(&account_id) {
        if account_dropdown.is_sensitive()
            && let Some(index) = account_index
        {
            account_dropdown.set_selected(index as u32);
        } else if !loading && account_index.is_none() {
            deferred.borrow_mut().take();
        }
        return;
    }
    if loading {
        return;
    }

    let folder_index = state.borrow().folder_index(&folder_id);
    if let Some(row) = folder_index.and_then(|index| sidebar.folder_list.row_at_index(index as i32))
    {
        deferred.borrow_mut().take();
        sidebar.folder_list.select_row(Some(&row));
    }
}

fn build_sidebar(
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
    let loading_spinner = gtk::Spinner::new();
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
        loading_spinner,
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

fn build_thread_panel(
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
    let loading_spinner = gtk::Spinner::new();
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
        loading_spinner,
        loading_title,
        loading_description,
        empty_page,
        error_page,
        preview_widgets: detail_widgets,
    };
    rebuild_thread_panel(state, &widgets);
    widgets
}

fn rebuild_sidebar(state: &Rc<RefCell<MailboxViewModel>>, widgets: &SidebarWidgets) {
    let content = state.borrow().sidebar_content();
    widgets.account_title.set_label(&gettext("Folders"));

    match content {
        state::SidebarContent::Loading => {
            widgets.loading_spinner.start();
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

            widgets.loading_spinner.stop();
            widgets.content_stack.set_visible_child_name("folders");
        }
    }
}

fn rebuild_thread_panel(state: &Rc<RefCell<MailboxViewModel>>, widgets: &ThreadPanelWidgets) {
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
        state::ThreadListContent::SearchFailed(error) => widgets.show_error(error),
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
            if let Some(index) = selected.as_ref().and_then(|selected_id| {
                (0..threads.len()).find(|&index| {
                    widgets
                        .thread_list
                        .row_at_index(index as i32)
                        .is_some_and(|row| row.widget_name().as_str() == selected_id.0)
                })
            }) {
                widgets
                    .thread_list
                    .select_row(widgets.thread_list.row_at_index(index as i32).as_ref());
            }
        }
    }

    refresh_preview(state, &widgets.preview_widgets);
}

fn refresh_preview(state: &Rc<RefCell<MailboxViewModel>>, widgets: &PreviewWidgets) {
    let (content, actions) = {
        let mailbox = state.borrow();
        (mailbox.preview_content(), mailbox.preview_actions())
    };
    widgets.refresh_content(&content);
    widgets.sync_action_state(&actions);
}

fn refresh_preview_actions(state: &Rc<RefCell<MailboxViewModel>>, widgets: &PreviewWidgets) {
    let actions = state.borrow().preview_actions();
    widgets.sync_action_state(&actions);
}

fn append_thread_row(thread_list: &gtk::ListBox, thread: &crate::model::mail::ConversationSummary) {
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
        FolderKind::Inbox => "mail-inbox-symbolic",
        FolderKind::Drafts => "document-edit-symbolic",
        FolderKind::Outbox => "mail-outbox-symbolic",
        FolderKind::Sent => "mail-send-symbolic",
        FolderKind::Archive => "mail-archive-symbolic",
        FolderKind::Trash => "user-trash-symbolic",
        FolderKind::Spam => "mail-mark-junk-symbolic",
        FolderKind::Custom => "folder-symbolic",
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

fn bind_preview_actions(
    state: &Rc<RefCell<MailboxViewModel>>,
    preview: &PreviewWidgets,
    compose_page: ComposePage,
    coordinator: MailCoordinator,
) {
    let state_for_reply = Rc::clone(state);
    let compose_page_for_reply = compose_page.clone();
    preview.reply_button.connect_clicked(move |_| {
        let model = {
            let mailbox = state_for_reply.borrow();
            let action = preview_primary_compose_action(&mailbox);
            ComposeViewModel::for_action(&mailbox, action)
        };
        if let Some(model) = model {
            compose_page_for_reply.request_open(model);
        }
    });

    let state_for_reply_all = Rc::clone(state);
    let compose_page_for_reply_all = compose_page.clone();
    preview.reply_all_button.connect_clicked(move |_| {
        let model = {
            let mailbox = state_for_reply_all.borrow();
            ComposeViewModel::for_action(&mailbox, ComposeKind::ReplyAll)
        };
        if let Some(model) = model {
            compose_page_for_reply_all.request_open(model);
        }
    });

    let state_for_forward = Rc::clone(state);
    let compose_page_for_forward = compose_page;
    preview.forward_button.connect_clicked(move |_| {
        let model = {
            let mailbox = state_for_forward.borrow();
            ComposeViewModel::for_action(&mailbox, ComposeKind::Forward)
        };
        if let Some(model) = model {
            compose_page_for_forward.request_open(model);
        }
    });

    let state_for_attachment_actions = Rc::clone(state);
    let coordinator_for_attachment_actions = coordinator.clone();
    preview.set_attachment_dispatch({
        let state = Rc::clone(&state_for_attachment_actions);
        move |operation, attachment| {
            let source = state.borrow().current_attachment_source();
            coordinator_for_attachment_actions.request_attachment(
                source,
                operation,
                attachment,
            );
        }
    });

    let state_for_star = Rc::clone(state);
    let preview_for_star = preview.clone();
    let coordinator_for_star = coordinator.clone();
    preview.star_button.connect_clicked(move |_| {
        let request = state_for_star.borrow_mut().begin_toggle_star();
        dispatch_message_action(&coordinator_for_star, request);
        refresh_preview_actions(&state_for_star, &preview_for_star);
    });

    let state_for_read = Rc::clone(state);
    let preview_for_read = preview.clone();
    let coordinator_for_read = coordinator.clone();
    preview.read_button.connect_clicked(move |_| {
        let request = state_for_read.borrow_mut().begin_toggle_read();
        dispatch_message_action(&coordinator_for_read, request);
        refresh_preview_actions(&state_for_read, &preview_for_read);
    });

    let state_for_archive = Rc::clone(state);
    let preview_for_archive = preview.clone();
    let coordinator_for_archive = coordinator.clone();
    preview.archive_button.connect_clicked(move |_| {
        let request = state_for_archive.borrow_mut().begin_archive_selected();
        dispatch_message_action(&coordinator_for_archive, request);
        refresh_preview_actions(&state_for_archive, &preview_for_archive);
    });

    let state_for_trash = Rc::clone(state);
    let preview_for_trash = preview.clone();
    let coordinator_for_trash = coordinator;
    preview.trash_button.connect_clicked(move |_| {
        let request = state_for_trash.borrow_mut().begin_trash_selected();
        dispatch_message_action(&coordinator_for_trash, request);
        refresh_preview_actions(&state_for_trash, &preview_for_trash);
    });

    refresh_preview(state, preview);
}

#[derive(Clone)]
struct SidebarWidgets {
    container: gtk::ScrolledWindow,
    account_title: gtk::Label,
    folder_list: gtk::ListBox,
    content_stack: gtk::Stack,
    loading_spinner: gtk::Spinner,
}

#[derive(Clone)]
struct ThreadPanelWidgets {
    container: gtk::Box,
    heading: gtk::Label,
    thread_list: gtk::ListBox,
    content_stack: gtk::Stack,
    loading_spinner: gtk::Spinner,
    loading_title: gtk::Label,
    loading_description: gtk::Label,
    empty_page: adw::StatusPage,
    error_page: adw::StatusPage,
    preview_widgets: PreviewWidgets,
}

impl ThreadPanelWidgets {
    fn show_loading(&self, title: &str, description: &str) {
        self.loading_title.set_label(title);
        self.loading_description.set_label(description);
        self.loading_spinner.start();
        self.content_stack.set_visible_child_name("loading");
    }

    fn show_messages(&self) {
        self.loading_spinner.stop();
        self.content_stack.set_visible_child_name("messages");
    }

    fn show_empty(&self, title: &str, description: &str) {
        self.loading_spinner.stop();
        self.empty_page.set_title(title);
        self.empty_page.set_description(Some(description));
        self.content_stack.set_visible_child_name("empty");
    }

    fn show_error(&self, description: &str) {
        self.loading_spinner.stop();
        self.error_page.set_description(Some(description));
        self.content_stack.set_visible_child_name("error");
    }
}

fn preview_primary_compose_action(mailbox: &MailboxViewModel) -> ComposeKind {
    match mailbox.current_folder().map(|folder| folder.kind) {
        Some(crate::model::mail::FolderKind::Drafts) => ComposeKind::EditDraft,
        _ => ComposeKind::Reply,
    }
}

fn handle_prepared_attachment(
    parent: &adw::ApplicationWindow,
    toast_overlay: &adw::ToastOverlay,
    operation: AttachmentOperation,
    attachment_name: String,
    result: Result<String, String>,
) {
    let uri = match result {
        Ok(uri) => uri,
        Err(error) => {
            toast_overlay.add_toast(adw::Toast::new(&error));
            return;
        }
    };

    match operation {
        AttachmentOperation::Open => {
            let launcher = gtk::FileLauncher::new(Some(&gio::File::for_uri(&uri)));
            let toast_overlay = toast_overlay.clone();
            launcher.launch(Some(parent), None::<&gio::Cancellable>, move |result| {
                let message = match result {
                    Ok(_) => gettext_f(
                        "Opening {attachment}",
                        &[("attachment", &attachment_name)],
                    ),
                    Err(error) => {
                        crate::logging::report_failure("attachment-open", &error.into());
                        gettext_f(
                            "Not opened: {attachment}",
                            &[("attachment", &attachment_name)],
                        )
                    }
                };
                toast_overlay.add_toast(adw::Toast::new(&message));
            });
        }
        AttachmentOperation::SaveAs => {
            let dialog = gtk::FileDialog::builder()
                .title(gettext("Save attachment"))
                .accept_label(gettext("Save"))
                .initial_name(&attachment_name)
                .build();
            let parent = parent.clone();
            let toast_overlay = toast_overlay.clone();
            dialog.save(Some(&parent), None::<&gio::Cancellable>, move |result| {
                let Ok(target) = result else {
                    return;
                };
                let source = gio::File::for_uri(&uri);
                source.copy_async(
                    &target,
                    gio::FileCopyFlags::OVERWRITE,
                    glib::Priority::DEFAULT,
                    None::<&gio::Cancellable>,
                    None,
                    move |result| {
                        let message = match result {
                            Ok(_) => gettext_f(
                                "Saved {attachment}",
                                &[("attachment", &attachment_name)],
                            ),
                            Err(error) => {
                                crate::logging::report_failure("attachment-save", &error.into());
                                gettext_f(
                                    "Not saved: {attachment}",
                                    &[("attachment", &attachment_name)],
                                )
                            }
                        };
                        toast_overlay.add_toast(adw::Toast::new(&message));
                    },
                );
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        folder_icon_name, notification_belongs_to_current_account, thread_subject_label,
    };
    use crate::model::account::MailAccountId;
    use crate::model::mail::{ConversationId, ConversationSummary, FolderId, FolderKind};

    fn thread(subject: &str, message_count: u32) -> ConversationSummary {
        ConversationSummary {
            id: ConversationId("conversation-1".into()),
            folder_id: FolderId("inbox".into()),
            subject: subject.into(),
            participants: vec!["Sender <sender@example.test>".into()],
            message_count,
            unread_count: 0,
            attachment_count: 0,
            starred: false,
            last_updated_unix_ms: 0,
            preview: "Preview".into(),
        }
    }

    #[test]
    fn thread_subject_labels_handle_empty_single_and_grouped_conversations() {
        assert!(!thread_subject_label(&thread("", 1)).is_empty());
        assert_eq!(thread_subject_label(&thread("  Subject  ", 1)), "Subject");
        let grouped = thread_subject_label(&thread("Subject", 4));
        assert!(grouped.contains("Subject"));
        assert!(grouped.contains('4'));
        assert!(thread_subject_label(&thread("   ", 2)).contains('2'));
    }

    #[test]
    fn every_folder_kind_has_a_semantic_symbolic_icon() {
        let icons = [
            (FolderKind::Inbox, "mail-inbox-symbolic"),
            (FolderKind::Drafts, "document-edit-symbolic"),
            (FolderKind::Outbox, "mail-outbox-symbolic"),
            (FolderKind::Sent, "mail-send-symbolic"),
            (FolderKind::Archive, "mail-archive-symbolic"),
            (FolderKind::Trash, "user-trash-symbolic"),
            (FolderKind::Spam, "mail-mark-junk-symbolic"),
            (FolderKind::Custom, "folder-symbolic"),
        ];

        for (kind, expected) in icons {
            assert_eq!(folder_icon_name(kind), expected);
        }
    }

    #[test]
    fn notifications_are_limited_to_the_current_account() {
        let current = MailAccountId("current-account".into());
        let other = MailAccountId("other-account".into());

        assert!(notification_belongs_to_current_account(
            Some(&current),
            &current,
        ));
        assert!(!notification_belongs_to_current_account(
            Some(&current),
            &other,
        ));
        assert!(!notification_belongs_to_current_account(None, &current));
    }

}
