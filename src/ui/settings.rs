mod state;

use adw::prelude::*;
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::core::coordinator::MailCoordinator;
use crate::i18n::gettext;
use crate::integration::webkit::WebKitComposer;
use crate::model::account::{MailAccount, SendingIdentity, Signature};
use crate::model::event::RequestId;

use super::editor::DualFormatEditor;
use super::mailbox::MailboxViewModel;
use state::{SettingsDraftState, SettingsViewSnapshot};

type SettingsSaveCompletion = Box<dyn FnOnce(Result<(), String>)>;

struct SettingsSaveRequest {
    request_id: RequestId,
    completed: SettingsSaveCompletion,
}

#[derive(Clone, Default)]
struct SettingsSaveState {
    active: Rc<RefCell<Option<SettingsSaveRequest>>>,
}

impl SettingsSaveState {
    fn request(
        &self,
        coordinator: &MailCoordinator,
        accounts: Vec<MailAccount>,
        completed: impl FnOnce(Result<(), String>) + 'static,
    ) {
        let request_id = RequestId::next();
        let request = SettingsSaveRequest {
            request_id,
            completed: Box::new(completed),
        };
        assert!(
            self.active.borrow_mut().replace(request).is_none(),
            "settings saves must be single-flight"
        );
        coordinator.request_account_identities_save(request_id, accounts);
    }

    fn finish(
        &self,
        request_id: RequestId,
        result: Result<(), String>,
    ) -> bool {
        let mut active = self.active.borrow_mut();
        let Some(save) = active.as_ref() else {
            return false;
        };
        if save.request_id != request_id {
            return false;
        }
        let completed = active
            .take()
            .expect("the matched settings save exists")
            .completed;
        drop(active);
        completed(result);
        true
    }

    fn is_saving(&self) -> bool {
        self.active.borrow().is_some()
    }
}

type SharedSettingsDraftState = Rc<RefCell<SettingsDraftState>>;

#[derive(Clone)]
struct SettingsMainView {
    account: gtk::DropDown,
    name: gtk::Entry,
    aliases: gtk::ListBox,
    edit: gtk::Button,
    remove: gtk::Button,
    set_default: gtk::Button,
    projecting: Rc<Cell<bool>>,
}

impl SettingsMainView {
    fn is_projecting(&self) -> bool {
        self.projecting.get()
    }

    fn refresh(&self, snapshot: &SettingsViewSnapshot) {
        self.projecting.set(true);
        let names = gtk::StringList::new(
            &snapshot
                .account_names
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        );
        self.account.set_model(Some(&names));
        self.account
            .set_selected(snapshot.selected_account as u32);
        self.project_account(snapshot);
        self.projecting.set(false);
    }

    fn show_account(&self, snapshot: &SettingsViewSnapshot) {
        self.projecting.set(true);
        self.project_account(snapshot);
        self.projecting.set(false);
    }

    fn project_account(&self, snapshot: &SettingsViewSnapshot) {
        let account = snapshot.account.as_ref();
        let display_name = snapshot
            .account_names
            .get(snapshot.selected_account)
            .map(String::as_str)
            .unwrap_or("");
        if self.name.text().as_str() != display_name {
            self.name.set_text(display_name);
        }
        rebuild_alias_list(&self.aliases, account);
        select_alias_row(
            &self.aliases,
            account,
            snapshot.selected_address.as_deref(),
        );
        self.update_actions(snapshot);
    }

    fn update_actions(&self, snapshot: &SettingsViewSnapshot) {
        let role = snapshot.account.as_ref().and_then(|account| {
            snapshot.selected_address.as_deref().map(|address| {
                (
                    account.is_primary_identity(address),
                    account.is_default_identity(address),
                )
            })
        });
        self.edit.set_sensitive(role.is_some());
        self.remove
            .set_sensitive(role.is_some_and(|(primary, default)| !primary && !default));
        self.set_default
            .set_sensitive(role.is_some_and(|(_, default)| !default));
    }
}

#[derive(Clone)]
struct AliasEditorView {
    page: adw::NavigationPage,
    name: gtk::Entry,
    address: gtk::Entry,
    reply_to: gtk::Entry,
    composer: Rc<WebKitComposer>,
    text: gtk::TextView,
}

