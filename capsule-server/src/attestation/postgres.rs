//! [`PostgresReceipts`] — the durable custody receipt log (`S-C15`, #446).
//!
//! # One row per receipt, holding the signed bytes
//!
//! `custody_receipts` keys each receipt on its chain position and stores the receipt as its
//! canonical CBOR — the exact bytes a client verifies and its successor's `prior_receipt_hash`
//! is taken over. Holding the signed form rather than reassembling it from columns means there
//! is no second encoding that could drift from what was signed. `upload_id` (unique) and
//! `asset_id` are projected out beside it for the two lookups the port makes.
//!
//! # Allocate, sign and append under one table lock
//!
//! [`ReceiptLog::issue`] is one operation because the position and the predecessor are inside
//! the signed core (see the module docs one level up). Here that is one transaction opened with
//! `LOCK TABLE custody_receipts IN SHARE ROW EXCLUSIVE MODE`: the mode conflicts with itself, so
//! two issuers take turns, and it does not conflict with plain reads, so `for_upload` and
//! `for_asset` are never blocked by an issue in flight. Under the lock the retry check, the head
//! read, the signature and the insert all see one chain.
//!
//! A table lock rather than `pg_advisory_xact_lock(hashtext(..))`: the advisory keyspace is the
//! whole database's and a collision would serialize this log against an unrelated caller; a
//! table lock is scoped to exactly the object it protects and needs no key at all. The cost is
//! that issuing is serial per server — which the chain already is by definition.
//!
//! # Append-only is enforced below the port as well
//!
//! The port has no method that replaces or removes a receipt. The migration adds the other half:
//! a trigger that refuses `UPDATE`, `DELETE` and `TRUNCATE` on the table, so a receipt is not
//! rewritable by anything that reaches the database without going through this adapter.

use capsule_core::crypto::receipts::CustodyReceipt;
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, QueryResult, Statement,
    TransactionTrait as _, Value,
};

use super::{ReceiptDraft, ReceiptLog, ReceiptSigner, chain_hash, sign_receipt};
use crate::postgres::error::Port;
use crate::store::{AssetId, StoreError, StoreFuture, UploadId};

/// Which port is speaking, for every error this adapter raises.
const PORT: Port = Port {
    store: "receipts",
    record: "CustodyReceipt",
};

/// The durable receipt log.
#[derive(Debug, Clone)]
pub struct PostgresReceipts {
    connection: DatabaseConnection,
}

impl PostgresReceipts {
    /// A log over `connection`.
    pub fn new(connection: DatabaseConnection) -> Self {
        Self { connection }
    }
}

/// Begin a transaction, or say why not.
async fn begin(connection: &DatabaseConnection) -> Result<DatabaseTransaction, StoreError> {
    connection
        .begin()
        .await
        .map_err(PORT.failing("opening a transaction"))
}

/// Decode one stored row, checking that the receipt sits at the position its key names.
///
/// A receipt whose signed `receipt_seq` disagrees with the row it is filed under is a chain
/// this adapter did not write, and is refused as corrupt rather than served.
fn receipt_from(row: &QueryResult, doing: &'static str) -> Result<CustodyReceipt, StoreError> {
    let failed = PORT.failing(doing);
    let seq: i64 = row.try_get("", "receipt_seq").map_err(&failed)?;
    let bytes: Vec<u8> = row.try_get("", "receipt").map_err(&failed)?;
    let receipt = CustodyReceipt::from_canonical_cbor(&bytes).map_err(|error| {
        PORT.undecodable(format!(
            "receipt {seq} is not a canonical receipt ({error})"
        ))
    })?;
    if i64::try_from(receipt.core.receipt_seq).ok() != Some(seq) {
        return Err(PORT.undecodable(format!(
            "the receipt filed at position {seq} is signed at position {}",
            receipt.core.receipt_seq
        )));
    }
    Ok(receipt)
}

/// The receipt already issued for `upload`, read through `connection`.
async fn issued_for<C: ConnectionTrait>(
    connection: &C,
    upload: &UploadId,
) -> Result<Option<CustodyReceipt>, StoreError> {
    let doing = "reading an upload's receipt";
    connection
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT receipt_seq, receipt FROM custody_receipts WHERE upload_id = $1",
            [Value::from(upload.as_str().to_owned())],
        ))
        .await
        .map_err(PORT.failing(doing))?
        .map(|row| receipt_from(&row, doing))
        .transpose()
}

