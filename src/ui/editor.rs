use adw::prelude::*;
use std::rc::Rc;

use crate::i18n::gettext;
use crate::integration::webkit::WebKitComposer;

pub(super) struct DualFormatEditor {
    pub composer: Rc<WebKitComposer>,
    pub text_editor: gtk::TextView,
    pub stack: adw::ViewStack,
    pub convert_button: gtk::Button,
    pub mode_controls: gtk::Box,
    pub frame: gtk::Frame,
}

impl DualFormatEditor {
    pub fn new(label: &str) -> Self {
        let composer = Rc::new(WebKitComposer::new());
        let rich_editor = composer.build_view();
        let text_editor = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .vexpand(true)
            .build();
        text_editor.set_top_margin(18);
        text_editor.set_bottom_margin(18);
        text_editor.set_left_margin(18);
        text_editor.set_right_margin(18);
        let text_scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&text_editor)
            .build();
        let stack = adw::ViewStack::builder()
            .hhomogeneous(false)
            .vhomogeneous(false)
            .vexpand(true)
            .build();
        stack.add_titled(&rich_editor, Some("html"), &gettext("HTML"));
        stack.add_titled(&text_scroller, Some("text"), &gettext("Text"));
        stack.set_visible_child_name("html");

        let formatting_bar = rich_text_formatting_bar(&composer);
        let formatting_scroller = gtk::ScrolledWindow::builder()
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .child(&formatting_bar)
            .build();
        let switcher = adw::InlineViewSwitcher::builder()
            .stack(&stack)
            .homogeneous(false)
            .can_shrink(true)
            .build();
        let convert_button = gtk::Button::builder()
            .icon_name("format-text-plaintext-symbolic")
            .tooltip_text(gettext("Convert"))
            .build();
        let mode_controls = gtk::Box::builder()
            .spacing(6)
            .halign(gtk::Align::Start)
            .build();
        mode_controls.append(&switcher);
        mode_controls.append(&convert_button);
        formatting_bar.set_margin_start(6);
        formatting_bar.set_margin_end(6);
        let frame = editor_frame(label, &formatting_scroller, &stack);

        let formatting_for_mode = formatting_scroller.clone();
        let convert_for_mode = convert_button.clone();
        stack.connect_visible_child_name_notify(move |stack| {
            let html_active = stack.visible_child_name().as_deref() == Some("html");
            formatting_for_mode.set_visible(html_active);
            convert_for_mode.set_icon_name(if html_active {
                "format-text-plaintext-symbolic"
            } else {
                "format-text-rich-symbolic"
            });
        });

        Self {
            composer,
            text_editor,
            stack,
            convert_button,
            mode_controls,
            frame,
        }
    }
}

fn rich_text_formatting_bar(composer: &Rc<WebKitComposer>) -> gtk::Box {
    let bar = gtk::Box::builder().spacing(4).build();
    for (icon, tooltip, command) in [
        ("format-text-bold-symbolic", gettext("Bold"), "Bold"),
        ("format-text-italic-symbolic", gettext("Italic"), "Italic"),
        (
            "format-text-underline-symbolic",
            gettext("Underline"),
            "Underline",
        ),
        (
            "format-text-strikethrough-symbolic",
            gettext("Strikethrough"),
            "Strikethrough",
        ),
        (
            "view-list-bullet-symbolic",
            gettext("Bulleted list"),
            "InsertUnorderedList",
        ),
        (
            "view-list-ordered-symbolic",
            gettext("Numbered list"),
            "InsertOrderedList",
        ),
    ] {
        let button = gtk::Button::builder()
            .icon_name(icon)
            .tooltip_text(&tooltip)
            .css_classes(["flat"])
            .build();
        let composer = Rc::clone(composer);
        button.connect_clicked(move |_| composer.execute_command(command));
        bar.append(&button);
    }
    bar
}

fn editor_frame(
    label: &str,
    controls: &impl IsA<gtk::Widget>,
    editor: &impl IsA<gtk::Widget>,
) -> gtk::Frame {
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .vexpand(true)
        .build();
    content.append(controls);
    content.append(editor);
    gtk::Frame::builder()
        .label(label)
        .child(&content)
        .css_classes(["compact-list-frame"])
        .vexpand(true)
        .build()
}
