//! Published device directories (`S-C9`) and the identity anchor each account is held to
//! (`S-C42`).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // One row per account: the directory currently in force, its signed bytes verbatim, and
        // the identity key the first publish anchored. The anchor is never rewritten — a later
        // publish under a different key is refused before the row is touched — so the column
        // holds the key of the account's very first directory for as long as the row exists.
        manager
            .create_table(
                Table::create()
                    .table(DeviceDirectories::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(DeviceDirectories::UserId)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(DeviceDirectories::DirectoryVersion)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DeviceDirectories::Document)
                            .binary()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DeviceDirectories::IdentityKey)
                            .binary()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DeviceDirectories::PublishedAt)
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
            .drop_table(Table::drop().table(DeviceDirectories::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum DeviceDirectories {
    Table,
    UserId,
    DirectoryVersion,
    Document,
    IdentityKey,
    PublishedAt,
}