impl AliasEditorView {
    fn load(&self, account: &MailAccount, identity: &SendingIdentity) {
        self.name.set_text(&identity.display_name);
        self.address.set_text(&identity.address);
        self.address
            .set_sensitive(!account.is_primary_identity(&identity.address));
        self.reply_to
            .set_text(identity.reply_to.as_deref().unwrap_or(""));
        self.composer
            .set_content(&identity.signature.html, &identity.signature.text);
        self.text.buffer().set_text(&identity.signature.text);
    }

    fn clear(&self) {
        self.name.set_text("");
        self.address.set_text("");
        self.address.set_sensitive(true);
        self.reply_to.set_text("");
        self.composer.set_content("", "");
        self.text.buffer().set_text("");
    }
}

#[derive(Clone)]
pub(super) struct SettingsDialog {
    inner: adw::Window,
    saves: SettingsSaveState,
    mailbox: Rc<RefCell<MailboxViewModel>>,
    state: SharedSettingsDraftState,
    main_view: SettingsMainView,
    editor_view: AliasEditorView,
}

impl SettingsDialog {
    pub(super) fn present(&self) {
        self.inner.present();
    }

    pub(super) fn finish_save(
        &self,
        request_id: RequestId,
        result: Result<(), String>,
    ) -> bool {
        self.saves.finish(request_id, result)
    }

    pub(super) fn connect_close(&self, closed: impl Fn() + 'static) {
        let saves = self.saves.clone();
        self.inner.connect_close_request(move |_| {
            if saves.is_saving() {
                glib::Propagation::Stop
            } else {
                closed();
                glib::Propagation::Proceed
            }
        });
    }

    pub(super) fn mailbox_changed(&self) {
        let accounts = self.mailbox.borrow().accounts().to_vec();
        let mut state = self.state.borrow_mut();
        let editor_identity = state.rebase(accounts);
        let snapshot = state.view_snapshot();
        drop(state);
        self.main_view.refresh(&snapshot);
        if let Some((account, identity)) = editor_identity {
            self.editor_view.page.set_title(&gettext("Edit Alias"));
            self.editor_view.load(&account, &identity);
        }
    }
}

