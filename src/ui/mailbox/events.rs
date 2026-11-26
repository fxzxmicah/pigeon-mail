use std::rc::Rc;

use adw::prelude::*;
use futures::StreamExt;
use gtk::glib::variant::ToVariant;
use gtk::{gio, glib};

use crate::i18n::{gettext, ngettext_f};
use crate::model::event::MailEvent;
use crate::ui::attachment::handle_prepared_attachment;

use super::panels::{
    append_thread_row, rebuild_thread_panel, refresh_preview, refresh_preview_actions,
};
use super::{MainWindow, request_view_reload, state};

impl MainWindow {
    pub(super) fn bind_events(self: &Rc<Self>) {
        let mut events = self.coordinator.take_event_stream();
        let window = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while let Some(event) = events.next().await {
                let Some(window) = window.upgrade() else {
                    break;
                };
                window.handle_event(event);
            }
        });
    }

    fn handle_event(self: &Rc<Self>, event: MailEvent) {
        match event {
            MailEvent::AccountActivated {
                request_id,
                load,
            } => {
                let activation_applied = self
                    .mailbox
                    .borrow_mut()
                    .finish_account_activation(request_id, load)
                    .is_some();
                if activation_applied {
                    let (account_ready, status_summary) = {
                        let state = self.mailbox.borrow();
                        (state.account_ready(), state.status_summary())
                    };
                    self.update_account_controls(account_ready);
                    self.compose_page.mailbox_changed();
                    self.title.set_subtitle(&status_summary);
                    self.rebuild_lists();
                    self.show_deferred_folder();
                    self.present_mailto_queue();
                }
            }
            MailEvent::AccountRefreshCompleted {
                account_id,
                failure,
            } => {
                let reload = self
                    .mailbox
                    .borrow_mut()
                    .finish_account_refresh(&account_id, failure);
                if self
                    .mailbox
                    .borrow()
                    .current_account_id()
                    .as_ref()
                    == Some(&account_id)
                {
                    let status_summary = self.mailbox.borrow().status_summary();
                    self.title.set_subtitle(&status_summary);
                }
                if let Some(reload) = reload {
                    request_view_reload(&self.coordinator, reload);
                }
            }
            MailEvent::MailboxReloaded {
                request_id,
                result,
            } => {
                let outcome = self
                    .mailbox
                    .borrow_mut()
                    .finish_mailbox_reload(request_id, result);
                match outcome {
                    Some(state::MailboxReloadOutcome::Applied) => {
                        self.rebuild_lists();
                        self.show_deferred_folder();
                    }
                    Some(state::MailboxReloadOutcome::Failed(error)) => {
                        self.toast_overlay.add_toast(adw::Toast::new(&error));
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
                    self.mailbox.borrow().current_account_id().as_ref(),
                    &account_id,
                ) {
                    return;
                }
                let account_name = self
                    .mailbox
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
                if let Some(application) = self.inner.application() {
                    application.send_notification(
                        Some(&format!("new-mail-{}-{}", account_id.0, folder_id.0)),
                        &notification,
                    );
                }
            }
            MailEvent::MailboxCacheChanged { account_id } => {
                let reload = self
                    .mailbox
                    .borrow_mut()
                    .begin_cache_change(&account_id);
                if let Some(reload) = reload {
                    request_view_reload(&self.coordinator, reload);
                }
            }
            MailEvent::ThreadPageLoaded {
                request_id,
                result,
            } => {
                let error = result.as_ref().err().cloned();
                let outcome = self
                    .mailbox
                    .borrow_mut()
                    .finish_thread_page_load(request_id, result);
                if let Some(outcome) = outcome {
                    match outcome {
                        state::ThreadPageOutcome::Initial => {
                            rebuild_thread_panel(&self.mailbox, &self.thread_panel);
                        }
                        state::ThreadPageOutcome::Additional(added) => {
                            for thread in &added {
                                append_thread_row(&self.thread_panel.thread_list, thread);
                            }
                        }
                    }
                    if let Some(error) = error {
                        self.toast_overlay.add_toast(adw::Toast::new(&error));
                    }
                }
            }
            MailEvent::SearchCompleted {
                request_id,
                result,
            } => {
                let applied = self
                    .mailbox
                    .borrow_mut()
                    .finish_search(request_id, result);
                if applied {
                    rebuild_thread_panel(&self.mailbox, &self.thread_panel);
                }
            }
            MailEvent::MessageDetailLoaded {
                request_id,
                result,
            } => {
                let mut state = self.mailbox.borrow_mut();
                let applied = state.finish_message_detail_load(request_id, result);
                drop(state);
                if applied {
                    refresh_preview(
                        &self.mailbox,
                        &self.thread_panel.preview_widgets,
                    );
                }
            }
            MailEvent::MessageActionCompleted {
                request_id,
                result,
            } => {
                let outcome = self
                    .mailbox
                    .borrow_mut()
                    .finish_message_action(request_id, result);
                match outcome {
                    Some(state::MessageActionOutcome::Failed(error)) => {
                        refresh_preview_actions(
                            &self.mailbox,
                            &self.thread_panel.preview_widgets,
                        );
                        self.toast_overlay.add_toast(adw::Toast::new(&error));
                    }
                    Some(outcome) => {
                        if let state::MessageActionOutcome::SelectionChanged(request) =
                            outcome
                        {
                            self.coordinator.request_message_detail(
                                request.request_id,
                                request.account_id,
                                request.conversation_id,
                            );
                        }
                        self.rebuild_lists();
                    }
                    None => {}
                }
            }
            MailEvent::AccountIdentitiesSaveCompleted {
                request_id,
                result,
            } => {
                let settings_dialog = self.settings_dialog.borrow().clone();
                let handled = settings_dialog.is_some_and(|dialog| {
                    dialog.finish_save(request_id, &result)
                });
                if !handled && let Err(message) = result {
                    self.toast_overlay.add_toast(adw::Toast::new(&message));
                }
            }
            MailEvent::PendingWorkChanged => {
                self.update_pending_work_toast();
            }
            MailEvent::DraftSaveCompleted { result } => {
                self.compose_page.finish_save(result);
            }
            MailEvent::SendCompleted { result } => {
                self.compose_page.finish_send(result);
            }
            MailEvent::AttachmentPrepared {
                operation,
                display_name,
                result,
            } => {
                handle_prepared_attachment(
                    &self.inner,
                    &self.toast_overlay,
                    operation,
                    display_name,
                    result,
                );
            }
        }
    }

    fn update_pending_work_toast(self: &Rc<Self>) {
        let count = self.coordinator.pending_work_count();
        if count == 0 {
            let toast = self.pending_work_toast.borrow_mut().take();
            if let Some(toast) = toast {
                toast.dismiss();
            }
            return;
        }
        let current = self.pending_work_toast.borrow().clone();
        if let Some(toast) = current {
            toast.set_title(&pending_tasks_label(count));
            return;
        }

        let toast = adw::Toast::new(&pending_tasks_label(count));
        let window = Rc::downgrade(self);
        toast.connect_dismissed(move |dismissed| {
            let Some(window) = window.upgrade() else {
                return;
            };
            let mut current = window.pending_work_toast.borrow_mut();
            if current.as_ref() == Some(dismissed) {
                *current = None;
            }
        });
        *self.pending_work_toast.borrow_mut() = Some(toast.clone());
        self.toast_overlay.add_toast(toast);
    }
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

#[cfg(test)]
mod tests {
    use super::notification_belongs_to_current_account;
    use crate::model::account::MailAccountId;

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
