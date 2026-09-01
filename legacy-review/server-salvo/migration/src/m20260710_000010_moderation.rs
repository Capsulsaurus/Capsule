//! The moderation-hooks schema (slice `S-C8`).
//!
//! Capsule is end-to-end encrypted, so moderation never touches content — it operates on
//! account-level signals, user reports, and federated peer reputation (SSoT: the
//! [Moderation design doc]). This migration lands the durable state the four hooks read and
//! write:
//!
//! - `federation_peers` — the Ed25519 signing key each known peer server publishes. A
//!   federated report is verified against the row for its `reporting_server` before it ever
//!   reaches the admin queue; an unknown peer cannot be verified, so its report is dropped
//!   (invariant 24 — signed intake).
//! - `moderation_reports` — the admin queue. One row per accepted federated report, carrying
//!   the alleged asset's **content hash + album pointer only** (never plaintext or key
//!   material). The `(reporting_server, reported_user, received_at)` index backs the
//!   per-pair rate budget (invariant 24 — rate-limited intake).
//! - `server_blocklist` — peer servers this server refuses federated requests from
//!   (federation-capability layer). A blocked peer cannot pull and cannot report.
//! - `user_blocks` — the per-user block ledger. Scoped to the blocked user (the blocker's
//!   home server removes them from shared albums); deliberately **not** a server-wide
//!   federation block, so blocks cannot be weaponized to sever a peer.
//! - `moderation_events` — the server-visible **moderation provenance record** the user sees
//!   in their audit log ("[No silent operations]"): every takedown / suspension the server
//!   applies to a user's account or asset is appended here, queryable by the user.
//! - `assets.served` — the takedown serving flag. `true` (default) = servable; a takedown
//!   flips it `false` and federated fetches then return `410 Gone`. The blob is **never**
//!   deleted — takedown is a serving constraint, not destruction.
//!
//! [Moderation design doc]: ../../../capsule-docs/src/content/docs/design/moderation.md
//! [No silent operations]: ../../../capsule-docs/src/content/docs/design/moderation.md

use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Known federated peers and their published Ed25519 signing keys (32 bytes). The
        // report intake verifies a report's signature against the row for its
        // `reporting_server`; a missing row means the peer is unknown and the report is
        // unverifiable (dropped).
        manager
            .create_table(
                Table::create()
                    .table(FederationPeers::Table)
                    .if_not_exists()
                    .col(string(FederationPeers::ServerId).primary_key())
                    .col(
                        ColumnDef::new(FederationPeers::Ed25519PublicKey)
                            .binary()
                            .not_null(),
                    )
                    .col(
                        timestamp_with_time_zone(FederationPeers::CreatedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // The federated-report admin queue.
        manager
            .create_table(
                Table::create()
                    .table(ModerationReports::Table)
                    .if_not_exists()
                    // UUIDv7 (time-ordered) report id.
                    .col(string(ModerationReports::Id).primary_key())
                    .col(string(ModerationReports::ReportingServer))
                    .col(string(ModerationReports::ReportedUser))
                    // The alleged asset's content hash (64-char lowercase hex) — never plaintext.
                    .col(string_len(ModerationReports::ContentHash, 64))
                    // Optional album pointer locating the asset for an admin with album access.
                    .col(string_null(ModerationReports::AlbumPointer))
                    .col(string_null(ModerationReports::Reason))
                    .col(timestamp_with_time_zone(ModerationReports::ReceivedAt))
                    .to_owned(),
            )
            .await?;

        // Rate-budget scan: count a peer's recent reports against one user.
        manager
            .create_index(
                Index::create()
                    .name("idx_moderation_reports_pair")
                    .table(ModerationReports::Table)
                    .col(ModerationReports::ReportingServer)
                    .col(ModerationReports::ReportedUser)
                    .col(ModerationReports::ReceivedAt)
                    .to_owned(),
            )
            .await?;

        // Server-level blocklist (peer servers whose federated requests are refused).
        manager
            .create_table(
                Table::create()
                    .table(ServerBlocklist::Table)
                    .if_not_exists()
                    .col(string(ServerBlocklist::ServerId).primary_key())
                    .col(string_null(ServerBlocklist::Reason))
                    .col(
                        timestamp_with_time_zone(ServerBlocklist::BlockedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // Per-user block ledger (blocker → blocked). Composite key so a block is idempotent.
        manager
            .create_table(
                Table::create()
                    .table(UserBlocks::Table)
                    .if_not_exists()
                    .col(string(UserBlocks::BlockerId))
                    .col(string(UserBlocks::BlockedId))
                    .col(
                        timestamp_with_time_zone(UserBlocks::CreatedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .primary_key(
                        Index::create()
                            .col(UserBlocks::BlockerId)
                            .col(UserBlocks::BlockedId),
                    )
                    .to_owned(),
            )
            .await?;

        // The user-visible moderation audit log (append-only).
        manager
            .create_table(
                Table::create()
                    .table(ModerationEvents::Table)
                    .if_not_exists()
                    .col(string(ModerationEvents::Id).primary_key())
                    .col(string(ModerationEvents::UserId))
                    // The affected asset for a takedown; NULL for account-level events.
                    .col(string_null(ModerationEvents::AssetId))
                    // `takedown | takedown_lifted | legal_hold | suspended | unsuspended`.
                    .col(string_len(ModerationEvents::Kind, 32))
                    .col(string_null(ModerationEvents::Reason))
                    .col(timestamp_with_time_zone(ModerationEvents::CreatedAt))
                    .to_owned(),
            )
            .await?;

        // The user's audit-log scan (newest first).
        manager
            .create_index(
                Index::create()
                    .name("idx_moderation_events_user")
                    .table(ModerationEvents::Table)
                    .col(ModerationEvents::UserId)
                    .col(ModerationEvents::CreatedAt)
                    .to_owned(),
            )
            .await?;

        // The takedown serving flag on assets (default: servable).
        manager
            .alter_table(
                Table::alter()
                    .table(Assets::Table)
                    .add_column(boolean(Assets::Served).default(true))
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Assets::Table)
                    .drop_column(Assets::Served)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(ModerationEvents::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(UserBlocks::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(ServerBlocklist::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(ModerationReports::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(FederationPeers::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum FederationPeers {
    Table,
    ServerId,
    Ed25519PublicKey,
    CreatedAt,
}

#[derive(DeriveIden)]
enum ModerationReports {
    Table,
    Id,
    ReportingServer,
    ReportedUser,
    ContentHash,
    AlbumPointer,
    Reason,
    ReceivedAt,
}

#[derive(DeriveIden)]
enum ServerBlocklist {
    Table,
    ServerId,
    Reason,
    BlockedAt,
}

#[derive(DeriveIden)]
enum UserBlocks {
    Table,
    BlockerId,
    BlockedId,
    CreatedAt,
}

#[derive(DeriveIden)]
enum ModerationEvents {
    Table,
    Id,
    UserId,
    AssetId,
    Kind,
    Reason,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Assets {
    Table,
    Served,
}
