Name:           pigeon-mail
Version:        0.1.3
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
%cargo_license_summary
%cargo_build

%install
install -Dpm0755 target/rpm/pigeon %{buildroot}%{_bindir}/pigeon

desktop-file-install \
    --dir=%{buildroot}%{_datadir}/applications \
    data/org.gnome.pigeon.desktop

install -Dpm0644 \
    data/org.gnome.pigeon.metainfo.xml \
    %{buildroot}%{_metainfodir}/org.gnome.pigeon.metainfo.xml

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

%check
%cargo_test
desktop-file-validate %{buildroot}%{_datadir}/applications/org.gnome.pigeon.desktop
appstreamcli validate --no-net --pedantic %{buildroot}%{_metainfodir}/org.gnome.pigeon.metainfo.xml
glib-compile-schemas --strict --dry-run %{buildroot}%{_datadir}/glib-2.0/schemas

%files
%license LICENSE
%{_bindir}/pigeon
%{_datadir}/applications/org.gnome.pigeon.desktop
%{_metainfodir}/org.gnome.pigeon.metainfo.xml
%{_datadir}/dbus-1/services/org.gnome.pigeon.service
%{_datadir}/glib-2.0/schemas/org.gnome.pigeon.gschema.xml
%{_datadir}/icons/hicolor/scalable/apps/org.gnome.pigeon.svg

%changelog
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
