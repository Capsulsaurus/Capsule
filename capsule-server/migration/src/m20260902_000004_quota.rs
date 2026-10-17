//! The quota ledger (`S-C6`): attribution by content address, and the over-limit clock.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Keyed on the **content address**, globally, and that is the security property rather
        // than a normalization: a blob shared between two uploaders counts against only the
        // first, so re-uploading blobs whose addresses you already know cannot exhaust somebody
        // else's quota. The primary key is what makes the check and the debit one operation.
        manager
            .create_table(
                Table::create()
                    .table(QuotaAttributions::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(QuotaAttributions::Address)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(QuotaAttributions::UserId).text().not_null())
                    .col(
                        ColumnDef::new(QuotaAttributions::Size)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(QuotaAttributions::ChargedAt)
                            .big_integer()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;
        // What `usage` sums over.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_quota_attributions_user")
                    .table(QuotaAttributions::Table)
                    .col(QuotaAttributions::UserId)
                    .to_owned(),
            )
            .await?;

        // Only the clock. A user's total is **derived** by summing their attributions rather
        // than stored beside them, because a stored total is a second copy of a derivable fact
        // and a total that drifts low hands somebody free storage. What cannot be derived is
        // *when* an account crossed the hard limit and has not been under it since — that is
        // the one column here.
        manager
            .create_table(
                Table::create()
                    .table(QuotaUsage::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(QuotaUsage::UserId)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(QuotaUsage::OverSince).big_integer().null())
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(QuotaUsage::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(QuotaAttributions::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum QuotaAttributions {
    Table,
    Address,
    UserId,
    Size,
    ChargedAt,
}

#[derive(DeriveIden)]
enum QuotaUsage {
    Table,
    UserId,
    OverSince,
}
