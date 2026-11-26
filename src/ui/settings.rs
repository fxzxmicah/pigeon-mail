mod state;

use adw::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::core::coordinator::MailCoordinator;
use crate::i18n::gettext;
use crate::integration::webkit::WebKitComposer;
use crate::model::account::{AccountIdentityEdit, MailAccount, SendingIdentity, Signature};
use crate::model::event::RequestId;

use super::editor::DualFormatEditor;
use state::{SettingsDraftState, SettingsViewSnapshot};

struct SettingsSaveRequest {
    request_id: RequestId,
    submitted: Vec<AccountIdentityEdit>,
}

struct SettingsMainView {
    account: gtk::DropDown,
    name: adw::EntryRow,
    aliases: gtk::ListBox,
    edit: gtk::Button,
    remove: gtk::Button,
    set_default: gtk::Button,
    projecting: Cell<bool>,
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

struct AliasEditorView {
    page: adw::NavigationPage,
    name: adw::EntryRow,
    address: adw::EntryRow,
    reply_to: adw::EntryRow,
    composer: Rc<WebKitComposer>,
    text: gtk::TextView,
    stack: adw::ViewStack,
}

impl AliasEditorView {
    fn signature_text(&self) -> String {
        let buffer = self.text.buffer();
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), true)
            .to_string()
    }

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

pub(super) struct SettingsDialog {
    inner: adw::Dialog,
    navigation: adw::NavigationView,
    toast: adw::ToastOverlay,
    save: RefCell<Option<SettingsSaveRequest>>,
    state: RefCell<SettingsDraftState>,
    main_view: SettingsMainView,
    editor_view: AliasEditorView,
}

impl SettingsDialog {
    pub(super) fn present(&self, parent: &impl IsA<gtk::Widget>) {
        self.inner.present(Some(parent));
    }

    fn save(&self, coordinator: &MailCoordinator) {
        let edits = self.state.borrow().edits_to_save();
        let Some(edits) = edits else {
            self.toast.add_toast(adw::Toast::new(&gettext("Account name is empty.")));
            return;
        };
        if edits.is_empty() {
            return;
        }
        let request_id = RequestId::next();
        let request = SettingsSaveRequest {
            request_id,
            submitted: edits.clone(),
        };
        assert!(
            self.save.borrow_mut().replace(request).is_none(),
            "settings saves must be single-flight"
        );
        self.inner.set_can_close(false);
        self.navigation.set_sensitive(false);
        coordinator.request_account_identities_save(request_id, edits);
    }

    pub(super) fn finish_save(
        &self,
        request_id: RequestId,
        result: &Result<(), String>,
    ) -> bool {
        let mut pending = self.save.borrow_mut();
        if !pending.as_ref().is_some_and(|save| save.request_id == request_id) {
            return false;
        }
        let save = pending.take().expect("the matched settings save exists");
        drop(pending);
        match result {
            Ok(()) => {
                self.state.borrow_mut().record_saved(save.submitted);
            }
            Err(message) => {
                self.toast.add_toast(adw::Toast::new(message));
            }
        }
        self.inner.set_can_close(true);
        self.navigation.set_sensitive(true);
        true
    }

    pub(super) fn connect_closed(&self, closed: impl Fn() + 'static) {
        self.inner.connect_closed(move |_| closed());
    }

