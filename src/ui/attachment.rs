use gtk::{pango, prelude::*};

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
