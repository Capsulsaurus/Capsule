//! The per-blob garbage-collection state table (owned by the GC worker, slice `S-C11`;
//! read by storage verification, slice `S-C3`).
//!
//! One `blob_gc` row per content-addressed blob that is **not** in the ordinary live
//! state. A referenced, un-quarantined blob carries no row — absence encodes "live", so the
//! table stays small. `collectable_since` starts the grace clock when refcount reaches 0;
//! `quarantined` flags an integrity fault. Both drive the key-free `retrievable` verdict.

use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(BlobGc::Table)
                    .if_not_exists()
                    // The global content address is the row key.
                    .col(string_len(BlobGc::ContentHash, 64).primary_key())
                    // NULL = live; set = mid-collection since this instant (grace clock).
                    .col(timestamp_with_time_zone_null(BlobGc::CollectableSince))
                    .col(boolean(BlobGc::Quarantined).default(false))
                    .to_owned(),
            )
            .await?;

        // The GC sweep scans for blobs already past their grace window.
        manager
            .create_index(
                Index::create()
                    .name("idx_blob_gc_collectable")
                    .table(BlobGc::Table)
                    .col(BlobGc::CollectableSince)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(BlobGc::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum BlobGc {
    Table,
    ContentHash,
    CollectableSince,
    Quarantined,
}
