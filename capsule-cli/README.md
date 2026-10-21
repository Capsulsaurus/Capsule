# Capsule CLI

`capsule` is Capsule's reference client: a scriptable CLI and, on a terminal, a full-screen TUI.
It imports a library, pushes it to a server, verifies custody, syncs and restores. It is the
first client to support each feature and remains the most robust client for the life of the
project, because it is also the simplest: every other client is held to what the CLI proves.

**English only.** The CLI and TUI are not localized and will stay English-only. The shared
`error.*` codes the server returns are rendered through the English catalog; the native and web
apps own localization ([Internationalization](../capsule-docs/src/content/docs/design/i18n.md)).

## Shape

- `capsule` with no arguments on a TTY opens the TUI.
- Every subcommand stays non-interactive and supports `--json`, so scripts and the automated
  test suites drive exactly the code paths the TUI uses.
- Local state: the library catalog (SQLite, owned by `capsule-core`) plus the CLI's own SQLite
  store for the sync feed and session state. Login tokens live in a `0600` file.
- Every network call goes through `capsule-sdk`; the CLI never hand-rolls a request.

## Usage

`capsule --help` lists the command tree; the [CLI reference](../capsule-docs/src/content/docs/reference/cli.md)
is generated from it. Day-to-day use is covered by the
[user guide](../capsule-docs/src/content/docs/guides/user-guide.md).

## Development

- `cargo run -p capsule-cli -- <args>` runs the CLI.
- `cargo nextest run -p capsule-cli` runs its tests, which spawn the real binary.
- Building needs cmake, a C++17 compiler, libclang and, on x86_64, nasm, for the linked image
  codecs (`media-linked-codecs`).
- `mise run cli-surface` regenerates `cli-surface.json` after a command-tree change;
  `mise run cli-surface-check` gates drift.
