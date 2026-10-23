//! Share links: the link record, and the addresses it — and only it — may serve.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // One row per link, keyed on the opaque id that is the link's whole credential. A
        // revoked link keeps its row with `revoked_at` set: the owner's own listing still shows
        // it, and the public path collapses revoked, expired and unknown into one answer above
        // the store.
        manager
            .create_table(
                Table::create()
                    .table(Shares::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Shares::OpaqueId)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Shares::OwnerId).text().not_null())
                    // `asset` or `album`, and the scope's id beside it.
                    .col(ColumnDef::new(Shares::ScopeKind).text().not_null())
                    .col(ColumnDef::new(Shares::ScopeId).text().not_null())
                    .col(ColumnDef::new(Shares::Metadata).text().not_null())
                    .col(ColumnDef::new(Shares::WrappedSecret).binary().null())
                    .col(ColumnDef::new(Shares::ExpiresAt).big_integer().null())
                    .col(ColumnDef::new(Shares::RevokedAt).big_integer().null())
                    .to_owned(),
            )
            .await?;

        // The enumerated serve set, one row per address. Enumerated rather than derived: the
        // serve path has no route from an opaque id to anything outside these rows, which is
        // the enforceable half of the privacy strip. Rows go with their link.
        manager
            .create_table(
                Table::create()
                    .table(ShareServes::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(ShareServes::OpaqueId).text().not_null())
                    .col(ColumnDef::new(ShareServes::Address).text().not_null())
                    .primary_key(
                        Index::create()
                            .col(ShareServes::OpaqueId)
                            .col(ShareServes::Address),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_share_serves_share")
                            .from(ShareServes::Table, ShareServes::OpaqueId)
                            .to(Shares::Table, Shares::OpaqueId)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(ShareServes::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Shares::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Shares {
    Table,
    OpaqueId,
    OwnerId,
    ScopeKind,
    ScopeId,
    Metadata,
    WrappedSecret,
    ExpiresAt,
    RevokedAt,
}

#[derive(DeriveIden)]
enum ShareServes {
    Table,
    OpaqueId,
    Address,
}
