use adw::prelude::*;
use gtk::{gio, glib, pango};
use std::cell::RefCell;
use std::rc::Rc;

mod state;

pub(crate) use state::*;

use crate::core::cache::CacheManager;
use crate::integration::webkit::{configure_mail_view, load_html_document, stop_html_loading};
use crate::model::event::{AttachmentDisposition, CacheEvent};
use crate::model::mail::MailtoRequest;

use super::compose::{ComposeKind, ComposePage, ComposeViewModel};
use super::dialogs::{build_settings, present_about};

const AUTO_REFRESH_INTERVAL_SECS: u32 = 300;

#[derive(Clone)]
pub struct MainWindow {
    inner: adw::ApplicationWindow,
    mailbox: Rc<RefCell<MailboxViewModel>>,
    title: adw::WindowTitle,
    account_dropdown: gtk::DropDown,
    sidebar: SidebarWidgets,
    thread_panel: ThreadPanelWidgets,
    cache: CacheManager,
    compose_page: ComposePage,
    mailbox_controls: MailboxControls,
    toast_overlay: adw::ToastOverlay,
    pending_mailto: Rc<RefCell<Vec<MailtoRequest>>>,
}

#[derive(Clone)]
struct MailboxControls {
    compose_action: gio::SimpleAction,
    refresh_button: gtk::Button,
    search_entry: gtk::SearchEntry,
}

impl MailboxControls {
    fn update(&self, mailbox: &MailboxViewModel) {
        let current_account = mailbox.current_account();
        let account_ready = !mailbox.is_loading() && current_account.is_some();
        self.compose_action
            .set_enabled(account_ready && mailbox.can_compose());
        self.refresh_button.set_sensitive(account_ready);
        self.search_entry.set_sensitive(account_ready);
    }

    fn suspend_for_account_activation(&self) {
        self.compose_action.set_enabled(false);
        self.refresh_button.set_sensitive(false);
        self.search_entry.set_sensitive(false);
    }
}

impl MainWindow {
    pub fn new(app: &adw::Application, mailbox: MailboxViewModel) -> Self {
        let state = Rc::new(RefCell::new(mailbox));
        let pending_mailto = Rc::new(RefCell::new(Vec::new()));

        let header = adw::HeaderBar::new();
        let backend_summary = state.borrow().backend_summary();
        let title = adw::WindowTitle::builder()
            .title(crate::config::APP_NAME)
            .subtitle(&backend_summary)
            .build();

        let compose_button = gtk::Button::builder()
            .label("Compose")
            .action_name("win.compose")
            .css_classes(["suggested-action"])
            .build();
        let about_button = gtk::Button::builder()
            .icon_name("help-about-symbolic")
            .tooltip_text(&format!("About {}", crate::config::APP_NAME))
            .action_name("win.about")
            .build();
        header.pack_end(&about_button);

        let preferences_button = gtk::Button::builder()
            .icon_name("emblem-system-symbolic")
            .tooltip_text("Mail Settings")
            .action_name("win.preferences")
            .build();
        header.pack_end(&preferences_button);

        let refresh_button = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Refresh current account")
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
            .hexpand(true)
            .vexpand(true)
            .build();
        let cache = CacheManager::new();
        let compose_page = ComposePage::new(
            &inner,
            &page_stack,
            Rc::clone(&state),
            cache.clone(),
            &toast_overlay,
        );
        let title_stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();
        title_stack.add_named(&title, Some("mailbox"));
        title_stack.add_named(compose_page.title(), Some("compose"));
        header.set_title_widget(Some(&title_stack));

        let preview_widgets = PreviewWidgets::new(&toast_overlay);
        let state_for_preview_mode = Rc::clone(&state);
        preview_widgets
            .mode_stack
            .connect_visible_child_name_notify(move |stack| {
                state_for_preview_mode
                    .borrow_mut()
                    .set_prefer_html_view(stack.visible_child_name().as_deref() == Some("html"));
            });
        let thread_panel = build_thread_panel(&state, preview_widgets.clone(), cache.clone());
        let sidebar = build_sidebar(
            &state,
            preview_widgets.clone(),
            thread_panel.clone(),
            cache.clone(),
        );

        let preview_panel =
            build_preview_panel(&state, preview_widgets, compose_page.clone(), cache.clone());
        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text("Search current account")
            .width_chars(22)
            .build();
        let state_for_compose = Rc::clone(&state);
        let compose_page_for_action = compose_page.clone();
        let compose_action = gio::SimpleAction::new("compose", None);
        compose_action.connect_activate(move |_, _| {
            if let Some(model) =
                ComposeViewModel::for_action(&state_for_compose.borrow(), ComposeKind::New)
            {
                compose_page_for_action.request_open(model);
            }
        });
        inner.add_action(&compose_action);
        let mailbox_controls = MailboxControls {
            compose_action,
            refresh_button,
            search_entry,
        };
        mailbox_controls.update(&state.borrow());
        let account_dropdown = build_account_dropdown(
            &state,
            cache.clone(),
            sidebar.clone(),
            thread_panel.clone(),
            title.clone(),
            compose_page.clone(),
            mailbox_controls.clone(),
        );
        let account_ready = {
            let state = state.borrow();
            !state.is_loading() && state.current_account().is_some()
        };
        account_dropdown.set_sensitive(account_ready);
        app.remove_action("show-account");
        let show_account_action =
            gio::SimpleAction::new("show-account", Some(glib::VariantTy::STRING));
        let state_for_notification_action = Rc::clone(&state);
        let dropdown_for_notification_action = account_dropdown.clone();
        let window_for_notification_action = inner.clone();
        show_account_action.connect_activate(move |_, parameter| {
            let Some(account_id) = parameter.and_then(glib::Variant::str) else {
                return;
            };
            if dropdown_for_notification_action.is_sensitive()
                && let Some(index) = state_for_notification_action
                    .borrow()
                    .account_index(&crate::model::account::MailAccountId(account_id.into()))
            {
                dropdown_for_notification_action.set_selected(index as u32);
            }
            window_for_notification_action.present();
        });
        app.add_action(&show_account_action);
        header.pack_start(compose_page.back_button());
        header.pack_start(&account_dropdown);
        header.pack_start(&compose_button);
        header.pack_end(&mailbox_controls.search_entry);
        header.pack_end(compose_page.send_button());
        header.pack_end(compose_page.save_button());

        let main_columns = gtk::Box::builder().hexpand(true).vexpand(true).build();
        let sidebar_separator = gtk::Separator::new(gtk::Orientation::Vertical);
        let thread_separator = gtk::Separator::new(gtk::Orientation::Vertical);
        main_columns.append(&sidebar.container);
        main_columns.append(&sidebar_separator);
        main_columns.append(&thread_panel.container);
        main_columns.append(&thread_separator);
        main_columns.append(&preview_panel);

        page_stack.add_named(&main_columns, Some("mailbox"));
        page_stack.add_named(compose_page.root(), Some("compose"));
        page_stack.set_visible_child_name("mailbox");

        compose_page.back_button().set_visible(false);
        compose_page.save_button().set_visible(false);
        compose_page.send_button().set_visible(false);
        let compose_button_for_page = compose_button.clone();
        let refresh_button_for_page = mailbox_controls.refresh_button.clone();
        let search_entry_for_page = mailbox_controls.search_entry.clone();
        let back_button_for_page = compose_page.back_button().clone();
        let save_button_for_page = compose_page.save_button().clone();
        let send_button_for_page = compose_page.send_button().clone();
        let title_stack_for_page = title_stack.clone();
        page_stack.connect_visible_child_name_notify(move |stack| {
            let composing = stack.visible_child_name().as_deref() == Some("compose");
            compose_button_for_page.set_visible(!composing);
            refresh_button_for_page.set_visible(!composing);
            search_entry_for_page.set_visible(!composing);
            back_button_for_page.set_visible(composing);
            save_button_for_page.set_visible(composing);
            send_button_for_page.set_visible(composing);
            title_stack_for_page.set_visible_child_name(if composing {
                "compose"
            } else {
                "mailbox"
            });
        });

        let compose_page_for_close = compose_page.clone();
        inner.connect_close_request(move |_| compose_page_for_close.handle_close_request());

        let compose_page_for_back = compose_page.clone();
        let back_action = gio::SimpleAction::new("back", None);
        back_action.connect_activate(move |_, _| compose_page_for_back.request_back());
        inner.add_action(&back_action);

        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header);
        toolbar_view.set_content(Some(&page_stack));

