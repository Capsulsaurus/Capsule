//! The account activity log (design/activity-log.md): one append-only table of an account's
//! security events.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // `seq` is the order events were recorded in, which is the order the port promises —
        // not `at`, which may tie. `event_id` is unique so a duplicate is refused by the insert
        // itself. Rows are never updated; they leave only by the retention bound.
        manager
            .create_table(
                Table::create()
                    .table(ActivityEvents::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ActivityEvents::Seq)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(ActivityEvents::EventId)
                            .text()
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(ActivityEvents::UserId).text().not_null())
                    .col(ColumnDef::new(ActivityEvents::Kind).text().not_null())
                    .col(ColumnDef::new(ActivityEvents::At).big_integer().not_null())
                    .col(ColumnDef::new(ActivityEvents::RequestId).text().null())
                    .col(ColumnDef::new(ActivityEvents::SessionId).text().null())
                    .col(ColumnDef::new(ActivityEvents::Subject).text().null())
                    .to_owned(),
            )
            .await?;
        // The one read ("an account's events, newest first") and both retention deletes walk
        // one account's rows in `seq` order.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_activity_events_user")
                    .table(ActivityEvents::Table)
                    .col(ActivityEvents::UserId)
                    .col(ActivityEvents::Seq)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(ActivityEvents::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum ActivityEvents {
    Table,
    Seq,
    EventId,
    UserId,
    Kind,
    At,
    RequestId,
    SessionId,
    Subject,
}
