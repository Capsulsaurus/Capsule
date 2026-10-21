---
title: Dependencies
description: Canonical library and tooling pins for every Capsule platform
status: draft
---

This doc owns Capsule's **canonical dependency pins**: for each domain concern, the one library that
implements it and its bounded exceptions. **Introducing a dependency for a domain not listed here
requires adding a row here first**; changing a pin is an edit here, never in-place drift.

Owned elsewhere: crypto primitives ([Cryptography — Primitives](/design/cryptography/primitives/)),
TLS version policy ([Failure Modes](/design/cryptography/failure-modes/#transport-security)), client
test tooling ([Clients](/design/clients/#test-and-performance-tooling)), which identifier uses which
UUID version ([Metadata — Identifiers](/design/metadata/#identifiers)), media formats and the
[LQIP contract](/design/thumbnails/#lqip), and which licences and linkage are admissible
([Licensing](/design/licensing/), enforced by `deny.toml`).

Every Rust version is pinned once in the root `[workspace.dependencies]`; members use
`workspace = true`. Toolchains (Rust, bun, tuist, …) are pinned by mise.

## Rust

| Domain | Canonical choice | Notes and exceptions |
| --- | --- | --- |
| Datetime | `jiff` | Wire formats carry RFC 3339 or epochs, never a library type. `chrono` only as the sea-orm column type in `capsule-cli/entity` and in `capsule-server-migration` (a server dev-dependency). |
| Error handling | `thiserror` (libraries); `eyre` + `color-eyre` (binaries) | No `anyhow`. |
| Logging | `tracing` + `tracing-subscriber` | The `log` facade is forbidden in new code; remaining call sites in `capsule-core-ffi` are to migrate. |
| TLS | `rustls` (`tokio-rustls` async), `ring` provider | Never native-tls/openssl; no exceptions. |
| X.509 leaf generation | `rcgen` | Ephemeral self-signed leaf for [LAN-peering](/design/peering/) mTLS; trust is decided at the application layer. |
| LAN service discovery | mocked seam (`capsule-sdk::peering::Discovery`) | A live responder (`mdns-sd`) gets its own row when added. |
| Identifiers | `uuid`, **UUIDv7 for every new identifier** | UUIDv4 where creation time must not leak (e.g. `device_id`); capability ids are not UUIDs. |
| Async runtime | `tokio` | — |
| HTTP server | Kynos `0.1.0` from crates.io, feature `openapi32` | The document is pinned to 3.2 with `openapi_as(SpecVersion::V3_2)`; the feature alone does not select it. No secondary public transport. |
| HTTP body | `http-body-util` | Only the server's coded-problem interceptor; already transitive. |
| HTTP types | `http` | Only the SDK's refresh-on-401 layer; already transitive. |
| HTTP client | `reqwest` (`default-features = false`, `rustls-tls`, `json`) | `capsule-sdk`, and the server's OIDC relying party as its only egress. |
| OIDC relying party | `jsonwebtoken` + hand-written discovery, JWKS cache and token exchange | Every claim check is Capsule's ([Authentication](/design/authentication/#signing-in-through-an-identity-provider)). Not `openidconnect` (pulls `chrono`, `log`, vulnerable `rsa`). |
| REST client codegen | `spargen` **0.4.0** | SDK build-dependency only, from the committed `capsule-server/openapi.json`. Everything that parses is generated; only orchestration is hand-written. Bump the `bytes`/`reqwest`/`serde`/`serde_json` floors together with it. Not Progenitor. |
| Second factor | `totp-rs` | SHA-1, six digits, 30 s step, one step of drift; replay refusal is the store's compare-and-set. |
| Constant-time comparison | `subtle` | The TOTP code check; already transitive. |
| Shamir secret sharing | `blahaj` **0.7** | [2-of-3 social recovery](/design/backup-recovery/#opt-in-shamir-secret-sharing); share encoding byte-identical to `sharks`. `sharks` is retired (RUSTSEC-2024-0398). |
| WebAuthn | **none** — passkeys are not in v1 | Reintroducing `webauthn-rs` would reopen the openssl question. |
| ORM | `sea-orm` (`sqlx-postgres` server, `sqlx-sqlite` CLI) | Rebuildable indexes only; sidecars stay canonical. |
| Volatile state (Valkey) | `redis` **1.2.2** (`tokio-comp`, `connection-manager`, `tokio-rustls-comp`, `script`) | One `ConnectionManager`, no pool (no `bb8`); Lua scripts for multi-key writes, so no Redis Cluster. No `object_store` or generic TTL/CAS crate. |
| Embedded SQLite | `rusqlite` (`bundled`) | `library.sqlite`. |
| Vector index | `sqlite-vec` (`vec0`) | Client-local, `native`-only; not on wasm32. |
| Still decode / encode | `rawshift-image` **0.1.1** from crates.io (`default-features = false`; JPEG, PNG, JXL, TIFF, GIF, ARW, DNG decode) | Pure Rust by default. Opt-in `media-linked-codecs` (vendored libjxl, ravif) and `media-avif-decode` (system libdav1d, dynamically linked; not Windows), never in `default`. Refused: **HEIC** (libheif/libde265 are LGPL-3.0), WebP (fails to compile on aarch64), SVG, other RAW families. Absent from wasm32. |
| Media metadata (EXIF, XMP, ICC) | `rawshift-image` | `kamadak-exif` is transitional: removed once Rawshift's extraction covers capture time, GPS and orientation ([Thumbnails — Media Is Rawshift's](/design/thumbnails/#media-is-rawshifts)). |
| LQIP placeholder codec | `chromahash` **0.7.1** | Imported directly, never through Rawshift. **ThumbHash is retired**; see ADR-0003. |
| Free-space probe | `rustix` (Unix) + `windows-sys` (Windows) | `native` only. |
| Windows TPM (TBS) | `windows-sys` (`Win32_System_TpmBaseServices`) | Not tss-esapi on Windows. |
| MLS group layer | `openmls` (`libcrux-provider`) + `openmls_memory_storage` and companions | X-Wing PQ suite `0x004D`; `mls` feature, not on wasm32. Not `mls-rs` ([MLS](/design/cryptography/mls/)). |
| CBOR | `ciborium` (+ `serde_bytes`, `half`) | Rules: [Metadata — Canonical CBOR](/design/metadata/#canonical-cbor-encoding). |
| Serialization | `serde` + `serde_json` | — |
| CLI | `clap` (derive) | — |
| Tooling image processing | `image` (`jpeg`, `png`) | xtask screenshot tooling only; no product crate may depend on it. |
| Test runner | `cargo-nextest`; `testcontainers` for backing services | Binary smokes use `CARGO_BIN_EXE_<name>`; no `assert_cmd`. |
| Property tests | `proptest` (dev-dependency, `std` only) | CRDT merge laws and generated inputs, per the [testing plan](/development/testing/). |
| Unicode normalization | `unicode-normalization` | NFC for user labels ([Organization — Labels](/design/organization/#labels)); already transitive. |
| FFI bindings | `uniffi` | One workspace version. |
| Browser FFI (WASM) | `wasm-bindgen` + `wasm-bindgen-cli` at the same version | `capsule-wasm`; not wasm-pack (breaks the offline gate). |

## Web

| Domain | Canonical choice | Notes |
| --- | --- | --- |
| Framework | React 19 + rsbuild | — |
| Routing / data | TanStack Router / TanStack Query | — |
| Styling | Tailwind v4, class-based dark mode | — |
| Validation | zod | — |
| Datetime | **none** — native `Intl` / `Date` | Add a row before adding one. |
| LQIP decode | **none** in JavaScript | Decoding goes through `capsule-wasm`; no `thumbhash` package. |
| i18n runtime | FormatJS over the generated catalogs | [Internationalization](/design/i18n/). |
| Guest share crypto | `capsule-wasm` (`mise run build-wasm`) | Generated, never committed. |
| Lint/format | Biome | — |
| Runtime / package manager | bun | — |

## Swift

Bindings come from the single uniffi version; Tuist, SwiftLint and SwiftFormat are mise-pinned.

| Domain | Canonical choice | Notes |
| --- | --- | --- |
| Reverse geocoding | `CLGeocoder`, **opt-in per device, off by default** | It sends coordinates to Apple. The preference is a device value, never synced; no third-party geocoder. |

## Kotlin

Bindings ride the same uniffi version, loaded through JNA. JUnit 5 is the test harness.

## Validation

- `mise run architecture-check` runs `cargo tree -p <crate> -i chrono -e no-dev` per crate (must be
  empty outside the exceptions), asserts `capsule-server-migration` still needs its exemption, and
  keeps `thumbhash` on a retired list.
- `rg thumbhash capsule-web` must be empty; new `log::` or `openssl`/`native-tls` edges are
  review-blocking.
