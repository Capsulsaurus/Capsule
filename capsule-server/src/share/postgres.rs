//! [`PostgresShares`] — the durable share-link store (#446).
//!
//! # Two tables, one link
//!
//! `shares` holds one row per link and `share_serves` one row per address the link may serve.
//! The serve set is rows rather than an array column so the enumeration the privacy strip leans
//! on is a primary key, not a value somebody decodes: a link serves what its rows name and
//! nothing else.
//!
//! # `issue` is one transaction, `resolve` one statement, and a held id is refused
//!
//! The link row and its serve set are written together, and `resolve` reads them back in one
//! joined `SELECT` that sees one snapshot, so no reader ever resolves a link whose serve set is
//! half-written. The row is written `ON CONFLICT (opaque_id) DO NOTHING`, and a row count of
//! zero is [`IssueOutcome::Conflict`]: the transaction is dropped before the serve set is
//! touched, so the held link keeps its owner and every address it enumerated (#553). The
//! primary key is the check, so two concurrent issues of one id cannot both be recorded.
//!
//! # `revoke` is one statement
//!
//! `UPDATE … WHERE owner_id = $2 AND revoked_at IS NULL`, whose row count is the answer. The
//! owner check and the write cannot be separated, so two concurrent revocations cannot race
//! into one being lost — which for a revocation is the direction that keeps a link alive.

use std::collections::BTreeSet;

use capsule_core::sharing::ShareScope;
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, Statement,
    TransactionTrait, Value,
};
use uuid::Uuid;

use super::{IssueOutcome, ShareRecord, ShareStore};
use crate::blob::ContentAddress;
use crate::postgres::error::Port;
use crate::postgres::time::{from_micros, to_micros};
use crate::store::{StoreError, StoreFuture, UserId};

/// Which port is speaking, for every error this adapter raises.
const PORT: Port = Port {
    store: "share",
    record: "ShareRecord",
};

/// The durable share-link store.
#[derive(Debug, Clone)]
pub struct PostgresShares {
    connection: DatabaseConnection,
}

impl PostgresShares {
    /// A store over `connection`.
    pub fn new(connection: DatabaseConnection) -> Self {
        Self { connection }
    }
}

/// A scope as its two columns.
fn scope_columns(scope: &ShareScope) -> (&'static str, String) {
    match scope {
        ShareScope::Asset(id) => ("asset", id.to_string()),
        ShareScope::Album(id) => ("album", id.to_string()),
    }
}

/// The scope two columns name.
fn scope_from(kind: &str, id: &str) -> Result<ShareScope, StoreError> {
    let id = Uuid::parse_str(id)
        .map_err(|_| PORT.undecodable(format!("`{id}` is not a share scope id")))?;
    match kind {
        "asset" => Ok(ShareScope::Asset(id)),
        "album" => Ok(ShareScope::Album(id)),
        other => Err(PORT.undecodable(format!("`{other}` is not a share scope kind"))),
    }
}

/// An optional instant as its column.
fn optional_micros(at: Option<jiff::Timestamp>) -> Value {
    Value::BigInt(at.map(to_micros))
}

/// An optional column as an instant.
fn optional_instant(micros: Option<i64>) -> Result<Option<jiff::Timestamp>, StoreError> {
    micros
        .map(|micros| {
            from_micros(micros).ok_or_else(|| {
                PORT.undecodable(format!("{micros}µs is not a representable instant"))
            })
        })
        .transpose()
}

/// A content address as a column holds it.
fn address_from(raw: &str) -> Result<ContentAddress, StoreError> {
    ContentAddress::parse(raw)
        .map_err(|_| PORT.undecodable(format!("`{raw}` is not a content address")))
}

/// Begin a transaction, or say why not.
async fn begin(connection: &DatabaseConnection) -> Result<DatabaseTransaction, StoreError> {
    connection
        .begin()
        .await
        .map_err(PORT.failing("opening a transaction"))
}

