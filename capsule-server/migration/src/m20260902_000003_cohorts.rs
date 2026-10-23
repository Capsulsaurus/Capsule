//! The durable device-cohort map (`S-C13`).
//!
//! Advisory storage, structurally: nothing here is read by an authorization path, and the port
//! offers no lookup that could tempt one. The map outlives sessions deliberately — a cohort
//! becomes worth knowing exactly when the sessions that carried it have expired.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(DeviceCohorts::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(DeviceCohorts::UserId).text().not_null())
                    .col(ColumnDef::new(DeviceCohorts::CohortHash).text().not_null())
                    .col(
                        ColumnDef::new(DeviceCohorts::FirstSeen)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(DeviceCohorts::LastSeen)
                            .big_integer()
                            .not_null(),
                    )
                    // The composite key **is** the idempotence the port states: seeing the same
                    // cohort twice is one row, not two, so `observe` is an upsert rather than a
                    // read followed by a decision.
                    .primary_key(
                        Index::create()
                            .col(DeviceCohorts::UserId)
                            .col(DeviceCohorts::CohortHash),
                    )
                    .to_owned(),
            )
            .await?;

        // The listing's order is part of the contract — oldest first sighting first — because
        // it is a user-visible surface.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_device_cohorts_user_first_seen")
                    .table(DeviceCohorts::Table)
                    .col(DeviceCohorts::UserId)
                    .col(DeviceCohorts::FirstSeen)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(DeviceCohorts::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum DeviceCohorts {
    Table,
    UserId,
    CohortHash,
    FirstSeen,
    LastSeen,
}
