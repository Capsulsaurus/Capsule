//! Capsule's stateless core: pure functions over values — cryptography and the MLS authority,
//! canonical CBOR, sidecar and CRDT logic, validation, media, LQIP, sharing and drop crypto, and
//! ML orchestration.
//!
//! It holds no connection, file handle or database, and reads no environment, clock or
//! filesystem: storage reaches it through ports such as `ml::VectorIndex`, and time and
//! identifier randomness through [`ambient::Clock`] and [`ambient::Rng`], all of which callers
//! implement. Local state — the SQLite index, the on-disk library, the `Workspace`, import
//! execution, backup I/O and the hardware signers — lives in `capsule-library`. The exceptions
//! are the OS CSPRNG behind `crypto::rng` and `catch_unwind` in `media::decode`;
//! `cargo xtask architecture-check` enforces the rest. See
//! [Module Map — Stateless Core](https://docs/design/module-map/#stateless-core).

// ── WASM sealing surface (always compiled) ──────────────────────────────────
// `cbor`, `crypto`, and `drop` are the only modules the guest web client (WASM) needs:
// the [`drop::seal_drop`] path plus its cryptographic dependencies. They are free of any
// dependency that cannot target `wasm32-unknown-unknown`, so a `--no-default-features` build
// compiles exactly this surface. See
// [Web Upload — the web client class](https://docs/design/web-upload/).
pub mod cbor;
pub mod crypto;
pub mod drop;

/// The `Clock` and `Rng` a caller injects, and the `IdSource` that mints UUIDv7s from them.
/// Always compiled: pure over its two traits, no native deps.
pub mod ambient;

/// Share-link generation, encapsulation crypto, and the recipient-side [`sharing::open_scope`]
/// path. Always compiled: it depends only on `cbor` + `crypto`, both of which
/// target `wasm32-unknown-unknown`, so the browser share viewer (via `capsule-wasm`) can unwrap
/// a link client-side. The native issuer (`ShareLinkIssuer` on `capsule-library`'s `Workspace`)
/// is not needed in the browser; only the pure crypto + recipient-open path is.
pub mod sharing;

/// Client build identification — the `client_version` / `generated_by_client` grammar and the
/// build-embedded git commit. Always compiled: pure string formatting, no native deps.
pub mod client_build;

/// The closed set of still-derivative formats and the structural check over it — the tier
/// table's format column as a type. Always compiled, and for the same reason [`lqip`] is: the
/// crates that *receive* a `DerivativeManifest` (`capsule-server`, `capsule-wasm`) build with
/// `default-features = false`, so a check they cannot link is a check that never runs. Depends
/// only on [`crypto::provenance`]. The one public path to its items: `media` does not
/// re-export them.
pub mod derivative_format;

/// LQIP — the chromahash placeholder carried in the signed sidecar's `lqip` field.
/// Always compiled, and deliberately so: the placeholder is produced by the import pipeline,
/// read by the apps through the FFI crates, and read by the browser through `capsule-wasm`, so
/// there is exactly one implementation and every surface links it. `chromahash` has zero
/// runtime dependencies and targets `wasm32-unknown-unknown`. The one `native`-gated part is
/// [`lqip::sidecar`], because [`sidecar`] itself is.
pub mod lqip;

// ── Native surface (`native`, default) ──────────────────────────────────────
// Everything below serves the on-device library `capsule-library` builds — it links OpenMLS and
// the media stack, neither of which the browser sealing path needs. Gated behind the default
// `native` feature so a `--no-default-features` build drops it for the WASM sealing build.
#[cfg(feature = "native")]
pub mod backup;
#[cfg(feature = "native")]
pub mod cohort;
#[cfg(feature = "native")]
pub mod constants;
#[cfg(feature = "native")]
pub mod culling;
#[cfg(feature = "native")]
pub mod domain;
// The cross-device add ceremony's wire form: the advertised key bundles, the relayed payloads
// and the safety code. The Workspace verbs that draw and cross-sign them are `capsule-library`'s.
// A plain comment for the reason `notify`'s is: an outer doc comment would resolve the module's
// short intra-doc links at this declaration site.
#[cfg(feature = "native")]
pub mod enrollment;
#[cfg(feature = "native")]
pub mod exif;
#[cfg(feature = "native")]
pub mod federation;
#[cfg(feature = "native")]
pub mod import;
/// Still decode, orientation, metadata normalisation and derivative generation over
/// `rawshift-image` (`media` feature, implied by `native`). Feature-gated
/// rather than `native`-gated so the codec stack is one manifest edit away from being dropped
/// from a size-constrained build, and so the `wasm32-unknown-unknown` sealing surface provably
/// does not link it. See [`media`].
#[cfg(feature = "media")]
pub mod media;
#[cfg(feature = "native")]
pub mod metadata;
#[cfg(feature = "native")]
pub mod ml;
// `notify` — alert classes and their trigger predicates: one shared decision
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
// Deliberately **not** `native`-gated: these are pure, key-less structural checks over
// `crypto::{encryption, hash, primitives, provenance}` and nothing else. Gating them behind
// `native` forced `capsule-server` — a key-free server that touches no SQLite and no MLS — to link
// `rusqlite`, `sqlite-vec`, OpenMLS and libcrux in order to call the refuse-by-default invariants
// it exists to enforce.
pub mod validation;
