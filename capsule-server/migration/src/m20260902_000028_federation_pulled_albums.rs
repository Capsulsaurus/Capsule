//! What the federation pull worker has accepted from each peer's album (design/federation.md,
//! Egress Pull Worker): the position each job's next pull resumes from, every asset's chain head
//! with the heads it superseded, the entries that passed re-validation, and the metadata blobs
//! a job holds. The blobs' bytes live in the blob store at their content addresses; these rows
//! are what says a job holds them.
//!
//! Every table is keyed by the job, `(peer_id, album_id)`, and every child row cascades from
//! the job's `federation_pulled_albums` row, so forgetting a job is one delete.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // One row per job. `high_water` is what a commit is conditional on; `entries_written`
        // numbers the job's accepted entries in the order they were written, because the feed
        // re-mints `sync_seq` after a restart and cannot order them itself.
        manager
            .create_table(
                Table::create()
                    .table(FederationPulledAlbums::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(FederationPulledAlbums::PeerId).text().not_null())
                    .col(ColumnDef::new(FederationPulledAlbums::AlbumId).text().not_null())
                    .col(ColumnDef::new(FederationPulledAlbums::Cursor).text().null())
                    .col(ColumnDef::new(FederationPulledAlbums::HighWater).big_integer().null())
                    .col(ColumnDef::new(FederationPulledAlbums::AmkVersion).big_integer().null())
                    .col(
                        ColumnDef::new(FederationPulledAlbums::EntriesWritten)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .primary_key(
                        Index::create()
                            .col(FederationPulledAlbums::PeerId)
                            .col(FederationPulledAlbums::AlbumId),
                    )
                    .to_owned(),
            )
            .await?;
        // Each asset's chain position. `superseded` is the bounded list of heads the asset moved
        // past, oldest first, as concatenated 32-byte hashes.
        manager
            .create_table(
                Table::create()
                    .table(FederationPulledAssets::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(FederationPulledAssets::PeerId).text().not_null())
                    .col(ColumnDef::new(FederationPulledAssets::AlbumId).text().not_null())
                    .col(ColumnDef::new(FederationPulledAssets::AssetId).text().not_null())
                    .col(ColumnDef::new(FederationPulledAssets::Head).binary().not_null())
                    .col(ColumnDef::new(FederationPulledAssets::Superseded).binary().not_null())
                    .col(ColumnDef::new(FederationPulledAssets::Deleted).boolean().not_null())
                    .col(
                        ColumnDef::new(FederationPulledAssets::AmkVersion)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(FederationPulledAssets::MetadataBlob).text().null())
                    .primary_key(
                        Index::create()
                            .col(FederationPulledAssets::PeerId)
                            .col(FederationPulledAssets::AlbumId)
                            .col(FederationPulledAssets::AssetId),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_federation_pulled_assets_album")
                            .from(
                                FederationPulledAssets::Table,
                                (FederationPulledAssets::PeerId, FederationPulledAssets::AlbumId),
                            )
                            .to(
                                FederationPulledAlbums::Table,
                                (FederationPulledAlbums::PeerId, FederationPulledAlbums::AlbumId),
                            )
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        // Every entry that passed re-validation, numbered per job in the order written.
        manager
            .create_table(
                Table::create()
                    .table(FederationAcceptedEntries::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(FederationAcceptedEntries::PeerId).text().not_null())
                    .col(ColumnDef::new(FederationAcceptedEntries::AlbumId).text().not_null())
                    .col(
                        ColumnDef::new(FederationAcceptedEntries::Position)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(FederationAcceptedEntries::AssetId).text().not_null())
                    .col(
                        ColumnDef::new(FederationAcceptedEntries::SyncSeq)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(FederationAcceptedEntries::Change).text().not_null())
                    .col(
                        ColumnDef::new(FederationAcceptedEntries::ChangedAt)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(FederationAcceptedEntries::Manifest).binary().null())
                    .col(ColumnDef::new(FederationAcceptedEntries::Head).binary().null())
                    .col(ColumnDef::new(FederationAcceptedEntries::MetadataBlob).text().null())
                    .col(
                        ColumnDef::new(FederationAcceptedEntries::OriginalHeld)
                            .boolean()
                            .not_null(),
                    )
                    .col(ColumnDef::new(FederationAcceptedEntries::Purged).boolean().not_null())
                    .primary_key(
                        Index::create()
                            .col(FederationAcceptedEntries::PeerId)
                            .col(FederationAcceptedEntries::AlbumId)
                            .col(FederationAcceptedEntries::Position),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_federation_accepted_entries_album")
                            .from(
                                FederationAcceptedEntries::Table,
                                (FederationAcceptedEntries::PeerId, FederationAcceptedEntries::AlbumId),
                            )
                            .to(
                                FederationPulledAlbums::Table,
                                (FederationPulledAlbums::PeerId, FederationPulledAlbums::AlbumId),
                            )
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        // The blobs an accepted entry names, by address, in the entry's own order. Their bytes
        // are fetched when a member views them; the address index is what a reference check
        // reads.
        manager
            .create_table(
                Table::create()
                    .table(FederationAcceptedEntryBlobs::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(FederationAcceptedEntryBlobs::PeerId).text().not_null())
                    .col(
                        ColumnDef::new(FederationAcceptedEntryBlobs::AlbumId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FederationAcceptedEntryBlobs::Position)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FederationAcceptedEntryBlobs::Ordinal)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(FederationAcceptedEntryBlobs::Role).text().not_null())
                    .col(ColumnDef::new(FederationAcceptedEntryBlobs::Address).text().not_null())
                    .col(
                        ColumnDef::new(FederationAcceptedEntryBlobs::Size)
                            .big_integer()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(FederationAcceptedEntryBlobs::PeerId)
                            .col(FederationAcceptedEntryBlobs::AlbumId)
                            .col(FederationAcceptedEntryBlobs::Position)
                            .col(FederationAcceptedEntryBlobs::Ordinal),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_federation_accepted_entry_blobs_entry")
                            .from(
                                FederationAcceptedEntryBlobs::Table,
                                (
                                    FederationAcceptedEntryBlobs::PeerId,
                                    FederationAcceptedEntryBlobs::AlbumId,
                                    FederationAcceptedEntryBlobs::Position,
                                ),
                            )
                            .to(
                                FederationAcceptedEntries::Table,
                                (
                                    FederationAcceptedEntries::PeerId,
                                    FederationAcceptedEntries::AlbumId,
                                    FederationAcceptedEntries::Position,
                                ),
                            )
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_federation_accepted_entry_blobs_address")
                    .table(FederationAcceptedEntryBlobs::Table)
                    .col(FederationAcceptedEntryBlobs::Address)
                    .to_owned(),
            )
            .await?;
        // The signed derivative manifests an accepted entry carries, verbatim, in its order.
        manager
            .create_table(
                Table::create()
                    .table(FederationAcceptedEntryDerivatives::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(FederationAcceptedEntryDerivatives::PeerId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FederationAcceptedEntryDerivatives::AlbumId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FederationAcceptedEntryDerivatives::Position)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FederationAcceptedEntryDerivatives::Ordinal)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FederationAcceptedEntryDerivatives::Manifest)
                            .binary()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(FederationAcceptedEntryDerivatives::PeerId)
                            .col(FederationAcceptedEntryDerivatives::AlbumId)
                            .col(FederationAcceptedEntryDerivatives::Position)
                            .col(FederationAcceptedEntryDerivatives::Ordinal),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_federation_accepted_entry_derivatives_entry")
                            .from(
                                FederationAcceptedEntryDerivatives::Table,
                                (
                                    FederationAcceptedEntryDerivatives::PeerId,
                                    FederationAcceptedEntryDerivatives::AlbumId,
                                    FederationAcceptedEntryDerivatives::Position,
                                ),
                            )
                            .to(
                                FederationAcceptedEntries::Table,
                                (
                                    FederationAcceptedEntries::PeerId,
                                    FederationAcceptedEntries::AlbumId,
                                    FederationAcceptedEntries::Position,
                                ),
                            )
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        // The metadata blobs a job holds: the blob store has the bytes, and this row is the
        // job's claim on them, what its storage is charged by, and what a reference check reads.
        manager
            .create_table(
                Table::create()
                    .table(FederationPulledMetadataBlobs::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(FederationPulledMetadataBlobs::PeerId).text().not_null())
                    .col(
                        ColumnDef::new(FederationPulledMetadataBlobs::AlbumId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FederationPulledMetadataBlobs::Address)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FederationPulledMetadataBlobs::Size)
                            .big_integer()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(FederationPulledMetadataBlobs::PeerId)
                            .col(FederationPulledMetadataBlobs::AlbumId)
                            .col(FederationPulledMetadataBlobs::Address),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_federation_pulled_metadata_blobs_album")
                            .from(
                                FederationPulledMetadataBlobs::Table,
                                (FederationPulledMetadataBlobs::PeerId, FederationPulledMetadataBlobs::AlbumId),
                            )
                            .to(
                                FederationPulledAlbums::Table,
                                (FederationPulledAlbums::PeerId, FederationPulledAlbums::AlbumId),
                            )
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_federation_pulled_metadata_blobs_address")
                    .table(FederationPulledMetadataBlobs::Table)
                    .col(FederationPulledMetadataBlobs::Address)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Children first, so no foreign key is left pointing at a dropped table.
        manager
            .drop_table(
                Table::drop()
                    .table(FederationPulledMetadataBlobs::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(FederationAcceptedEntryDerivatives::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(FederationAcceptedEntryBlobs::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(FederationAcceptedEntries::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(FederationPulledAssets::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(FederationPulledAlbums::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum FederationPulledAlbums {
    Table,
    PeerId,
    AlbumId,
    Cursor,
    HighWater,
    AmkVersion,
    EntriesWritten,
}

#[derive(DeriveIden)]
enum FederationPulledAssets {
    Table,
    PeerId,
    AlbumId,
    AssetId,
    Head,
    Superseded,
    Deleted,
    AmkVersion,
    MetadataBlob,
}

#[derive(DeriveIden)]
enum FederationAcceptedEntries {
    Table,
    PeerId,
    AlbumId,
    Position,
    AssetId,
    SyncSeq,
    Change,
    ChangedAt,
    Manifest,
    Head,
    MetadataBlob,
    OriginalHeld,
    Purged,
}

#[derive(DeriveIden)]
enum FederationAcceptedEntryBlobs {
    Table,
    PeerId,
    AlbumId,
    Position,
    Ordinal,
    Role,
    Address,
    Size,
}

#[derive(DeriveIden)]
enum FederationAcceptedEntryDerivatives {
    Table,
    PeerId,
    AlbumId,
    Position,
    Ordinal,
    Manifest,
}

#[derive(DeriveIden)]
enum FederationPulledMetadataBlobs {
    Table,
    PeerId,
    AlbumId,
    Address,
    Size,
}
