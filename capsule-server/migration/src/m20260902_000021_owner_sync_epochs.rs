//! The per-owner sync epoch (#697): bumped when an index rebuild cannot prove an owner's
//! sequence complete, so a cursor minted before the rebuild is refused rather than trusted.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // A table of its own rather than a column on `owner_sequences`: the epoch is not part of
        // allocation, and an owner whose epoch is bumped before anything was minted must not
        // gain a sequence row that reads as "minted zero". An owner with no row is at epoch 0,
        // which is every owner when this runs.
        manager
            .create_table(
                Table::create()
                    .table(OwnerSyncEpochs::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(OwnerSyncEpochs::OwnerId)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(OwnerSyncEpochs::Epoch)
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
            .drop_table(Table::drop().table(OwnerSyncEpochs::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum OwnerSyncEpochs {
    Table,
    OwnerId,
    Epoch,
}