impl ShareStore for PostgresShares {
    fn issue(&self, record: ShareRecord) -> StoreFuture<'_, IssueOutcome> {
        Box::pin(async move {
            let (scope_kind, scope_id) = scope_columns(&record.scope);
            let transaction = begin(&self.connection).await?;
            let inserted = transaction
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "INSERT INTO shares \
                       (opaque_id, owner_id, scope_kind, scope_id, metadata, wrapped_secret, \
                        expires_at, revoked_at) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
                     ON CONFLICT (opaque_id) DO NOTHING",
                    [
                        Value::from(record.opaque_id.clone()),
                        Value::from(record.owner_id.as_str().to_owned()),
                        Value::from(scope_kind.to_owned()),
                        Value::from(scope_id),
                        Value::from(record.metadata.as_str().to_owned()),
                        Value::Bytes(record.wrapped_secret.clone()),
                        optional_micros(record.expires_at),
                        optional_micros(record.revoked_at),
                    ],
                ))
                .await
                .map_err(PORT.failing("recording a share link"))?;
            if inserted.rows_affected() == 0 {
                // Dropping the transaction rolls it back; nothing was written.
                drop(transaction);
                tracing::info!(
                    owner = %record.owner_id,
                    "a share link was refused: its opaque id is already held"
                );
                return Ok(IssueOutcome::Conflict);
            }
            // The row is new, so the serve set is too: written inside the same transaction.
            let serves: Vec<String> = record
                .serves
                .iter()
                .map(|address| address.as_str().to_owned())
                .collect();
            transaction
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "INSERT INTO share_serves (opaque_id, address) \
                     SELECT $1, address FROM unnest($2::text[]) AS address",
                    [Value::from(record.opaque_id.clone()), Value::from(serves)],
                ))
                .await
                .map_err(PORT.failing("recording a share link's serve set"))?;
            transaction
                .commit()
                .await
                .map_err(PORT.failing("committing a share link"))?;
            tracing::info!(
                owner = %record.owner_id,
                serves = record.serves.len(),
                "a share link was issued"
            );
            Ok(IssueOutcome::Issued)
        })
    }

    fn resolve<'a>(&'a self, opaque_id: &'a str) -> StoreFuture<'a, Option<ShareRecord>> {
        Box::pin(async move {
            // One statement, so the row and its serve set come from one snapshot: a concurrent
            // issue cannot hand back a row whose serve set is not yet committed. The
            // join yields one row per served address, one row with a NULL address for a link
            // that serves nothing, and no row for an unknown id.
            let rows = self
                .connection
                .query_all_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT s.owner_id, s.scope_kind, s.scope_id, s.metadata, s.wrapped_secret, \
                            s.expires_at, s.revoked_at, v.address \
                     FROM shares s \
                     LEFT JOIN share_serves v ON v.opaque_id = s.opaque_id \
                     WHERE s.opaque_id = $1 \
                     ORDER BY v.address COLLATE \"C\"",
                    [Value::from(opaque_id.to_owned())],
                ))
                .await
                .map_err(PORT.failing("resolving a share link"))?;
            let Some(row) = rows.first() else {
                return Ok(None);
            };
            let failed = PORT.failing("resolving a share link");
            let owner_id: String = row.try_get("", "owner_id").map_err(&failed)?;
            let scope_kind: String = row.try_get("", "scope_kind").map_err(&failed)?;
            let scope_id: String = row.try_get("", "scope_id").map_err(&failed)?;
            let metadata: String = row.try_get("", "metadata").map_err(&failed)?;
            let wrapped_secret: Option<Vec<u8>> =
                row.try_get("", "wrapped_secret").map_err(&failed)?;
            let expires_at: Option<i64> = row.try_get("", "expires_at").map_err(&failed)?;
            let revoked_at: Option<i64> = row.try_get("", "revoked_at").map_err(&failed)?;

            let mut serves = BTreeSet::new();
            for row in &rows {
                let address: Option<String> = row.try_get("", "address").map_err(&failed)?;
                if let Some(address) = address {
                    serves.insert(address_from(&address)?);
                }
            }

            Ok(Some(ShareRecord {
                opaque_id: opaque_id.to_owned(),
                owner_id: UserId::new(owner_id),
                scope: scope_from(&scope_kind, &scope_id)?,
                serves,
                metadata: address_from(&metadata)?,
                wrapped_secret,
                expires_at: optional_instant(expires_at)?,
                revoked_at: optional_instant(revoked_at)?,
            }))
        })
    }

    fn revoke<'a>(
        &'a self,
        owner: &'a UserId,
        opaque_id: &'a str,
        at: jiff::Timestamp,
    ) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let result = self
                .connection
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "UPDATE shares SET revoked_at = $3 \
                     WHERE opaque_id = $1 AND owner_id = $2 AND revoked_at IS NULL",
                    [
                        Value::from(opaque_id.to_owned()),
                        Value::from(owner.as_str().to_owned()),
                        Value::from(to_micros(at)),
                    ],
                ))
                .await
                .map_err(PORT.failing("revoking a share link"))?;
            let revoked = result.rows_affected() == 1;
            if revoked {
                tracing::info!(%owner, "a share link was revoked");
            }
            Ok(revoked)
        })
    }

    fn names_live<'a>(
        &'a self,
        address: &'a ContentAddress,
        now: jiff::Timestamp,
    ) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            // The liveness rule is `ShareRecord::is_live_at`'s, spelled in SQL: not revoked, and
            // either no expiry or one strictly after `now`.
            let row = self
                .connection
                .query_one_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT EXISTS ( \
                       SELECT 1 FROM share_serves v \
                       JOIN shares s ON s.opaque_id = v.opaque_id \
                       WHERE v.address = $1 \
                         AND s.revoked_at IS NULL \
                         AND (s.expires_at IS NULL OR s.expires_at > $2) \
                     ) AS named",
                    [
                        Value::from(address.as_str().to_owned()),
                        Value::from(to_micros(now)),
                    ],
                ))
                .await
                .map_err(PORT.failing("asking whether a live link names an address"))?
                .ok_or_else(|| PORT.undecodable("an EXISTS query returned no row".to_owned()))?;
            row.try_get("", "named")
                .map_err(PORT.failing("asking whether a live link names an address"))
        })
    }
}

#[cfg(test)]
mod tests {
    /// The suite, against a real Postgres.
    mod postgres_conformance {
        use super::super::PostgresShares;
        use crate::postgres::testing;
        use crate::share::ShareStore;
        use crate::share::conformance::{self, Harness};

        /// A store over one container.
        #[derive(Debug)]
        struct PostgresHarness {
            shares: PostgresShares,
        }

        impl Harness for PostgresHarness {
            fn shares(&self) -> &dyn ShareStore {
                &self.shares
            }
        }

        #[tokio::test]
        async fn the_postgres_share_store_conforms() {
            let Some(database) = testing::start("the Postgres share store").await else {
                return;
            };
            let harness = PostgresHarness {
                shares: PostgresShares::new(database.connection().clone()),
            };
            conformance::run_all(&harness).await;
        }
    }
}
