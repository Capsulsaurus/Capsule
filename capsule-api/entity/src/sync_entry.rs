//! The key-free sync feed log (slice `S-C2`). One append-only row per finalized blob,
//! minted inside the upload finalization transaction (slice `S-C1`).
//!
//! Each row carries the signed asset manifest as opaque canonical CBOR (`manifest_cbor`),
//! the small encrypted metadata blob (`metadata_blob`, when this blob *is* the metadata
//! blob), the per-role blob content addresses (`blobs`), and the derived `original_held`
//! completeness fact. Blob **bytes** never live here — only their content addresses.
//!
//! `feed_seq` is the global append order (the opaque cursor's pagination key); `sync_seq`
//! is the per-album strictly-increasing anti-rewind counter the client checks. Both are
//! monotone because minting happens under the per-album counter row lock inside the
//! finalization transaction (SSoT: the download-sync design doc).

use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "sync_entries")]
pub struct Model {
    /// Global append order — the opaque sync cursor's pagination key (bigserial).
    #[sea_orm(primary_key)]
    pub feed_seq: i64,
    /// The album this entry belongs to.
    #[sea_orm(indexed)]
    pub album_id: String,
    /// Per-album strictly-increasing sequence (the client's anti-rewind high-water mark).
    pub sync_seq: i64,
    /// The album pin this entry conforms to (`YYYY-MM-DD`).
    pub protocol_version: String,
    /// Change kind: 1 = created, 2 = metadata-updated, 3 = deleted.
    pub kind: i16,
    /// The asset id this entry refers to.
    pub asset_id: String,
    /// The signed `AssetManifest` as opaque canonical CBOR (never re-modeled).
    #[sea_orm(column_type = "Blob")]
    pub manifest_cbor: Vec<u8>,
    /// The encrypted metadata blob, inlined when this row's blob is the metadata blob.
    #[sea_orm(column_type = "Blob", nullable)]
    pub metadata_blob: Option<Vec<u8>>,
    /// Per-role blob content addresses (`FeedBlobManifest`), never blob bytes.
    #[sea_orm(column_type = "JsonBinary")]
    pub blobs: Json,
    /// Whether the asset's original blob is finalized on the server (derived, S-C1).
    pub original_held: bool,
    /// Row creation instant.
    #[sea_orm(column_type = "TimestampWithTimeZone")]
    pub created_at: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
