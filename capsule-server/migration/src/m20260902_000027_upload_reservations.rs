//! The quota ledger's reservation state (#1059): which attributions an upload session reserved
//! and no committed blob has settled yet, and until when.
//!
//! An attribution is written when an upload session is created, before a byte arrives. Without
//! this column a row cannot say whether its bytes were ever committed, so a session that expired
//! without finishing left its uploader charged forever. `reserved_until` is that state:
//! non-null while the attribution is only a reservation, the instant past which the reservation
//! sweep may release it, and null once a finalization settles it — and for every charge that
//! pays for bytes already stored.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

/// The index's name, shared by `up` and `down` so the rollback drops exactly what was created.
const INDEX: &str = "idx_quota_attributions_reserved_until";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Nullable, and added rather than backfilled: every attribution that exists when this
        // runs is treated as settled, which keeps an account charged rather than freeing bytes
        // a committed blob may still use. A reservation in flight across the upgrade is never
        // swept; it stays the over-charge it was before this column existed.
        manager
            .alter_table(
                Table::alter()
                    .table(QuotaAttributions::Table)
                    .add_column_if_not_exists(
                        ColumnDef::new(QuotaAttributions::ReservedUntil)
                            .big_integer()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;
        // What the sweep's bounded listing walks, earliest deadline first, so a pass reads the
        // expired reservations and never the settled attributions beside them.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name(INDEX)
                    .table(QuotaAttributions::Table)
                    .col(QuotaAttributions::ReservedUntil)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .if_exists()
                    .name(INDEX)
                    .table(QuotaAttributions::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(QuotaAttributions::Table)
                    .drop_column(QuotaAttributions::ReservedUntil)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum QuotaAttributions {
    Table,
    ReservedUntil,
}