        toast_overlay.set_child(Some(&toolbar_view));

        inner.set_content(Some(&toast_overlay));

        let parent_for_preferences = inner.clone();
        let state_for_preferences = Rc::clone(&state);
        let sidebar_for_preferences = sidebar.clone();
        let account_dropdown_for_preferences = account_dropdown.clone();
        let preferences_action = gio::SimpleAction::new("preferences", None);
        preferences_action.connect_activate(move |_, _| {
            let settings_window = build_settings(
                &parent_for_preferences,
                Rc::clone(&state_for_preferences),
                sidebar_for_preferences.clone(),
                account_dropdown_for_preferences.clone(),
            );
            settings_window.present();
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
            present_about(&parent_for_about);
        });
        inner.add_action(&about_action);

        let state_for_search = Rc::clone(&state);
        let thread_for_search = thread_panel.clone();
        let cache_for_search = cache.clone();
        mailbox_controls
            .search_entry
            .connect_search_changed(move |entry| {
                if let Some(account_id) = state_for_search.borrow().current_account_id() {
                    cache_for_search.cancel_pending_message_detail(&account_id);
                }
                let request = state_for_search.borrow_mut().search(entry.text().as_str());
                if let Some(request) = request {
                    cache_for_search.request_search(
                        request.service,
                        request.request_id,
                        request.account_id,
                        request.query,
                    );
                } else if entry.text().trim().is_empty()
                    && let Some(account_id) = state_for_search.borrow().current_account_id()
                {
                    cache_for_search.cancel_pending_search(&account_id);
                    let reload = state_for_search
                        .borrow_mut()
                        .begin_cache_change_reload(&account_id);
                    if let Some(reload) = reload {
                        request_mailbox_reload(&cache_for_search, reload);
                    }
                }
                rebuild_thread_panel(&state_for_search, &thread_for_search);
            });

        let state_for_refresh = Rc::clone(&state);
        let cache_for_button = cache.clone();
        mailbox_controls.refresh_button.connect_clicked(move |_| {
            queue_current_account_refresh(&state_for_refresh, &cache_for_button);
        });

