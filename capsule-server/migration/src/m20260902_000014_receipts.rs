//! The custody receipt log: the server's signed, append-only chain of what it accepted.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub(crate) struct Migration;

/// Refuses every `UPDATE`, `DELETE` and `TRUNCATE` on the log.
///
/// Invariant 33 asks that an overwrite or delete attempt be "rejected at the structural layer".
/// The port already has no method that could do either; this is the same property for anything
/// that reaches the table without going through the port — an operator's `psql`, a botched
/// script. Dropping the table (the `down` below) is not refused: that is a schema act, and it
/// is the one an operator has to mean.
const APPEND_ONLY: &str = "
CREATE OR REPLACE FUNCTION custody_receipts_append_only() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'custody_receipts is append-only: % is refused', TG_OP;
END;
$$;
CREATE TRIGGER custody_receipts_no_update_or_delete
    BEFORE UPDATE OR DELETE ON custody_receipts
    FOR EACH ROW EXECUTE FUNCTION custody_receipts_append_only();
CREATE TRIGGER custody_receipts_no_truncate
    BEFORE TRUNCATE ON custody_receipts
    FOR EACH STATEMENT EXECUTE FUNCTION custody_receipts_append_only();
";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // One row per receipt, keyed on its chain position. The receipt itself is held as its
        // canonical CBOR — the exact bytes a client verifies and the predecessor hash is taken
        // over — so the stored form *is* the signed form and there is no column-by-column
        // reassembly that could drift from what was signed. `upload_id` and `asset_id` are
        // projected out beside it for the two lookups the port makes.
        manager
            .create_table(
                Table::create()
                    .table(CustodyReceipts::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(CustodyReceipts::ReceiptSeq)
                            .big_integer()
                            .not_null()
                            .primary_key(),
                    )
                    // Unique: one custody event, one receipt. The adapter answers a retried
                    // finalization with the receipt already issued; this is the backstop.
                    .col(
                        ColumnDef::new(CustodyReceipts::UploadId)
                            .text()
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(CustodyReceipts::AssetId).text().not_null())
                    .col(ColumnDef::new(CustodyReceipts::Receipt).binary().not_null())
                    .to_owned(),
            )
            .await?;
        // "Every receipt for this asset, in chain order" — the primary key does not answer it.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_custody_receipts_asset")
                    .table(CustodyReceipts::Table)
                    .col(CustodyReceipts::AssetId)
                    .col(CustodyReceipts::ReceiptSeq)
                    .to_owned(),
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(APPEND_ONLY)
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(CustodyReceipts::Table).to_owned())
            .await?;
        manager
            .get_connection()
            .execute_unprepared("DROP FUNCTION IF EXISTS custody_receipts_append_only()")
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum CustodyReceipts {
    Table,
    ReceiptSeq,
    UploadId,
    AssetId,
    Receipt,
}
