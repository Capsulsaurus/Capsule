//! The sync-fed local store (slice `S-D5`): the opaque cursor and the assets the
//! sync feed has landed, which `capsule list` queries client-side.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // The single opaque feed cursor (one row, id = 0).
        manager
            .create_table(
                Table::create()
                    .table(SyncCursor::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SyncCursor::Id)
                            .integer()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(SyncCursor::Cursor).blob().not_null())
                    .to_owned(),
            )
            .await?;

        // The assets the feed has landed.
        manager
            .create_table(
                Table::create()
                    .table(SyncedAssets::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SyncedAssets::AssetId)
                            .string()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(SyncedAssets::AlbumId).string().not_null())
                    .col(
                        ColumnDef::new(SyncedAssets::SyncSeq)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(SyncedAssets::Kind).string().not_null())
                    .col(
                        ColumnDef::new(SyncedAssets::OriginalHeld)
                            .boolean()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SyncedAssets::Tombstoned)
                            .boolean()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_synced_assets_album_id")
                    .table(SyncedAssets::Table)
                    .col(SyncedAssets::AlbumId)
                    .index_type(IndexType::BTree)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_synced_assets_tombstoned")
                    .table(SyncedAssets::Table)
                    .col(SyncedAssets::Tombstoned)
                    .index_type(IndexType::BTree)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(SyncedAssets::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(SyncCursor::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum SyncCursor {
    Table,
    Id,
    Cursor,
}

#[derive(DeriveIden)]
enum SyncedAssets {
    Table,
    AssetId,
    AlbumId,
    SyncSeq,
    Kind,
    OriginalHeld,
    Tombstoned,
}
