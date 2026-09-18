# Pigeon Mail

Pigeon Mail is a native email client for GNOME. It uses the accounts already
configured through GNOME Online Accounts and stores mail through Evolution Data
Server and Camel, so reading and common message actions remain cache-first.

## Features

- Per-account three-pane mailbox with folders, conversations, search, and
  on-demand pagination
- Cached message reading with HTML and plain-text views
- Read, unread, starred, archive, trash, draft, and sent-mail workflows
- Text, HTML, and multipart composition with explicit conversion between text
  and HTML
- Reply, reply-all, forward, aliases, Reply-To addresses, and signatures
- Attachment opening, asynchronous saving, and sending
- Manual draft saving and a durable local outbox for deferred delivery
- Desktop notifications for new unread mail in every folder of the current account
- `mailto:` integration and a reusable full-window composer
- A non-persistent stub mailbox when no eligible account is available

## Accounts and synchronization

Pigeon Mail does not maintain a separate account database or provide an account
setup wizard. Add an account in GNOME Settings under **Online Accounts** and
enable its mail service. Pigeon Mail lists accounts for which Evolution Data
Server exposes a complete GOA-linked mail account, identity, and transport.

Only the selected account receives automatic foreground work. If it is changed
while a write is still running, the previous account drains that write's complete
synchronization chain, including required replay and confirmation, and then
retains its materialized backend and local cache while quiescent.
Unmaterialized accounts are not synchronized speculatively. Message bodies
and attachments are fetched when opened.

Local actions are committed to the EDS/Camel cache first. Network-dependent work
is then synchronized in the background. Unresolved operation intents are kept
for the current run and their count is shown in the window; closing with pending
work warns that those tasks will be lost.

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

The Microsoft 365 transport in evolution-ews can replace an Outlook.com alias
with the account's primary address when it submits raw MIME. A provider-side
patch keeps primary-address delivery on the MIME path and uses the structured
Microsoft 365 submission path for alternate sending identities.

Fedora users can find `SOURCES/evolution-ews-send-from-alias.patch` in the
[`fedora-rpm-rebuild` repository](https://github.com/fxzxmicah/fedora-rpm-rebuild).
Apply it when rebuilding evolution-ews; Pigeon Mail itself continues to use
EDS/Camel exclusively and does not connect to provider APIs directly.

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

Pigeon Mail is designed for a modern GNOME desktop and the provider support
available through GOA and EDS. It has no unified inbox, account-creation UI, or
automatic draft saving. Provider-specific behavior and uncommon MIME structures
remain ongoing interoperability work.

Mail cache ownership remains with EDS/Camel. Lightweight preferences use
GSettings, identity extensions and signatures remain on their EDS sources.

## Contributing

Bug reports and focused patches are welcome through the
[GitHub issue tracker](https://github.com/fxzxmicah/pigeon-mail/issues). When
reporting a problem, include the Pigeon Mail version, desktop version, account
provider, and privacy-reviewed logs. Do not publish addresses, subjects, message
bodies, authentication data, or attachment paths.

## License

Pigeon Mail is released under the [MIT License](LICENSE).