        let state_for_refresh_events = Rc::clone(&state);
        let sidebar_for_refresh_events = sidebar.clone();
        let thread_for_refresh_events = thread_panel.clone();
        let cache_for_events = cache.clone();
        let parent_for_events = inner.clone();
        let toast_for_events = toast_overlay.clone();
        let title_for_events = title.clone();
        let account_dropdown_for_events = account_dropdown.clone();
        let mailbox_controls_for_events = mailbox_controls.clone();
        let compose_page_for_events = compose_page.clone();
        let pending_mailto_for_events = Rc::clone(&pending_mailto);
        glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
            for event in cache_for_events.drain() {
                match event {
                    CacheEvent::AccountActivated {
                        request_id,
                        account_id,
                        result,
                    } => {
                        if state_for_refresh_events
                            .borrow_mut()
                            .finish_account_activation(request_id, &account_id, result)
                        {
                            account_dropdown_for_events.set_sensitive(true);
                            mailbox_controls_for_events.update(&state_for_refresh_events.borrow());
                            compose_page_for_events.account_activation_finished();
                            title_for_events
                                .set_subtitle(&state_for_refresh_events.borrow().backend_summary());
                            rebuild_sidebar(&state_for_refresh_events, &sidebar_for_refresh_events);
                            rebuild_thread_panel(
                                &state_for_refresh_events,
                                &thread_for_refresh_events,
                            );
                            queue_current_account_refresh(
                                &state_for_refresh_events,
                                &cache_for_events,
                            );
                            drain_pending_mailto(
                                &pending_mailto_for_events,
                                &state_for_refresh_events,
                                &compose_page_for_events,
                                &toast_for_events,
                            );
                        }
                    }
                    CacheEvent::AccountRefreshCompleted {
                        account_id,
                        failure,
                    } => {
                        let refresh_succeeded = failure.is_none();
                        let reload = state_for_refresh_events
                            .borrow_mut()
                            .finish_account_refresh(&account_id, failure);
                        if let Some(reload) = reload {
                            request_mailbox_reload(&cache_for_events, reload);
                            title_for_events
                                .set_subtitle(&state_for_refresh_events.borrow().backend_summary());
                        }
                        if refresh_succeeded
                            && state_for_refresh_events
                                .borrow()
                                .current_account_id()
                                .as_ref()
                                == Some(&account_id)
                        {
                            start_current_account_change_monitor(
                                &state_for_refresh_events,
                                &cache_for_events,
                            );
                        }
                    }
                    CacheEvent::RemoteAccountChanged { account_id } => {
                        cache_for_events.acknowledge_remote_change(&account_id);
                        if state_for_refresh_events
                            .borrow()
                            .current_account_id()
                            .as_ref()
                            == Some(&account_id)
                        {
                            queue_current_account_refresh(
                                &state_for_refresh_events,
                                &cache_for_events,
                            );
                        }
                    }
                    CacheEvent::MailboxReloaded {
                        request_id,
                        account_id,
                        result,
                    } => {
                        if state_for_refresh_events.borrow_mut().finish_mailbox_reload(
                            request_id,
                            &account_id,
                            result,
                        ) {
                            rebuild_sidebar(&state_for_refresh_events, &sidebar_for_refresh_events);
                            rebuild_thread_panel(
                                &state_for_refresh_events,
                                &thread_for_refresh_events,
                            );
                        }
                    }
                    CacheEvent::NewMailAvailable { account_id, count } => {
                        if !notification_belongs_to_active_account(
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
                            .unwrap_or("mail account")
                            .to_string();
                        let notification = gio::Notification::new(if count == 1 {
                            "New mail"
                        } else {
                            "New mail available"
                        });
                        notification.set_body(Some(&format!(
                            "{} new conversation{} in {}",
                            count,
                            if count == 1 { "" } else { "s" },
                            account_name,
                        )));
                        let notification_target = glib::Variant::from(account_id.0.as_str());
                        notification.set_default_action_and_target_value(
                            "app.show-account",
                            Some(&notification_target),
                        );
                        if let Some(application) = parent_for_events.application() {
                            application.send_notification(
                                Some(&format!("new-mail-{}", account_id.0)),
                                &notification,
                            );
                        }
                    }
                    CacheEvent::MailboxCacheChanged { account_id } => {
                        let reload = state_for_refresh_events
                            .borrow_mut()
                            .begin_cache_change_reload(&account_id);
                        if let Some(reload) = reload {
                            request_mailbox_reload(&cache_for_events, reload);
                        }
                    }
                    CacheEvent::ThreadPageLoaded {
                        request_id,
                        account_id,
                        folder_id,
                        offset,
                        result,
                    } => {
                        let error = result.as_ref().err().cloned();
                        let added = state_for_refresh_events
                            .borrow_mut()
                            .finish_thread_page_load(
                                request_id,
                                &account_id,
                                &folder_id,
                                offset,
                                result,
                            );
                        if let Some(added) = added {
                            if offset == 0 {
                                rebuild_thread_panel(
                                    &state_for_refresh_events,
                                    &thread_for_refresh_events,
                                );
                            } else {
                                for thread in &added {
                                    append_thread_row(
                                        &thread_for_refresh_events.thread_list,
                                        thread,
                                    );
                                }
                            }
                            if let Some(error) = error {
                                toast_for_events.add_toast(adw::Toast::new(&error));
                            }
                        }
                    }
                    CacheEvent::SearchCompleted {
                        request_id,
                        account_id,
                        query,
                        result,
                    } => {
                        if state_for_refresh_events.borrow_mut().finish_search(
                            request_id,
                            &account_id,
                            &query,
                            result,
                        ) {
                            rebuild_thread_panel(
                                &state_for_refresh_events,
                                &thread_for_refresh_events,
                            );
                        }
                    }
                    CacheEvent::MessageDetailLoaded {
                        request_id,
                        account_id,
                        conversation_id,
                        result,
                    } => {
                        let mut state = state_for_refresh_events.borrow_mut();
                        if state.finish_message_detail_load(
                            request_id,
                            &account_id,
                            &conversation_id,
                            result,
                        ) {
                            sidebar_for_refresh_events
                                .preview_widgets
                                .refresh_from_mailbox(&state);
                            sidebar_for_refresh_events
                                .preview_widgets
                                .sync_action_state(&state);
                        }
                    }
                    CacheEvent::MessageActionCompleted {
                        account_id,
                        conversation_id,
                        action,
                        result,
                    } => {
                        let error = result.as_ref().err().cloned();
                        let changed = state_for_refresh_events.borrow_mut().finish_message_action(
                            &account_id,
                            &conversation_id,
                            &action,
                            &result,
                        );
                        if changed {
                            rebuild_sidebar(&state_for_refresh_events, &sidebar_for_refresh_events);
                            rebuild_thread_panel(
                                &state_for_refresh_events,
                                &thread_for_refresh_events,
                            );
                        } else {
                            sidebar_for_refresh_events
                                .preview_widgets
                                .sync_action_state(&state_for_refresh_events.borrow());
                        }
                        if let Some(error) = error {
                            toast_for_events.add_toast(adw::Toast::new(&error));
                        }
                    }
                    CacheEvent::ComposeOperationCompleted { operation, result } => {
                        compose_page_for_events.finish_operation(operation, result);
                    }
                    CacheEvent::AttachmentPrepared {
                        disposition,
                        display_name,
                        source_uri,
                        result,
                    } => handle_prepared_attachment(
                        &parent_for_events,
                        &toast_for_events,
                        disposition,
                        display_name,
                        source_uri,
                        result,
                    ),
                }
            }
            glib::ControlFlow::Continue
        });

        let state_for_auto_refresh = Rc::clone(&state);
        let cache_for_auto = cache.clone();
        glib::idle_add_local_once(move || {
            queue_current_account_refresh(&state_for_auto_refresh, &cache_for_auto);
        });

        let state_for_periodic = Rc::clone(&state);
        let cache_for_periodic = cache.clone();
        glib::timeout_add_seconds_local(AUTO_REFRESH_INTERVAL_SECS, move || {
            queue_current_account_refresh(&state_for_periodic, &cache_for_periodic);
            glib::ControlFlow::Continue
        });

        let network_monitor = gio::NetworkMonitor::default();
        let state_for_network = Rc::clone(&state);
        let cache_for_network = cache.clone();
        network_monitor.connect_network_changed(move |_, available| {
            if available {
                if let Some(account_id) = state_for_network.borrow().current_account_id() {
                    cache_for_network.reset_account_change_monitor(&account_id);
                }
                queue_current_account_refresh(&state_for_network, &cache_for_network);
            }
        });

        Self {
            inner,
            mailbox: state,
            title,
            account_dropdown,
            sidebar,
            thread_panel,
            cache,
            compose_page,
            mailbox_controls,
            toast_overlay,
            pending_mailto,
        }
    }

    pub fn present(&self) {
        self.inner.present();
    }

    pub fn replace_mailbox(&self, mailbox: MailboxViewModel) {
        if let Some(account_id) = self.mailbox.borrow().current_account_id() {
            self.cache.cancel_pending_search(&account_id);
            self.cache.cancel_pending_message_detail(&account_id);
        }
        *self.mailbox.borrow_mut() = mailbox;
        self.compose_page.mailbox_replaced();
        let prefer_html_view = self.mailbox.borrow().prefer_html_view;
        self.sidebar
            .preview_widgets
            .set_prefer_html_view(prefer_html_view);
        {
            let state = self.mailbox.borrow();
            self.title.set_subtitle(&state.backend_summary());
            refresh_account_dropdown(&self.account_dropdown, &state);
            self.account_dropdown
                .set_sensitive(!state.is_loading() && state.current_account().is_some());
        }
        rebuild_sidebar(&self.mailbox, &self.sidebar);
        rebuild_thread_panel(&self.mailbox, &self.thread_panel);
        queue_current_account_refresh(&self.mailbox, &self.cache);
        self.mailbox_controls.update(&self.mailbox.borrow());
        self.present_pending_mailto();
    }

    pub fn replace_mailbox_after_registry_change(
        &self,
        mailbox: MailboxViewModel,
        changed_accounts: &[crate::model::account::MailAccountId],
    ) {
        if let Some(account_id) = self.mailbox.borrow().current_account_id()
            && changed_accounts.contains(&account_id)
        {
            self.cache.reset_account_change_monitor(&account_id);
            self.cache.acknowledge_remote_change(&account_id);
        }
        self.replace_mailbox(mailbox);
    }

    pub fn mailbox(&self) -> std::cell::Ref<'_, MailboxViewModel> {
        self.mailbox.borrow()
    }

    pub fn open_mailto(&self, request: MailtoRequest) {
        if self.mailbox.borrow().is_loading() {
            self.pending_mailto.borrow_mut().push(request);
            return;
        }
        present_mailto_request(
            &self.mailbox,
            &self.compose_page,
            &self.toast_overlay,
            request,
        );
    }

    fn present_pending_mailto(&self) {
        drain_pending_mailto(
            &self.pending_mailto,
            &self.mailbox,
            &self.compose_page,
            &self.toast_overlay,
        );
    }
}

