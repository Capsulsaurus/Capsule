//! The collector's marks (`S-C11`): which unreferenced blobs are waiting out their grace window.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // One row per marked content address, keyed on the address itself: a blob is either
        // marked or it is not, and the primary key is what makes "marked twice" one row. Durable
        // because the collector marks on one pass and sweeps on a later one — a mark that died
        // with the process would leave a collector that can only ever mark.
        manager
            .create_table(
                Table::create()
                    .table(CollectionMarks::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(CollectionMarks::Address)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(CollectionMarks::MarkedAt)
                            .big_integer()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(CollectionMarks::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum CollectionMarks {
    Table,
    Address,
    MarkedAt,
}
