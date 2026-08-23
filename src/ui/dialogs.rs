use adw::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

use super::mailbox::{MailboxViewModel, SidebarWidgets, rebuild_sidebar, refresh_account_dropdown};

pub(super) fn build_settings(
    parent: &adw::ApplicationWindow,
    mailbox_state: Rc<RefCell<MailboxViewModel>>,
    sidebar: SidebarWidgets,
    header_account_dropdown: gtk::DropDown,
) -> gtk::Window {
    let window = gtk::Window::builder()
        .transient_for(parent)
        .title("Mail Settings")
        .default_width(640)
        .default_height(560)
        .build();

    let header = adw::HeaderBar::new();
    let title = adw::WindowTitle::builder().title("Mail Settings").build();
    let back_button = gtk::Button::builder()
        .icon_name("go-previous-symbolic")
        .tooltip_text("Back")
        .visible(false)
        .build();
    header.pack_start(&back_button);
    header.set_title_widget(Some(&title));
    window.set_titlebar(Some(&header));

    let account_dropdown = gtk::DropDown::new(None::<gtk::StringList>, None::<gtk::Expression>);
    let initial_account = mailbox_state.borrow().selected_account;
    refresh_account_dropdown(&account_dropdown, &mailbox_state.borrow());

    let alias_list = gtk::ListBox::builder().css_classes(["boxed-list"]).build();
    rebuild_settings_alias_list(&alias_list, &mailbox_state.borrow(), initial_account);
    if let Some(row) = alias_list.row_at_index(0) {
        alias_list.select_row(Some(&row));
    }

    let alias_selection = Rc::new(RefCell::new(0usize));
    let stack = gtk::Stack::builder()
        .hexpand(true)
        .vexpand(true)
        .transition_type(gtk::StackTransitionType::SlideLeftRight)
        .build();

    let account_name_label = gtk::Label::builder()
        .label("Account Name")
        .xalign(0.0)
        .css_classes(["mail-form-label"])
        .build();
    let account_label = gtk::Label::builder()
        .label("Account")
        .xalign(0.0)
        .css_classes(["mail-form-label"])
        .build();
    let alias_header = gtk::Label::builder()
        .label("Aliases")
        .xalign(0.0)
        .css_classes(["title-3"])
        .margin_top(18)
        .margin_bottom(10)
        .build();
    let account_name_entry = gtk::Entry::builder().hexpand(true).build();
    let account_name_save_button = gtk::Button::builder()
        .label("Save")
        .css_classes(["suggested-action"])
        .build();
    let add_alias_button = gtk::Button::builder().label("Add").build();
    let edit_alias_button = gtk::Button::builder()
        .label("Edit")
        .sensitive(false)
        .build();
    let remove_alias_button = gtk::Button::builder()
        .label("Remove")
        .sensitive(false)
        .build();
    let default_alias_button = gtk::Button::builder()
        .label("Set Default")
        .sensitive(false)
        .build();

    let alias_actions = gtk::Box::builder().spacing(8).build();
    alias_actions.append(&add_alias_button);
    alias_actions.append(&edit_alias_button);
    alias_actions.append(&remove_alias_button);
    alias_actions.append(&default_alias_button);

    let alias_scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .min_content_height(220)
        .child(&alias_list)
        .build();

    let alias_frame = gtk::Frame::builder()
        .child(&alias_scroller)
        .css_classes(["compact-list-frame"])
        .vexpand(true)
        .build();

    let main_form = gtk::Grid::builder()
        .hexpand(true)
        .row_spacing(8)
        .column_spacing(12)
        .build();
    let account_name_row = gtk::Box::builder().spacing(8).build();
    account_name_row.append(&account_name_entry);
    account_name_row.append(&account_name_save_button);
    main_form.attach(&account_label, 0, 0, 1, 1);
    main_form.attach(&account_dropdown, 1, 0, 1, 1);
    main_form.attach(&account_name_label, 0, 1, 1, 1);
    main_form.attach(&account_name_row, 1, 1, 1, 1);
    let alias_column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .hexpand(true)
        .vexpand(true)
        .build();
    alias_column.append(&alias_header);
    alias_column.append(&alias_frame);
    alias_column.append(&alias_actions);
    main_form.attach(&alias_column, 0, 2, 2, 1);

    let main_page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(14)
        .margin_top(14)
        .margin_bottom(14)
        .margin_start(14)
        .margin_end(14)
        .build();
    main_page.append(&main_form);

    let username_entry = gtk::Entry::builder().hexpand(true).build();
    let email_entry = gtk::Entry::builder().hexpand(true).build();
    let reply_to_entry = gtk::Entry::builder()
        .placeholder_text("Optional Reply-To")
        .hexpand(true)
        .build();
    let signature_view = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(12)
        .bottom_margin(12)
        .left_margin(12)
        .right_margin(12)
        .vexpand(true)
        .build();
    let save_button = gtk::Button::builder()
        .label("Save Alias")
        .css_classes(["suggested-action"])
        .halign(gtk::Align::End)
        .build();

    let editor_form = gtk::Grid::builder()
        .hexpand(true)
        .row_spacing(8)
        .column_spacing(12)
        .build();
    let username_label = gtk::Label::builder()
        .label("Name")
        .xalign(0.0)
        .css_classes(["mail-form-label"])
        .build();
    let email_label = gtk::Label::builder()
        .label("Address")
        .xalign(0.0)
        .css_classes(["mail-form-label"])
        .build();
    let reply_label = gtk::Label::builder()
        .label("Reply-To")
        .xalign(0.0)
        .css_classes(["mail-form-label"])
        .build();
    editor_form.attach(&username_label, 0, 0, 1, 1);
    editor_form.attach(&username_entry, 1, 0, 1, 1);
    editor_form.attach(&email_label, 0, 1, 1, 1);
    editor_form.attach(&email_entry, 1, 1, 1, 1);
    editor_form.attach(&reply_label, 0, 2, 1, 1);
    editor_form.attach(&reply_to_entry, 1, 2, 1, 1);

    let signature_frame = gtk::Frame::builder()
        .label("Signature")
        .child(&signature_view)
        .css_classes(["compact-list-frame"])
        .vexpand(true)
        .build();

    let edit_page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(14)
        .margin_top(14)
        .margin_bottom(14)
        .margin_start(14)
        .margin_end(14)
        .build();
    edit_page.append(&editor_form);
    edit_page.append(&signature_frame);
    edit_page.append(&save_button);

    stack.add_named(&main_page, Some("main"));
    stack.add_named(&edit_page, Some("editor"));
    stack.set_visible_child_name("main");
    window.set_child(Some(&stack));

    let title_for_notify = title.clone();
    let back_for_notify = back_button.clone();
    stack.connect_visible_child_name_notify(move |stack| {
        let in_editor = stack.visible_child_name().as_deref() == Some("editor");
        back_for_notify.set_visible(in_editor);
        title_for_notify.set_title(if in_editor {
            "Edit Alias"
        } else {
            "Mail Settings"
        });
    });

    account_name_entry.set_text(
        mailbox_state
            .borrow()
            .accounts
            .get(initial_account)
            .map(|account| account.display_name.as_str())
            .unwrap_or(""),
    );

    let mailbox_for_account_name_save = Rc::clone(&mailbox_state);
    let account_dropdown_for_account_name_save = account_dropdown.clone();
    let header_dropdown_for_account_name_save = header_account_dropdown.clone();
    let settings_dropdown_for_account_name_save = account_dropdown.clone();
    let account_name_entry_for_save = account_name_entry.clone();
    account_name_save_button.connect_clicked(move |_| {
        let account_index = account_dropdown_for_account_name_save.selected() as usize;
        {
            let mut mailbox = mailbox_for_account_name_save.borrow_mut();
            mailbox.update_account_name(
                account_index,
                account_name_entry_for_save.text().to_string(),
            );
        }
        let mailbox = mailbox_for_account_name_save.borrow();
        refresh_account_dropdown(&header_dropdown_for_account_name_save, &mailbox);
        refresh_account_dropdown(&settings_dropdown_for_account_name_save, &mailbox);
    });

    let mailbox_for_account = Rc::clone(&mailbox_state);
    let alias_list_for_account = alias_list.clone();
    let account_name_for_account = account_name_entry.clone();
    let alias_selection_for_account = alias_selection.clone();
    let edit_for_account = edit_alias_button.clone();
    let remove_for_account = remove_alias_button.clone();
    let default_for_account = default_alias_button.clone();
    account_dropdown.connect_selected_notify(move |dropdown| {
        let account_index = dropdown.selected() as usize;
        let (account_name, status) = {
            let mailbox = mailbox_for_account.borrow();
            rebuild_settings_alias_list(&alias_list_for_account, &mailbox, account_index);
            let account_name = mailbox
                .accounts
                .get(account_index)
                .map(|account| account.display_name.clone())
                .unwrap_or_default();
            let status = mailbox
                .account_identity(account_index, 0)
                .map(|identity| (identity.is_primary_address, identity.is_default))
                .unwrap_or((false, false));
            (account_name, status)
        };
        if account_name_for_account.text().as_str() != account_name {
            account_name_for_account.set_text(&account_name);
        }
        *alias_selection_for_account.borrow_mut() = 0;
        if let Some(row) = alias_list_for_account.row_at_index(0) {
            alias_list_for_account.select_row(Some(&row));
        }
        let (is_primary, is_default) = status;
        edit_for_account.set_sensitive(true);
        remove_for_account.set_sensitive(!is_primary);
        default_for_account.set_sensitive(!is_default);
    });

    let mailbox_for_select = Rc::clone(&mailbox_state);
    let account_dropdown_for_select = account_dropdown.clone();
    let alias_selection_for_select = alias_selection.clone();
    let edit_for_select = edit_alias_button.clone();
    let remove_for_select = remove_alias_button.clone();
    let default_for_select = default_alias_button.clone();
    alias_list.connect_row_selected(move |_, row| {
        let alias_index = row.map(|row| row.index() as usize).unwrap_or(0);
        *alias_selection_for_select.borrow_mut() = alias_index;
        let status = mailbox_for_select
            .borrow()
            .account_identity(account_dropdown_for_select.selected() as usize, alias_index)
            .map(|identity| (identity.is_primary_address, identity.is_default))
            .unwrap_or((false, false));
        let (is_primary, is_default) = status;
        edit_for_select.set_sensitive(row.is_some());
        remove_for_select.set_sensitive(!is_primary);
        default_for_select.set_sensitive(!is_default);
    });

    let mailbox_for_edit = Rc::clone(&mailbox_state);
    let account_dropdown_for_edit = account_dropdown.clone();
    let alias_selection_for_edit = alias_selection.clone();
    let username_for_edit = username_entry.clone();
    let email_for_edit = email_entry.clone();
    let reply_for_edit = reply_to_entry.clone();
    let signature_for_edit = signature_view.clone();
    let save_for_edit = save_button.clone();
    let stack_for_edit = stack.clone();
    edit_alias_button.connect_clicked(move |_| {
        let alias_index = *alias_selection_for_edit.borrow();
        load_alias_editor_fields(
            &mailbox_for_edit.borrow(),
            account_dropdown_for_edit.selected() as usize,
            alias_index,
            &username_for_edit,
            &email_for_edit,
            &reply_for_edit,
            &signature_for_edit,
            &save_for_edit,
        );
        stack_for_edit.set_visible_child_name("editor");
    });

    let stack_for_back = stack.clone();
    back_button.connect_clicked(move |_| {
        stack_for_back.set_visible_child_name("main");
    });

    let mailbox_for_add = Rc::clone(&mailbox_state);
    let account_dropdown_for_add = account_dropdown.clone();
    let alias_list_for_add = alias_list.clone();
    let alias_selection_for_add = alias_selection.clone();
    let save_for_email_validation = save_button.clone();
    email_entry.connect_changed(move |entry| {
        let has_address = !entry.text().trim().is_empty();
        let allow_save = !entry.is_sensitive() || has_address;
        save_for_email_validation.set_sensitive(allow_save);
    });
    let default_button_for_add = default_alias_button.clone();
    let remove_for_add = remove_alias_button.clone();
    add_alias_button.connect_clicked(move |_| {
        let account_index = account_dropdown_for_add.selected() as usize;
        let Some(alias_index) = mailbox_for_add.borrow_mut().add_identity(account_index) else {
            return;
        };
        rebuild_settings_alias_list(
            &alias_list_for_add,
            &mailbox_for_add.borrow(),
            account_index,
        );
        *alias_selection_for_add.borrow_mut() = alias_index;
        if let Some(row) = alias_list_for_add.row_at_index(alias_index as i32) {
            alias_list_for_add.select_row(Some(&row));
        }
        remove_for_add.set_sensitive(true);
        default_button_for_add.set_sensitive(true);
    });

    let mailbox_for_remove = Rc::clone(&mailbox_state);
    let account_dropdown_for_remove = account_dropdown.clone();
    let alias_list_for_remove = alias_list.clone();
    let alias_selection_for_remove = alias_selection.clone();
    let default_for_remove = default_alias_button.clone();
    let remove_for_remove = remove_alias_button.clone();
    remove_alias_button.connect_clicked(move |_| {
        let account_index = account_dropdown_for_remove.selected() as usize;
        let alias_index = *alias_selection_for_remove.borrow();
        if !mailbox_for_remove
            .borrow_mut()
            .remove_identity(account_index, alias_index)
        {
            return;
        }
        let alias_count = mailbox_for_remove
            .borrow()
            .accounts
            .get(account_index)
            .map(|account| account.aliases.len())
            .unwrap_or(0);
        rebuild_settings_alias_list(
            &alias_list_for_remove,
            &mailbox_for_remove.borrow(),
            account_index,
        );
        let next_index = alias_index
            .saturating_sub(1)
            .min(alias_count.saturating_sub(1));
        *alias_selection_for_remove.borrow_mut() = next_index;
        if let Some(row) = alias_list_for_remove.row_at_index(next_index as i32) {
            alias_list_for_remove.select_row(Some(&row));
        }
        let status = mailbox_for_remove
            .borrow()
            .account_identity(account_index, next_index)
            .map(|identity| (identity.is_primary_address, identity.is_default))
            .unwrap_or((false, false));
        let (is_primary, is_default) = status;
        default_for_remove.set_sensitive(!is_default);
        remove_for_remove.set_sensitive(!is_primary);
    });

    let mailbox_for_default = Rc::clone(&mailbox_state);
    let sidebar_for_default = sidebar.clone();
    let account_dropdown_for_default = account_dropdown.clone();
    let alias_selection_for_default = alias_selection.clone();
    let alias_list_for_default = alias_list.clone();
    let default_for_default = default_alias_button.clone();
    default_alias_button.connect_clicked(move |_| {
        let account_index = account_dropdown_for_default.selected() as usize;
        let alias_index = *alias_selection_for_default.borrow();
        mailbox_for_default
            .borrow_mut()
            .set_default_identity(account_index, alias_index);
        rebuild_settings_alias_list(
            &alias_list_for_default,
            &mailbox_for_default.borrow(),
            account_index,
        );
        if let Some(row) = alias_list_for_default.row_at_index(alias_index as i32) {
            alias_list_for_default.select_row(Some(&row));
        }
        default_for_default.set_sensitive(false);
        rebuild_sidebar(&mailbox_for_default, &sidebar_for_default);
    });

    if let Some(identity) = mailbox_state
        .borrow()
        .account_identity(initial_account, *alias_selection.borrow())
    {
        edit_alias_button.set_sensitive(true);
        remove_alias_button.set_sensitive(!identity.is_primary_address);
        default_alias_button.set_sensitive(!identity.is_default);
    }

    let mailbox_for_save = Rc::clone(&mailbox_state);
    let sidebar_for_save = sidebar.clone();
    let account_dropdown_for_save = account_dropdown.clone();
    let alias_selection_for_save = alias_selection.clone();
    let alias_list_for_save = alias_list.clone();
    let stack_for_save = stack.clone();
    save_button.connect_clicked(move |_| {
        let account_index = account_dropdown_for_save.selected() as usize;
        let alias_index = *alias_selection_for_save.borrow();
        let start = signature_view.buffer().start_iter();
        let end = signature_view.buffer().end_iter();
        let signature_text = signature_view
            .buffer()
            .text(&start, &end, false)
            .to_string();
        mailbox_for_save.borrow_mut().update_identity(
            account_index,
            alias_index,
            username_entry.text().to_string(),
            email_entry.text().to_string(),
            Some(reply_to_entry.text().to_string()).filter(|value| !value.is_empty()),
            signature_text,
        );

        rebuild_settings_alias_list(
            &alias_list_for_save,
            &mailbox_for_save.borrow(),
            account_index,
        );
        if let Some(row) = alias_list_for_save.row_at_index(alias_index as i32) {
            alias_list_for_save.select_row(Some(&row));
        }
        rebuild_sidebar(&mailbox_for_save, &sidebar_for_save);
        stack_for_save.set_visible_child_name("main");
    });

    window
}

