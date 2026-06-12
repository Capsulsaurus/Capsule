# Capsule translations (`locales/`)

This directory is the **single source of truth** for every user-facing string in
Capsule. You can translate Capsule **without touching any application code** —
just edit JSON files here and open a pull request.

A build step (`just i18n`) compiles these files into each platform's native
format (web, iOS, Android, and the Rust server/CLI). Nothing here is hand-copied
into the apps; the generated files are produced from this directory.

> Design rationale and the full contract live in the
> [Internationalization design doc](../capsule-docs/src/content/docs/design/i18n.md).

## Files

| File | Purpose |
| --- | --- |
| `config.json` | The supported-locale set, the source locale, and per-locale fallbacks. |
| `en.json` | The source (authoring) catalog. Every key is defined here first. |
| `<locale>.json` | A translation of `en.json` into another locale (e.g. `fr.json`). |
| `schema/catalog.schema.json` | JSON Schema for the catalog shape (editor autocomplete + validation). |

## Catalog format

Each catalog is a flat JSON object mapping a **key** to an entry:

```json
{
    "label_title": {
        "message": "Title",
        "context": "Field label for an asset's title."
    }
}
```

- `message` — the string itself, written in
  [ICU MessageFormat](https://unicode-org.github.io/icu/userguide/format_parse/messages/).
  Plain text needs no special syntax.
- `context` — an optional note for translators describing where the string
  appears. Only the source catalog (`en.json`) needs `context`; translations may
  omit it.

### ICU MessageFormat, briefly

Interpolate a value with braces, and pluralize with a `plural` block:

```text
Hello, {name}!
{count, plural, one {# photo} other {# photos}}
```

The same ICU syntax compiles to every platform, so you write a message once.

### Key naming

New keys use **dotted namespaces** — `area.subarea.name`
(e.g. `error.auth.invalid_credentials`, `album.create.title`). A handful of
legacy UI keys inherited from the Android catalog keep their original flat names
(`app_name`, `back`, …); leave those as-is. Codegen sanitizes any key into each
platform's native identifier rules (for Android, `.` and `-` become `_`).

### Error codes

Keys under the `error.*` namespace are the **stable error codes** the server
sends to clients. The server attaches the code (e.g. `error.auth.invalid_credentials`)
alongside an English detail message; clients look the code up here to show a
localized high-level message. See the design doc for the full contract.

## Add a language

1. Add the locale tag to `supportedLocales` in `config.json` (BCP-47, e.g. `fr`,
   `pt-BR`).
2. Copy `en.json` to `<locale>.json` and translate each `message`. Keep the keys
   identical — every key in `en.json` must exist in every translation.
3. Run `just i18n` to regenerate the per-platform files, then
   `just i18n-check` to confirm there is no drift.
4. Open a pull request. See [CONTRIBUTING.md](../CONTRIBUTING.md) for the commit
   and review flow.

A future translation-management hub (Weblate or Crowdin) will let non-technical
contributors translate through a web UI backed by these same files; until then,
the JSON-via-pull-request flow above is the supported path.
