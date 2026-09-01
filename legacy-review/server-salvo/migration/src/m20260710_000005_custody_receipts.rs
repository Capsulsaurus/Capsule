//! The append-only custody-receipt log + its per-server sequence counter (slice `S-C15`).
//!
//! Three pieces:
//! - `custody_receipt_seq` — the per-`server_id` monotonic counter. Minting the next
//!   `receipt_seq` is an atomic `INSERT … ON CONFLICT DO UPDATE … RETURNING` that takes the
//!   server's row lock, so concurrent finalizations are linearised and the sequence is
//!   strictly increasing with no gaps (receipt-log monotonicity, invariant 33).
//! - `custody_receipts` — the append-only receipt log, one row per signed receipt, minted
//!   inside the upload finalization transaction.
//! - An **append-only trigger** that raises on every `UPDATE`/`DELETE` of a receipt row, so
//!   "no API path overwrites or deletes an existing receipt" is enforced structurally in the
//!   database, not merely by convention (invariant 33).

use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Per-server monotonic receipt counter.
        manager
            .create_table(
                Table::create()
                    .table(CustodyReceiptSeq::Table)
                    .if_not_exists()
                    .col(string(CustodyReceiptSeq::ServerId).primary_key())
                    .col(big_integer(CustodyReceiptSeq::LastSeq).default(0))
                    .to_owned(),
            )
            .await?;

        // The append-only receipt log. PK (server_id, receipt_seq) is the chain key.
        manager
            .create_table(
                Table::create()
                    .table(CustodyReceipts::Table)
                    .if_not_exists()
                    .col(string(CustodyReceipts::ServerId))
                    .col(big_integer(CustodyReceipts::ReceiptSeq))
                    .col(string_len_null(CustodyReceipts::PriorReceiptHash, 64))
                    .col(string_len(CustodyReceipts::ReceiptHash, 64))
                    .col(string(CustodyReceipts::UploadId))
                    .col(string(CustodyReceipts::AssetId))
                    .col(string(CustodyReceipts::BlobRole))
                    .col(string_len(CustodyReceipts::CiphertextHash, 64))
                    .col(big_integer(CustodyReceipts::Size))
                    .col(string_len_null(CustodyReceipts::EnvelopeHash, 64))
                    .col(string(CustodyReceipts::UploadedByUser))
                    .col(string_null(CustodyReceipts::UploadedByDevice))
                    .col(string_len(CustodyReceipts::ServerKeyId, 64))
                    .col(timestamp_with_time_zone(CustodyReceipts::ReceivedAt))
                    .col(
                        ColumnDef::new(CustodyReceipts::ReceiptCbor)
                            .binary()
                            .not_null(),
                    )
                    .col(
                        timestamp_with_time_zone(CustodyReceipts::CreatedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .primary_key(
                        Index::create()
                            .col(CustodyReceipts::ServerId)
                            .col(CustodyReceipts::ReceiptSeq),
                    )
                    .to_owned(),
            )
            .await?;

        // Durable lookup by session (lost-ACK recovery) and by asset (the owner/uploader fetch).
        manager
            .create_index(
                Index::create()
                    .name("idx_custody_receipts_upload")
                    .table(CustodyReceipts::Table)
                    .col(CustodyReceipts::UploadId)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_custody_receipts_asset")
                    .table(CustodyReceipts::Table)
                    .col(CustodyReceipts::AssetId)
                    .to_owned(),
            )
            .await?;

        // Structural append-only enforcement: reject every UPDATE/DELETE on a receipt row.
        let db = manager.get_connection();
        db.execute_unprepared(
            r"CREATE OR REPLACE FUNCTION capsule_custody_receipts_append_only()
              RETURNS trigger AS $$
              BEGIN
                RAISE EXCEPTION 'custody_receipts is append-only: % rejected', TG_OP
                  USING ERRCODE = 'restrict_violation';
              END;
              $$ LANGUAGE plpgsql;",
        )
        .await?;
        db.execute_unprepared(
            r"CREATE TRIGGER custody_receipts_append_only
              BEFORE UPDATE OR DELETE ON custody_receipts
              FOR EACH ROW EXECUTE FUNCTION capsule_custody_receipts_append_only();",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "DROP TRIGGER IF EXISTS custody_receipts_append_only ON custody_receipts;",
        )
        .await?;
        db.execute_unprepared("DROP FUNCTION IF EXISTS capsule_custody_receipts_append_only();")
            .await?;
        manager
            .drop_table(Table::drop().table(CustodyReceipts::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(CustodyReceiptSeq::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum CustodyReceiptSeq {
    Table,
    ServerId,
    LastSeq,
}

#[derive(DeriveIden)]
enum CustodyReceipts {
    Table,
    ServerId,
    ReceiptSeq,
    PriorReceiptHash,
    ReceiptHash,
    UploadId,
    AssetId,
    BlobRole,
    CiphertextHash,
    Size,
    EnvelopeHash,
    UploadedByUser,
    UploadedByDevice,
    ServerKeyId,
    ReceivedAt,
    ReceiptCbor,
    CreatedAt,
}
