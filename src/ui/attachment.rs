use gtk::{gio, glib, pango, prelude::*};

use crate::i18n::{gettext, gettext_f};
use crate::model::mail::AttachmentOperation;

pub(super) fn file_selection<T>(
    result: Result<T, gtk::glib::Error>,
    operation: &'static str,
    toast: &adw::ToastOverlay,
    failure_message: impl FnOnce() -> String,
) -> Option<T> {
    match result {
        Ok(selection) => {
            Some(selection)
        }
        Err(error) if file_dialog_cancelled(&error) => {
            None
        }
        Err(error) => {
            crate::logging::report_failure(operation, &error.into());
            toast.add_toast(adw::Toast::new(&failure_message()));
            None
        }
    }
}

fn file_dialog_cancelled(error: &gtk::glib::Error) -> bool {
    error.matches(gtk::DialogError::Dismissed) || error.matches(gtk::DialogError::Cancelled)
}

pub(super) fn viewport(list: &gtk::ListBox) -> gtk::Overlay {
    // The themed compound rows reach this ceiling at roughly two and a half
    // items, leaving the partial row as the overflow cue.
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::External)
        .propagate_natural_height(true)
        .max_content_height(225)
        .child(list)
        .build();
    let adjustment = scroller.vadjustment();
    let scrollbar = gtk::Scrollbar::new(gtk::Orientation::Vertical, Some(&adjustment));
    scrollbar.set_halign(gtk::Align::End);
    scrollbar.set_valign(gtk::Align::Fill);
    scrollbar.set_visible(false);
    let scrollbar_for_adjustment = scrollbar.downgrade();
    adjustment.connect_changed(move |adjustment| {
        let Some(scrollbar) = scrollbar_for_adjustment.upgrade() else {
            return;
        };
        let page_size = adjustment.page_size();
        scrollbar.set_visible(page_size > 0.0 && adjustment.upper() > page_size + 0.5);
    });

    let viewport = gtk::Overlay::new();
    viewport.set_child(Some(&scroller));
    viewport.add_overlay(&scrollbar);
    viewport
}

pub(super) fn row(
    icon_name: &str,
    label: &str,
    trailing: Option<&gtk::Widget>,
) -> (gtk::ListBoxRow, gtk::Button) {
    let content = gtk::Box::builder()
        .spacing(12)
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(12)
        .margin_end(12)
        .build();
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.add_css_class("dim-label");
    let title_label = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .lines(1)
        .label(label)
        .build();
    let open_content = gtk::Box::builder().spacing(12).build();
    open_content.append(&icon);
    open_content.append(&title_label);
    let open = gtk::Button::builder()
        .can_shrink(true)
        .css_classes(["flat"])
        .child(&open_content)
        .build();
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    content.append(&open);
    content.append(&spacer);
    if let Some(trailing) = trailing {
        content.append(trailing);
    }

    let row = gtk::ListBoxRow::new();
    row.set_activatable(false);
    row.set_child(Some(&content));
    (row, open)
}

pub(super) fn handle_prepared_attachment(
    parent: &adw::ApplicationWindow,
    toast_overlay: &adw::ToastOverlay,
    operation: AttachmentOperation,
    attachment_name: String,
    result: Result<String, String>,
) {
    let uri = match result {
        Ok(uri) => {
            uri
        }
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
                    Ok(_) => {
                        gettext_f(
                            "Opening {attachment}",
                            &[("attachment", &attachment_name)],
                        )
                    }
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
                let Some(target) = file_selection(
                    result,
                    "attachment-save-choose",
                    &toast_overlay,
                    || gettext_f("Not saved: {attachment}", &[("attachment", &attachment_name)]),
                ) else {
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
                            Ok(_) => {
                                gettext_f(
                                    "Saved {attachment}",
                                    &[("attachment", &attachment_name)],
                                )
                            }
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
    use super::file_dialog_cancelled;
    use gtk::{DialogError, glib};

    #[test]
    fn only_dismissal_and_cancellation_are_silent() {
        assert!(file_dialog_cancelled(&glib::Error::new(DialogError::Dismissed, "")));
        assert!(file_dialog_cancelled(&glib::Error::new(DialogError::Cancelled, "")));
        assert!(!file_dialog_cancelled(&glib::Error::new(DialogError::Failed, "")));
    }
}
