//! The federation soft-fail table and the breadcrumb index's read path (design/federation.md,
//! Soft-Fail Semantics and Federated Breadcrumb Index).
//!
//! `federation_rejected_hashes` remembers each federated event this server refused, by its
//! hash. It is bounded by the adapter, by count and by age, so the one index besides the key is
//! the eviction order: last reference, then hash.
//!
//! The breadcrumb index stores nothing of its own: it is each pulled asset's latest accepted
//! entry. The index added here on `federation_accepted_entries` is what reads that latest entry
//! per asset without scanning the album's whole history.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Instants are BIGINT microseconds, as every durable port stores them.
        manager
            .create_table(
                Table::create()
                    .table(FederationRejectedHashes::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(FederationRejectedHashes::Hash)
                            .binary()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(FederationRejectedHashes::PeerId).text().not_null())
                    .col(ColumnDef::new(FederationRejectedHashes::AlbumId).text().not_null())
                    .col(ColumnDef::new(FederationRejectedHashes::AssetId).text().not_null())
                    .col(ColumnDef::new(FederationRejectedHashes::Rule).text().not_null())
                    .col(
                        ColumnDef::new(FederationRejectedHashes::FirstRejectedAt)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FederationRejectedHashes::LastReferencedAt)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FederationRejectedHashes::ReferenceCount)
                            .big_integer()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_federation_rejected_hashes_last_referenced")
                    .table(FederationRejectedHashes::Table)
                    .col(FederationRejectedHashes::LastReferencedAt)
                    .col(FederationRejectedHashes::Hash)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_federation_accepted_entries_asset")
                    .table(FederationAcceptedEntries::Table)
                    .col(FederationAcceptedEntries::PeerId)
                    .col(FederationAcceptedEntries::AlbumId)
                    .col(FederationAcceptedEntries::AssetId)
                    .col(FederationAcceptedEntries::Position)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx_federation_accepted_entries_asset")
                    .table(FederationAcceptedEntries::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(FederationRejectedHashes::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum FederationRejectedHashes {
    Table,
    Hash,
    PeerId,
    AlbumId,
    AssetId,
    Rule,
    FirstRejectedAt,
    LastReferencedAt,
    ReferenceCount,
}

#[derive(DeriveIden)]
enum FederationAcceptedEntries {
    Table,
    PeerId,
    AlbumId,
    AssetId,
    Position,
}
