---
title: Local Development
description: The mise task graph, the services a local server needs, and which gates actually run on a developer machine.
status: draft
---

Every command runs through **[mise](https://mise.jdx.dev/)**. `mise.toml` pins the toolchain and
defines the task graph (long tasks live in `mise-tasks/`); `mise tasks` lists everything. A bare task
**fixes**; its `-check` variant **verifies only**. CI jobs map 1:1 onto the `check-*` tasks, so a
green local `check-*` is what CI runs. The Rust toolchain is pinned in `rust-toolchain.toml`, Swift
tooling in `capsule-swift/mise.toml`.

## First-time setup

```bash
mise install           # pinned tools (hk, convco, cargo-nextest, wasm-bindgen-cli, ...)
mise run setup         # bun install in capsule-web + capsule-docs
mise run hooks-install # git hooks (hk)
```

## Gates

| Task | Scope |
| --- | --- |
| `mise run check` | the full gate: `format-check` → `lint-check` → `test` |
| `mise run check-rust` | Rust format, clippy, docs, codegen drift (`i18n`, OpenAPI, READMEs), architecture and licence checks, builds and bindings — sequential, one `target/` lock |
| `mise run check-web` | web format, lint, test, build |
| `mise run check-docs` | docs format, lint, Astro build (a broken internal link fails it) |
| `mise run check-md` | markdownlint over every `.md` |
| `mise run check-kotlin` | Kotlin format and lint |

Frequently used: `test-rust` (nextest over the workspace, then `capsule-sdk --features ffi`),
`test-rust-release` (the `#[cfg(not(debug_assertions))]` tests in `RELEASE_TEST_CRATES`),
`test-rust-containers` ([below](#the-container-tiers)), `test-web` (builds the WASM glue and KAT
fixtures first), `i18n`, `openapi-kynos`, and for FFI `targets-add` then `build-ffi` and
`gen-bindings`. On macOS, `setup-swift` builds the xcframework and the Xcode workspace.

## Running a server locally

```text
capsule-server [--config PATH] <SUBCOMMAND>
  serve       [--listen HOST:PORT] [--memory] [--blob-root PATH]
  gc          [--apply] [--grace-window-hours N] [--memory] --blob-root PATH
  purge       [--apply] [--limit N]              [--memory] --blob-root PATH
  scrub       [--deep] [--budget BYTES]          [--memory] --blob-root PATH
  gen-openapi [FILE] [--check]
```

Settings come from flag, then environment, then default; `capsule-server/.env.example` is a starting template, and
`Config::load` in `capsule-server/src/config.rs` reads the full set.
There is no configuration file (`--config` is refused). Every configuration fault is reported in
**one** message with exit code 2.

### The development profile

```bash
mise run serve-memory
```

Binds `127.0.0.1`, prints its address, and answers every operation with real Argon2id sign-in. Blobs
go to a real filesystem store under `target/capsule-server-blobs`; everything else is in-process and
lost on exit. So after a restart `scrub` reports every blob as an orphan, and `gc` can only mark,
never sweep.

It signs tokens with the published example key unless `JWT_ED25519_DER` is set — tokens are forgeable,
which is why it is loopback-only:

```bash
JWT_ED25519_DER="$(openssl genpkey -algorithm ed25519 -outform DER | base64 | tr -d '\n')" \
  mise run serve-memory
```

### A configured server

```bash
cp capsule-server/.env.example capsule-server/.env   # then edit it
mise run serve-deps                                  # Postgres + Valkey, on loopback
mise run serve
```

Both secrets, `JWT_ED25519_DER` and `ATTESTATION_KEY_SEED`, ship commented out, so an unedited copy
refuses to start. Uncomment each and supply your own. `ATTESTATION_KEY_SEED` MUST be independent of
the token key ([Failure Modes](/design/cryptography/failure-modes/)). Values are literal (no shell
expressions) because podman, compose, and systemd read the file too.

`serve` checks `DATABASE_URL`'s schema against its migrations (behind → refuses, naming
`capsule-server-migration up`), then `PING`s `VALKEY_URL`. A missing or unreachable store is a
refusal, never a silent fall-back to in-memory adapters. `serve-deps` is separate on purpose; stop
it with `podman compose -f capsule-server/compose.yaml down`.

TLS is terminated by the ingress, not the server.

### Logs

Logs go to **stderr**; stdout is data (`serve` prints `listening on <url>`, operator commands print
their report). `LOG_FORMAT` is `pretty` in debug builds and `json` in release; `RUST_LOG` filters.

### The operator commands

`gc`, `purge`, and `scrub` are the jobs in [Filesystem — Maintenance](/design/filesystem/maintenance/).
They take a blob root and **no key material**. Without `--memory` they read Postgres (index, marks)
and, for `scrub`, Valkey (live upload sessions). `gc` and `purge` dry-run unless `--apply`; `scrub`
mutates nothing and exits non-zero on a non-empty report, including a `--deep` pass that ran out of
budget.

### Without running anything

```bash
cargo nextest run -p capsule-server   # in-process, via kynos::test::TestClient
mise run openapi-kynos                # regenerate capsule-server/openapi.json
```

### The container tiers

`test-rust` needs no podman: cases needing Postgres or Valkey are gated on
`CAPSULE_TEST_POSTGRES=1` / `CAPSULE_TEST_VALKEY=1` and otherwise print a skip line and pass.
`test-rust-containers` enables both and runs every gated case one container at a time, **failing on
any skip line**; CI runs it as the blocking `rust-containers` job. Locally, with rootless podman:

```bash
systemctl --user start podman.socket
export DOCKER_HOST=unix:///run/user/$(id -u)/podman/podman.sock
export CAPSULE_TEST_CONTAINER_USERNS=keep-id
mise run test-rust-containers
```

`CAPSULE_TEST_VALKEY_URL=redis://127.0.0.1:6379` runs the Valkey suite against an existing server
instead; it writes under the `capsule:` namespace.

## Profiling a build

The `profiling` Cargo profile is `release` plus line tables. It has the same LTO and codegen units,
so the samples describe the code that ships, and the line tables let a profiler map them to
`file:line`, including inlined frames:

```bash
cargo build --profile profiling -p capsule-server
```

The binary lands in `target/profiling/`. Any sampler that reads DWARF works on it; neither is a
mise tool, so install one yourself. With [samply](https://github.com/mstange/samply), run it the
way `serve-memory` does, with a fresh key and bound to loopback:

```bash
JWT_ED25519_DER="$(openssl genpkey -algorithm ed25519 -outform DER | base64 | tr -d '\n')" \
BLOB_ROOT=./target/capsule-server-blobs \
  samply record target/profiling/capsule-server serve --memory --listen 127.0.0.1:3000
```

`perf record --call-graph dwarf` works on the same binary.

## Git hooks

`mise run hooks-install` installs [hk](https://hk.jdx.dev/) from `hk.pkl`; each step delegates to a
mise task. **pre-commit** runs auto-fixing formatters and linters on staged files and re-stages the
fixes; **pre-push** runs read-only checks, tests, and `check-commits` (convco). `mise run
hooks-uninstall` removes them.

## Known-broken local gates

These are CI-only; a local failure is not a regression you introduced.

- **`format-swift` / `format-check-swift`** — swiftformat is SIGKILLed on macOS dev hosts. Swift
  tasks self-skip off macOS.
- **Kotlin lanes** (`*-kotlin`) — the root Gradle build fails on recent JDKs. `i18n-guard` covers
  Compose from Rust so that check stays local.
