// ── WASM sealing surface (always compiled) ──────────────────────────────────
// `cbor`, `crypto`, and `drop` are the only modules the guest web client (WASM) needs:
// the [`drop::seal_drop`] path plus its cryptographic dependencies. They are free of any
// dependency that cannot target `wasm32-unknown-unknown` (notably the bundled SQLite in
// `db`/`library`), so a `--no-default-features` build compiles exactly this surface. See
// [Web Upload — the web client class](https://docs/design/web-upload/).
pub mod cbor;
pub mod crypto;
pub mod drop;

/// Client build identification — the `client_version` / `generated_by_client` grammar and the
/// build-embedded git commit (S-D15). Always compiled: pure string formatting, no native deps.
pub mod client_build;

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
#[cfg(feature = "media")]
pub mod media;
#[cfg(feature = "native")]
pub mod metadata;
#[cfg(feature = "native")]
pub mod ml;
#[cfg(feature = "native")]
pub mod models;
#[cfg(feature = "native")]
pub mod sharing;
#[cfg(feature = "native")]
pub mod sidecar;
#[cfg(feature = "native")]
pub mod utils;
#[cfg(feature = "native")]
pub mod validation;

/// uniffi-generated bindings surface for Kotlin/Swift (`ffi` feature). The exported API is a
/// thin wrapper over [`lifecycle::Workspace`]; see [`ffi`].
#[cfg(feature = "ffi")]
pub mod ffi;

#[cfg(feature = "ffi")]
uniffi::setup_scaffolding!();
