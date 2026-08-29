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

The current `capsule-api` server is being rebuilt on [Kynos](/development/architecture/) as a single REST/OpenAPI surface; until that rebuild reaches parity the existing server is what runs locally, and `serve-api` keeps working unchanged.

One command:

```bash
mise run serve-api
```

It brings up the server's two external services from `capsule-api/compose.yaml` — **PostgreSQL** and **Valkey**, which are all it needs; there is no object store, because ciphertext blobs are files under `UPLOAD_DIR`. It then seeds `capsule-api/.env` if absent and tops up any required variable that is missing, minting a `JWT_ED25519_DER` via `mise run keygen` (no `openssl` required — the generator lives beside the parser that reads the key, so a minted key is provably one the server accepts). Finally it waits for both services to answer and runs the server on `http://127.0.0.1:3000`.

Point a client at it with `export CAPSULE_ENDPOINT=http://127.0.0.1:3000`.

Debug builds run the Sea-ORM migrations automatically at startup, so a fresh database needs no extra step locally. A release build does **not** — see [Self-Hosting](/guides/self-hosting/). That is also why `serve-api` never passes `--release`.

To drive it by hand, note that `dotenvy` searches the *current* directory, so `cargo run -p capsule-api` from the repo root will not see `capsule-api/.env` — either `cd capsule-api` first, or export the file yourself as `serve-api` does.

Tests that need a real database use testcontainers rather than the compose stack. Under podman that requires a Docker-compatible socket (`systemctl --user enable --now podman.socket`); see `capsule-api/README.md` for the platform notes.

### Testcontainers leak under podman — reclaim before you are stuck

Rootless podman needs `TESTCONTAINERS_RYUK_DISABLED=true`, and Ryuk is the component that would
otherwise reap containers after a run. Disabled, **every testcontainer suite leaves its container
and its volume behind.** The podman VM's root filesystem is only 8.5 GB, so after a few dozen runs
it fills and the next suite fails like this:

```text
start Valkey: … creating container storage: … no space left on device
```

That reads like a test failure and is not one — nothing is wrong with the code, and no amount of
re-running helps. Reclaim, then re-run:

```bash
# Leaked testcontainers use different image tags than the compose services
# (postgres:15-alpine / valkey:8.0.1 vs postgres:17 / valkey:8.1.1), so this
# cannot touch the stack `serve-api` depends on.
podman rm -f $(podman ps -aq \
  --filter ancestor=docker.io/library/postgres:15-alpine \
  --filter ancestor=docker.io/valkey/valkey:8.0.1)
podman volume prune -f        # the containers' volumes outlive them
podman machine ssh "df -h /"  # confirm headroom before re-running
```

One clearing recovered 46 containers and 48 volumes, taking the VM from 457 MB free to 2.4 GB.
`podman system df` shows what is reclaimable if that is not enough — unused images are usually the
next largest bucket.

## Git hooks

`mise run hooks-install` installs [hk](https://hk.jdx.dev/) from `hk.pkl`. Every hook step delegates to a mise task, so the hook and the CI job run identical commands:

- **pre-commit** — auto-fixing formatters and linters, scoped by glob to the files actually staged, with the fixes re-staged into the commit.
- **pre-push** — read-only checks plus tests over the range being pushed, including `check-commits` (convco).

`mise run hooks-uninstall` removes them.

## Known-broken local gates

Two lanes cannot be run on a developer machine today. Both are CI-only; do not treat a local failure in either as a regression you introduced.

- **`format-swift` / `format-check-swift`** — swiftformat 0.55 is SIGKILLed on macOS dev hosts, so the Swift formatting tasks cannot complete locally. The Swift tasks self-skip entirely off macOS.
- **The Kotlin lanes** (`format-kotlin`, `format-check-kotlin`, `lint-kotlin`, `lint-check-kotlin`, `test-kotlin`, `build-kotlin`) — the root Gradle build fails on recent JDKs, so `./gradlew` does not run locally. Kotlin and Android changes are verified in CI only. Note that the i18n half of the Compose gate deliberately does *not* go through Gradle: `i18n-guard` covers web, Swift, and Compose from Rust precisely so it stays locally runnable.