impl ReceiptLog for PostgresReceipts {
    fn issue<'a>(
        &'a self,
        draft: ReceiptDraft,
        signer: &'a dyn ReceiptSigner,
    ) -> StoreFuture<'a, CustodyReceipt> {
        Box::pin(async move {
            let transaction = begin(&self.connection).await?;
            // The critical section starts here and ends with the transaction.
            transaction
                .execute(Statement::from_string(
                    DbBackend::Postgres,
                    "LOCK TABLE custody_receipts IN SHARE ROW EXCLUSIVE MODE",
                ))
                .await
                .map_err(PORT.failing("locking the receipt log"))?;

            // A retried finalization gets the receipt it already has, never a second one: two
            // signed statements about one custody event read as the server double-counting.
            if let Some(existing) = issued_for(&transaction, &draft.upload_id).await? {
                tracing::debug!(
                    upload_id = %draft.upload_id,
                    receipt_seq = existing.core.receipt_seq,
                    "a receipt was already issued for this upload; returning it"
                );
                return Ok(existing);
            }

            let doing = "reading the receipt chain's head";
            let head = transaction
                .query_one(Statement::from_string(
                    DbBackend::Postgres,
                    "SELECT receipt_seq, receipt FROM custody_receipts \
                     ORDER BY receipt_seq DESC LIMIT 1",
                ))
                .await
                .map_err(PORT.failing(doing))?
                .map(|row| receipt_from(&row, doing))
                .transpose()?;
            let receipt_seq = head
                .as_ref()
                .map_or(Some(1), |prior| prior.core.receipt_seq.checked_add(1))
                .ok_or_else(|| PORT.undecodable("the receipt chain is at u64::MAX".to_owned()))?;
            let column_seq = i64::try_from(receipt_seq).map_err(|_| StoreError::Rejected {
                store: PORT.store,
                detail: format!("receipt position {receipt_seq} is past what a BIGINT holds"),
            })?;
            let prior_receipt_hash = head.as_ref().map(chain_hash);

            let upload_id = draft.upload_id.clone();
            let receipt = sign_receipt(draft, signer, receipt_seq, prior_receipt_hash);
            transaction
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "INSERT INTO custody_receipts (receipt_seq, upload_id, asset_id, receipt) \
                     VALUES ($1, $2, $3, $4)",
                    [
                        Value::from(column_seq),
                        Value::from(upload_id.as_str().to_owned()),
                        Value::from(receipt.core.asset_id.clone()),
                        Value::from(receipt.to_canonical_cbor()),
                    ],
                ))
                .await
                .map_err(PORT.failing("appending a receipt"))?;
            transaction
                .commit()
                .await
                .map_err(PORT.failing("committing a receipt"))?;

            tracing::info!(
                upload_id = %upload_id,
                asset = %receipt.core.asset_id,
                receipt_seq,
                "issued a custody receipt"
            );
            Ok(receipt)
        })
    }

    fn for_upload<'a>(&'a self, upload: &'a UploadId) -> StoreFuture<'a, Option<CustodyReceipt>> {
        Box::pin(async move { issued_for(&self.connection, upload).await })
    }

    fn for_asset<'a>(&'a self, asset: &'a AssetId) -> StoreFuture<'a, Vec<CustodyReceipt>> {
        Box::pin(async move {
            let doing = "reading an asset's receipts";
            let rows = self
                .connection
                .query_all(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT receipt_seq, receipt FROM custody_receipts WHERE asset_id = $1 \
                     ORDER BY receipt_seq ASC",
                    [Value::from(asset.as_str().to_owned())],
                ))
                .await
                .map_err(PORT.failing(doing))?;
            rows.iter().map(|row| receipt_from(row, doing)).collect()
        })
    }
}

#[cfg(test)]
mod tests {
    /// The suite, against a real Postgres — plus the one property only the schema can show.
    mod postgres_conformance {
        use sea_orm::{ConnectionTrait as _, DbBackend, Statement};

        use super::super::PostgresReceipts;
        use crate::attestation::ReceiptLog;
        use crate::attestation::conformance::{self, Harness};
        use crate::postgres::testing;

        /// A log over one container.
        #[derive(Debug)]
        struct PostgresHarness {
            receipts: PostgresReceipts,
        }

        impl Harness for PostgresHarness {
            fn receipts(&self) -> &dyn ReceiptLog {
                &self.receipts
            }
        }

        #[tokio::test]
        async fn the_postgres_receipt_log_conforms() {
            let Some(database) = testing::start("the Postgres receipt log").await else {
                return;
            };
            let harness = PostgresHarness {
                receipts: PostgresReceipts::new(database.connection().clone()),
            };
            conformance::run_all(&harness).await;

            // Invariant 33's structural half: the table refuses a rewrite or a removal from
            // anything that reaches it without the port, which has no such method.
            let connection = database.connection();
            for statement in [
                "UPDATE custody_receipts SET asset_id = 'rewritten'",
                "DELETE FROM custody_receipts",
                "TRUNCATE custody_receipts",
            ] {
                let refused = connection
                    .execute(Statement::from_string(DbBackend::Postgres, statement))
                    .await;
                assert!(
                    refused.is_err(),
                    "the receipt log must refuse `{statement}`, and it was accepted"
                );
            }
            assert!(
                !harness
                    .receipts
                    .for_asset(&crate::store::AssetId::new("chain-asset"))
                    .await
                    .expect("the log answers")
                    .is_empty(),
                "a refused rewrite changed nothing"
            );
        }
    }
}
