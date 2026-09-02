---
title: Local Development
description: The mise task graph, the services a local server needs, and which gates actually run on a developer machine.
status: draft
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
| `mise run check-rust` | `format-check-rust`, `lint-check-rust`, `i18n-check`, `i18n-guard`, `openapi-check-kynos`, `architecture-check`, `license-check`, `translate-readme-check`, `build-rust`, `build-check-wasm`, `build-ffi`, `lint-check-ffi`, `gen-bindings`, `verify-examples` — sequential, because they all contend on one `target/` lock |
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

`capsule-server` is one binary with subcommands:

```text
capsule-server [--config PATH] <SUBCOMMAND>
  serve       [--listen HOST:PORT] [--memory] [--blob-root PATH]
  gc          [--apply] [--grace-window-hours N] [--memory] [--blob-root PATH]
  purge       [--apply] [--limit N] [--memory] [--blob-root PATH]
  scrub       [--deep] [--budget BYTES] [--memory] [--blob-root PATH]
  gen-openapi [FILE] [--check]
```

### The development profile

```bash
mise run serve-memory
```

That is a server you can point a client at: it binds, prints the address it bound, and answers
every operation. An account registers and signs in — the credential is checked with Argon2id
against a real in-memory account directory, so a wrong password is refused rather than accepted.

What it is missing is durability. The blob store is a **real** filesystem store under
`target/capsule-server-blobs`; everything else — the index, sessions, albums, the device
directory, quota, the collector's marks — lives in the process and is gone when it exits. That
is not a gap to route around, it is the shape of a profile whose durable half is exactly the one
adapter that has been written. Two consequences worth knowing before they surprise you:

- After a restart, `capsule-server scrub` will honestly report every blob still on disk as an
  orphan, because the index that referenced them is gone.
- `capsule-server gc` can only ever **mark** in this profile. Collection is two passes by
  design — a blob that reaches zero references is marked, and swept on a later pass once the
  grace window has passed — and the mark store does not outlive the process.

The signing key `serve-memory` falls back to is the published example in
`capsule-server/.env.example`. Every token it mints is forgeable by anyone who has read this
repository, which is why that task is `serve-memory` and not `serve`. Set `JWT_ED25519_DER`
yourself and it is used instead:

```bash
JWT_ED25519_DER="$(openssl genpkey -algorithm ed25519 -outform DER | base64 | tr -d '\n')" \
  mise run serve-memory
```

### A configured server

```bash
cp capsule-server/.env.example capsule-server/.env   # then edit it
mise run serve-deps                                  # Postgres 18 + Valkey 9
mise run serve
```

`serve-deps` and `serve` are separate tasks on purpose: a task that silently starts containers is
a task that leaks them. Bring them down with
`podman compose -f capsule-server/compose.yaml down` (`docker compose` accepts the same file).

**`mise run serve` does not work yet, and refuses rather than pretending.** The Postgres and
Valkey adapters are not written. Without `VALKEY_URL` and without `--memory` it exits 2 naming
the variable — the refusal `capsule-server/src/store/mod.rs` has documented since `S-C29` and
nothing could enforce until there was a boot path; with `VALKEY_URL` set it exits non-zero naming
the issue that will honour it. Neither ever silently falls back to the in-memory adapters, which
is the whole point: a deployment that forgot a variable must fail closed.

Every configuration fault is reported in **one** message with exit code 2, so bringing a
deployment up is one read of one log line rather than one restart per variable.

`capsule-server/.env.example` is the full list of settings. The precedence is command-line flag,
then the environment, then the built-in default; there is no configuration file, and `--config
PATH` is accepted and refused with a sentence saying so.

### TLS

The server does not terminate it. HTTPS is the ingress or reverse proxy's job — see
[Cryptography — Failure Modes](/design/cryptography/failure-modes/) — so there is no certificate
setting and Kynos's `tls` feature is off.

### Logs and reports

Every log line goes to **stderr**; stdout is a data channel. `serve` writes one
`listening on <url>` line there (which is how a `--listen 127.0.0.1:0` caller learns its port),
`gen-openapi` writes the path it wrote, and the operator commands write their report. `LOG_FORMAT`
is `pretty` in a debug build and `json` in a release one; `RUST_LOG` is the usual filter.

### The operator commands

`gc`, `purge` and `scrub` are the three jobs
[Filesystem — Maintenance](/design/filesystem/maintenance/) describes. They need a blob root and
deliberately **no key material**: a maintenance host that had to hold the production
token-signing key to sweep a directory would be a reason to put the key on a maintenance host.

Dry run is the default for the two that write; `--apply` opts in, and the report says which
posture produced it. `scrub` mutates nothing at all and exits non-zero on a non-empty report,
which is what makes it usable as a monitoring probe — and a `--deep` pass that ran out of budget
says so, because a clean report from a pass that stopped early is not a clean store.

### Without running anything

To exercise the server the way its own tests do — in process, no socket, no container:

```bash
cargo nextest run -p capsule-server
```

`kynos::test::TestClient` drives a built `Service` directly. Two test files do use a socket:
`capsule-server/tests/sdk_client.rs`, because the property it proves is that the **generated**
SDK client round-trips the real router over TCP, and `capsule-server/tests/binary.rs`, because
the properties it proves — that the binary binds, reports its port, and drains to exit 0 on
SIGTERM — belong to a process rather than to a router.

To read the served contract without running anything:

```bash
mise run openapi-kynos      # regenerate capsule-server/openapi.json
```

### Nothing here needs a container

No test in the workspace starts a container, so `mise run test-rust` has no podman prerequisite
and cannot leak one. `mise run serve-deps` is the only task that starts anything, and it is never
a dependency of another task. (The `containers` nextest group is kept, empty, for the first real
adapter — the one-thread rule it encodes was learned by watching CI flake, and that is the
expensive way to learn it.)

## Git hooks

`mise run hooks-install` installs [hk](https://hk.jdx.dev/) from `hk.pkl`. Every hook step delegates to a mise task, so the hook and the CI job run identical commands:

- **pre-commit** — auto-fixing formatters and linters, scoped by glob to the files actually staged, with the fixes re-staged into the commit.
- **pre-push** — read-only checks plus tests over the range being pushed, including `check-commits` (convco).

`mise run hooks-uninstall` removes them.

## Known-broken local gates

Two lanes cannot be run on a developer machine today. Both are CI-only; do not treat a local failure in either as a regression you introduced.

- **`format-swift` / `format-check-swift`** — swiftformat 0.55 is SIGKILLed on macOS dev hosts, so the Swift formatting tasks cannot complete locally. The Swift tasks self-skip entirely off macOS.
- **The Kotlin lanes** (`format-kotlin`, `format-check-kotlin`, `lint-kotlin`, `lint-check-kotlin`, `test-kotlin`, `build-kotlin`) — the root Gradle build fails on recent JDKs, so `./gradlew` does not run locally. Kotlin and Android changes are verified in CI only. Note that the i18n half of the Compose gate deliberately does *not* go through Gradle: `i18n-guard` covers web, Swift, and Compose from Rust precisely so it stays locally runnable.