fn drain_pending_mailto(
    pending: &Rc<RefCell<Vec<MailtoRequest>>>,
    mailbox: &Rc<RefCell<MailboxViewModel>>,
    compose_page: &ComposePage,
    toast_overlay: &adw::ToastOverlay,
) {
    let requests = std::mem::take(&mut *pending.borrow_mut());
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
    let Some(model) = ComposeViewModel::for_mailto(&mailbox.borrow(), &request) else {
        toast_overlay.add_toast(adw::Toast::new("Mail link unavailable."));
        tracing::warn!("mailto request could not be opened without a sending identity");
        return;
    };
    compose_page.request_open(model);
}

fn queue_current_account_refresh(state: &Rc<RefCell<MailboxViewModel>>, cache: &CacheManager) {
    cache.set_fully_active_account(state.borrow().current_account_id());
    let Some(request) = state.borrow().current_account_refresh_handle() else {
        return;
    };

    cache.request_account_refresh(request.account_id, request.service);
}

fn start_current_account_change_monitor(
    state: &Rc<RefCell<MailboxViewModel>>,
    cache: &CacheManager,
) {
    let (account_id, binding) = {
        let state = state.borrow();
        let Some(account_id) = state.current_account_id() else {
            return;
        };
        let Some(binding) = state.current_eds_binding() else {
            return;
        };
        (account_id, binding)
    };
    cache.start_account_change_monitor(account_id, binding);
}

fn notification_belongs_to_active_account(
    active_account_id: Option<&crate::model::account::MailAccountId>,
    event_account_id: &crate::model::account::MailAccountId,
) -> bool {
    active_account_id == Some(event_account_id)
}

fn dispatch_message_action(
    cache: &CacheManager,
    request: Option<crate::ui::mailbox::MessageActionRequest>,
) {
    if let Some(request) = request {
        cache.request_message_action(
            request.service,
            request.account_id,
            request.conversation_id,
            request.action,
        );
    }
}

fn request_mailbox_reload(cache: &CacheManager, request: crate::ui::mailbox::MailboxReloadLoad) {
    cache.request_mailbox_reload(
        request.service,
        request.request_id,
        request.account_id,
        request.selected_folder_id,
        request.conversation_limit,
    );
}

fn build_account_dropdown(
    state: &Rc<RefCell<MailboxViewModel>>,
    cache: CacheManager,
    sidebar: SidebarWidgets,
    thread_panel: ThreadPanelWidgets,
    title: adw::WindowTitle,
    compose_page: ComposePage,
    controls: MailboxControls,
) -> gtk::DropDown {
    let dropdown = gtk::DropDown::new(None::<gtk::StringList>, None::<gtk::Expression>);
    refresh_account_dropdown(&dropdown, &state.borrow());
    let state_for_change = Rc::clone(state);
    dropdown.connect_selected_notify(move |dropdown| {
        let selected = dropdown.selected() as usize;
        if selected == state_for_change.borrow().selected_account {
            return;
        }
        if let Some(account_id) = state_for_change.borrow().current_account_id() {
            cache.cancel_pending_search(&account_id);
            cache.cancel_pending_message_detail(&account_id);
        }
        let activation = state_for_change
            .borrow_mut()
            .begin_account_activation(selected);
        if activation.is_some() {
            dropdown.set_sensitive(false);
            controls.suspend_for_account_activation();
            compose_page.account_activation_started();
        }
        controls.search_entry.set_text("");
        title.set_subtitle(&state_for_change.borrow().backend_summary());
        rebuild_sidebar(&state_for_change, &sidebar);
        rebuild_thread_panel(&state_for_change, &thread_panel);
        if let Some(activation) = activation {
            cache.request_account_activation(
                activation.service,
                activation.request_id,
                activation.account,
                activation.conversation_limit,
            );
        }
    });
    dropdown
}

pub(super) fn refresh_account_dropdown(dropdown: &gtk::DropDown, mailbox: &MailboxViewModel) {
    let labels: Vec<String> = mailbox
        .accounts
        .iter()
        .map(|account| account.selector_label())
        .collect();
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let model = gtk::StringList::new(&refs);
    let _notifications = dropdown.freeze_notify();
    dropdown.set_model(Some(&model));
    dropdown.set_selected(mailbox.selected_account as u32);
}

