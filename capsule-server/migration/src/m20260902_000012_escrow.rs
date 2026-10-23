//! The master-key escrow: one wrapped master key per account, and nothing older.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // One row per account, keyed on the account, and no history table beside it. Replacing
        // an escrow is one upsert on this key — never a delete followed by an insert — and a
        // previous-escrow table would preserve exactly the artifact a rotation exists to
        // destroy.
        manager
            .create_table(
                Table::create()
                    .table(Escrows::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Escrows::UserId)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Escrows::Blob).binary().not_null())
                    .col(ColumnDef::new(Escrows::StoredAt).big_integer().not_null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Escrows::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Escrows {
    Table,
    UserId,
    Blob,
    StoredAt,
}
