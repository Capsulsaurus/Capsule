//! Signed manifests and append-only provenance chains (SSoT: [Cryptography — Provenance]).
//!
//! - [`action`] — the closed lifecycle-action and derivative-role enums.
//! - [`manifest`] — [`AssetManifest`] / [`DerivativeManifest`] with their canonical signing
//!   bytes and two hybrid signatures.
//! - [`record`] — [`ProvenanceRecord`] and the hash-chained [`ProvenanceChain`].
//!
//! Verification of all of this flows through the single [`verify_asset`] chokepoint.
//!
//! [`verify_asset`]: crate::crypto::verify_asset
//! [Cryptography — Provenance]: https://docs/design/cryptography/provenance/

pub mod action;
pub mod manifest;
pub mod record;

pub use action::{Action, DerivativeRole};
pub use manifest::{
    ASSET_MANIFEST_VERSION, AssetManifest, DERIVATIVE_MANIFEST_VERSION, DerivativeCore,
    DerivativeManifest, ManifestCore,
};
pub use record::{ChainError, ProvenanceChain, ProvenanceRecord};