pub(super) fn build_settings(
    parent: &adw::ApplicationWindow,
    mailbox_state: Rc<RefCell<MailboxViewModel>>,
    coordinator: MailCoordinator,
) -> SettingsDialog {
    let window = adw::Window::builder()
        .transient_for(parent)
        .modal(true)
        .title(gettext("Mail Settings"))
        .default_width(640)
        .default_height(560)
        .build();
    let initial = mailbox_state.borrow().selected_account_index();
    let accounts = mailbox_state.borrow().accounts().to_vec();
    let state = Rc::new(RefCell::new(SettingsDraftState::new(accounts, initial)));
    let saves = SettingsSaveState::default();

    let account_dropdown = gtk::DropDown::new(None::<gtk::StringList>, None::<gtk::Expression>);
    let account_name_entry = gtk::Entry::builder().hexpand(true).build();
    let account_name_label = gtk::Label::builder()
        .label(gettext("Account _Name"))
        .use_underline(true)
        .xalign(0.0)
        .css_classes(["mail-form-label"])
        .build();
    account_name_label.set_mnemonic_widget(Some(&account_name_entry));

    let alias_list = gtk::ListBox::builder().css_classes(["boxed-list"]).build();
    let add_alias_button = gtk::Button::with_label(&gettext("Add"));
    let edit_alias_button = gtk::Button::with_label(&gettext("Edit"));
    let remove_alias_button = gtk::Button::with_label(&gettext("Remove"));
    let default_alias_button = gtk::Button::with_label(&gettext("Set Default"));
    let alias_actions = adw::WrapBox::builder()
        .child_spacing(8)
        .line_spacing(8)
        .build();
    for button in [
        &add_alias_button,
        &edit_alias_button,
        &remove_alias_button,
        &default_alias_button,
    ] {
        alias_actions.append(button);
    }
    let alias_scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .min_content_height(220)
        .child(&alias_list)
        .build();
    let alias_frame = gtk::Frame::builder()
        .css_classes(["compact-list-frame"])
        .vexpand(true)
        .child(&alias_scroller)
        .build();
    let main_page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(14)
        .margin_top(14)
        .margin_bottom(14)
        .margin_start(14)
        .margin_end(14)
        .build();
    let name_row = gtk::Grid::builder().column_spacing(12).build();
    name_row.attach(&account_name_label, 0, 0, 1, 1);
    name_row.attach(&account_name_entry, 1, 0, 1, 1);
    let alias_header = gtk::Label::builder()
        .label(gettext("Aliases"))
        .xalign(0.0)
        .css_classes(["title-3"])
        .margin_top(4)
        .build();
    main_page.append(&name_row);
    main_page.append(&alias_header);
    main_page.append(&alias_frame);
    main_page.append(&alias_actions);

    let username_entry = gtk::Entry::builder().hexpand(true).build();
    let email_entry = gtk::Entry::builder().hexpand(true).build();
    let reply_to_entry = gtk::Entry::builder()
        .placeholder_text(gettext("Optional Reply-To"))
        .hexpand(true)
        .build();
    let DualFormatEditor {
        composer: signature_composer,
        text_editor: signature_text_editor,
        stack: signature_stack,
        convert_button: convert_signature_button,
        mode_controls: signature_mode_controls,
        frame: signature_frame,
    } = DualFormatEditor::new(&gettext("Signature"));
    let signature_section = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .vexpand(true)
        .build();
    signature_section.append(&signature_mode_controls);
    signature_section.append(&signature_frame);
    let apply_alias_button = gtk::Button::builder()
        .label(gettext("Apply"))
        .css_classes(["suggested-action"])
        .halign(gtk::Align::End)
        .build();
    let editor_form = gtk::Grid::builder()
        .hexpand(true)
        .row_spacing(8)
        .column_spacing(12)
        .build();
    for (row, label_text, entry) in [
        (0, gettext("_Name"), &username_entry),
        (1, gettext("_Address"), &email_entry),
        (2, gettext("_Reply-To"), &reply_to_entry),
    ] {
        let label = gtk::Label::builder()
            .label(&label_text)
            .use_underline(true)
            .xalign(0.0)
            .css_classes(["mail-form-label"])
            .build();
        label.set_mnemonic_widget(Some(entry));
        editor_form.attach(&label, 0, row, 1, 1);
        editor_form.attach(entry, 1, row, 1, 1);
    }
    let edit_page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(14)
        .margin_top(14)
        .margin_bottom(14)
        .margin_start(14)
        .margin_end(14)
        .build();
    edit_page.append(&editor_form);
    edit_page.append(&signature_section);
    edit_page.append(&apply_alias_button);

    let main_header = adw::HeaderBar::new();
    main_header.pack_start(&account_dropdown);
    let save_button = gtk::Button::builder()
        .label(gettext("Save"))
        .css_classes(["suggested-action"])
        .build();
    main_header.pack_end(&save_button);
    let main_toolbar = adw::ToolbarView::new();
    main_toolbar.add_top_bar(&main_header);
    let main_scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&main_page)
        .build();
    main_toolbar.set_content(Some(&main_scroller));

    let editor_toolbar = adw::ToolbarView::new();
    editor_toolbar.add_top_bar(&adw::HeaderBar::new());
    let editor_scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&edit_page)
        .build();
    editor_toolbar.set_content(Some(&editor_scroller));

    let main_navigation_page =
        adw::NavigationPage::with_tag(&main_toolbar, &gettext("Mail Settings"), "main");
    let editor_navigation_page =
        adw::NavigationPage::with_tag(&editor_toolbar, &gettext("Edit Alias"), "editor");
    let navigation = adw::NavigationView::new();
    navigation.add(&main_navigation_page);
    navigation.add(&editor_navigation_page);
    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&navigation));
    window.set_content(Some(&toast_overlay));

    let main_view = SettingsMainView {
        account: account_dropdown.clone(),
        name: account_name_entry.clone(),
        aliases: alias_list.clone(),
        edit: edit_alias_button.clone(),
        remove: remove_alias_button.clone(),
        set_default: default_alias_button.clone(),
        projecting: Rc::new(Cell::new(false)),
    };
    let editor_view = AliasEditorView {
        page: editor_navigation_page.clone(),
        name: username_entry.clone(),
        address: email_entry.clone(),
        reply_to: reply_to_entry.clone(),
        composer: Rc::clone(&signature_composer),
        text: signature_text_editor.clone(),
    };
    {
        let state = state.borrow();
        let snapshot = state.view_snapshot();
        drop(state);
        main_view.refresh(&snapshot);
    }

    let state_for_entry = Rc::clone(&state);
    let main_view_for_entry = main_view.clone();
    let dropdown_for_entry = account_dropdown.clone();
    account_name_entry.connect_changed(move |entry| {
        if main_view_for_entry.is_projecting() {
            return;
        }
        let name = entry.text().to_string();
        let mut state = state_for_entry.borrow_mut();
        let Some(selected) = state.rename_selected(name.clone()) else {
            return;
        };
        drop(state);
        if let Some(names) = dropdown_for_entry.model()
            .and_then(|model| model.downcast::<gtk::StringList>().ok())
        {
            names.splice(selected as u32, 1, &[&name]);
        }
    });

    let composer = Rc::clone(&signature_composer);
    let text = signature_text_editor.clone();
    let stack = signature_stack.clone();
    let nav = navigation.clone();
    let toast = toast_overlay.clone();
    convert_signature_button.connect_clicked(move |_| {
        if stack.visible_child_name().as_deref() == Some("text") {
            let buffer = text.buffer();
            let value = buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), true)
                .to_string();
            composer.set_content(
                &crate::model::mail::plain_text_to_html(&value),
                &value,
            );
            stack.set_visible_child_name("html");
        } else {
            nav.set_sensitive(false);
            let nav = nav.clone();
            let text = text.clone();
            let stack = stack.clone();
            let toast = toast.clone();
            composer.capture_content(move |content| {
                nav.set_sensitive(true);
                match content {
                    Ok(content) => {
                        text.buffer().set_text(&content.text);
                        stack.set_visible_child_name("text");
                    }
                    Err(()) => {
                        toast.add_toast(adw::Toast::new(&gettext("Signature not converted.")));
                    }
                }
            });
        }
    });

    let state_for_account = Rc::clone(&state);
    let view_for_account = main_view.clone();
    account_dropdown.connect_selected_notify(move |dropdown| {
        if view_for_account.is_projecting() {
            return;
        }
        let selected = dropdown.selected() as usize;
        let mut state = state_for_account.borrow_mut();
        if !state.select_account(selected) {
            return;
        }
        let snapshot = state.view_snapshot();
        drop(state);
        view_for_account.show_account(&snapshot);
    });

    let state_for_selection = Rc::clone(&state);
    let view_for_selection = main_view.clone();
    alias_list.connect_row_selected(move |_, row| {
        if view_for_selection.is_projecting() {
            return;
        }
        let mut state = state_for_selection.borrow_mut();
        state.select_identity(row.map(|row| row.index() as usize));
        let snapshot = state.view_snapshot();
        drop(state);
        view_for_selection.update_actions(&snapshot);
    });

    let state_for_edit = Rc::clone(&state);
    let nav_for_edit = navigation.clone();
    let editor_for_edit = editor_view.clone();
    edit_alias_button.connect_clicked(move |_| {
        let Some((account, identity)) = state_for_edit.borrow_mut().begin_edit() else {
            return;
        };
        editor_for_edit.load(&account, &identity);
        editor_for_edit.page.set_title(&gettext("Edit Alias"));
        nav_for_edit.push_by_tag("editor");
    });

    let state_for_add = Rc::clone(&state);
    let nav_for_add = navigation.clone();
    let editor_for_add = editor_view.clone();
    add_alias_button.connect_clicked(move |_| {
        let started = state_for_add.borrow_mut().begin_add();
        if !started {
            return;
        }
        editor_for_add.clear();
        editor_for_add.page.set_title(&gettext("Add Alias"));
        nav_for_add.push_by_tag("editor");
        editor_for_add.name.grab_focus();
    });
    let apply_for_validation = apply_alias_button.clone();
    email_entry.connect_changed(move |entry| {
        apply_for_validation.set_sensitive(!entry.text().trim().is_empty());
    });

    let state_for_apply = Rc::clone(&state);
    let view_for_apply = main_view.clone();
    let editor_for_apply = editor_view.clone();
    let nav_for_apply = navigation.clone();
    let toast_for_apply = toast_overlay.clone();
    apply_alias_button.connect_clicked(move |_| {
        let username = editor_for_apply.name.text().to_string();
        let address = editor_for_apply.address.text().to_string();
        let reply = Some(editor_for_apply.reply_to.text().to_string());
        let signature_text = {
            let buffer = editor_for_apply.text.buffer();
            buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), true)
                .to_string()
        };
        let Some(target) = state_for_apply.borrow().editor_target() else {
            return;
        };
        let state = Rc::clone(&state_for_apply);
        let view = view_for_apply.clone();
        let nav = nav_for_apply.clone();
        let toast = toast_for_apply.clone();
        nav_for_apply.set_sensitive(false);
        editor_for_apply.composer.capture_content(move |content| {
            nav.set_sensitive(true);
            let Ok(content) = content else {
                toast.add_toast(adw::Toast::new(&gettext("Alias not changed.")));
                return;
            };
            let identity = SendingIdentity::new(
                address,
                username,
                reply,
                Signature {
                    html: content.html,
                    text: signature_text,
                },
            );
            let mut state = state.borrow_mut();
            if !state.apply_editor(&target, identity) {
                drop(state);
                toast.add_toast(adw::Toast::new(&gettext("Alias not changed.")));
                return;
            }
            let snapshot = state.view_snapshot();
            drop(state);
            view.show_account(&snapshot);
            nav.pop();
        });
    });

    let state_for_remove = Rc::clone(&state);
    let view_for_remove = main_view.clone();
    remove_alias_button.connect_clicked(move |_| {
        let mut state = state_for_remove.borrow_mut();
        if !state.remove_selected_identity() {
            return;
        }
        let snapshot = state.view_snapshot();
        drop(state);
        view_for_remove.show_account(&snapshot);
    });

    let state_for_default = Rc::clone(&state);
    let view_for_default = main_view.clone();
    default_alias_button.connect_clicked(move |_| {
        let mut state = state_for_default.borrow_mut();
        if !state.set_selected_default() {
            return;
        }
        let snapshot = state.view_snapshot();
        drop(state);
        view_for_default.show_account(&snapshot);
    });

    let state_for_save = Rc::clone(&state);
    let saves_for_save = saves.clone();
    let coordinator_for_save = coordinator.clone();
    let nav_for_save = navigation.clone();
    let toast_for_save = toast_overlay.clone();
    save_button.connect_clicked(move |_| {
        let accounts = state_for_save.borrow().accounts_to_save();
        let Some(accounts) = accounts else {
            toast_for_save.add_toast(adw::Toast::new(&gettext("Account name is empty.")));
            return;
        };
        nav_for_save.set_sensitive(false);
        let nav = nav_for_save.clone();
        let toast = toast_for_save.clone();
        let state = Rc::clone(&state_for_save);
        let submitted = accounts.clone();
        saves_for_save.request(&coordinator_for_save, accounts, move |result| {
            nav.set_sensitive(true);
            match result {
                Ok(()) => state.borrow_mut().record_saved(submitted),
                Err(message) => toast.add_toast(adw::Toast::new(&message)),
            }
        });
    });

    let state_for_pop = Rc::clone(&state);
    navigation.connect_popped(move |_, page| {
        if page.tag().as_deref() == Some("editor") {
            state_for_pop.borrow_mut().finish_editor();
        }
    });

    SettingsDialog {
        inner: window,
        saves,
        mailbox: mailbox_state,
        state,
        main_view,
        editor_view,
    }
}

