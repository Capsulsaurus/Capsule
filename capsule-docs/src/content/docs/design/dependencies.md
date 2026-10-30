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
| LAN service discovery | mocked seam (`capsule-sdk::peering::Discovery`) | Peering's live responder will use the mDNS / DNS-SD row below. |
| mDNS / DNS-SD browsing | `mdns-sd` **0.21** (`default-features = false`) | Pure Rust, no async runtime, builds for every client target; MIT OR Apache-2.0. `capsule-cli` only for now: [camera discovery](/design/import/camera-import/#discovery) browses `_ptp._tcp` behind a seam its tests script. No default features (`logging` pulls the `log` facade). Not `zeroconf` (binds Avahi/Bonjour) or `libmdns` (responder only). |
| Identifiers | `uuid`, **UUIDv7 for every new identifier** | UUIDv4 where creation time must not leak (e.g. `device_id`); capability ids are not UUIDs. |
| Async runtime | `tokio` | — |
| HTTP server | Kynos `0.4.0` from crates.io, feature `openapi32` | The document is pinned to 3.2 with `openapi_as(SpecVersion::V3_2)`; the feature alone does not select it. No secondary public transport. |
| HTTP body | `http-body-util`; `http-body` | `http-body-util`: only the server's coded-problem interceptor. `http-body`: only the SDK's paced upload body, which records send progress for the per-request stall timer. Both already transitive. |
| HTTP types | `http` | Only the SDK's refresh-on-401 layer; already transitive. |
| HTTP client | `reqwest` (`default-features = false`, `rustls-tls`, `json`) | `capsule-sdk`, and the server's OIDC relying party as its only egress. |
| OIDC relying party | `jsonwebtoken` + hand-written discovery, JWKS cache and token exchange | Every claim check is Capsule's ([Authentication](/design/authentication/#signing-in-through-an-identity-provider)). Not `openidconnect` (pulls `chrono`, `log`, vulnerable `rsa`). |
| REST client codegen | `spargen` **0.5.0** | SDK build-dependency only, from the committed `capsule-server/openapi.json`. Everything that parses is generated; only orchestration is hand-written. Bump the `bytes`/`reqwest`/`serde`/`serde_json` floors together with it. Not Progenitor. |
| Second factor | `totp-rs` | SHA-1, six digits, 30 s step, one step of drift; replay refusal is the store's compare-and-set. |
| Constant-time comparison | `subtle` | The TOTP code check; already transitive. |
| Shamir secret sharing | `blahaj` **0.7** | [2-of-3 social recovery](/design/backup-recovery/#opt-in-shamir-secret-sharing); share encoding byte-identical to `sharks`. `sharks` is retired (RUSTSEC-2024-0398). |
| WebAuthn | **none** — passkeys are not in v1 | Reintroducing `webauthn-rs` would reopen the openssl question. |
| ORM | `sea-orm` (`sqlx-postgres` server, `sqlx-sqlite` CLI) | Rebuildable indexes only; sidecars stay canonical. |
| Volatile state (Valkey) | `redis` **1.2.2** (`tokio-comp`, `connection-manager`, `tokio-rustls-comp`, `script`) | One `ConnectionManager`, no pool (no `bb8`); Lua scripts for multi-key writes, so no Redis Cluster. No `object_store` or generic TTL/CAS crate. |
| Embedded SQLite | `rusqlite` (`bundled`) | `library.sqlite`, in `capsule-library` only; never a `capsule-core` dependency. |
| Vector index | `sqlite-vec` (`vec0`) | Client-local, in `capsule-library` only; not on wasm32. |
| Still decode / encode | `rawshift-image` **0.1.1** from crates.io (`default-features = false`; JPEG, PNG, JXL, TIFF, GIF, ARW, DNG decode) | Pure Rust by default. Opt-in `media-linked-codecs` (vendored libjxl, ravif) and `media-avif-decode` (system libdav1d, dynamically linked; not Windows), never in `default`. Refused: **HEIC** (libheif/libde265 are LGPL-3.0), WebP (fails to compile on aarch64), SVG, other RAW families. Absent from wasm32. |
| Media metadata (EXIF, XMP, ICC) | `rawshift-image` | `kamadak-exif` is transitional: removed once Rawshift's extraction covers capture time, GPS and orientation ([Thumbnails — Media Is Rawshift's](/design/thumbnails/#media-is-rawshifts)). |
| LQIP placeholder codec | `chromahash` **0.7.1** | Imported directly, never through Rawshift. **ThumbHash is retired**; see ADR-0003. |
| Free-space probe | `rustix` (Unix) + `windows-sys` (Windows) | `capsule-library` only. |
| Windows TPM (TBS) | `windows-sys` (`Win32_System_TpmBaseServices`) | Not tss-esapi on Windows. |
| MLS group layer | `openmls` (`libcrux-provider`) + `openmls_memory_storage` and companions | X-Wing PQ suite `0x004D`; `mls` feature, not on wasm32. Not `mls-rs` ([MLS](/design/cryptography/mls/)). |
| CBOR | `ciborium` (+ `serde_bytes`, `half`) | Rules: [Metadata — Canonical CBOR](/design/metadata/#canonical-cbor-encoding). |
| Serialization | `serde` + `serde_json` | — |
| CLI | `clap` (derive) | — |
| Terminal UI | `ratatui` **0.30** (`default-features = false`, `crossterm_0_29`, `layout-cache`) + `crossterm` **0.29** (`event-stream`) | `capsule-cli` only. `crossterm` is pinned to the backend version `ratatui` is built against, so the graph holds one copy. No `widget-calendar`: it pulls the `time` crate (Datetime row). |
| Terminal image preview | `ratatui-image` **11.1** (`default-features = false`) | `capsule-cli` only: the TUI library browser's inline preview over Kitty, Sixel or iTerm2 graphics, with a text fallback where the terminal answers with none of them. No `chafa-dyn` (it loads the LGPL libchafa). Pixels come from `capsule-core::media`; the crate only fits them to the pane and encodes them for the terminal. |
| Tooling image processing | `image` (`jpeg`, `png`) | xtask screenshot tooling only; no product crate may depend on it for codecs. The one exception is `capsule-cli` naming `image` with no features for the `DynamicImage` buffer `ratatui-image` takes; decoding stays with Rawshift. |
| Test runner | `cargo-nextest`; `testcontainers` for backing services | Binary smokes use `CARGO_BIN_EXE_<name>`; no `assert_cmd`. A smoke that must answer a terminal-only prompt gets a pseudo-terminal from `libc::openpty` (a unix-only dev-dependency), not an `expect`-style crate. |
| Property tests | `proptest` (dev-dependency, `std` only) | CRDT merge laws, canonical codecs and SDK fault schedules, per the [testing plan](/development/testing/). Seeded and printed; `PROPTEST_RNG_SEED` replays. |
| Snapshot tests | `insta` (dev-dependency, no default features) | The CLI `--json` and exit-code contract; plain-string snapshots under `tests/snapshots/`. TUI frames rendered on ratatui's `TestBackend`, snapshotted as plain text beside their unit tests. |
| Fuzzing and mutation testing | `cargo-fuzz` (`libfuzzer-sys`), `cargo-mutants` | Tools, not dependencies of any shipped crate: the fuzz harness is its own workspace under `capsule-core/fuzz`. |
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

| Domain | Canonical choice | Notes |
| --- | --- | --- |
| In-app language (Android 8 to 12) | `androidx.appcompat` **1.7.0** (`AppCompatDelegate.setApplicationLocales`, `autoStoreLocales`) | Only the per-app language below Android 13, where the system has no per-app setting; Android 13+ uses the system's. The choice stays on the device and is never synced; region stays the system's. |

## Validation

- `mise run architecture-check` runs `cargo tree -p <crate> -i chrono -e no-dev` per crate (must be
  empty outside the exceptions), asserts `capsule-server-migration` still needs its exemption, and
  keeps `thumbhash` on a retired list.
- `rg thumbhash capsule-web` must be empty; new `log::` or `openssl`/`native-tls` edges are
  review-blocking.
