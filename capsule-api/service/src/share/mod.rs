//! The public share-link store: the issuer publish surface and the authoritative serve-path
//! resolution (slice `S-C4`).
//!
//! This is the DB layer the media share routes (`capsule-api-media::routes::share`) orchestrate
//! through the media-crate serve engine (`capsule-api-media::service::share`). It owns two
//! server responsibilities the [Share Links design doc] fixes:
//!
//! 1. **Publish** ([`Mutation::publish_share`], [`Mutation::revoke_share`]) — the server half of
//!    the issuer's Provision step (mirrors [`crate::drop::Mutation::create_link`]). The issuing
//!    client mints the random 128-bit `opaque_id` and the encapsulated [`WrappedScope`]; the
//!    server stores them **opaquely** (it can neither open the material nor observe the
//!    passphrase) plus the per-asset served metadata.
//! 2. **Authoritative resolution** ([`Query::resolve_by_opaque`]) — the single, cache-backing
//!    read the serve path consults: it returns [`ShareResolution::Serve`] for a live,
//!    home-server-owned link, [`ShareResolution::Foreign`] for a link this server does not host
//!    (the client resolves the `{ home_server }` pointer), or [`ShareResolution::Gone`] for a
//!    not-found / revoked / expired link — the three of which the serve path renders as one
//!    **indistinguishable `404`**.
//!
//! [Share Links design doc]: ../../../../capsule-docs/src/content/docs/design/share-links.md
//! [`WrappedScope`]: capsule_core::sharing::WrappedScope

mod mutation;
mod query;

use capsule_core::sharing::WrappedScope;
use capsule_core::sidecar::sidecar_v1::SidecarV1;
use data_encoding::BASE64;
pub use entity::public_share::ShareScopeKind;
use jiff::Timestamp;
pub use mutation::Mutation;
pub use query::Query;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The `served_metadata` JSON document shape (one row's per-asset served metadata). Sidecars
/// are carried as base64 canonical CBOR so the strip-on-serve path decodes → strips → re-encodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct StoredMetadata {
    pub(super) assets: Vec<StoredAsset>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct StoredAsset {
    pub(super) asset_id: String,
    pub(super) content_hash: String,
    pub(super) content_type: String,
    pub(super) size: u64,
    /// Base64 of the sidecar's canonical CBOR (un-stripped; stripped on serve).
    pub(super) sidecar_cbor_b64: String,
}

/// Encode a [`WrappedScope`] to its opaque stored form: base64 of its canonical CBOR.
pub(super) fn encode_wrapped(wrapped: &WrappedScope) -> Result<String, ShareError> {
    let cbor = capsule_core::cbor::to_canonical_vec(wrapped)
        .map_err(|_| ShareError::Encoding("wrapped_scope serialize"))?;
    Ok(BASE64.encode(&cbor))
}

/// A failure surfaced by the share store. The media routes map each variant to its transport
/// status + stable `error.*` code; the serve-path `Gone` never reaches here (it is an
/// indistinguishable `404`, not an error).
#[derive(Debug, Error)]
pub enum ShareError {
    /// The published material could not be encoded/decoded to its opaque stored form.
    #[error("share material encoding failed: {0}")]
    Encoding(&'static str),
    /// A database failure.
    #[error(transparent)]
    Db(#[from] sea_orm::DbErr),
}

/// A request to publish (register) a share link — the server half of the Provision step. The
/// `opaque_id` is minted by the caller from `capsule_core::sharing::generate_opaque_id` (a
/// random 128-bit token, never a structured id).
#[derive(Debug, Clone)]
pub struct PublishShare {
    /// The issuing user (revocation / listing authorization).
    pub owner_id: String,
    /// The random 128-bit opaque URL-path token (lowercase hex of 16 bytes).
    pub opaque_id: String,
    /// The album's single home server (only it serves the share).
    pub home_server: String,
    /// Whether the link points at a single asset or a whole album.
    pub scope_kind: ShareScopeKind,
    /// The scoped album/asset id.
    pub scope_id: String,
    /// The issuer-published encapsulated scope material, stored + served opaquely.
    pub wrapped_scope: WrappedScope,
    /// The per-asset served metadata (the sidecar is stripped on every serve).
    pub assets: Vec<ShareAssetInput>,
    /// Optional expiry.
    pub expires_at: Option<Timestamp>,
}

/// One asset's served metadata as published. The `sidecar` is stored verbatim and **stripped on
/// every serve** — the server never serves it un-stripped and offers no opt-out (Security
/// Contract — Privacy strip on serve).
#[derive(Debug, Clone)]
pub struct ShareAssetInput {
    /// The asset id.
    pub asset_id: String,
    /// The ciphertext blob's content address (served from `/s/{opaque-id}/blob/{hash}`).
    pub content_hash: String,
    /// The asset's content type.
    pub content_type: String,
    /// The ciphertext blob size in bytes.
    pub size: u64,
    /// The asset's sidecar (stripped for export on serve; the local copy is never modified).
    pub sidecar: SidecarV1,
}

/// The outcome of an authoritative serve-path resolution.
#[derive(Debug, Clone)]
pub enum ShareResolution {
    /// A live link this server hosts — serve it.
    Serve(ServeRecord),
    /// A link this server does not host: return the `{ home_server }` pointer, never content
    /// (Security Contract — Home-server-only serving).
    Foreign {
        /// The authoritative home server the client resolves.
        home_server: String,
    },
    /// Not found, revoked, or expired — the serve path renders one indistinguishable `404`.
    Gone,
}

/// The servable facts of a live share link (the cache-backing record the serve path holds).
#[derive(Debug, Clone)]
pub struct ServeRecord {
    /// The opaque URL-path token.
    pub opaque_id: String,
    /// Whether the link points at a single asset or a whole album.
    pub scope_kind: ShareScopeKind,
    /// The scoped album/asset id.
    pub scope_id: String,
    /// The home server (always this server for a [`ShareResolution::Serve`]).
    pub home_server: String,
    /// The issuer-published encapsulated material (canonical CBOR, base64); served opaquely.
    pub wrapped_scope_b64: String,
    /// Whether an Argon2id passphrase layer wraps the material.
    pub passphrase_protected: bool,
    /// RFC 3339 expiry, if any.
    pub expires_at: Option<String>,
    /// The per-asset served metadata (sidecar CBOR **un-stripped**; the serve layer strips it).
    pub assets: Vec<ServeAsset>,
}

/// One asset's servable metadata (the sidecar CBOR is stripped by the serve layer, never here).
#[derive(Debug, Clone)]
pub struct ServeAsset {
    /// The asset id.
    pub asset_id: String,
    /// The ciphertext blob's content address (the only hashes `/blob/{hash}` will serve).
    pub content_hash: String,
    /// The asset's content type.
    pub content_type: String,
    /// The ciphertext blob size in bytes.
    pub size: u64,
    /// The un-stripped sidecar as canonical CBOR (the serve layer applies the export strip).
    pub sidecar_cbor: Vec<u8>,
}
