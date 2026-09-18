# Pigeon Mail

Pigeon Mail is a native email client for GNOME. It uses the accounts already
configured through GNOME Online Accounts and stores mail through Evolution Data
Server and Camel, so reading and common message actions remain cache-first.

## Features

- Per-account three-pane mailbox with folders, conversations, search, and
  on-demand pagination
- Cached message reading with HTML and plain-text views
- Read, starred, archive, trash, draft, outbox, and sent-mail workflows
- Text, HTML, and multipart composition
- Reply, reply-all, forward, aliases, Reply-To addresses, and signatures
- Attachment opening, asynchronous saving, and sending
- Desktop notifications and `mailto:` integration

## Accounts and synchronization

Add accounts in GNOME Settings under **Online Accounts** and enable mail.
Pigeon Mail uses EDS/Camel as its account and mail authority: local actions are
cache-first and network synchronization continues in the background.

## Requirements

Pigeon Mail targets current GNOME desktops and requires:

- Rust with support for edition 2024
- GTK 4.20 or newer
- libadwaita 1.8 or newer
- WebKitGTK 6.0 with the 2.50 API
- Evolution Data Server development files providing `camel-1.2` and
  `libedataserver-1.2`
- GNU gettext, `glib-compile-schemas`, a C compiler, and `pkg-config`
- GNOME Online Accounts at runtime

Fedora is the primary development and packaging environment. See
[`spec/pigeon-mail.spec`](spec/pigeon-mail.spec) for the authoritative RPM build
requirements.

### Microsoft 365 alias sending

Microsoft 365 alias sending currently requires
`SOURCES/evolution-ews-send-from-alias.patch` from the
[`fedora-rpm-rebuild` repository](https://github.com/fxzxmicah/fedora-rpm-rebuild).

## Build from source

```sh
cargo build --release
cargo test --release
```

The executable is written to `target/release/pigeon`. A packaged installation
also installs the desktop entry, AppStream metadata, application icon, GSettings
schema, and D-Bus service needed for desktop and `mailto:` integration.

For a development run:

```sh
cargo run
```

Development builds enable detailed Pigeon diagnostics by default. Release
builds keep normal logs concise; user-facing diagnostic logging can be enabled
when needed:

```sh
RUST_LOG=pigeon=debug pigeon
```

Logs intentionally avoid including message content and other detailed personal
data at normal release log levels.

## Current scope

Pigeon Mail currently has no unified inbox, account-creation UI, or automatic
draft saving. Provider-specific behavior and uncommon MIME structures remain
ongoing interoperability work.

## Contributing

Bug reports and focused patches are welcome through the
[GitHub issue tracker](https://github.com/fxzxmicah/pigeon-mail/issues). When
reporting a problem, include the Pigeon Mail version, desktop version, account
provider, and privacy-reviewed logs. Do not publish addresses, subjects, message
bodies, authentication data, or attachment paths.

## License

Pigeon Mail is released under the [MIT License](LICENSE).
