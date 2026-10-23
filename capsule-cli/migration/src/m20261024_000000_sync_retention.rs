//! The retention deadline a synced delete names: the `retention_until` of the asset's signed
//! delete manifest, as unix seconds rounded up, kept on the asset's `synced_assets` row while it
//! sits in trash. Once that deadline has passed no `trash-restore` can recover the asset, so the
//! CLI records it as purged ([Peering § Sync]) and clears the deadline. Null on a live asset, on
//! a purge already recorded, and on a delete whose manifest names no deadline.
//!
//! [Peering § Sync]: https://docs/design/peering/#sync

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(SyncedAssets::Table)
                    .add_column(
                        ColumnDef::new(SyncedAssets::RetentionUntil)
                            .big_integer()
                            .null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(SyncedAssets::Table)
                    .drop_column(SyncedAssets::RetentionUntil)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum SyncedAssets {
    Table,
    RetentionUntil,
}
