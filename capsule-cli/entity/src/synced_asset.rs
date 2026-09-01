//! An asset landed by the sync feed into the CLI's local store (slice `S-D5`).
//!
//! This is the client-side, sync-fed library `capsule list` queries — the CLI's
//! analogue of a `library.sqlite`. Ids arrive from the feed as opaque bytes
//! (UTF-8 nanoids or raw UUIDs), so `asset_id`/`album_id` are stored base64-encoded
//! (encoding owned by the `syncstore` module) to survive non-UTF-8 ids losslessly.
//! The per-album high-water mark is *derived* from `MAX(sync_seq)` here — every
//! applied entry is recorded, so no separate high-water table is needed.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "synced_assets")]
pub struct Model {
    /// The asset id (base64 of the feed's opaque id bytes).
    #[sea_orm(primary_key, auto_increment = false)]
    pub asset_id: String,
    /// The owning album id (base64 of the feed's opaque id bytes).
    #[sea_orm(indexed)]
    pub album_id: String,
    /// The per-album, strictly-increasing feed sequence — the anti-rewind mark.
    pub sync_seq: i64,
    /// The change kind that last touched this asset (`created` / `metadata_updated`
    /// / `deleted`).
    pub kind: String,
    /// Whether the original blob is finalized on the server (`false` ⇒
    /// awaiting-original; staged uploads).
    pub original_held: bool,
    /// Whether the asset is tombstoned (the last applied change was a delete).
    #[sea_orm(indexed)]
    pub tombstoned: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
