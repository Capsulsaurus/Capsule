---
title: Local Development
description: The mise task graph, the services a local server needs, and which gates actually run on a developer machine.
---

Every command in this repository runs through **[mise](https://mise.jdx.dev/)**. `mise.toml` at the repo root pins the shared toolchain and defines the whole task graph; anything too long for a TOML line lives as a shebang script in `mise-tasks/`. CI jobs map 1:1 onto the `check-*` tasks below, so a green local `check-*` is the same command CI runs.

## First-time setup

```bash
mise install          # installs the pinned tools from [tools] in mise.toml
mise run setup        # bun install in capsule-web + capsule-docs, uv sync in capsule-vision
mise run hooks-install # installs the git hooks (hk)
```

`mise install` provides `hk` (the git-hook runner, which replaced lefthook), `convco` (Conventional Commits), `cargo-nextest`, and `wasm-bindgen-cli`. The Rust toolchain itself is pinned separately in `/rust-toolchain.toml`, and the Apple client's Swift tooling in `capsule-swift/mise.toml`.

`mise tasks` lists everything. The naming convention is uniform: a bare task **fixes**, and the `-check` variant **verifies only**.

## The task graph

### Aggregates

| Task | What it does |
| --- | --- |
| `mise run check` | The full gate: `format-check` → `lint-check` → `test`, sequentially |
| `mise run format` / `format-check` | Every toolchain |
| `mise run lint` / `lint-check` | Every toolchain |
| `mise run test` | Rust + web + Kotlin |
| `mise run build` | Every toolchain |

### Per-toolchain entrypoints

| Task | Contents |
| --- | --- |
| `mise run check-rust` | `format-check-rust`, `lint-check-rust`, `i18n-check`, `i18n-guard`, `openapi-check`, `translate-readme-check`, `build-rust`, `build-ffi`, `lint-check-ffi`, `gen-bindings`, `verify-examples` — sequential, because they all contend on one `target/` lock |
| `mise run check-web` | `format-check-web`, `lint-check-web`, `test-web`, `build-web` |
| `mise run check-docs` | `format-check-docs`, `lint-check-docs`, `build-docs` (a real Astro build — a broken internal link fails it) |
| `mise run check-md` | `lint-check-md` (markdownlint over every `.md` in the repo) |
| `mise run check-vision` | `format-check-vision`, `lint-check-vision` |
| `mise run check-kotlin` | `format-check-kotlin`, `lint-check-kotlin` |

### The ones you run most

- **Rust:** `format-rust` (`cargo fmt`), `lint-rust` / `lint-check-rust` (`cargo clippy --workspace` with the shared `$CLIPPY_FLAGS` set — lints are configured once in `mise.toml`, never per crate), `test-rust` (`cargo nextest run`, three invocations: the workspace, then `capsule-core` and `capsule-sdk` with `--features ffi`), `build-rust`, `test-coverage-rust`.
- **Web:** `format-web` / `lint-web` (biome), `test-web` (bun; depends on `build-wasm`, `share-kat`, and `drop-kat`, which generate the WASM glue and the cross-language KAT fixtures the tests load), `build-web`.
- **Docs:** `format-docs` / `lint-docs` (biome over `capsule-docs`), `build-docs` (Astro/Starlight).
- **Markdown:** `lint-md` / `lint-check-md`.
- **Codegen gates:** `i18n` and `i18n-check` (compile `locales/` into the per-platform catalogs), `i18n-guard` (fails on hardcoded user-facing literals in web/Swift/Compose), `openapi` and `openapi-check` (dump/verify `capsule-sdk/openapi.json`), `translate-readme` and `translate-readme-check` (regenerate/verify the translated READMEs).
- **FFI and cross-compilation:** `targets-add` first, then `build-ffi`, `gen-bindings` (uniffi Kotlin/Swift sources), `build-apple`, `build-android`, `build-linux-cross`, `build-windows`, `build-targets`. On macOS, `setup-swift` builds the FFI xcframework and generates the Xcode workspace.

## Running a server locally

The server's two external services come up from `capsule-api/compose.yaml`, under podman or docker:

```bash
podman compose -f capsule-api/compose.yaml up -d   # or: docker compose -f …
```

That brings up **PostgreSQL** and **Valkey**. (The file still defines a leftover `minio` service; nothing in the server reads it — the blob store is a filesystem directory. It is removed by slice `S-P7`.)

Then copy `capsule-api/.env.example` to `capsule-api/.env` and fill in `JWT_ED25519_DER` (the file carries the `openssl genpkey` one-liner that generates it), and run:

```bash
cargo run -p capsule-api
```

Debug builds run the Sea-ORM migrations automatically at startup, so a fresh database needs no extra step locally. A release build does **not** — see [Self-Hosting](/guides/self-hosting/).

Tests that need a real database use testcontainers rather than the compose stack. Under podman that requires a Docker-compatible socket (`systemctl --user enable --now podman.socket`); see `capsule-api/README.md` for the platform notes.

## Git hooks

`mise run hooks-install` installs [hk](https://hk.jdx.dev/) from `hk.pkl`. Every hook step delegates to a mise task, so the hook and the CI job run identical commands:

- **pre-commit** — auto-fixing formatters and linters, scoped by glob to the files actually staged, with the fixes re-staged into the commit.
- **pre-push** — read-only checks plus tests over the range being pushed, including `check-commits` (convco).

`mise run hooks-uninstall` removes them.

## Known-broken local gates

Two lanes cannot be run on a developer machine today. Both are CI-only; do not treat a local failure in either as a regression you introduced.

- **`format-swift` / `format-check-swift`** — swiftformat 0.55 is SIGKILLed on macOS dev hosts, so the Swift formatting tasks cannot complete locally. The Swift tasks self-skip entirely off macOS.
- **The Kotlin lanes** (`format-kotlin`, `format-check-kotlin`, `lint-kotlin`, `lint-check-kotlin`, `test-kotlin`, `build-kotlin`) — the root Gradle build fails on recent JDKs, so `./gradlew` does not run locally. Kotlin and Android changes are verified in CI only. Note that the i18n half of the Compose gate deliberately does *not* go through Gradle: `i18n-guard` covers web, Swift, and Compose from Rust precisely so it stays locally runnable.
