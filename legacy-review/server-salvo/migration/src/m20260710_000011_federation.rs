//! The federation-capability revocation store (slice `S-E2`).
//!
//! Federation reuses S-C8's `federation_peers` table for peer identity (a peer's published
//! Ed25519 operational key) — this migration adds **no** second peer-identity store. It lands
//! only the durable revocation list the capability lifecycle needs:
//!
//! - `federation_revoked_jti` — one row per revoked capability `jti`. The issuing server
//!   publishes the active rows as its `/.well-known/capsule/revoked-jti` list and consults them
//!   when verifying its own tokens. A row is pruned once its `expires_at` passes, so the list
//!   stays bounded by at most 24 hours of revocations (SSoT: the [Federation design doc]).
//!
//! [Federation design doc]: ../../../capsule-docs/src/content/docs/design/federation.md

use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // The revoked-jti list. `jti` (UUIDv7) is the revocation key; `expires_at` bounds the
        // row's lifetime (pruned once it passes — an expired token is rejected unconditionally).
        manager
            .create_table(
                Table::create()
                    .table(FederationRevokedJti::Table)
                    .if_not_exists()
                    .col(string(FederationRevokedJti::Jti).primary_key())
                    .col(timestamp_with_time_zone(FederationRevokedJti::ExpiresAt))
                    .col(
                        timestamp_with_time_zone(FederationRevokedJti::RevokedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // The prune scan (delete rows whose exp has passed) and the active-list publish both
        // range over `expires_at`.
        manager
            .create_index(
                Index::create()
                    .name("idx_federation_revoked_jti_expires")
                    .table(FederationRevokedJti::Table)
                    .col(FederationRevokedJti::ExpiresAt)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(FederationRevokedJti::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum FederationRevokedJti {
    Table,
    Jti,
    ExpiresAt,
    RevokedAt,
}
