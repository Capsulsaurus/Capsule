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
| Datetime | `jiff` | All domain logic — parsing, formatting, arithmetic. Signed and wire formats carry RFC 3339 strings or integer epochs, never a datetime library type, so the pin never touches serialized bytes. | `chrono` remains **only** as the sea-orm column type in `capsule-cli/entity`, converted to jiff at the entity boundary, and in non-buildable review-only server code. |
| Error handling | `thiserror` in libraries; `eyre` + `color-eyre` in binaries | Libraries define typed error enums; binaries (CLI, server `main`, xtask) wrap them in reports. | `anyhow` is not used. |
| Logging | `tracing` (facade) + `tracing-subscriber` (binaries) | All crates; structured fields and hot-path spans per the traceability rule in `AGENTS.md`. The `log` facade is forbidden in new code. | Remaining `log::` call sites in `capsule-core` / `capsule-core-ffi` migrate in slice S-F6. |
| TLS implementation | `rustls` | Wherever Capsule code holds a TLS stack: the SDK's HTTP client, [LAN-peering](/design/peering/) mutual TLS, server egress, sea-orm's `runtime-tokio-rustls`. Never native-tls/openssl. | `openssl` appears only as a transitive dependency of `webauthn-rs` attestation-certificate verification — it is never a TLS stack. |
| Identifiers | `uuid` — **UUIDv7 for every newly introduced identifier** | Time-ordered v7 is the default (index locality); the assignment of existing ids is owned by [Metadata — Identifiers](/design/metadata/#identifiers). | UUIDv4 where an id must not leak creation time (e.g. `device_id`). Capability-bearing opaque ids (share links, drops) are not UUIDs at all — they carry their own ≥128-bit entropy per their owner docs. |
| Async runtime | `tokio` | All async code. | — |
| HTTP server | Kynos | All `capsule-api` REST/OpenAPI surfaces, including sync and federation. | No secondary public transport. |
| HTTP client | `reqwest` (`default-features = false`, `rustls-tls`) | `capsule-sdk` — the sanctioned network path. | — |
| ORM | `sea-orm` (`sqlx-postgres` on the server, `sqlx-sqlite` in the CLI) | The rebuildable index databases only — sidecars stay canonical per [Principles](/design/principles/). | — |
| Embedded SQLite | `rusqlite` (`bundled`) | `capsule-core`'s `library.sqlite`. | — |
| CBOR | `ciborium` (+ `serde_bytes`, `half`) | All CBOR; the canonical-encoding rules are owned by [Metadata — Canonical CBOR Encoding](/design/metadata/#canonical-cbor-encoding). | — |
| Serialization | `serde` + `serde_json` | Derives and JSON surfaces. | — |
| CLI | `clap` (derive) | `capsule-cli`, xtask argument parsing where non-trivial. | — |
| Test runner | `cargo-nextest`; `testcontainers` (+ podman) for real backing services | The Unit/Smoke tiers per [Principles — Validation Tiers](/design/principles/#validation-tiers). | — |
| FFI bindings | `uniffi` | One workspace version — consolidation is slice S-F1. | — |

## Web

| Domain | Canonical choice | Notes |
| --- | --- | --- |
| Framework | React 19 + rsbuild | — |
| Routing / data | TanStack Router / TanStack Query | — |
| Styling | Tailwind v4, class-based dark mode (`@custom-variant dark` + `ThemeProvider`) | — |
| Validation | zod | — |
| Datetime | **none** — native `Intl` / `Date` | A date library is added only by adding a row here. |
| i18n runtime | FormatJS over the generated catalogs | Contract owned by [Internationalization](/design/i18n/). |
| Lint/format | Biome | — |
| Runtime / package manager | bun | — |

## Swift

Test frameworks and performance tooling are owned by [Clients — Test and Performance Tooling](/design/clients/#test-and-performance-tooling). Bindings come from the single-version uniffi strategy (slice S-F1). Project generation and formatting (Tuist, SwiftLint, SwiftFormat) are mise-pinned toolchain, not library pins.

## Kotlin

Bindings likewise ride the uniffi strategy (S-F1); JNA loads the produced library. JUnit 5 is the current test harness; the canonical pin is recorded in [Clients — Test and Performance Tooling](/design/clients/#test-and-performance-tooling) when the Android harness stabilizes.

## Validation

The pins are enforced structurally, not by convention: `cargo tree -i chrono -e no-dev` must resolve to the entity crates, the frozen library crate, and sea-orm internals only; `rg 'log::'` outside the S-F6 scope, and any `openssl`/`native-tls` edge outside webauthn-rs, are review-blocking. The per-platform `mise run check-*` gates run the pinned toolchains.
