//! The asset index (`S-C37`): the rows, the blobs they hold, the manifests they have moved
//! past, the per-owner sequence, and the applied-manifest ledger.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // The one sequence, per **owner**. Never a Postgres `SEQUENCE` or a `bigserial`:
        // `nextval` is non-transactional, so two concurrent finalizations get 5 and 6 and a
        // reader who sees 6 commit first can page past 5 forever. A counter row updated inside
        // the allocating transaction makes allocation order equal commit order, which is the
        // whole of `S-C21`'s fix.
        manager
            .create_table(
                Table::create()
                    .table(OwnerSequences::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(OwnerSequences::OwnerId)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(OwnerSequences::NextSeq)
                            .big_integer()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(Assets::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Assets::AssetId)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Assets::OwnerId).text().not_null())
                    .col(ColumnDef::new(Assets::AlbumId).text().not_null())
                    .col(ColumnDef::new(Assets::ProtocolVersion).text().not_null())
                    .col(ColumnDef::new(Assets::CryptoSuiteId).integer().not_null())
                    .col(ColumnDef::new(Assets::State).text().not_null())
                    // Null is the ordinary case: a hold is placed by an admin action and never
                    // by a write path.
                    .col(ColumnDef::new(Assets::Hold).text().null())
                    // Null while the row is pending — a row nothing can see occupies no place
                    // in the feed, which is what keeps an abandoned half-bundle from consuming
                    // a number.
                    .col(ColumnDef::new(Assets::SyncSeq).big_integer().null())
                    .col(ColumnDef::new(Assets::FirstSeq).big_integer().null())
                    // Invariant 17's stored chain head: SHA-256 over the signed manifest, and
                    // deliberately not the provenance blob's content address — the two are
                    // equal today and are not the same identifier (`S-C31`).
                    .col(ColumnDef::new(Assets::ChainHead).binary().null())
                    .col(
                        ColumnDef::new(Assets::AmkVersion)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(ColumnDef::new(Assets::RetentionUntil).big_integer().null())
                    .col(ColumnDef::new(Assets::CreatedAt).big_integer().not_null())
                    .col(ColumnDef::new(Assets::UpdatedAt).big_integer().not_null())
                    .to_owned(),
            )
            .await?;

        // The feed's own index: every page is "this owner, above this number, in order".
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_assets_owner_sync_seq")
                    .table(Assets::Table)
                    .col(Assets::OwnerId)
                    .col(Assets::SyncSeq)
                    .to_owned(),
            )
            .await?;
        // The duplicate lookup's, which is owner- **and** album-scoped for two different
        // reasons (see `AssetIndex::find_by_address`).
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_assets_owner_album")
                    .table(Assets::Table)
                    .col(Assets::OwnerId)
                    .col(Assets::AlbumId)
                    .to_owned(),
            )
            .await?;
        // The purge worker's input: tombstoned rows, oldest change first.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_assets_state_updated_at")
                    .table(Assets::Table)
                    .col(Assets::State)
                    .col(Assets::UpdatedAt)
                    .to_owned(),
            )
            .await?;

        // `(asset_id, role, address)` is the primary key, and that is what makes a retried
        // finalization free: the insert is a no-op rather than a second row.
        manager
            .create_table(
                Table::create()
                    .table(AssetBlobs::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(AssetBlobs::AssetId).text().not_null())
                    .col(ColumnDef::new(AssetBlobs::Role).text().not_null())
                    .col(ColumnDef::new(AssetBlobs::Address).text().not_null())
                    .col(ColumnDef::new(AssetBlobs::Size).big_integer().not_null())
                    .primary_key(
                        Index::create()
                            .col(AssetBlobs::AssetId)
                            .col(AssetBlobs::Role)
                            .col(AssetBlobs::Address),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_asset_blobs_asset")
                            .from(AssetBlobs::Table, AssetBlobs::AssetId)
                            .to(Assets::Table, Assets::AssetId)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        // `find_reference` and `reference_count` both start from an address.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_asset_blobs_address")
                    .table(AssetBlobs::Table)
                    .col(AssetBlobs::Address)
                    .to_owned(),
            )
            .await?;

        // The manifests a chain has moved past (`S-C52`). These count as references, so the
        // collector does not reclaim the server's own rebuttal evidence. `Position` keeps the
        // order the port contracts — oldest first — rather than leaving it to the backend.
        manager
            .create_table(
                Table::create()
                    .table(AssetSuperseded::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(AssetSuperseded::AssetId).text().not_null())
                    .col(
                        ColumnDef::new(AssetSuperseded::Position)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(AssetSuperseded::Address).text().not_null())
                    .primary_key(
                        Index::create()
                            .col(AssetSuperseded::AssetId)
                            .col(AssetSuperseded::Position),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_asset_superseded_asset")
                            .from(AssetSuperseded::Table, AssetSuperseded::AssetId)
                            .to(Assets::Table, Assets::AssetId)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_asset_superseded_address")
                    .table(AssetSuperseded::Table)
                    .col(AssetSuperseded::Address)
                    .to_owned(),
            )
            .await?;

        // The whole idempotency store for a lifecycle write: a replay needs the number the
        // first application minted, and everything else in the response is derivable from the
        // manifest itself. The retired implementation kept the serialized response body in a
        // table — a second copy of something derivable, and therefore a second thing that can
        // be wrong.
        manager
            .create_table(
                Table::create()
                    .table(AppliedManifests::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AppliedManifests::ManifestHash)
                            .binary()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AppliedManifests::SyncSeq)
                            .big_integer()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(AppliedManifests::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(AssetSuperseded::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(AssetBlobs::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Assets::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(OwnerSequences::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Assets {
    Table,
    AssetId,
    OwnerId,
    AlbumId,
    ProtocolVersion,
    CryptoSuiteId,
    State,
    Hold,
    SyncSeq,
    FirstSeq,
    ChainHead,
    AmkVersion,
    RetentionUntil,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum AssetBlobs {
    Table,
    AssetId,
    Role,
    Address,
    Size,
}

#[derive(DeriveIden)]
enum AssetSuperseded {
    Table,
    AssetId,
    Position,
    Address,
}

#[derive(DeriveIden)]
enum OwnerSequences {
    Table,
    OwnerId,
    NextSeq,
}

#[derive(DeriveIden)]
enum AppliedManifests {
    Table,
    ManifestHash,
    SyncSeq,
}
