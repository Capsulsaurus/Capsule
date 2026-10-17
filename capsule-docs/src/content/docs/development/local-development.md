---
title: Local Development
description: The mise task graph, the services a local server needs, and which gates actually run on a developer machine.
status: draft
---

Every command in this repository runs through **[mise](https://mise.jdx.dev/)**. `mise.toml` at the repo root pins the shared toolchain and defines the whole task graph; anything too long for a TOML line lives as a shebang script in `mise-tasks/`. CI jobs map 1:1 onto the `check-*` tasks below, so a green local `check-*` is the same command CI runs.

## First-time setup

```bash
mise install          # installs the pinned tools from [tools] in mise.toml
mise run setup        # bun install in capsule-web + capsule-docs
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
| `mise run check-kotlin` | `format-check-kotlin`, `lint-check-kotlin` |

### The ones you run most

- **Rust:** `format-rust` (`cargo fmt`), `lint-rust` / `lint-check-rust` (`cargo clippy --workspace` with the shared `$CLIPPY_FLAGS` set — lints are configured once in `mise.toml`, never per crate), `test-rust` (`cargo nextest run`, three invocations: the workspace, then `capsule-core` and `capsule-sdk` with `--features ffi`), `test-rust-release` (the `#[cfg(not(debug_assertions))]` tests, which every test-profile build compiles out: `cargo nextest run --cargo-profile release` over the crates listed in `RELEASE_TEST_CRATES`, after a guard that fails when such a test appears in any other crate — CI's test job and pre-push both run it), `test-rust-containers` (the Postgres and Valkey container tiers `test-rust` skips, failing on any skip — see [The container tiers](#the-container-tiers)), `build-rust`, `test-coverage-rust`.
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
  gc          [--apply] [--grace-window-hours N] [--memory] --blob-root PATH
  purge       [--apply] [--limit N]              [--memory] --blob-root PATH
  scrub       [--deep] [--budget BYTES]          [--memory] --blob-root PATH
  gen-openapi [FILE] [--check]
```

Without `--memory` the three operator commands run on the same Postgres and Valkey stores `serve`
writes, and need `DATABASE_URL` and then `VALKEY_URL`.

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
`capsule-server/.env.example` — commented out there, so a `cp .env.example .env` cannot silently
produce a forgeable deployment. Every token the task mints under it is forgeable by anyone who has
read this repository, which is why it is `serve-memory` and not `serve`, and why it binds
`127.0.0.1` rather than every interface. Set `JWT_ED25519_DER` yourself and it is used instead:

```bash
JWT_ED25519_DER="$(openssl genpkey -algorithm ed25519 -outform DER | base64 | tr -d '\n')" \
  mise run serve-memory
```

### A configured server

```bash
cp capsule-server/.env.example capsule-server/.env   # then edit it
mise run serve-deps                                  # Postgres 18 + Valkey 9, on loopback
mise run serve
```

The template ships with **both secrets commented out** — `JWT_ED25519_DER` and
`ATTESTATION_KEY_SEED` — so a copy you have not finished editing produces a server that refuses
and names what it wants, rather than one that starts under a published key. Uncomment each and
put your own value in. Every other setting is either a working default or optional.

Nothing in the template is a shell expression, deliberately: the file is read by more than a
shell — `podman --env-file`, compose's `env_file:`, systemd's `EnvironmentFile=` — and those take
a line literally, so a placeholder shaped like `$(...)` would be stored as the value rather than
replaced.

`serve-deps` and `serve` are separate tasks on purpose: a task that silently starts containers is
a task that leaks them. Bring them down with
`podman compose -f capsule-server/compose.yaml down` (`docker compose` accepts the same file).

**`mise run serve` runs on Postgres and Valkey once the schema is migrated.** `DATABASE_URL` is
opened first and its schema checked against the migrations this binary was built for — a
database that is behind refuses, naming `capsule-server-migration up`. Then `VALKEY_URL` is
connected to and `PING`ed, and a server that cannot be reached is a refusal naming the failure
(never the URL). Every port is then on one of the two. Without `VALKEY_URL` and without
`--memory` it exits 2 naming the variable — the refusal `capsule-server/src/store/mod.rs` has
documented since `S-C29`. With `OIDC_ISSUER` set it refuses too, because the OIDC stores have no
durable adapter yet (#460). None of these ever silently falls back to the in-memory adapters,
which is the whole point: a deployment that forgot a variable must fail closed.

The Valkey adapters are proven against a live server by `capsule-server/tests/valkey.rs`, which
runs the same conformance suites the in-memory doubles pass. It is env-gated so `mise run
test-rust` needs no podman: `CAPSULE_TEST_VALKEY=1` starts a `valkey/valkey` container through
testcontainers (`DOCKER_HOST` pointing at the podman socket; `CAPSULE_TEST_VALKEY_TAG` overrides
the image tag), and `CAPSULE_TEST_VALKEY_URL=redis://127.0.0.1:6379` runs it against a server
already up — the one `mise run serve-deps` starts, say. The suite writes under the `capsule:`
namespace, so point it at a database you do not mind sharing with test keys.

A configured server also has to supply `ATTESTATION_KEY_SEED`. It is **not** derived from
`JWT_ED25519_DER`, and that is deliberate: the attestation key signs custody receipts and has to
be distinct from the key that signs session tokens, or anyone holding the operational key could
manufacture custody evidence — see
[Cryptography — Failure Modes](/design/cryptography/failure-modes/). A different HKDF label over
the same input is not a separation. `serve --memory` derives it, because a development server's
whole state is discarded when it exits.

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

Without `--memory` they read the durable stores: the index and the collector's marks in Postgres,
and — for the scrub, which tells a live transfer from an orphan by them — the upload sessions in
Valkey. The marks being durable is what lets one `gc` invocation sweep what an earlier one marked;
under `--memory` each fresh process can only mark.

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

### The container tiers

`mise run test-rust` has no podman prerequisite and cannot leak a container: every case that
needs a real Postgres or Valkey is gated on `CAPSULE_TEST_POSTGRES=1` / `CAPSULE_TEST_VALKEY=1`
and, without its variable, prints one `skipping <case>:` (or `skipped:`) line and passes.

`mise run test-rust-containers` is the other half. It admits both tiers itself and runs every
gated case — the Postgres adapter conformance suites, the migration up/down round trip, the
durable boot arm and process cases (all under a `postgres_conformance` test module) and
`capsule-server/tests/valkey.rs` — one container per case, one case at a time, in the
`containers` nextest group. It **fails when any case prints a skip line**, because a tier that
silently ran nothing reads exactly like one that passed. CI runs it as the blocking
`rust-containers` job on the runner's Docker. Locally, point it at rootless podman:

```bash
systemctl --user start podman.socket
export DOCKER_HOST=unix:///run/user/$(id -u)/podman/podman.sock
export CAPSULE_TEST_CONTAINER_USERNS=keep-id
mise run test-rust-containers
```

`mise run serve-deps` is the only task that starts a long-lived container, and it is never a
dependency of another task.

## Git hooks

`mise run hooks-install` installs [hk](https://hk.jdx.dev/) from `hk.pkl`. Every hook step delegates to a mise task, so the hook and the CI job run identical commands:

- **pre-commit** — auto-fixing formatters and linters, scoped by glob to the files actually staged, with the fixes re-staged into the commit.
- **pre-push** — read-only checks plus tests over the range being pushed, including `check-commits` (convco).

`mise run hooks-uninstall` removes them.

## Known-broken local gates

Two lanes cannot be run on a developer machine today. Both are CI-only; do not treat a local failure in either as a regression you introduced.

- **`format-swift` / `format-check-swift`** — swiftformat 0.55 is SIGKILLed on macOS dev hosts, so the Swift formatting tasks cannot complete locally. The Swift tasks self-skip entirely off macOS.
- **The Kotlin lanes** (`format-kotlin`, `format-check-kotlin`, `lint-kotlin`, `lint-check-kotlin`, `test-kotlin`, `build-kotlin`) — the root Gradle build fails on recent JDKs, so `./gradlew` does not run locally. Kotlin and Android changes are verified in CI only. Note that the i18n half of the Compose gate deliberately does *not* go through Gradle: `i18n-guard` covers web, Swift, and Compose from Rust precisely so it stays locally runnable.
