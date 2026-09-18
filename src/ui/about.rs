use adw::prelude::*;

use crate::i18n::gettext;

pub(super) fn present(parent: &adw::ApplicationWindow) {
    let dialog = adw::AboutDialog::builder()
        .application_name(crate::config::APP_NAME)
        .application_icon(crate::config::APP_ID)
        .version(crate::config::APP_VERSION)
        .developer_name(gettext("Pigeon Mail contributors"))
        .comments(gettext(
            "A GNOME mail client powered by Evolution Data Server.",
        ))
        .copyright(gettext("© 2026 Pigeon Mail contributors"))
        .website(crate::config::PROJECT_URL)
        .issue_url(crate::config::ISSUE_URL)
        .license_type(gtk::License::MitX11)
        .build();
    dialog.present(Some(parent));
}
