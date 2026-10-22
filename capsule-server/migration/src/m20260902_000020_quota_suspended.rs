//! The quota ledger's suspended state (#705): when an operator suspended an account's quota.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Nullable, and added rather than backfilled: every account that exists when this runs
        // is not suspended, which is what `NULL` says. An instant rather than a flag, so the
        // ledger records when the suspension began.
        manager
            .alter_table(
                Table::alter()
                    .table(QuotaUsage::Table)
                    .add_column_if_not_exists(
                        ColumnDef::new(QuotaUsage::SuspendedSince)
                            .big_integer()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(QuotaUsage::Table)
                    .drop_column(QuotaUsage::SuspendedSince)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum QuotaUsage {
    Table,
    SuspendedSince,
}
