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
- **Codegen gates:** `i18n` and `i18n-check` (compile `locales/` into the per-platform catalogs), `i18n-guard` (fails on hardcoded user-facing literals in web/Swift/Compose), `openapi-kynos` and `openapi-check-kynos` (dump/verify `capsule-server/openapi.json`), `translate-readme` and `translate-readme-check` (regenerate/verify the translated READMEs).
- **FFI and cross-compilation:** `targets-add` first, then `build-ffi`, `gen-bindings` (uniffi Kotlin/Swift sources), `build-apple`, `build-android`, `build-linux-cross`, `build-windows`, `build-targets`. On macOS, `setup-swift` builds the FFI xcframework and generates the Xcode workspace.

## Running a server locally

**There is no local server today, and that is a known gap rather than a missing instruction.**

`mise run serve-api` and the compose stack behind it went with the Salvo tree in slice `S-C59`. The Kynos server that replaces it is complete as a *surface* — fifty-nine operations, a committed OpenAPI 3.2 document, and a test suite that drives the real router — and it has **no binary, no configuration loading and no Postgres or Valkey adapter**. Nothing reads `JWT_ED25519_DER`, `SYNC_CURSOR_MAC_KEY` or `ATTESTATION_KEY_SEED` yet.

That ordering is deliberate: every port in `capsule-server` has a deterministic in-memory adapter and a conformance suite, because the suite is what a real adapter is written *against*, and a port with two implementations before it has one suite is a port whose implementations will disagree. Until those adapters land, the way to exercise the server is the way its own tests do — in process, with no container:

```bash
cargo nextest run -p capsule-server
```

`kynos::test::TestClient` drives a built `Service` directly: no socket, no port, no runtime flavour. One test (`tests/sdk_client.rs`) does bind an ephemeral port, because the property it proves — that the **generated** SDK client round-trips the real router over TCP — is the one an in-process client cannot.

To read the served contract without running anything:

```bash
mise run openapi-kynos      # regenerate capsule-server/openapi.json
```

### Nothing here needs a container any more

The testcontainers section this page used to carry is gone with the crate that needed it. No test in the workspace starts a container, so `mise run test-rust` has no podman prerequisite and cannot leak one. (The `containers` nextest group is kept, empty, for the first real adapter — the one-thread rule it encodes was learned by watching CI flake, and that is the expensive way to learn it.)

## Git hooks

`mise run hooks-install` installs [hk](https://hk.jdx.dev/) from `hk.pkl`. Every hook step delegates to a mise task, so the hook and the CI job run identical commands:

- **pre-commit** — auto-fixing formatters and linters, scoped by glob to the files actually staged, with the fixes re-staged into the commit.
- **pre-push** — read-only checks plus tests over the range being pushed, including `check-commits` (convco).

`mise run hooks-uninstall` removes them.

## Known-broken local gates

Two lanes cannot be run on a developer machine today. Both are CI-only; do not treat a local failure in either as a regression you introduced.

- **`format-swift` / `format-check-swift`** — swiftformat 0.55 is SIGKILLed on macOS dev hosts, so the Swift formatting tasks cannot complete locally. The Swift tasks self-skip entirely off macOS.
- **The Kotlin lanes** (`format-kotlin`, `format-check-kotlin`, `lint-kotlin`, `lint-check-kotlin`, `test-kotlin`, `build-kotlin`) — the root Gradle build fails on recent JDKs, so `./gradlew` does not run locally. Kotlin and Android changes are verified in CI only. Note that the i18n half of the Compose gate deliberately does *not* go through Gradle: `i18n-guard` covers web, Swift, and Compose from Rust precisely so it stays locally runnable.
