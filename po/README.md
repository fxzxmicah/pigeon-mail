# Translation workflow

Run this from the project root after changing translatable application, desktop,
or AppStream product text:

```sh
sh po/update.sh
```

The script updates every language in `po/LINGUAS`. Preserve named placeholders
such as `{count}` and mnemonic underscores. AppStream release notes and internal
diagnostics are intentionally not translated. A normal build validates and
compiles the catalogs.
