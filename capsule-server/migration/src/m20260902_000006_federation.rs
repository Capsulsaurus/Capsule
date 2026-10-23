//! Federation (`S-E2`, `S-E5`): the capabilities this server issued, the revocation list it
//! publishes, and the peers an operator has pinned or blocked.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // One row per capability this server minted. The record carries what the token does not:
        // the roster member the grant was made for and the epoch their membership was granted
        // at, which is what a presentation re-checks so a member removed and re-admitted later
        // cannot reuse an older grant. `refreshed_to` links a predecessor to its successor and
        // is what makes a replayed refresh idempotent without an idempotency table.
        manager
            .create_table(
                Table::create()
                    .table(FederationCapabilities::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(FederationCapabilities::Jti)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(FederationCapabilities::AlbumId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FederationCapabilities::PeerId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FederationCapabilities::MemberId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FederationCapabilities::Scope)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FederationCapabilities::GrantedEpoch)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FederationCapabilities::MinProtocolVersion)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FederationCapabilities::IssuedAt)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FederationCapabilities::ExpiresAt)
                            .big_integer()
                            .not_null(),
                    )
                    // The absolute deadline of the whole grant, fixed at the original mint and
                    // copied unchanged into every successor. `expires_at` is one token's life
                    // and a refresh replaces it; this is the column a refresh cannot move, and
                    // it is what keeps a chain of refreshes from outliving the lifetime the
                    // album's owner chose. Equal to `expires_at` for a grant nobody made
                    // renewable, which is the default.
                    .col(
                        ColumnDef::new(FederationCapabilities::NotAfter)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FederationCapabilities::RevokedAt)
                            .big_integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(FederationCapabilities::RefreshedTo)
                            .text()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;
        // "Every live grant over this album" — what a roster change consults — and "every live
        // grant this peer holds" — what a block cascades over. Neither is the primary key's
        // question.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_federation_capabilities_album")
                    .table(FederationCapabilities::Table)
                    .col(FederationCapabilities::AlbumId)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_federation_capabilities_peer")
                    .table(FederationCapabilities::Table)
                    .col(FederationCapabilities::PeerId)
                    .to_owned(),
            )
            .await?;

        // The published list, and its own table rather than a view over the one above: a
        // revocation is also accepted for a `jti` no record backs — an operator cutting a token
        // named in a peer's report, or one that predates this store — and the list must carry
        // it either way. `expires_at` is what the list is pruned by, so an entry never outlives
        // the token it is about.
        manager
            .create_table(
                Table::create()
                    .table(FederationRevokedJti::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(FederationRevokedJti::Jti)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(FederationRevokedJti::ExpiresAt)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FederationRevokedJti::RevokedAt)
                            .big_integer()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;
        // The published record is read in expiry order and pruned by it on every read.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_federation_revoked_jti_expiry")
                    .table(FederationRevokedJti::Table)
                    .col(FederationRevokedJti::ExpiresAt)
                    .to_owned(),
            )
            .await?;

        // The peers this server knows. `signing_key` is nullable because a block may name a
        // server nobody ever pinned — an operator blocking a server they never wanted to hear
        // from is legitimate — and `blocked_at` is the blocklist itself, a column rather than a
        // table because the blocklist operates at the federation-capability layer.
        manager
            .create_table(
                Table::create()
                    .table(FederationPeers::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(FederationPeers::ServerId)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(FederationPeers::SigningKey).binary().null())
                    .col(
                        ColumnDef::new(FederationPeers::FirstSeenAt)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FederationPeers::BlockedAt)
                            .big_integer()
                            .null(),
                    )
                    .col(ColumnDef::new(FederationPeers::Note).text().null())
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(FederationPeers::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(FederationRevokedJti::Table).to_owned())
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(FederationCapabilities::Table)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum FederationCapabilities {
    Table,
    Jti,
    AlbumId,
    PeerId,
    MemberId,
    Scope,
    GrantedEpoch,
    MinProtocolVersion,
    IssuedAt,
    ExpiresAt,
    NotAfter,
    RevokedAt,
    RefreshedTo,
}

#[derive(DeriveIden)]
enum FederationRevokedJti {
    Table,
    Jti,
    ExpiresAt,
    RevokedAt,
}

#[derive(DeriveIden)]
enum FederationPeers {
    Table,
    ServerId,
    SigningKey,
    FirstSeenAt,
    BlockedAt,
    Note,
}
