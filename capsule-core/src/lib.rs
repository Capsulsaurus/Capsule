// ── WASM sealing surface (always compiled) ──────────────────────────────────
// `cbor`, `crypto`, and `drop` are the only modules the guest web client (WASM) needs:
// the [`drop::seal_drop`] path plus its cryptographic dependencies. They are free of any
// dependency that cannot target `wasm32-unknown-unknown` (notably the bundled SQLite in
// `db`/`library`), so a `--no-default-features` build compiles exactly this surface. See
// [Web Upload — the web client class](https://docs/design/web-upload/).
pub mod cbor;
pub mod crypto;
pub mod drop;

/// Share-link generation, encapsulation crypto, and the recipient-side [`sharing::open_scope`]
/// path. Always compiled (slice S-E1): it depends only on `cbor` + `crypto`, both of which
/// target `wasm32-unknown-unknown`, so the browser share viewer (via `capsule-wasm`) can unwrap
/// a link client-side. The native issuer (`ShareLinkIssuer` on [`lifecycle::Workspace`]) still
/// rides the `native` feature; only the pure crypto + recipient-open path is needed in the browser.
pub mod sharing;

/// Client build identification — the `client_version` / `generated_by_client` grammar and the
/// build-embedded git commit (S-D15). Always compiled: pure string formatting, no native deps.
pub mod client_build;

/// LQIP — the chromahash placeholder carried in the signed sidecar's `lqip` field (S-B14).
/// Always compiled, and deliberately so: the placeholder is produced by the import pipeline,
/// read by the apps through the uniffi FFI, and read by the browser through `capsule-wasm`, so
/// there is exactly one implementation and every surface links it. `chromahash` has zero
/// runtime dependencies and targets `wasm32-unknown-unknown`. The one `native`-gated part is
/// [`lqip::sidecar`], because [`sidecar`] itself is.
pub mod lqip;

// ── Native surface (`native`, default) ──────────────────────────────────────
// Everything below drives the on-device library, import, and lifecycle — it links SQLite,
// the filesystem, and the media stack, none of which the browser sealing path needs. Gated
// behind the default `native` feature so a `--no-default-features` build drops it (and the
// bundled-SQLite dependency) for the WASM sealing build.
#[cfg(feature = "native")]
pub mod backup;
#[cfg(feature = "native")]
pub mod cohort;
#[cfg(feature = "native")]
pub mod constants;
#[cfg(feature = "native")]
pub mod culling;
#[cfg(feature = "native")]
pub mod db;
#[cfg(feature = "native")]
pub mod domain;
#[cfg(feature = "native")]
pub mod exif;
#[cfg(feature = "native")]
pub mod federation;
#[cfg(feature = "native")]
pub mod import;
#[cfg(feature = "native")]
pub mod library;
#[cfg(feature = "native")]
pub mod lifecycle;
#[cfg(feature = "native")]
pub mod metadata;
#[cfg(feature = "native")]
pub mod ml;
// `notify` — alert classes and their trigger predicates (`S-D29`): one shared decision
// function every platform evaluates, so the taxonomy is not reimplemented per client.
// `native`-gated because an alert is composed from *decrypted device state*, and the
// un-gated surface here is the key-free guest sealing path, which holds none of it. The
// rationale lives as a plain comment rather than a doc comment because rustdoc merges an
// outer `mod` doc with the module's own `//!` block and then resolves the whole thing at
// the declaration site — which would break every short intra-doc link in `notify/mod.rs`.
#[cfg(feature = "native")]
pub mod notify;
#[cfg(feature = "native")]
pub mod sidecar;
#[cfg(feature = "native")]
pub mod utils;
// Deliberately **not** `native`-gated: these are pure, key-less structural checks over
// `crypto::{encryption, hash, primitives, provenance}` and nothing else. Gating them behind
// `native` forced `capsule-server` — a key-free server that touches no SQLite and no MLS — to link
// `rusqlite`, `sqlite-vec`, OpenMLS and libcrux in order to call the refuse-by-default invariants
// it exists to enforce.
pub mod validation;

/// uniffi-generated bindings surface for Kotlin/Swift (`ffi` feature). The exported API is a
/// thin wrapper over [`lifecycle::Workspace`]; see [`ffi`].
#[cfg(feature = "ffi")]
pub mod ffi;

#[cfg(feature = "ffi")]
uniffi::setup_scaffolding!();