fn rebuild_alias_list(list: &gtk::ListBox, account: Option<&MailAccount>) {
    list.remove_all();
    let Some(account) = account else {
        return;
    };
    for identity in account.aliases() {
        let row = gtk::ListBoxRow::new();
        let line = gtk::Box::builder()
            .spacing(10)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(12)
            .margin_end(12)
            .build();
        let title = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .label(&format!(
                "{} ({})",
                identity.display_name_or_address(),
                identity.address
            ))
            .build();
        line.append(&title);
        if account.is_default_identity(&identity.address) {
            let badge = gtk::Label::builder()
                .label(gettext("Default"))
                .css_classes(["accent", "caption"])
                .valign(gtk::Align::Center)
                .build();
            line.append(&badge);
        }
        row.set_child(Some(&line));
        list.append(&row);
    }
}

fn select_alias_row(list: &gtk::ListBox, account: Option<&MailAccount>, address: Option<&str>) {
    let index = account.and_then(|account| {
        address.and_then(|address| {
            account.aliases().iter().position(|identity| {
                crate::model::address::normalized_mailbox_address(&identity.address)
                    == crate::model::address::normalized_mailbox_address(address)
            })
        })
    });
    list.select_row(index.and_then(|index| list.row_at_index(index as i32)).as_ref());
}
