//! Local album subscriptions (design/federation.md, Token Lifecycle step 1): which album this
//! server pulls from which peer for which local member, under the capability that member's
//! client posted.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // One row per `(subscriber, peer, album)`; a fresher capability replaces the token in
        // place. The token is the capability the subscriber's client posted, verified against
        // the peer's pinned key before it was written.
        manager
            .create_table(
                Table::create()
                    .table(FederationSubscriptions::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(FederationSubscriptions::SubscriberId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FederationSubscriptions::PeerId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FederationSubscriptions::AlbumId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FederationSubscriptions::Scope)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(FederationSubscriptions::Jti).text().not_null())
                    .col(
                        ColumnDef::new(FederationSubscriptions::Token)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FederationSubscriptions::IssuedAt)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FederationSubscriptions::ExpiresAt)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FederationSubscriptions::MinProtocolVersion)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FederationSubscriptions::SubscribedAt)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FederationSubscriptions::UpdatedAt)
                            .big_integer()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(FederationSubscriptions::SubscriberId)
                            .col(FederationSubscriptions::PeerId)
                            .col(FederationSubscriptions::AlbumId),
                    )
                    .to_owned(),
            )
            .await?;
        // The pull worker's two reads: a job's subscriptions, and the distinct live jobs.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_federation_subscriptions_job")
                    .table(FederationSubscriptions::Table)
                    .col(FederationSubscriptions::PeerId)
                    .col(FederationSubscriptions::AlbumId)
                    .col(FederationSubscriptions::ExpiresAt)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(FederationSubscriptions::Table)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum FederationSubscriptions {
    Table,
    SubscriberId,
    PeerId,
    AlbumId,
    Scope,
    Jti,
    Token,
    IssuedAt,
    ExpiresAt,
    MinProtocolVersion,
    SubscribedAt,
    UpdatedAt,
}
