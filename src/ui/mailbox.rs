use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

mod events;
mod layout;
mod panels;
mod preview;
mod state;

pub(crate) use state::MailboxViewModel;
use layout::{MailboxNavigation, install_adaptive_mailbox_layout};
use panels::{
    SidebarWidgets, ThreadPanelWidgets, build_sidebar, build_thread_panel, rebuild_sidebar,
    rebuild_thread_panel, refresh_preview, refresh_preview_actions,
};
use preview::PreviewWidgets;

use crate::core::coordinator::MailCoordinator;
use crate::i18n::{gettext, ngettext_f};
use crate::model::account::MailAccount;
use crate::model::mail::{FolderId, MailtoRequest};
use crate::model::settings::AppSettings;

use super::compose::{ComposeKind, ComposePage, ComposeViewModel};
use super::settings::{SettingsDialog, build_settings};

pub(crate) struct MailboxSessionSnapshot {
    pub settings: AppSettings,
    pub current_account_id: Option<crate::model::account::MailAccountId>,
    pub accounts: Vec<MailAccount>,
}

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
    settings_dialog: Rc<RefCell<Option<Rc<SettingsDialog>>>>,
    pending_work_toast: RefCell<Option<adw::Toast>>,
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

impl MainWindow {
    pub fn new(
        app: &adw::Application,
        mailbox: MailboxViewModel,
        coordinator: MailCoordinator,
    ) -> Rc<Self> {
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

        let parent_for_preferences = inner.downgrade();
        let state_for_preferences = Rc::clone(&state);
        let coordinator_for_preferences = coordinator.clone();
        let settings_dialog = Rc::new(RefCell::new(None::<Rc<SettingsDialog>>));
        let settings_for_action = Rc::clone(&settings_dialog);
        let preferences_action = gio::SimpleAction::new("preferences", None);
        preferences_action.connect_activate(move |_, _| {
            let Some(parent) = parent_for_preferences.upgrade() else {
                return;
            };
            let existing_dialog = settings_for_action.borrow().clone();
            if let Some(dialog) = existing_dialog {
                dialog.present(&parent);
                return;
            }
            let (accounts, selected) = {
                let mailbox = state_for_preferences.borrow();
                (mailbox.accounts().to_vec(), mailbox.selected_account_index())
            };
            let dialog = build_settings(accounts, selected, coordinator_for_preferences.clone());
            let settings_for_close = Rc::downgrade(&settings_for_action);
            dialog.connect_closed(move || {
                if let Some(settings) = settings_for_close.upgrade() {
                    settings.borrow_mut().take();
                }
            });
            *settings_for_action.borrow_mut() = Some(dialog.clone());
            dialog.present(&parent);
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

        let window = Rc::new(Self {
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
            pending_work_toast: RefCell::new(None),
        });
        window.bind_events();
        window
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
        self.update_account_controls(account_ready);
        self.rebuild_lists();
        let settings_dialog = self.settings_dialog.borrow().clone();
        if let Some(dialog) = settings_dialog {
            dialog.update_account_catalog(accounts);
        }
        self.show_deferred_folder();
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
        let settings_dialog = self.settings_dialog.borrow().clone();
        if let Some(dialog) = settings_dialog {
            dialog.update_account_catalog(accounts);
        }
        self.update_account_controls(account_ready);
        self.show_deferred_folder();
    }

    fn update_account_controls(&self, account_ready: bool) {
        self.account_dropdown.set_sensitive(account_ready);
        self.mailbox_controls.update(account_ready);
    }

    fn rebuild_lists(&self) {
        rebuild_sidebar(&self.mailbox, &self.sidebar);
        rebuild_thread_panel(&self.mailbox, &self.thread_panel);
    }

    fn show_deferred_folder(&self) {
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
        state::ViewReloadRequest::Search(request) => {
            coordinator.request_search(request.request_id, request.account_id, request.query);
        }
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

fn preview_primary_compose_action(mailbox: &MailboxViewModel) -> ComposeKind {
    match mailbox.current_folder().map(|folder| folder.kind) {
        Some(crate::model::mail::FolderKind::Drafts) => {
            ComposeKind::EditDraft
        }
        _ => {
            ComposeKind::Reply
        }
    }
}