pub(super) fn present_about(parent: &adw::ApplicationWindow) {
    let dialog = adw::AboutDialog::builder()
        .application_name(crate::config::APP_NAME)
        .application_icon(crate::config::APP_ID)
        .version(crate::config::APP_VERSION)
        .developer_name("Pigeon Mail contributors")
        .comments("A GNOME mail client powered by Evolution Data Server.")
        .copyright("© 2026 Pigeon Mail contributors")
        .website(crate::config::PROJECT_URL)
        .issue_url(crate::config::ISSUE_URL)
        .license_type(gtk::License::MitX11)
        .build();
    dialog.present(Some(parent));
}

fn load_alias_editor_fields(
    mailbox: &MailboxViewModel,
    account_index: usize,
    alias_index: usize,
    username_entry: &gtk::Entry,
    email_entry: &gtk::Entry,
    reply_to_entry: &gtk::Entry,
    signature_view: &gtk::TextView,
    save_button: &gtk::Button,
) {
    if let Some(identity) = mailbox.account_identity(account_index, alias_index) {
        username_entry.set_text(&identity.display_name);
        email_entry.set_text(&identity.address);
        reply_to_entry.set_text(identity.reply_to.as_deref().unwrap_or(""));
        signature_view.buffer().set_text(&identity.signature_text);
        email_entry.set_sensitive(!identity.is_primary_address);
        save_button.set_sensitive(true);
    } else {
        username_entry.set_text("");
        email_entry.set_text("");
        reply_to_entry.set_text("");
        signature_view.buffer().set_text("");
        email_entry.set_sensitive(false);
        save_button.set_sensitive(false);
    }
}

fn rebuild_settings_alias_list(
    list: &gtk::ListBox,
    mailbox: &MailboxViewModel,
    account_index: usize,
) {
    list.remove_all();

    let Some(account) = mailbox.accounts.get(account_index) else {
        let row = gtk::ListBoxRow::new();
        row.set_selectable(false);
        row.set_activatable(false);
        let cell = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .margin_top(10)
            .margin_bottom(10)
            .margin_start(12)
            .margin_end(12)
            .css_classes(["dim-label"])
            .label("No aliases yet. Add one below.")
            .build();
        row.set_child(Some(&cell));
        list.append(&row);
        return;
    };

    for identity in &account.aliases {
        let row = gtk::ListBoxRow::new();
        let line = gtk::Box::builder()
            .spacing(10)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(12)
            .margin_end(12)
            .build();
        let text = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .label(&format!(
                "{} ({})",
                identity.display_name_or_address(),
                identity.address
            ))
            .build();
        line.append(&text);
        if identity.is_default {
            let badge = gtk::Label::builder()
                .label("Default")
                .css_classes(["accent", "caption"])
                .valign(gtk::Align::Center)
                .build();
            line.append(&badge);
        }
        row.set_child(Some(&line));
        list.append(&row);
    }
}