fn build_sidebar(
    state: &Rc<RefCell<MailboxViewModel>>,
    preview_widgets: PreviewWidgets,
    thread_panel: ThreadPanelWidgets,
    cache: CacheManager,
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
        .propagate_natural_width(true)
        .vexpand(true)
        .child(&content)
        .css_classes(["mailbox-sidebar"])
        .build();
    container.set_width_request(184);
    container.set_max_content_width(184);

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
        .label("Loading folders…")
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
        preview_widgets,
    };
    rebuild_sidebar(state, &widgets);

    let state_for_folder = Rc::clone(state);
    let thread_panel_for_folder = thread_panel.clone();
    widgets.folder_list.connect_row_selected(move |_, row| {
        let Some(row) = row else {
            return;
        };

        if row.index() as usize == state_for_folder.borrow().selected_folder {
            return;
        }

        if let Some(account_id) = state_for_folder.borrow().current_account_id() {
            cache.cancel_pending_message_detail(&account_id);
        }
        let request = state_for_folder
            .borrow_mut()
            .select_folder(row.index() as usize);
        rebuild_thread_panel(&state_for_folder, &thread_panel_for_folder);
        if let Some(request) = request {
            cache.request_thread_page(
                request.service,
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
    cache: CacheManager,
) -> ThreadPanelWidgets {
    let container = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .vexpand(true)
        .css_classes(["conversation-panel"])
        .build();
    container.set_width_request(320);

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
        .label("Loading messages")
        .css_classes(["title-3"])
        .build();
    let loading_description = gtk::Label::builder()
        .label("Reading the local mail cache…")
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
        .title("No messages")
        .description("Folder is empty.")
        .css_classes(["compact"])
        .build();
    let error_page = adw::StatusPage::builder()
        .icon_name("dialog-error-symbolic")
        .title("Messages unavailable")
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
    let cache_for_selection = cache.clone();
    thread_list.connect_row_activated(move |_, row| {
        let id = row.widget_name();
        {
            let mut state = state_for_selection.borrow_mut();
            state.select_thread(crate::model::mail::ConversationId(id.to_string()));
        }
        if let Some(request) = state_for_selection.borrow_mut().begin_message_detail_load() {
            cache_for_selection.request_message_detail(
                request.service,
                request.request_id,
                request.account_id,
                request.conversation_id,
            );
        }
        let state = state_for_selection.borrow();
        preview_for_selection.refresh_from_mailbox(&state);
        preview_for_selection.sync_action_state(&state);
    });

    let state_for_scroll = Rc::clone(state);
    let cache_for_scroll = cache;
    content
        .vadjustment()
        .connect_value_changed(move |adjustment| {
            let threshold = 96.0;
            let at_bottom =
                adjustment.value() + adjustment.page_size() >= adjustment.upper() - threshold;
            if !at_bottom {
                return;
            }

            if let Some(request) = state_for_scroll.borrow_mut().begin_thread_page_load() {
                cache_for_scroll.request_thread_page(
                    request.service,
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

pub(super) fn rebuild_sidebar(state: &Rc<RefCell<MailboxViewModel>>, widgets: &SidebarWidgets) {
    let state_ref = state.borrow();
    widgets.account_title.set_label("Folders");

    if state_ref.is_loading() {
        widgets.loading_spinner.start();
        widgets.content_stack.set_visible_child_name("loading");
        widgets.preview_widgets.refresh_from_mailbox(&state_ref);
        widgets.preview_widgets.sync_action_state(&state_ref);
        return;
    }

    widgets.folder_list.remove_all();
    for folder in &state_ref.folders {
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

    if let Some(row) = widgets
        .folder_list
        .row_at_index(state_ref.selected_folder as i32)
    {
        widgets.folder_list.select_row(Some(&row));
    }

    widgets.loading_spinner.stop();
    widgets.content_stack.set_visible_child_name("folders");
    widgets.preview_widgets.refresh_from_mailbox(&state_ref);
    widgets.preview_widgets.sync_action_state(&state_ref);
}

fn rebuild_thread_panel(state: &Rc<RefCell<MailboxViewModel>>, widgets: &ThreadPanelWidgets) {
    widgets.thread_list.remove_all();

    let state_ref = state.borrow();
    if state_ref.is_loading() {
        widgets.heading.set_label("Mailbox");
        widgets.show_loading("Loading mail", "Opening local cache…");
        widgets.preview_widgets.refresh_from_mailbox(&state_ref);
        widgets.preview_widgets.sync_action_state(&state_ref);
        return;
    }

    let folder = state_ref
        .current_folder()
        .map(|folder| folder.name.clone())
        .unwrap_or_else(|| "Mailbox".into());
    if state_ref.search_query.is_empty() {
        widgets.heading.set_label(&folder);
        if state_ref.initial_threads_loading() {
            widgets.show_loading("Loading messages", "Opening local cache…");
        }
    } else if state_ref.search_loading() {
        widgets.heading.set_label("Search");
        widgets.show_loading("Searching mail", "Searching local cache…");
    } else if let Some(error) = state_ref.search_error.as_deref() {
        widgets.heading.set_label("Search");
        widgets.show_error(error);
    } else {
        widgets.heading.set_label(&format!(
            "Search results for \"{}\"",
            state_ref.search_query
        ));
    }

    let showing_transient_state = state_ref.initial_threads_loading()
        || state_ref.search_loading()
        || state_ref.search_error.is_some();
    if !showing_transient_state {
        if state_ref.threads.is_empty() {
            if state_ref.search_query.is_empty() {
                widgets.show_empty("No messages", "Folder is empty.");
            } else {
                widgets.show_empty("No matches", "Local cache has no matches.");
            }
        } else {
            widgets.show_messages();
        }
    }

    for thread in &state_ref.threads {
        append_thread_row(&widgets.thread_list, thread);
    }

    let selected_id = state_ref.selected_thread.clone();
    let thread_count = state_ref.threads.len();
    drop(state_ref);

    if let Some(index) = selected_id.as_ref().and_then(|selected_id| {
        (0..thread_count).find(|&index| {
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

    let state_ref = state.borrow();
    widgets.preview_widgets.refresh_from_mailbox(&state_ref);
    widgets.preview_widgets.sync_action_state(&state_ref);
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
            "Unknown sender".into()
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
        let tooltip = if thread.attachment_count == 1 {
            "1 attachment".to_string()
        } else {
            format!("{} attachments", thread.attachment_count)
        };
        attachment.set_tooltip_text(Some(&tooltip));
        attachment.add_css_class("dim-label");
        sender_line.append(&attachment);
    }
    if thread.starred {
        let star = gtk::Image::from_icon_name("starred-symbolic");
        star.set_tooltip_text(Some("Starred"));
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
        "(No subject)"
    } else {
        thread.subject.trim()
    };
    if thread.message_count > 1 {
        format!("{subject}  ({})", thread.message_count)
    } else {
        subject.into()
    }
}

fn build_preview_panel(
    state: &Rc<RefCell<MailboxViewModel>>,
    preview: PreviewWidgets,
    compose_page: ComposePage,
    cache: CacheManager,
) -> gtk::ScrolledWindow {
    {
        let state_ref = state.borrow();
        preview.refresh_from_mailbox(&state_ref);
    }

    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(24)
        .margin_end(24)
        .hexpand(true)
        .vexpand(true)
        .build();

    let action_buttons = adw::WrapBox::builder()
        .child_spacing(6)
        .line_spacing(6)
        .margin_top(8)
        .build();
    action_buttons.append(&preview.reply_button);
    action_buttons.append(&preview.reply_all_button);
    action_buttons.append(&preview.forward_button);
    action_buttons.append(&preview.star_button);
    action_buttons.append(&preview.read_button);
    action_buttons.append(&preview.archive_button);
    action_buttons.append(&preview.trash_button);
    let action_revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .child(&action_buttons)
        .build();
    let action_toggle = gtk::ToggleButton::builder().label("Actions").build();
    let action_revealer_for_toggle = action_revealer.clone();
    action_toggle.connect_toggled(move |button| {
        action_revealer_for_toggle.set_reveal_child(button.is_active());
    });

    let state_for_reply = Rc::clone(state);
    let compose_page_for_reply = compose_page.clone();
    preview.reply_button.connect_clicked(move |_| {
        let state_ref = state_for_reply.borrow();
        let action = preview_primary_compose_action(&state_ref);
        if let Some(model) = ComposeViewModel::for_action(&state_ref, action) {
            compose_page_for_reply.request_open(model);
        }
    });

    let state_for_reply_all = Rc::clone(state);
    let compose_page_for_reply_all = compose_page.clone();
    preview.reply_all_button.connect_clicked(move |_| {
        if let Some(model) =
            ComposeViewModel::for_action(&state_for_reply_all.borrow(), ComposeKind::ReplyAll)
        {
            compose_page_for_reply_all.request_open(model);
        }
    });

    let state_for_forward = Rc::clone(state);
    let compose_page_for_forward = compose_page;
    preview.forward_button.connect_clicked(move |_| {
        if let Some(model) =
            ComposeViewModel::for_action(&state_for_forward.borrow(), ComposeKind::Forward)
        {
            compose_page_for_forward.request_open(model);
        }
    });

    let state_for_attachment_actions = Rc::clone(state);
    let cache_for_attachment_actions = cache.clone();
    preview.set_attachment_dispatch({
        let state = Rc::clone(&state_for_attachment_actions);
        let toast_overlay = preview.toast_overlay.clone();
        move |disposition, attachment| {
            let request = state.borrow().current_attachment_request(&attachment.uri);
            if !cache_for_attachment_actions.request_attachment(
                request,
                disposition,
                attachment.display_name.clone(),
                attachment.uri.clone(),
            ) {
                toast_overlay.add_toast(adw::Toast::new(&format!(
                    "Unavailable: {}",
                    attachment.display_name
                )));
            }
        }
    });

    let state_for_star = Rc::clone(state);
    let preview_for_star = preview.clone();
    let cache_for_star = cache.clone();
    preview.star_button.connect_clicked(move |_| {
        dispatch_message_action(
            &cache_for_star,
            state_for_star.borrow_mut().begin_toggle_star(),
        );
        let state_ref = state_for_star.borrow();
        preview_for_star.sync_action_state(&state_ref);
    });

    let state_for_read = Rc::clone(state);
    let preview_for_read = preview.clone();
    let cache_for_read = cache.clone();
    preview.read_button.connect_clicked(move |_| {
        dispatch_message_action(
            &cache_for_read,
            state_for_read.borrow_mut().begin_toggle_read(),
        );
        let state_ref = state_for_read.borrow();
        preview_for_read.sync_action_state(&state_ref);
    });

    let state_for_archive = Rc::clone(state);
    let preview_for_archive = preview.clone();
    let cache_for_archive = cache.clone();
    preview.archive_button.connect_clicked(move |_| {
        dispatch_message_action(
            &cache_for_archive,
            state_for_archive.borrow_mut().begin_archive_selected(),
        );
        let state_ref = state_for_archive.borrow();
        preview_for_archive.sync_action_state(&state_ref);
    });

    let state_for_trash = Rc::clone(state);
    let preview_for_trash = preview.clone();
    let cache_for_trash = cache;
    preview.trash_button.connect_clicked(move |_| {
        dispatch_message_action(
            &cache_for_trash,
            state_for_trash.borrow_mut().begin_trash_selected(),
        );
        let state_ref = state_for_trash.borrow();
        preview_for_trash.sync_action_state(&state_ref);
    });

    let preview_controls = gtk::Box::builder().spacing(8).margin_top(8).build();
    let controls_spacer = gtk::Box::builder().hexpand(true).build();
    preview_controls.append(&action_toggle);
    preview_controls.append(&controls_spacer);
    preview_controls.append(&preview.mode_switcher);
    preview.actions.replace(Some(action_toggle.clone()));
    preview.content.append(&preview_controls);
    preview.content.append(&action_revealer);
    preview.content.append(&preview.mode_stack);
    body.append(&preview.state_stack);

    {
        let state_ref = state.borrow();
        preview.sync_action_state(&state_ref);
    }

    let panel = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .hexpand(true)
        .vexpand(true)
        .child(&body)
        .css_classes(["message-preview"])
        .build();
    panel.set_min_content_width(0);
    panel
}

#[derive(Clone)]
struct PreviewWidgets {
    toast_overlay: adw::ToastOverlay,
    content: gtk::Box,
    state_stack: gtk::Stack,
    subject: gtk::Label,
    meta: gtk::Label,
    recipients: gtk::Label,
    cc_recipients: gtk::Label,
    reply_to_value: gtk::Label,
    attachments_label: gtk::Label,
    attachments_list: gtk::ListBox,
    loading_spinner: gtk::Spinner,
    error_page: adw::StatusPage,
    reply_button: gtk::Button,
    reply_all_button: gtk::Button,
    forward_button: gtk::Button,
    star_button: gtk::Button,
    read_button: gtk::Button,
    archive_button: gtk::Button,
    trash_button: gtk::Button,
    mode_switcher: gtk::StackSwitcher,
    mode_stack: gtk::Stack,
    actions: Rc<RefCell<Option<gtk::ToggleButton>>>,
    html_view: webkit::WebView,
    loaded_html: Rc<RefCell<Option<(crate::model::mail::ConversationId, u64)>>>,
    body: gtk::Label,
    current_detail: Rc<RefCell<Option<crate::model::mail::MessageDetail>>>,
    current_loading: Rc<RefCell<bool>>,
    current_error: Rc<RefCell<Option<String>>>,
    attachment_dispatch:
        Rc<RefCell<Option<Rc<dyn Fn(AttachmentDisposition, crate::model::mail::AttachmentInfo)>>>>,
}

#[derive(Clone)]
pub(super) struct SidebarWidgets {
    container: gtk::ScrolledWindow,
    account_title: gtk::Label,
    folder_list: gtk::ListBox,
    content_stack: gtk::Stack,
    loading_spinner: gtk::Spinner,
    preview_widgets: PreviewWidgets,
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

impl PreviewWidgets {
    fn new(toast_overlay: &adw::ToastOverlay) -> Self {
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
        content.append(&recipients);
        content.append(&cc_recipients);
        content.append(&reply_to_value);
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
        let reply_button = gtk::Button::builder().label("Reply").build();
        let reply_all_button = gtk::Button::builder().label("Reply All").build();
        let forward_button = gtk::Button::builder().label("Forward").build();
        let star_button = gtk::Button::builder().label("Star").build();
        let read_button = gtk::Button::builder().label("Mark Read").build();
        let archive_button = gtk::Button::builder().label("Archive").build();
        let trash_button = gtk::Button::builder().label("Move to Trash").build();
        let html_view = webkit::WebView::new();
        configure_mail_view(&html_view);
        let body = gtk::Label::builder()
            .xalign(0.0)
            .yalign(0.0)
            .valign(gtk::Align::Start)
            .wrap(true)
            .selectable(true)
            .build();
        body.set_hexpand(true);
        body.set_wrap_mode(pango::WrapMode::WordChar);
        let mode_stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .hexpand(true)
            .vexpand(true)
            .build();
        mode_stack.add_titled(&html_view, Some("html"), "HTML");
        mode_stack.add_titled(&body, Some("text"), "Text");
        mode_stack.set_visible_child_name("html");
        let mode_switcher = gtk::StackSwitcher::builder()
            .stack(&mode_stack)
            .halign(gtk::Align::Start)
            .build();

        let preview = Self {
            toast_overlay: toast_overlay.clone(),
            content,
            state_stack,
            subject,
            meta,
            recipients,
            cc_recipients,
            reply_to_value,
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
            actions: Rc::new(RefCell::new(None)),
            html_view,
            loaded_html: Rc::new(RefCell::new(None)),
            body,
            current_detail: Rc::new(RefCell::new(None)),
            current_loading: Rc::new(RefCell::new(false)),
            current_error: Rc::new(RefCell::new(None)),
            attachment_dispatch: Rc::new(RefCell::new(None)),
        };

        let preview_for_mode = preview.clone();
        preview
            .mode_stack
            .connect_visible_child_name_notify(move |_| {
                preview_for_mode.render_current();
            });

        preview
    }

    fn set_attachment_dispatch<F>(&self, handler: F)
    where
        F: Fn(AttachmentDisposition, crate::model::mail::AttachmentInfo) + 'static,
    {
        *self.attachment_dispatch.borrow_mut() = Some(Rc::new(handler));
    }

    fn set_prefer_html_view(&self, prefer_html_view: bool) {
        self.mode_stack
            .set_visible_child_name(if prefer_html_view { "html" } else { "text" });
    }

    fn refresh_from_mailbox(&self, mailbox: &MailboxViewModel) {
        let detail = mailbox.message_detail.as_ref();
        *self.current_detail.borrow_mut() = detail.cloned();
        *self.current_loading.borrow_mut() = mailbox.message_detail_loading;
        *self.current_error.borrow_mut() = mailbox.message_detail_error.clone();
        if let Some(detail) = detail {
            self.star_button
                .set_label(if detail.starred { "Unstar" } else { "Star" });
            self.read_button.set_label(if detail.unread {
                "Mark Read"
            } else {
                "Mark Unread"
            });
            self.star_button.set_sensitive(true);
            self.read_button.set_sensitive(true);
            self.archive_button.set_sensitive(true);
            self.trash_button.set_sensitive(true);
        } else {
            self.star_button.set_label("Star");
            self.read_button.set_label("Mark Read");
            self.star_button.set_sensitive(false);
            self.read_button.set_sensitive(false);
            self.archive_button.set_sensitive(false);
            self.trash_button.set_sensitive(false);
        }
        if let Some(detail) = detail {
            self.meta
                .set_label(&format!("From: {}    {}", detail.from, detail.date_label));
        }
        self.render_current();
    }

    fn sync_action_state(&self, mailbox: &MailboxViewModel) {
        let Some(folder) = mailbox.current_folder() else {
            self.set_compose_actions(false, "Reply");
            self.archive_button.set_sensitive(false);
            self.archive_button.set_label("Archive");
            self.trash_button.set_sensitive(false);
            if let Some(toggle) = self.actions.borrow().as_ref() {
                toggle.set_active(false);
            }
            return;
        };

        let has_message = mailbox.message_detail.is_some();
        let action_ready = has_message
            && !mailbox.message_detail_loading
            && !mailbox.selected_message_action_pending();
        let is_drafts = matches!(folder.kind, crate::model::mail::FolderKind::Drafts);
        let is_archive = matches!(folder.kind, crate::model::mail::FolderKind::Archive);
        let is_trash = matches!(folder.kind, crate::model::mail::FolderKind::Trash);

        if let Some(toggle) = self.actions.borrow().as_ref() {
            if !has_message {
                toggle.set_active(false);
            }
        }
        self.set_compose_actions(action_ready, if is_drafts { "Open Draft" } else { "Reply" });
        self.read_button.set_sensitive(action_ready && !is_drafts);
        self.star_button.set_sensitive(action_ready);
        self.archive_button
            .set_sensitive(action_ready && !is_archive && !is_trash);
        self.archive_button
            .set_label(if is_archive { "Archived" } else { "Archive" });
        self.trash_button.set_sensitive(action_ready && !is_trash);
        self.trash_button.set_label(if is_trash {
            "In Trash"
        } else {
            "Move to Trash"
        });
    }

    fn set_compose_actions(&self, enabled: bool, primary_label: &str) {
        self.reply_button.set_sensitive(enabled);
        self.reply_button.set_label(primary_label);
        self.reply_all_button
            .set_sensitive(enabled && primary_label == "Reply");
        self.forward_button.set_sensitive(enabled);
    }

    fn render_current(&self) {
        let detail = self.current_detail.borrow();
        let loading = *self.current_loading.borrow();
        let error = self.current_error.borrow().clone();
        if let Some(detail) = detail.as_ref() {
            self.subject.set_visible(true);
            self.meta.set_visible(true);
            self.recipients.set_visible(true);
            self.subject.set_label(&detail.subject);
            self.recipients
                .set_label(&format!("To: {}", detail.to.join(", ")));
            self.cc_recipients
                .set_label(&format!("Cc: {}", detail.cc.join(", ")));
            self.cc_recipients.set_visible(!detail.cc.is_empty());
            self.reply_to_value.set_label(&format!(
                "Reply-To: {}",
                detail.reply_to.as_deref().unwrap_or("Use sender address")
            ));
            self.reply_to_value.set_visible(detail.reply_to.is_some());
            rebuild_preview_attachment_list(
                &self.attachments_list,
                &detail.attachments,
                self.attachment_dispatch.borrow().clone(),
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
            self.body.set_label(detail.body.presentation_text());
            if self.mode_stack.visible_child_name().as_deref() == Some("html") {
                let content_key = (detail.conversation_id.clone(), html_fingerprint(html_body));
                if self.loaded_html.borrow().as_ref() != Some(&content_key) {
                    load_html_document(&self.html_view, html_body);
                    *self.loaded_html.borrow_mut() = Some(content_key);
                }
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
            self.attachments_label.set_visible(false);
            self.attachments_list.set_visible(false);
            rebuild_preview_attachment_list(&self.attachments_list, &[], None);
            self.mode_switcher.set_visible(false);
            self.mode_stack.set_visible(false);
            stop_html_loading(&self.html_view);
            *self.loaded_html.borrow_mut() = None;
            self.body.set_label("");
            self.error_page.set_description(error.as_deref());
        }
        if loading {
            self.loading_spinner.start();
            self.state_stack.set_visible_child_name("loading");
        } else {
            self.loading_spinner.stop();
            self.state_stack
                .set_visible_child_name(if detail.is_some() {
                    "message"
                } else if error.is_some() {
                    "error"
                } else {
                    "empty"
                });
        }
    }
}

fn html_fingerprint(html: &str) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    html.hash(&mut hasher);
    hasher.finish()
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
    disposition: AttachmentDisposition,
    attachment_name: String,
    source_uri: String,
    result: Result<Option<String>, String>,
) {
    let resolved_uri = match result {
        Ok(Some(uri)) => Some(uri),
        Ok(None) => (source_uri.contains("://")
            && !source_uri.starts_with("pigeon-eds-attachment:"))
        .then_some(source_uri),
        Err(error) => {
            toast_overlay.add_toast(adw::Toast::new(&error));
            None
        }
    };
    let Some(uri) = resolved_uri else {
        return;
    };

    match disposition {
        AttachmentDisposition::Open => {
            let launcher = gtk::FileLauncher::new(Some(&gio::File::for_uri(&uri)));
            let toast_overlay = toast_overlay.clone();
            launcher.launch(Some(parent), None::<&gio::Cancellable>, move |result| {
                let message = match result {
                    Ok(_) => format!("Opening {attachment_name}"),
                    Err(error) => {
                        crate::logging::report_failure("attachment-open", &error);
                        format!("Not opened: {attachment_name}")
                    }
                };
                toast_overlay.add_toast(adw::Toast::new(&message));
            });
        }
        AttachmentDisposition::SaveAs => {
            let dialog = gtk::FileDialog::builder()
                .title("Save attachment")
                .accept_label("Save")
                .initial_name(&attachment_name)
                .modal(true)
                .build();
            let parent = parent.clone();
            let toast_overlay = toast_overlay.clone();
            dialog.save(Some(&parent), None::<&gio::Cancellable>, move |result| {
                let Ok(target) = result else {
                    return;
                };
                let source = gio::File::for_uri(&uri);
                let copy_result = source
                    .path()
                    .zip(target.path())
                    .ok_or_else(|| {
                        "attachment source or destination is not a local path".to_string()
                    })
                    .and_then(|(source_path, target_path)| {
                        std::fs::copy(&source_path, &target_path)
                            .map(|_| ())
                            .map_err(|error| {
                                format!(
                                    "copy from '{}' to '{}' failed: {error}",
                                    source_path.display(),
                                    target_path.display()
                                )
                            })
                    });
                let message = match copy_result {
                    Ok(_) => format!("Saved {attachment_name}"),
                    Err(error) => {
                        crate::logging::report_failure("attachment-save", &error);
                        format!("Not saved: {attachment_name}")
                    }
                };
                toast_overlay.add_toast(adw::Toast::new(&message));
            });
        }
    }
}

fn rebuild_preview_attachment_list(
    list: &gtk::ListBox,
    attachments: &[crate::model::mail::AttachmentInfo],
    dispatch: Option<Rc<dyn Fn(AttachmentDisposition, crate::model::mail::AttachmentInfo)>>,
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
        let title = gtk::Button::builder()
            .halign(gtk::Align::Start)
            .css_classes(["flat"])
            .label(&attachment.display_name)
            .build();
        let subtitle_text = attachment_preview_subtitle(&attachment.uri);
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
        let save_button = gtk::Button::builder().label("Save As").build();
        actions.append(&save_button);
        box_row.append(&actions);

        if let Some(dispatch) = dispatch.clone() {
            let attachment = attachment.clone();
            title.connect_clicked(move |_| {
                dispatch(AttachmentDisposition::Open, attachment.clone());
            });
        }
        if let Some(dispatch) = dispatch.clone() {
            let attachment = attachment.clone();
            save_button.connect_clicked(move |_| {
                dispatch(AttachmentDisposition::SaveAs, attachment.clone());
            });
        }
        row.set_child(Some(&box_row));
        row.set_widget_name(&attachment.uri);
        row.set_activatable(false);
        list.append(&row);
    }
}

fn attachment_preview_subtitle(uri: &str) -> &str {
    if uri.starts_with("pigeon-eds-attachment:") {
        ""
    } else {
        uri
    }
}

#[cfg(test)]
mod tests {
    use super::{folder_icon_name, notification_belongs_to_active_account, thread_subject_label};
    use crate::model::account::MailAccountId;
    use crate::model::mail::{ConversationId, ConversationSummary, FolderKind};

    fn thread(subject: &str, message_count: u32) -> ConversationSummary {
        ConversationSummary {
            id: ConversationId("conversation-1".into()),
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
        assert_eq!(thread_subject_label(&thread("", 1)), "(No subject)");
        assert_eq!(thread_subject_label(&thread("  Subject  ", 1)), "Subject");
        assert_eq!(thread_subject_label(&thread("Subject", 4)), "Subject  (4)");
        assert_eq!(thread_subject_label(&thread("   ", 2)), "(No subject)  (2)");
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
    fn notifications_are_limited_to_the_active_account() {
        let active = MailAccountId("active-account".into());
        let inactive = MailAccountId("inactive-account".into());

        assert!(notification_belongs_to_active_account(
            Some(&active),
            &active,
        ));
        assert!(!notification_belongs_to_active_account(
            Some(&active),
            &inactive,
        ));
        assert!(!notification_belongs_to_active_account(None, &active));
    }
}
