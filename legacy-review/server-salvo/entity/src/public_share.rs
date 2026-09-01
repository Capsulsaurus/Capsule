//! A public share link served on the key-free `/s/{opaque-id}` path (slice `S-C4`).
//!
//! The server-held half of a share link (SSoT: the [Share Links design doc]). This is the
//! **new** opaque-serving model — distinct from the frozen plaintext-era [`super::share_link`]
//! table (`token` / `password_hash` / `view_count`), which the design supersedes. Here the
//! server never holds the decryption secret and never sits in the passphrase-trust path:
//!
//! - `opaque_id` is the random **128-bit** URL-path token (hex of 16 bytes = 32 chars; the
//!   `#{fragment}` decryption secret never reaches the server). It is the lookup key.
//! - `wrapped_scope` is the issuer-published [`WrappedScope`] (canonical CBOR, base64),
//!   served **opaquely** from `/s/{opaque-id}/wrapped-secret` — the server can neither open it
//!   nor observe the passphrase.
//! - `served_metadata` carries the per-asset metadata (content address + the sidecar the serve
//!   path **strips** on every serve — no opt-out; see the Security Contract).
//! - `home_server` pins the album's single home server; a peer refuses to serve and returns a
//!   `{ home_server }` pointer instead of content.
//! - `revoked_at` is the fail-closed revocation flag; `expires_at` the optional expiry. A
//!   not-found / revoked / expired link resolves indistinguishably to an opaque `404`.
//!
//! [Share Links design doc]: ../../../../capsule-docs/src/content/docs/design/share-links.md
//! [`WrappedScope`]: capsule_core::sharing::WrappedScope

use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;

/// What a share link points at (the `{opaque-id}` itself carries no scope).
#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::N(15))")]
pub enum ShareScopeKind {
    /// A single asset.
    #[sea_orm(string_value = "asset")]
    Asset,
    /// A whole album.
    #[sea_orm(string_value = "album")]
    Album,
}

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "public_shares")]
pub struct Model {
    /// Owner-held revocation handle (never URL-exposed; a UUIDv7 is fine here).
    #[sea_orm(primary_key, auto_increment = false)]
    pub link_id: String,
    /// The random 128-bit opaque URL-path token (hex of 16 bytes = 32 chars); the lookup key.
    #[sea_orm(unique, column_type = "String(StringLen::N(32))")]
    pub opaque_id: String,
    /// The issuing user (revocation / listing authorization).
    pub owner_id: String,
    /// The album's single home server — only it serves the share; peers return a pointer.
    pub home_server: String,
    /// Whether the link points at a single asset or a whole album.
    pub scope_kind: ShareScopeKind,
    /// The scoped album/asset id.
    #[sea_orm(column_type = "Text")]
    pub scope_id: String,
    /// The issuer-published `WrappedScope` (canonical CBOR, base64); served opaquely.
    #[sea_orm(column_type = "Text")]
    pub wrapped_scope: String,
    /// Whether an Argon2id passphrase layer wraps the material (derived from `wrapped_scope`;
    /// stored so the metadata response can flag it without decoding the opaque material).
    #[sea_orm(default_value = "false")]
    pub passphrase_protected: bool,
    /// Per-asset served metadata (content address + the sidecar stripped on serve), opaque JSON.
    #[sea_orm(column_type = "JsonBinary")]
    pub served_metadata: Json,
    /// Optional RFC 3339 expiry; `NULL` = no expiry (revocation still applies).
    #[sea_orm(column_type = "TimestampWithTimeZone", nullable)]
    pub expires_at: Option<DateTime<Utc>>,
    /// Fail-closed revocation instant; `NULL` = live.
    #[sea_orm(column_type = "TimestampWithTimeZone", nullable)]
    pub revoked_at: Option<DateTime<Utc>>,
    /// Row creation instant.
    #[sea_orm(
        column_type = "TimestampWithTimeZone",
        default_value = "CURRENT_TIMESTAMP"
    )]
    pub created_at: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
