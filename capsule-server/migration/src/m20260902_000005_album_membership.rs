//! Album membership (`S-C51`): the roster the owner attested, and who it makes a member.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // One row per album: the roster the server currently accepts, with the signed document
        // verbatim. A replay is decided on the document's bytes, and an operator can re-verify
        // what was accepted against the owner's directory of the day.
        manager
            .create_table(
                Table::create()
                    .table(AlbumRosters::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AlbumRosters::AlbumId)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AlbumRosters::RosterVersion)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AlbumRosters::AmkEpoch)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AlbumRosters::AttestedByDevice)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AlbumRosters::ReceivedAt)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(AlbumRosters::Document).binary().not_null())
                    .to_owned(),
            )
            .await?;

        // One row per account that has ever been on one of the album's rosters. A member the
        // owner removes keeps their row with the two `revoked_*` columns set: the blob route's
        // `403` is reserved for a caller the server can see once *had* access, and a deleted row
        // would make a former member indistinguishable from a stranger.
        manager
            .create_table(
                Table::create()
                    .table(AlbumMembers::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(AlbumMembers::AlbumId).text().not_null())
                    .col(ColumnDef::new(AlbumMembers::UserId).text().not_null())
                    .col(ColumnDef::new(AlbumMembers::Role).text().not_null())
                    .col(
                        ColumnDef::new(AlbumMembers::SinceVersion)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AlbumMembers::GrantedEpoch)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AlbumMembers::RevokedAtVersion)
                            .big_integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(AlbumMembers::RevokedEpoch)
                            .big_integer()
                            .null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(AlbumMembers::AlbumId)
                            .col(AlbumMembers::UserId),
                    )
                    .to_owned(),
            )
            .await?;
        // "Which albums is this account on" — the sync side's question, and the one the
        // primary key does not answer.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_album_members_user")
                    .table(AlbumMembers::Table)
                    .col(AlbumMembers::UserId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(AlbumMembers::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(AlbumRosters::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum AlbumRosters {
    Table,
    AlbumId,
    RosterVersion,
    AmkEpoch,
    AttestedByDevice,
    ReceivedAt,
    Document,
}

#[derive(DeriveIden)]
enum AlbumMembers {
    Table,
    AlbumId,
    UserId,
    Role,
    SinceVersion,
    GrantedEpoch,
    RevokedAtVersion,
    RevokedEpoch,
}
