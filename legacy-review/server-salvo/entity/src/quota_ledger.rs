//! The quota ledger for auxiliary + federated blobs (slice `S-C6`).
//!
//! Original ciphertext blobs are accounted directly from the `assets` index (one row per
//! finalized/pending original, whose `file_size` is charged to its first uploader — the
//! content-addressed dedup attribution). Everything else the quota model counts —
//! per-asset **metadata** blobs, **derivatives** (thumbnails/previews), per-asset
//! **provenance** blobs, and blobs a home server **caches from a federated peer on a
//! user's behalf** — lives here, one row per distinct content address.
//!
//! - `content_hash` is the global content address and the row key: charging a hash that is
//!   already present is a merge (`refcount += 1`), never a second debit — this is the
//!   storage-side dedup that stops a blob shared between two uploaders from being counted
//!   twice (it counts against the first only).
//! - `attributed_user_id` is the user the bytes are charged to (the generating device's
//!   user for derivatives/metadata; the receiver for a federated cache).
//! - `source_peer` is `NULL` for locally produced blobs and set to the origin peer for a
//!   federated cache, so a per-`(attributed_user, source_peer)` caching budget can be summed.
//! - `refcount` is the number of live asset references; when it reaches zero the row is
//!   garbage-collected and the bytes are credited back to `attributed_user_id`.
//!
//! The SSoT for the accounting model is the [Quota design doc](../../../../capsule-docs/src/content/docs/design/quota.md).

use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "quota_ledger")]
pub struct Model {
    /// The global ciphertext content address (lowercase hex) — the dedup key.
    #[sea_orm(
        primary_key,
        auto_increment = false,
        column_type = "String(StringLen::N(64))"
    )]
    pub content_hash: String,
    /// The user the bytes are charged to (first writer / receiver).
    #[sea_orm(indexed)]
    pub attributed_user_id: String,
    /// Ciphertext size in bytes.
    pub byte_size: i64,
    /// `metadata | derivative | provenance | original` (closed enum; see [`super`]).
    #[sea_orm(column_type = "String(StringLen::N(16))")]
    pub blob_kind: String,
    /// `NULL` for a locally produced blob; the origin peer for a federated cache.
    #[sea_orm(nullable, indexed)]
    pub source_peer: Option<String>,
    /// Number of live asset references to this content address.
    pub refcount: i32,
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
