# Capsule translations (`locales/`)

This directory is the **single source of truth** for every user-facing string in
Capsule. You can translate Capsule **without touching any application code** —
just edit JSON files here and open a pull request.

A build step (`mise run i18n`) compiles these files into each platform's native
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
| `glossary.json` | Names kept verbatim in every language, and preferred renderings of domain terms. |
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
- `context` — **required in `en.json`, absent from translations**: the
  string's UI role (button, title, error, accessibility label), the part of
  speech of any ambiguous word (*Crop* the verb), and the screen it appears on.
  A translator sees nothing else, so write it for someone who has never opened
  the app.
- `origin` — **required in every translation, absent from `en.json`**: who
  produced the message.
- `source` — **required in every translation, absent from `en.json`**: the
  hash of the English message it was translated from. You never compute it by
  hand; see [Keeping translations current](#keeping-translations-current).

A translation entry looks like this:

```json
{
    "label_title": {
        "message": "Titre",
        "origin": "human",
        "source": "7e8cd2056da73a7f"
    }
}
```

### Provenance (`origin`)

| Value | Meaning |
| --- | --- |
| `machine` | A machine translation nobody fluent has reviewed yet. |
| `human` | Written, or reviewed and accepted, by a fluent reader. |

When you review a machine translation and keep or fix it, set its `origin` to
`human`. You do not record your name or the date: the commit that changes the
entry records both.

### Keeping translations current

Every key in `en.json` must exist in every translation, and every translation
must have been made from the current English: when an English message changes,
its translations are **stale** and `mise run i18n` fails. A change that adds or
rewords an English message therefore updates every catalog in the same pull
request.

```sh
cargo run -q -p xtask -- i18n --todo fr   # JSON: fr's missing and stale keys, with message and context
cargo run -q -p xtask -- i18n --stamp     # record `source` on entries that have none
```

To translate (by hand or with an LLM agent): take the `--todo` list, translate
each `message` using its `context` and `glossary.json`, write the entries with
`"origin": "machine"` (or `human` if you are fluent) and **without** a `source`
(delete the old one from an entry you rewrite), then run `--stamp`, which
records the hash of the English each entry now matches. `--stamp` never
overwrites a recorded `source`, so a rewritten entry that keeps its old one stays
stale. To accept a stale entry unchanged, delete its `source` and run `--stamp`.
Editing only a `context` changes nothing downstream.

### Glossary

`glossary.json` lists names that stay verbatim in every language (`verbatim`:
`Capsule`, `TOTP`, …); a translation that drops one fails the build. Its `terms`
give the preferred rendering of domain words (album, library, trash) in each
language, so they read the same everywhere. Follow them; they are not checked.

### Placeholders must match the source

Every translation must carry exactly the placeholders of its `en.json` message:
the same `{name}` arguments (in any order), and a `plural` block exactly where
the source has one, on the same selector. Plural categories are your language's
own (Russian uses `one`, `few`, `many` and `other`; Japanese only `other`). A
dropped or extra placeholder fails `mise run i18n` and `mise run i18n-check`,
which list every offending key.

### ICU MessageFormat, briefly

Interpolate a value with braces, and pluralize with a `plural` block:

```text
Hello, {name}!
{count, plural, one {# photo} other {# photos}}
```

The same ICU syntax compiles to every platform, so you write a message once —
and every target evaluates plurals, the Rust server and CLI included, so a
`plural` block is rendered rather than shown to the reader as message source.
`select`, `selectordinal` and `offset:` are not supported anywhere yet; a
message using one fails the build rather than reaching a user.

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
   identical — every key in `en.json` must exist in every translation. Replace
   each `context` with an `origin` (see [Provenance](#provenance-origin)), add the
   language to `glossary.json`'s `terms`, and run `xtask i18n --stamp`.
3. Run `mise run i18n` to regenerate the per-platform files, then
   `mise run i18n-check` to confirm there is no drift.
4. Open a pull request. See [CONTRIBUTING.md](../CONTRIBUTING.md) for the commit
   and review flow.

## Report a translation problem or request a language

- **A wrong, awkward, or missing translation:** open a
  [Translation Error](https://github.com/Capsulsaurus/Capsule/issues/new?template=translation-error.md)
  issue. Include the locale, the catalog key if you can find it in `en.json`,
  and your proposed fix in at least one language you speak natively, keeping
  every placeholder from the English message. Maintainers apply it and backfill
  the other languages.
- **A language Capsule does not ship yet:** open a
  [Language Request](https://github.com/Capsulsaurus/Capsule/issues/new?template=language-request.md)
  issue with its BCP-47 tag, script direction, and fallback, or follow
  [Add a language](#add-a-language) above to contribute the catalog directly.

These files are the system of record for translations and their provenance.
A translation-management hub (Weblate or Crowdin) may later let non-technical
contributors translate through a web UI, but it would edit these same files
through pull requests and keep `origin`; until then, the JSON-via-pull-request
flow above is the supported path.
