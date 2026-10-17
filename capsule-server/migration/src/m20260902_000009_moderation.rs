//! Moderation (`S-C8`, `S-C49`): account standing, the record a user reads about it, and the
//! federated report queue.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // One row per account anything has been done to. `suspended_since` is `NULL` for an
        // account that was reinstated, so a standing that was set is distinguishable in the
        // table from one that never was — the port answers `Active` for both.
        manager
            .create_table(
                Table::create()
                    .table(ModerationStanding::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ModerationStanding::UserId)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(ModerationStanding::SuspendedSince)
                            .big_integer()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        // The record a user reads, append-only. `seq` is the order the actions were applied in,
        // which is the order the port promises — not `at`, which is the caller's instant and may
        // tie or arrive out of order.
        manager
            .create_table(
                Table::create()
                    .table(ModerationEvents::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ModerationEvents::Seq)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(ModerationEvents::UserId).text().not_null())
                    .col(ColumnDef::new(ModerationEvents::Action).text().not_null())
                    .col(ColumnDef::new(ModerationEvents::AssetId).text().null())
                    .col(
                        ColumnDef::new(ModerationEvents::At)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(ModerationEvents::Reason).text().null())
                    .to_owned(),
            )
            .await?;
        // "Everything done to this account, oldest first" — the one read the record serves.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_moderation_events_user")
                    .table(ModerationEvents::Table)
                    .col(ModerationEvents::UserId)
                    .col(ModerationEvents::Seq)
                    .to_owned(),
            )
            .await?;

        // The federated report queue. Nothing is evicted: a durable queue is where a report that
        // must not be lost belongs, which is the reason the in-memory adapter's cap exists at all.
        // `signed` is the exact canonical CBOR the signature covers; every other column is
        // derived from it at intake.
        manager
            .create_table(
                Table::create()
                    .table(FederatedReports::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(FederatedReports::ReportId)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(FederatedReports::ReportingServer)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FederatedReports::ReportedUser)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FederatedReports::AssetHash)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(FederatedReports::AlbumId).text().not_null())
                    .col(ColumnDef::new(FederatedReports::Reason).text().null())
                    .col(
                        ColumnDef::new(FederatedReports::ReportedAt)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FederatedReports::ReceivedAt)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FederatedReports::Signature)
                            .binary()
                            .not_null(),
                    )
                    .col(ColumnDef::new(FederatedReports::Signed).binary().not_null())
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(FederatedReports::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(ModerationEvents::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(ModerationStanding::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum ModerationStanding {
    Table,
    UserId,
    SuspendedSince,
}

#[derive(DeriveIden)]
enum ModerationEvents {
    Table,
    Seq,
    UserId,
    Action,
    AssetId,
    At,
    Reason,
}

#[derive(DeriveIden)]
enum FederatedReports {
    Table,
    ReportId,
    ReportingServer,
    ReportedUser,
    AssetHash,
    AlbumId,
    Reason,
    ReportedAt,
    ReceivedAt,
    Signature,
    Signed,
}
