//! The sealed user identity beside the master-key escrow (#544): one blob per account, and
//! nothing older.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // The escrow table's shape, in a table of its own rather than a column beside `escrows`:
        // the two blobs are stored independently (a recovery-secret rotation re-wraps the escrow
        // and leaves the identity alone), so neither may need the other's row to exist. One row
        // per account, replaced by one upsert, with no history table.
        manager
            .create_table(
                Table::create()
                    .table(SealedIdentities::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SealedIdentities::UserId)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(SealedIdentities::Blob).binary().not_null())
                    .col(
                        ColumnDef::new(SealedIdentities::StoredAt)
                            .big_integer()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(SealedIdentities::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum SealedIdentities {
    Table,
    UserId,
    Blob,
    StoredAt,
}
