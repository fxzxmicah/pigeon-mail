Name:           pigeon-mail
Version:        0.2.2
Release:        1%{?dist}
Summary:        GNOME email client using Evolution Data Server
URL:            https://github.com/fxzxmicah/pigeon-mail
License:        MIT

Source0:        %{url}/archive/refs/tags/%{version}.tar.gz#/%{name}-%{version}.tar.gz

BuildRequires:  cargo
BuildRequires:  rust-packaging
BuildRequires:  gcc
BuildRequires:  desktop-file-utils
BuildRequires:  appstream
BuildRequires:  glib2
BuildRequires:  gettext
BuildRequires:  pkgconfig(camel-1.2)
BuildRequires:  pkgconfig(libedataserver-1.2)

Requires:       gnome-online-accounts
Recommends:     evolution-ews-core

%description
Pigeon Mail is a GNOME email client built on GNOME Online Accounts and
Evolution Data Server. It provides cached mail access, sending identities,
HTML mail, attachments, drafts, search, and cache-first message actions.

%generate_buildrequires
%cargo_generate_buildrequires

%prep
%autosetup -p1
%cargo_prep

%build
export PREFIX=%{_prefix}
%cargo_license_summary
%cargo_build

%install
install -Dpm0755 target/rpm/pigeon %{buildroot}%{_bindir}/pigeon

install -d %{buildroot}%{_datadir}/applications
msgfmt --desktop \
    --template=data/org.gnome.pigeon.desktop.in \
    -d po \
    -o %{buildroot}%{_datadir}/applications/org.gnome.pigeon.desktop

install -d %{buildroot}%{_metainfodir}
msgfmt --xml \
    --template=data/org.gnome.pigeon.metainfo.xml.in \
    -d po \
    -o %{buildroot}%{_metainfodir}/org.gnome.pigeon.metainfo.xml

languages=$(sed 's/#.*//' po/LINGUAS)
for lang in $languages; do
    install -d %{buildroot}%{_datadir}/locale/$lang/LC_MESSAGES
    msgfmt --check \
        -o %{buildroot}%{_datadir}/locale/$lang/LC_MESSAGES/pigeon.mo \
        po/$lang.po
done

install -Dpm0644 \
    data/icons/hicolor/scalable/apps/org.gnome.pigeon.svg \
    %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/org.gnome.pigeon.svg

install -Dpm0644 \
    data/org.gnome.pigeon.gschema.xml \
    %{buildroot}%{_datadir}/glib-2.0/schemas/org.gnome.pigeon.gschema.xml

install -d %{buildroot}%{_datadir}/dbus-1/services
sed 's#@bindir@#%{_bindir}#g' \
    data/org.gnome.pigeon.service.in \
    > %{buildroot}%{_datadir}/dbus-1/services/org.gnome.pigeon.service

%find_lang pigeon

%check
export PREFIX=%{_prefix}
%cargo_test
desktop-file-validate %{buildroot}%{_datadir}/applications/org.gnome.pigeon.desktop
appstreamcli validate --no-net --pedantic %{buildroot}%{_metainfodir}/org.gnome.pigeon.metainfo.xml
glib-compile-schemas --strict --dry-run %{buildroot}%{_datadir}/glib-2.0/schemas

%files -f pigeon.lang
%license LICENSE
%{_bindir}/pigeon
%{_datadir}/applications/org.gnome.pigeon.desktop
%{_metainfodir}/org.gnome.pigeon.metainfo.xml
%{_datadir}/dbus-1/services/org.gnome.pigeon.service
%{_datadir}/glib-2.0/schemas/org.gnome.pigeon.gschema.xml
%{_datadir}/icons/hicolor/scalable/apps/org.gnome.pigeon.svg

%changelog
* Fri Sep 18 2026 Fxzx micah <48860358+fxzxmicah@users.noreply.github.com> - 0.2.2-1
- Restore translations in packaged installations

* Fri Sep 18 2026 Fxzx micah <48860358+fxzxmicah@users.noreply.github.com> - 0.2.1-1
- Add German, Spanish, French, Japanese, and Simplified Chinese localization
- Align attachment presentation across reading and composition
- Refine localized mail dates and close confirmations

* Thu Sep 17 2026 Fxzx micah <48860358+fxzxmicah@users.noreply.github.com> - 0.2.0-1
- Store sending identities and signatures through Evolution Data Server
- Make local Drafts, Outbox, and Sent folders the cache-first write boundary
- Refine adaptive mailbox, composition, settings, and asynchronous task handling

* Mon Aug 24 2026 Fxzx micah <48860358+fxzxmicah@users.noreply.github.com> - 0.1.4-1
- Return cached messages without waiting for another message download
- Reject stale cache probes before they enter the remote detail queue
- Document the evolution-ews patch for Microsoft 365 alias sending

* Sun Aug 23 2026 Fxzx micah <48860358+fxzxmicah@users.noreply.github.com> - 0.1.3-1
- Refresh the active account every three minutes and retry interrupted rediscovery
- Save attachments asynchronously through GIO
- Tighten unavailable-account and composition state handling

* Sun Aug 23 2026 Fxzx micah <48860358+fxzxmicah@users.noreply.github.com> - 0.1.2-1
- Scan every mail folder for notifications and open the real source folder
- Keep cached conversation flags and folder counts consistent
- Tighten application and cache backend boundaries

* Sun Aug 23 2026 Fxzx micah <48860358+fxzxmicah@users.noreply.github.com> - 0.1.1-1
- Fix desktop Compose activation and mailto URI handling
- Keep Compose and mailto available in stub sessions

* Sun Aug 23 2026 Fxzx micah <48860358+fxzxmicah@users.noreply.github.com> - 0.1.0-1
- Initial Pigeon Mail package
