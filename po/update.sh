#!/bin/sh
set -eu

cd "$(dirname "$0")/.."

catalog=po/pigeon.pot

find src -name '*.rs' -type f -print | sort | xgettext \
    --language=Rust \
    --from-code=UTF-8 \
    --package-name='Pigeon Mail' \
    --package-version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)" \
    --msgid-bugs-address='https://github.com/fxzxmicah/pigeon-mail/issues' \
    --copyright-holder='Pigeon Mail contributors' \
    --keyword=gettext \
    --keyword=ngettext:1,2 \
    --keyword=gettext_f:1 \
    --keyword=ngettext_f:1,2 \
    --flag=gettext_f:1:rust-format \
    --flag=ngettext_f:1:rust-format \
    --flag=ngettext_f:2:rust-format \
    --files-from=- \
    --output="$catalog" \
    --directory=.

xgettext \
    --language=Desktop \
    --from-code=UTF-8 \
    --join-existing \
    --output="$catalog" \
    data/org.gnome.pigeon.desktop.in

xgettext \
    --from-code=UTF-8 \
    --join-existing \
    --output="$catalog" \
    data/org.gnome.pigeon.metainfo.xml.in

languages=$(sed 's/#.*//' po/LINGUAS)
for language in $languages; do
    msgmerge --update --backup=none "po/$language.po" "$catalog"
done
