//! The quota service schema (slice `S-C6`).
//!
//! Two tables (originals are accounted from the existing `assets` index, so they need no
//! table here):
//!
//! - `quota_ledger` — one row per distinct **auxiliary or federated** blob content address
//!   (metadata / derivative / provenance / federated cache). `content_hash` is the primary
//!   key, giving global content-addressed dedup: charging a hash already present is a merge
//!   (`refcount += 1`), never a second debit. `source_peer` marks a federated cache so a
//!   per-`(attributed_user, source_peer)` budget can be summed; `refcount` reaching zero
//!   garbage-collects the row and credits the bytes back.
//! - `user_quota` — one row per user holding `hard_exceeded_since` (the grace-window clock)
//!   and the moderation `suspended` flag.
//!
//! SSoT for the accounting model: the Quota design doc.

use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // The auxiliary + federated blob ledger.
        manager
            .create_table(
                Table::create()
                    .table(QuotaLedger::Table)
                    .if_not_exists()
                    // The global content address is the row key (dedup by content hash).
                    .col(string_len(QuotaLedger::ContentHash, 64).primary_key())
                    .col(string(QuotaLedger::AttributedUserId))
                    .col(big_integer(QuotaLedger::ByteSize))
                    .col(string_len(QuotaLedger::BlobKind, 16))
                    // NULL = locally produced; set = federated cache from this peer.
                    .col(string_null(QuotaLedger::SourcePeer))
                    .col(integer(QuotaLedger::Refcount).default(1))
                    .col(
                        timestamp_with_time_zone(QuotaLedger::CreatedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // Accounting scan: sum a user's charged bytes.
        manager
            .create_index(
                Index::create()
                    .name("idx_quota_ledger_user")
                    .table(QuotaLedger::Table)
                    .col(QuotaLedger::AttributedUserId)
                    .to_owned(),
            )
            .await?;

        // Per-peer caching budget scan: sum a user's federated bytes from one peer.
        manager
            .create_index(
                Index::create()
                    .name("idx_quota_ledger_user_peer")
                    .table(QuotaLedger::Table)
                    .col(QuotaLedger::AttributedUserId)
                    .col(QuotaLedger::SourcePeer)
                    .to_owned(),
            )
            .await?;

        // Per-user lifecycle state (grace clock + suspension flag).
        manager
            .create_table(
                Table::create()
                    .table(UserQuota::Table)
                    .if_not_exists()
                    .col(string(UserQuota::UserId).primary_key())
                    .col(timestamp_with_time_zone_null(UserQuota::HardExceededSince))
                    .col(boolean(UserQuota::Suspended).default(false))
                    .col(
                        timestamp_with_time_zone(UserQuota::UpdatedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(UserQuota::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(QuotaLedger::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum QuotaLedger {
    Table,
    ContentHash,
    AttributedUserId,
    ByteSize,
    BlobKind,
    SourcePeer,
    Refcount,
    CreatedAt,
}

#[derive(DeriveIden)]
enum UserQuota {
    Table,
    UserId,
    HardExceededSince,
    Suspended,
    UpdatedAt,
}