    pub(super) fn update_account_catalog(&self, accounts: Vec<MailAccount>) {
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
    accounts: Vec<MailAccount>,
    initial_account: usize,
    coordinator: MailCoordinator,
) -> Rc<SettingsDialog> {
    let dialog = adw::Dialog::builder()
        .title(gettext("Mail Settings"))
        .content_width(640)
        .content_height(560)
        .build();
    let state = RefCell::new(SettingsDraftState::new(accounts, initial_account));

    let account_dropdown = gtk::DropDown::new(None::<gtk::StringList>, None::<gtk::Expression>);
    let account_name_row = adw::EntryRow::builder()
        .title(gettext("Account _Name"))
        .use_underline(true)
        .build();
    let account_group = adw::PreferencesGroup::new();
    account_group.add(&account_name_row);

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
    let alias_group = adw::PreferencesGroup::builder()
        .title(gettext("Aliases"))
        .build();
    let alias_content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(14)
        .vexpand(true)
        .build();
    alias_content.append(&alias_frame);
    alias_content.append(&alias_actions);
    alias_group.add(&alias_content);
    main_page.append(&account_group);
    main_page.append(&alias_group);

    let name_row = adw::EntryRow::builder()
        .title(gettext("_Name"))
        .use_underline(true)
        .build();
    let address_row = adw::EntryRow::builder()
        .title(gettext("_Address"))
        .use_underline(true)
        .input_purpose(gtk::InputPurpose::Email)
        .build();
    let reply_to_row = adw::EntryRow::builder()
        .title(gettext("_Reply-To"))
        .use_underline(true)
        .input_purpose(gtk::InputPurpose::Email)
        .build();
    let identity_group = adw::PreferencesGroup::new();
    identity_group.add(&name_row);
    identity_group.add(&address_row);
    identity_group.add(&reply_to_row);
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
    let edit_page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(14)
        .margin_top(14)
        .margin_bottom(14)
        .margin_start(14)
        .margin_end(14)
        .build();
    edit_page.append(&identity_group);
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
    dialog.set_child(Some(&toast_overlay));

    let settings = Rc::new(SettingsDialog {
        inner: dialog,
        navigation: navigation.clone(),
        toast: toast_overlay,
        save: RefCell::new(None),
        state,
        main_view: SettingsMainView {
            account: account_dropdown.clone(),
            name: account_name_row.clone(),
            aliases: alias_list.clone(),
            edit: edit_alias_button.clone(),
            remove: remove_alias_button.clone(),
            set_default: default_alias_button.clone(),
            projecting: Cell::new(false),
        },
        editor_view: AliasEditorView {
            page: editor_navigation_page,
            name: name_row,
            address: address_row.clone(),
            reply_to: reply_to_row,
            composer: signature_composer,
            text: signature_text_editor,
            stack: signature_stack,
        },
    });
    let initial = settings.state.borrow().view_snapshot();
    settings.main_view.refresh(&initial);

    let session = Rc::downgrade(&settings);
    account_name_row.connect_changed(move |row| {
        let Some(settings) = session.upgrade() else {
            return;
        };
        if settings.main_view.is_projecting() {
            return;
        }
        let name = row.text().to_string();
        let selected = settings.state.borrow_mut().rename_selected(name.clone());
        if let Some(selected) = selected
            && let Some(names) = settings.main_view.account.model()
                .and_then(|model| model.downcast::<gtk::StringList>().ok())
        {
            names.splice(selected as u32, 1, &[&name]);
        }
    });

    let session = Rc::downgrade(&settings);
    convert_signature_button.connect_clicked(move |_| {
        let Some(settings) = session.upgrade() else {
            return;
        };
        let editor = &settings.editor_view;
        if editor.stack.visible_child_name().as_deref() == Some("text") {
            let text = editor.signature_text();
            editor.composer.set_content(&crate::model::mail::plain_text_to_html(&text), &text);
            editor.stack.set_visible_child_name("html");
        } else {
            settings.navigation.set_sensitive(false);
            let session = session.clone();
            editor.composer.capture_content(move |content| {
                let Some(settings) = session.upgrade() else {
                    return;
                };
                match content {
                    Ok(content) => {
                        let editor = &settings.editor_view;
                        editor.text.buffer().set_text(&content.text);
                        editor.stack.set_visible_child_name("text");
                    }
                    Err(()) => {
                        settings.toast.add_toast(adw::Toast::new(
                            &gettext("Signature not converted."),
                        ));
                    }
                }
                settings.navigation.set_sensitive(true);
            });
        }
    });

    let session = Rc::downgrade(&settings);
    account_dropdown.connect_selected_notify(move |dropdown| {
        let Some(settings) = session.upgrade() else {
            return;
        };
        if settings.main_view.is_projecting() {
            return;
        }
        let mut state = settings.state.borrow_mut();
        if !state.select_account(dropdown.selected() as usize) {
            return;
        }
        let snapshot = state.view_snapshot();
        drop(state);
        settings.main_view.show_account(&snapshot);
    });

    let session = Rc::downgrade(&settings);
    alias_list.connect_row_selected(move |_, row| {
        let Some(settings) = session.upgrade() else {
            return;
        };
        if settings.main_view.is_projecting() {
            return;
        }
        let mut state = settings.state.borrow_mut();
        state.select_identity(row.map(|row| row.index() as usize));
        let snapshot = state.view_snapshot();
        drop(state);
        settings.main_view.update_actions(&snapshot);
    });

    let session = Rc::downgrade(&settings);
    edit_alias_button.connect_clicked(move |_| {
        let Some(settings) = session.upgrade() else {
            return;
        };
        let Some((account, identity)) = settings.state.borrow_mut().begin_edit() else {
            return;
        };
        let editor = &settings.editor_view;
        editor.load(&account, &identity);
        editor.page.set_title(&gettext("Edit Alias"));
        settings.navigation.push_by_tag("editor");
    });

    let session = Rc::downgrade(&settings);
    add_alias_button.connect_clicked(move |_| {
        let Some(settings) = session.upgrade() else {
            return;
        };
        if !settings.state.borrow_mut().begin_add() {
            return;
        }
        let editor = &settings.editor_view;
        editor.clear();
        editor.page.set_title(&gettext("Add Alias"));
        settings.navigation.push_by_tag("editor");
        editor.name.grab_focus();
    });
    let apply_for_validation = apply_alias_button.clone();
    address_row.connect_changed(move |row| {
        apply_for_validation.set_sensitive(!row.text().trim().is_empty());
    });

    let session = Rc::downgrade(&settings);
    apply_alias_button.connect_clicked(move |_| {
        let Some(settings) = session.upgrade() else {
            return;
        };
        let Some(target) = settings.state.borrow().editor_target() else {
            return;
        };
        let editor = &settings.editor_view;
        let name = editor.name.text().to_string();
        let address = editor.address.text().to_string();
        let reply_to = Some(editor.reply_to.text().to_string());
        let text = editor.signature_text();
        let session = session.clone();
        settings.navigation.set_sensitive(false);
        editor.composer.capture_content(move |content| {
            let Some(settings) = session.upgrade() else {
                return;
            };
            let applied = content.is_ok_and(|content| {
                let identity = SendingIdentity::new(
                    address,
                    name,
                    reply_to,
                    Signature { html: content.html, text },
                );
                settings.state.borrow_mut().apply_editor(&target, identity)
            });
            settings.navigation.set_sensitive(true);
            if applied {
                let snapshot = settings.state.borrow().view_snapshot();
                settings.main_view.show_account(&snapshot);
                settings.navigation.pop();
            } else {
                settings.toast.add_toast(adw::Toast::new(&gettext("Alias not changed.")));
            }
        });
    });

    let session = Rc::downgrade(&settings);
    remove_alias_button.connect_clicked(move |_| {
        let Some(settings) = session.upgrade() else {
            return;
        };
        let mut state = settings.state.borrow_mut();
        if !state.remove_selected_identity() {
            return;
        }
        let snapshot = state.view_snapshot();
        drop(state);
        settings.main_view.show_account(&snapshot);
    });

    let session = Rc::downgrade(&settings);
    default_alias_button.connect_clicked(move |_| {
        let Some(settings) = session.upgrade() else {
            return;
        };
        let mut state = settings.state.borrow_mut();
        if !state.set_selected_default() {
            return;
        }
        let snapshot = state.view_snapshot();
        drop(state);
        settings.main_view.show_account(&snapshot);
    });

    let session = Rc::downgrade(&settings);
    save_button.connect_clicked(move |_| {
        if let Some(settings) = session.upgrade() {
            settings.save(&coordinator);
        }
    });

    let session = Rc::downgrade(&settings);
    navigation.connect_popped(move |_, page| {
        if page.tag().as_deref() == Some("editor")
            && let Some(settings) = session.upgrade()
        {
            settings.state.borrow_mut().finish_editor();
        }
    });

    settings
}

fn rebuild_alias_list(list: &gtk::ListBox, account: Option<&MailAccount>) {
    list.remove_all();
    let Some(account) = account else {
        return;
    };
    for identity in account.aliases() {
        let row = adw::ActionRow::builder()
            .use_markup(false)
            .title(identity.display_name_or_address())
            .build();
        if !identity.display_name.is_empty() {
            row.set_subtitle(&identity.address);
        }
        if account.is_default_identity(&identity.address) {
            let badge = gtk::Label::builder()
                .label(gettext("Default"))
                .css_classes(["accent", "caption"])
                .valign(gtk::Align::Center)
                .build();
            row.add_suffix(&badge);
        }
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
