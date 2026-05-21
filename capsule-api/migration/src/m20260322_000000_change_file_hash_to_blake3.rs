use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::string_len;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop the old BigInt column and add a VARCHAR(64) column for BLAKE3 hex hashes.
        // Existing hash values are incompatible (different algorithm), so data loss is expected.
        manager
            .alter_table(
                Table::alter()
                    .table(Assets::Table)
                    .drop_column(Assets::FileHash)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Assets::Table)
                    .add_column(string_len(Assets::FileHash, 64))
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Assets::Table)
                    .drop_column(Assets::FileHash)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Assets::Table)
                    .add_column(
                        ColumnDef::new(Assets::FileHash)
                            .big_integer()
                            .not_null()
                            .to_owned(),
                    )
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Assets {
    Table,
    FileHash,
}
