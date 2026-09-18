# Translation workflow

The English strings in Rust, the desktop entry, and AppStream metadata are the
source text for the `pigeon` gettext domain. Run the following from the project
root after changing user-visible text:

```sh
sh po/update.sh
```

The script regenerates the ignored `po/pigeon.pot` working file and merges it
into every language listed in `po/LINGUAS`. `LINGUAS` follows gettext syntax:
language names are whitespace-separated and `#` starts a comment. Translate the
resulting empty entries without changing named placeholders such as `{count}`
or mnemonic underscores. Extraction marks formatted messages as Rust format
strings, and a normal Cargo build validates and compiles every catalog with
`msgfmt --check`.

Internal diagnostics, protocol identifiers, CSS classes, action names, and test
fixtures are intentionally not translation inputs.
