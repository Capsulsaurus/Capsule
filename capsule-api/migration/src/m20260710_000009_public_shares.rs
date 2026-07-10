//! The public share-link serving schema (slice `S-C4`).
//!
//! One table backs the key-free `/s/{opaque-id}` serve path (SSoT: the Share Links design doc):
//!
//! - `public_shares` — one row per issued share link. `opaque_id` is the random **128-bit**
//!   URL-path token (unique), resolved on every serve; `wrapped_scope` is the issuer-published
//!   encapsulated scope material, served opaquely; `served_metadata` carries the per-asset
//!   metadata the serve path strips (no opt-out); `home_server` pins the single home server;
//!   `revoked_at`/`expires_at` are the fail-closed revocation + expiry, resolving a
//!   not-found/revoked/expired link to an indistinguishable `404`.
//!
//! Distinct from the frozen plaintext-era `share_links` table (the initial schema's
//! `token`/`password_hash`/`view_count` model), which this design supersedes.

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
                    .table(PublicShares::Table)
                    .if_not_exists()
                    // Revocation handle (owner-held; never URL-exposed).
                    .col(string(PublicShares::LinkId).primary_key())
                    // The random 128-bit opaque URL-path token (hex of 16 bytes = 32 chars).
                    .col(string_len(PublicShares::OpaqueId, 32))
                    .col(string(PublicShares::OwnerId))
                    // The album's single home server; only it serves the share.
                    .col(string(PublicShares::HomeServer))
                    .col(string_len(PublicShares::ScopeKind, 15))
                    .col(text(PublicShares::ScopeId))
                    // The issuer-published WrappedScope (canonical CBOR, base64); served opaquely.
                    .col(text(PublicShares::WrappedScope))
                    .col(boolean(PublicShares::PassphraseProtected).default(false))
                    // Per-asset served metadata (content address + strip-on-serve sidecar).
                    .col(json_binary(PublicShares::ServedMetadata))
                    .col(timestamp_with_time_zone_null(PublicShares::ExpiresAt))
                    // Fail-closed revocation flag.
                    .col(timestamp_with_time_zone_null(PublicShares::RevokedAt))
                    .col(
                        timestamp_with_time_zone(PublicShares::CreatedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // The opaque-id is the URL lookup key; unique and indistinguishable-404 fast.
        manager
            .create_index(
                Index::create()
                    .name("uq_public_shares_opaque")
                    .table(PublicShares::Table)
                    .col(PublicShares::OpaqueId)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // Owner-scoped listing / revocation scan.
        manager
            .create_index(
                Index::create()
                    .name("idx_public_shares_owner")
                    .table(PublicShares::Table)
                    .col(PublicShares::OwnerId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(PublicShares::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum PublicShares {
    Table,
    LinkId,
    OpaqueId,
    OwnerId,
    HomeServer,
    ScopeKind,
    ScopeId,
    WrappedScope,
    PassphraseProtected,
    ServedMetadata,
    ExpiresAt,
    RevokedAt,
    CreatedAt,
}
