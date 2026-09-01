//! The durable device-cohort map (slice `S-C13`).
//!
//! One row per `(user_id, cohort_hash)`: the advisory session-grouping aid from the
//! [Authentication — Device Cohorts] contract. The `cohort_hash` is client-asserted and
//! **unverifiable**, so this table is a legibility aid only — no authorization or capability
//! decision ever reads it (the security-bearing identity is `device_id`/the DSK, never this).
//!
//! It persists **beyond session expiry** on purpose: a session-store-only cohort would be
//! forgotten exactly when the "have I seen this physical device before?" question matters
//! (a reinstall re-enrolls with a fresh `device_id` but the same cohort). `first_seen` is
//! pinned on the first observation; `last_seen` is bumped on every re-observation.
//!
//! [Authentication — Device Cohorts]: https://docs/design/authentication/#device-cohorts

use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::*;

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
                    // Account id (nanoid) the cohort was observed under — the `user_id` fold
                    // in the hash means the same physical device under two accounts yields
                    // distinct rows here (unlinkable by construction).
                    .col(
                        ColumnDef::new(DeviceCohorts::UserId)
                            .char_len(21)
                            .not_null(),
                    )
                    // The advisory, client-asserted cohort hash (opaque string, stored
                    // verbatim). Never interpreted for any authorization decision.
                    .col(string(DeviceCohorts::CohortHash))
                    // First observation of this (user, cohort) — pinned, never moved back.
                    .col(
                        timestamp_with_time_zone(DeviceCohorts::FirstSeen)
                            .default(Expr::current_timestamp()),
                    )
                    // Most recent observation — bumped on every re-observation.
                    .col(
                        timestamp_with_time_zone(DeviceCohorts::LastSeen)
                            .default(Expr::current_timestamp()),
                    )
                    .primary_key(
                        Index::create()
                            .col(DeviceCohorts::UserId)
                            .col(DeviceCohorts::CohortHash),
                    )
                    .to_owned(),
            )
            .await
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
