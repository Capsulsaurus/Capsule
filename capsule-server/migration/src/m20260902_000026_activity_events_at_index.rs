//! An index on `activity_events (at)` (#1112), so the operator's sweep of expired activity
//! events is an index range rather than a scan of every account's log.
//!
//! The table's one index is `(user_id, seq)`, which answers "this account's events" for the
//! read and for the per-account retention deletes an append runs. The sweep asks the other
//! question — "every event recorded before this instant, whoever's it is" — for the accounts
//! that stopped recording, and without this index each of its batches reads the whole table.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

/// The index's name, shared by `up` and `down` so the rollback drops exactly what was created.
const INDEX: &str = "idx_activity_events_at";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name(INDEX)
                    .table(ActivityEvents::Table)
                    .col(ActivityEvents::At)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .if_exists()
                    .name(INDEX)
                    .table(ActivityEvents::Table)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum ActivityEvents {
    Table,
    At,
}
