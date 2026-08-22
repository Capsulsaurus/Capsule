---
title: Dependencies
description: Canonical library and tooling pins for every Capsule platform
status: draft
---

This doc is the single owner of Capsule's **canonical implementation-dependency pins**: for each domain concern, the one library that implements it, where the pin applies, and the bounded exceptions. Introducing a dependency for a domain not listed here requires adding a row here first; changing a pin is a one-doc edit plus a migration slice in the repo-root `SLICES.md` — never an in-place drift.

What this doc deliberately does **not** own, per the [SSoT rule](/design/principles/#single-source-of-truth):

- **Cryptographic primitives and their crates** — [Cryptography — Primitives](/design/cryptography/primitives/).
- **TLS version policy** — [Failure Modes — Transport Security](/design/cryptography/failure-modes/#transport-security). This doc pins the *implementation* (rustls); the policy lives there.
- **Client test and performance tooling** — [Clients — Test and Performance Tooling](/design/clients/#test-and-performance-tooling).
- **Which existing identifier uses which UUID version** — [Metadata — Identifiers](/design/metadata/#identifiers). This doc owns only the default-for-new rule.
- **Media formats and codecs** — [Thumbnails and Previews](/design/thumbnails/) owns derivative formats; the sidecar's `content_type` set is owned by [Metadata](/design/metadata/#closed-enum-value-sets).

Mechanically, every Rust version is pinned once in the root `Cargo.toml` `[workspace.dependencies]`; member crates consume it with `workspace = true` and never declare their own version. Toolchain versions (Rust nightly, bun, tuist, …) are pinned by mise per `CONTRIBUTING.md`.

## Rust

| Domain | Canonical choice | Scope | Exceptions |
| --- | --- | --- | --- |
| Datetime | `jiff` | All domain logic — parsing, formatting, arithmetic. Signed and wire formats carry RFC 3339 strings or integer epochs, never a datetime library type, so the pin never touches serialized bytes. | `chrono` remains **only** as the sea-orm column type in `capsule-cli/entity`, converted to jiff at the entity boundary, and in non-buildable review-only server code under `legacy-review/`. (The last buildable non-entity holdout, the frozen `capsule-api-library` GraphQL crate with its async-graphql `chrono` scalars, was removed with slice S-G1.) |
| Error handling | `thiserror` in libraries; `eyre` + `color-eyre` in binaries | Libraries define typed error enums; binaries (CLI, server `main`, xtask) wrap them in reports. | `anyhow` is not used. |
| Logging | `tracing` (facade) + `tracing-subscriber` (binaries) | All crates; structured fields and hot-path spans per the traceability rule in `AGENTS.md`. The `log` facade is forbidden in new code. | Remaining `log::` call sites in `capsule-core` / `capsule-core-ffi` migrate in slice S-F6. |
| TLS implementation | `rustls` (with `tokio-rustls` as the async adapter) | Wherever Capsule code holds a TLS stack: the SDK's HTTP client, [LAN-peering](/design/peering/) mutual TLS (`tokio-rustls`), server egress, sea-orm's `runtime-tokio-rustls`. The `ring` provider is pinned for the peering stack so it never depends on an ambiguous process-default `CryptoProvider`. Never native-tls/openssl. | `openssl` appears only as a transitive dependency of `webauthn-rs` attestation-certificate verification — it is never a TLS stack. |
| X.509 leaf generation | `rcgen` | The per-connection self-signed leaf the [LAN-peering](/design/peering/) mTLS handshake presents. The certificate carries no trust of its own — peering is CA-less and identity is decided by the application-layer hybrid check — so the leaf is ephemeral. `ring` provider, matching the rustls pin above. | Server-facing certificates are operator-provisioned, not minted in-process. |
| LAN service discovery | mocked seam (`capsule-sdk::peering::Discovery`) | Peering's mDNS advertisement/browse is behind a trait seam; the opaque, rotating descriptor is pure and unit-tested. A live responder (pure-Rust `mdns-sd`) is the sanctioned implementation to plug in — added by a follow-up slice with its own row, since a live multicast responder is non-deterministic and untestable in CI. | — |
| Identifiers | `uuid` — **UUIDv7 for every newly introduced identifier** | Time-ordered v7 is the default (index locality); the assignment of existing ids is owned by [Metadata — Identifiers](/design/metadata/#identifiers). | UUIDv4 where an id must not leak creation time (e.g. `device_id`). Capability-bearing opaque ids (share links, drops) are not UUIDs at all — they carry their own ≥128-bit entropy per their owner docs. |
| Async runtime | `tokio` | All async code. | — |
| HTTP server | Kynos | All `capsule-api` REST/OpenAPI surfaces, including sync and federation. | No secondary public transport. |
| Kynos sourcing | `kynos`, consumed as a **git dependency** pinned to rev `6513109b5725a3e0713808de0eaee6b4b74281e3` — crate path `crates/kynos` in `https://github.com/getkono/kynos` | Kynos is not published on crates.io (the name resolves to *no such crate*) and its README reads "Status: Not for public use yet!", so the root `[workspace.dependencies]` entry pins an immutable **rev**, never a branch or tag, and is bumped deliberately in its own change. Kynos is tokio-only, OpenAPI 3.1-native (3.2 opt-in), MSRV **1.85**, edition 2024. | Repin to a crates.io version the moment Kynos publishes; until then a rev bump is a reviewed dependency change, not a lockfile refresh. |
| HTTP client | `reqwest` (`default-features = false`, `rustls-tls`) | `capsule-sdk` — the sanctioned network path. | — |
| REST client codegen | `spargen` (in-house, OpenAPI **3.1**) | `capsule-sdk` **build-dependency only**: `build.rs` lowers the committed `capsule-sdk/openapi.json` — emitted deterministically from the Kynos server's own OpenAPI document, the checked contract Kynos derives from the types the server runs on (`mise run openapi`, no live infrastructure) — into the typed `rest::Client`, wrapped by `client::AuthenticatedClient` (slice S-D8). Its runtime support is embedded into the generated module, so spargen never enters the SDK's runtime tree. 3.1-native — the schema is **never** downgraded to 3.0, and Kynos never emits 3.0. | Progenitor is gone (3.0-only). The generated surface is narrowed to plain request/response operations: the hand-written upload protocol (S-D1) is not routed through it, and byte-serving asset endpoints are excluded (byte serving + an object query param spargen 0.1.0 cannot yet lower). |
| ORM | `sea-orm` (`sqlx-postgres` on the server, `sqlx-sqlite` in the CLI) | The rebuildable index databases only — sidecars stay canonical per [Principles](/design/principles/). | — |
| Embedded SQLite | `rusqlite` (`bundled`) | `capsule-core`'s `library.sqlite`. | — |
| Vector index | `sqlite-vec` (`vec0`) | The client-local embedding index in `capsule-core`'s `library.sqlite` — per-task `vec0` virtual tables under the [embedding-provenance](/design/ai/#embedding-provenance) invariant. Optional + `native`-gated alongside `rusqlite` (registers as a SQLite auto-extension; not `wasm32`). | Server-side vector-DB idioms (pgvector/HNSW) do not apply — the index is client-local SQLite by design. |
| Free-space probe | `rustix` (Unix, `fs`) + `windows-sys` (Windows, `Win32_Storage_FileSystem`) | `capsule-core::library::available_bytes` — the streaming-import free-space probe (`statvfs` / `GetDiskFreeSpaceEx`). Host-only, behind the `native` feature; the wasm32 sealing build links neither. | — |
| Windows TPM (TBS) | `windows-sys` (Windows, `Win32_System_TpmBaseServices`) | `capsule-core::crypto::keys::tbs` — the Windows device-key `HardwareSigner` (slice S-F4). The raw TPM 2.0 command channel (`Tbsi_Context_Create` / `Tbsip_Submit_Command`) the tss-esapi reference (`crypto::keys::tpm`, Linux) wraps; links `tbs.dll` via raw-dylib, so no new crate — an extra feature on the existing `windows-sys` row. `#[cfg(windows)]`-gated; the pure wire codec + mock tests run on any host. | Not tss-esapi on Windows: TBS is native and avoids the `libtss2`/bindgen build. |
| MLS group layer | `openmls` (`libcrux-provider`) + `openmls_libcrux_crypto` + `openmls_basic_credential` + `openmls_traits` + `openmls_memory_storage` | `capsule-core::crypto::authority::OpenMlsAuthority` — the live RFC 9420 MLS backend (slices S-X1/S-X2), pinned to the X-Wing PQ ciphersuite `MLS_256_XWING_CHACHA20POLY1305_SHA256_Ed25519` (`0x004D`). The libcrux provider is the only released one shipping the X-Wing suite (formally-verified ML-KEM + X25519 from Cryspen). `openmls_memory_storage` (S-X2) is the OpenMLS storage provider Capsule *owns* — the libcrux provider bundles a private one, so pulling it in directly lets the authority serialize durable group state (`export_state`/`import_state`) via the provider's public `values` keyspace. Same version the libcrux provider already resolves transitively. Host-only, behind the `mls` feature (implied by `native`); the wasm32 sealing build excludes it — libcrux does not target `wasm32-unknown-unknown`. The offline `ReferenceAuthority` stays available on every build. | Not `mls-rs`: no third-party audit and no PQ suite (see [Cryptography — MLS](/design/cryptography/mls/)). `0x004D` is a private/experimental codepoint (no IANA number) — acceptable for Capsule's closed deployment; a future move to a WG-standardized ML-KEM-hybrid suite rides the album upgrade ceremony (S-X3). |
| CBOR | `ciborium` (+ `serde_bytes`, `half`) | All CBOR; the canonical-encoding rules are owned by [Metadata — Canonical CBOR Encoding](/design/metadata/#canonical-cbor-encoding). | — |
| Serialization | `serde` + `serde_json` | Derives and JSON surfaces. | — |
| CLI | `clap` (derive) | `capsule-cli`, xtask argument parsing where non-trivial. | — |
| Test runner | `cargo-nextest`; `testcontainers` (+ podman) for real backing services | The Unit/Smoke tiers per [Principles — Validation Tiers](/design/principles/#validation-tiers). Binary-level smokes spawn the built binary through Cargo’s own `CARGO_BIN_EXE_<name>` from a `tests/` target (`capsule-cli/tests/cull_round_trip.rs`, slice S-D16) — a real process boundary with no crate to add. | No `assert_cmd`/`predicates`: they would buy assertion sugar over `std::process::Command`, not capability, and the minimalism rule does not trade a dependency for sugar. |
| FFI bindings | `uniffi` | One workspace version — consolidation is slice S-F1. | — |
| Browser FFI (WASM) | `wasm-bindgen` (+ `wasm-bindgen-cli`, mise-pinned) | `capsule-wasm` — the guest web client's share-link open surface (slice S-E1): parse the URL fragment secret, Argon2id passphrase unwrap, scope decapsulation, STREAM blob decrypt. Compiles `capsule-core` (`--no-default-features`: `crypto` + `cbor` + `sharing`) to `wasm32-unknown-unknown`; `wasm-bindgen-cli` (the `build-wasm` mise task, `--target web`) post-processes the cdylib into the browser JS glue. The CLI and crate are pinned to the **same** version (`0.2.100`) — wasm-bindgen requires it. Randomness rides the getrandom `wasm_js` backend already pinned in `.cargo/config.toml`. | Not wasm-pack: it fetches a version-matched wasm-bindgen at run time, breaking the offline gate; the `cargo build` + `wasm-bindgen` path uses only pre-installed toolchains. |

## Web

| Domain | Canonical choice | Notes |
| --- | --- | --- |
| Framework | React 19 + rsbuild | — |
| Routing / data | TanStack Router / TanStack Query | — |
| Styling | Tailwind v4, class-based dark mode (`@custom-variant dark` + `ThemeProvider`) | — |
| Validation | zod | — |
| Datetime | **none** — native `Intl` / `Date` | A date library is added only by adding a row here. |
| i18n runtime | FormatJS over the generated catalogs | Contract owned by [Internationalization](/design/i18n/). |
| Guest share crypto (WASM) | `capsule-wasm` module (built by `mise run build-wasm`) | The `/s/{opaque-id}` viewer's client-side share-link open path — the URL fragment secret and any passphrase never leave the browser (slice S-E1). Generated into the gitignored `src/generated/wasm/`; never committed. The Rust FFI crate + build tool are the [Rust — Browser FFI](#rust) row. |
| Lint/format | Biome | — |
| Runtime / package manager | bun | — |

## Swift

Test frameworks and performance tooling are owned by [Clients — Test and Performance Tooling](/design/clients/#test-and-performance-tooling). Bindings come from the single-version uniffi strategy (slice S-F1). Project generation and formatting (Tuist, SwiftLint, SwiftFormat) are mise-pinned toolchain, not library pins.

## Kotlin

Bindings likewise ride the uniffi strategy (S-F1); JNA loads the produced library. JUnit 5 is the current test harness; the canonical pin is recorded in [Clients — Test and Performance Tooling](/design/clients/#test-and-performance-tooling) when the Android harness stabilizes.

## Validation

The pins are enforced structurally, not by convention: `cargo tree -i chrono -e no-dev` must resolve to `capsule-cli/entity` and sea-orm internals only; `rg 'log::'` outside the S-F6 scope, and any `openssl`/`native-tls` edge outside webauthn-rs, are review-blocking. The per-platform `mise run check-*` gates run the pinned toolchains.
