//! An index on `share_serves (address)` (#659), so asking whether any live link serves one
//! address is an index probe rather than a scan of every link's serve set.
//!
//! The table's primary key is `(opaque_id, address)`, which leads with the link and so answers
//! "what does this link serve" but not "which links serve this address". The collector asks the
//! second for every blob the asset index holds no reference to (`ShareStore::names_live`), and
//! without this index each of those questions reads the whole table.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

/// The index's name, shared by `up` and `down` so the rollback drops exactly what was created.
const INDEX: &str = "idx_share_serves_address";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name(INDEX)
                    .table(ShareServes::Table)
                    .col(ShareServes::Address)
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
                    .table(ShareServes::Table)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum ShareServes {
    Table,
    Address,
}
