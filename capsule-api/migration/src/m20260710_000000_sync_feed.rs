//! The key-free sync feed schema (slice `S-C2`).
//!
//! Two tables:
//! - `sync_album_seq` — the per-album monotonic counter. Minting the next `sync_seq`
//!   is an atomic `INSERT … ON CONFLICT DO UPDATE … RETURNING` that takes the album's
//!   row lock, so concurrent finalizations are linearised (sync-feed monotonicity).
//! - `sync_entries` — the append-only feed log. `feed_seq` (bigserial) is the global
//!   append order behind the opaque cursor; `(album_id, sync_seq)` is unique.

use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Per-album monotonic counter.
        manager
            .create_table(
                Table::create()
                    .table(SyncAlbumSeq::Table)
                    .if_not_exists()
                    .col(char_len(SyncAlbumSeq::AlbumId, 21).primary_key())
                    .col(big_integer(SyncAlbumSeq::LastSeq).default(0))
                    .to_owned(),
            )
            .await?;

        // Append-only feed log.
        manager
            .create_table(
                Table::create()
                    .table(SyncEntries::Table)
                    .if_not_exists()
                    .col(
                        big_integer(SyncEntries::FeedSeq)
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(char_len(SyncEntries::AlbumId, 21))
                    .col(big_integer(SyncEntries::SyncSeq))
                    .col(string_len(SyncEntries::ProtocolVersion, 10))
                    .col(small_integer(SyncEntries::Kind))
                    .col(char_len(SyncEntries::AssetId, 21))
                    .col(
                        ColumnDef::new(SyncEntries::ManifestCbor)
                            .binary()
                            .not_null(),
                    )
                    .col(ColumnDef::new(SyncEntries::MetadataBlob).binary())
                    .col(ColumnDef::new(SyncEntries::Blobs).json_binary().not_null())
                    .col(boolean(SyncEntries::OriginalHeld))
                    .col(
                        timestamp_with_time_zone(SyncEntries::CreatedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // The per-album anti-rewind uniqueness: no two entries share (album, sync_seq).
        manager
            .create_index(
                Index::create()
                    .name("uq_sync_entries_album_seq")
                    .table(SyncEntries::Table)
                    .col(SyncEntries::AlbumId)
                    .col(SyncEntries::SyncSeq)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // Pagination scan: entries for an album's set, ordered by the global cursor key.
        manager
            .create_index(
                Index::create()
                    .name("idx_sync_entries_album_feed")
                    .table(SyncEntries::Table)
                    .col(SyncEntries::AlbumId)
                    .col(SyncEntries::FeedSeq)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(SyncEntries::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(SyncAlbumSeq::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum SyncAlbumSeq {
    Table,
    AlbumId,
    LastSeq,
}

#[derive(DeriveIden)]
enum SyncEntries {
    Table,
    FeedSeq,
    AlbumId,
    SyncSeq,
    ProtocolVersion,
    Kind,
    AssetId,
    ManifestCbor,
    MetadataBlob,
    Blobs,
    OriginalHeld,
    CreatedAt,
}
