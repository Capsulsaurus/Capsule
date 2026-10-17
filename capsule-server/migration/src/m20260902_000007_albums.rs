//! Provisioned albums (`S-C25`), with the upgrade ceremony in flight against each (`S-C24`).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // One row per album. The primary key is what makes provisioning one operation: the
        // insert either binds the id or conflicts, and a conflict is answered from the row that
        // won rather than from a read taken before the write.
        //
        // The upgrade ceremony is two nullable columns written and cleared together. The intent
        // is kept as the canonical CBOR its proposer signed rather than as one column per field,
        // because its expiry is decided by core's own predicate over the decoded intent and a
        // `u64` deadline does not fit a `BIGINT` without a rule this schema would have to invent.
        manager
            .create_table(
                Table::create()
                    .table(Albums::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Albums::AlbumId)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Albums::OwnerId).text().not_null())
                    .col(ColumnDef::new(Albums::ProtocolVersion).text().not_null())
                    .col(ColumnDef::new(Albums::CreatedAt).big_integer().not_null())
                    .col(ColumnDef::new(Albums::UpgradeIntent).binary().null())
                    .col(
                        ColumnDef::new(Albums::UpgradeReceivedAt)
                            .big_integer()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Albums::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Albums {
    Table,
    AlbumId,
    OwnerId,
    ProtocolVersion,
    CreatedAt,
    UpgradeIntent,
    UpgradeReceivedAt,
}
