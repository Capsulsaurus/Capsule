//! The per-owner ledger head (#757): the anchor a rebuild or a scrub verifies an owner ledger
//! against, so a tail cut at an entry boundary is reported rather than read as a shorter ledger.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // A table of its own rather than a column on `owner_sequences` or `owner_sync_epochs`:
        // the ledger's positions are not the feed's, and an owner whose ledger holds only a
        // directory entry has a head and no sequence row. An owner with no row has no anchor,
        // which is every owner when this runs; the next append records one.
        manager
            .create_table(
                Table::create()
                    .table(OwnerLedgerHeads::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(OwnerLedgerHeads::OwnerId)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(OwnerLedgerHeads::LedgerSeq)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(OwnerLedgerHeads::LedgerHash)
                            .binary()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(OwnerLedgerHeads::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum OwnerLedgerHeads {
    Table,
    OwnerId,
    LedgerSeq,
    LedgerHash,
}
