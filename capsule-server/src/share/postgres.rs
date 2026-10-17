//! [`PostgresShares`] — the durable share-link store (`S-C4`, #446).
//!
//! # Two tables, one link
//!
//! `shares` holds one row per link and `share_serves` one row per address the link may serve.
//! The serve set is rows rather than an array column so the enumeration the privacy strip leans
//! on is a primary key, not a value somebody decodes: a link serves what its rows name and
//! nothing else.
//!
//! # `issue` is one transaction, and a re-issue replaces
//!
//! The link row and its serve set are written together, so no reader ever resolves a link whose
//! serve set is half-written. Issuing an opaque id that is already held **replaces** the record,
//! exactly as the in-memory adapter's map insert does — the conformance suite holds both
//! adapters to one behaviour, and the port does not yet say otherwise.
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

use super::{ShareRecord, ShareStore};
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
    fn issue(&self, record: ShareRecord) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let (scope_kind, scope_id) = scope_columns(&record.scope);
            let transaction = begin(&self.connection).await?;
            transaction
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "INSERT INTO shares \
                       (opaque_id, owner_id, scope_kind, scope_id, metadata, wrapped_secret, \
                        expires_at, revoked_at) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
                     ON CONFLICT (opaque_id) DO UPDATE SET \
                       owner_id = EXCLUDED.owner_id, \
                       scope_kind = EXCLUDED.scope_kind, \
                       scope_id = EXCLUDED.scope_id, \
                       metadata = EXCLUDED.metadata, \
                       wrapped_secret = EXCLUDED.wrapped_secret, \
                       expires_at = EXCLUDED.expires_at, \
                       revoked_at = EXCLUDED.revoked_at",
                    [
                        Value::from(record.opaque_id.clone()),
                        Value::from(record.owner_id.as_str().to_owned()),
                        Value::from(scope_kind.to_owned()),
                        Value::from(scope_id),
                        Value::from(record.metadata.as_str().to_owned()),
                        Value::Bytes(record.wrapped_secret.clone().map(Box::new)),
                        optional_micros(record.expires_at),
                        optional_micros(record.revoked_at),
                    ],
                ))
                .await
                .map_err(PORT.failing("recording a share link"))?;
            // The serve set is replaced whole, inside the same transaction as the row.
            transaction
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "DELETE FROM share_serves WHERE opaque_id = $1",
                    [Value::from(record.opaque_id.clone())],
                ))
                .await
                .map_err(PORT.failing("clearing a share link's serve set"))?;
            let serves: Vec<String> = record
                .serves
                .iter()
                .map(|address| address.as_str().to_owned())
                .collect();
            transaction
                .execute(Statement::from_sql_and_values(
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
            Ok(())
        })
    }

    fn resolve<'a>(&'a self, opaque_id: &'a str) -> StoreFuture<'a, Option<ShareRecord>> {
        Box::pin(async move {
            let Some(row) = self
                .connection
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT owner_id, scope_kind, scope_id, metadata, wrapped_secret, \
                            expires_at, revoked_at \
                     FROM shares WHERE opaque_id = $1",
                    [Value::from(opaque_id.to_owned())],
                ))
                .await
                .map_err(PORT.failing("resolving a share link"))?
            else {
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

            let rows = self
                .connection
                .query_all(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT address FROM share_serves WHERE opaque_id = $1 \
                     ORDER BY address COLLATE \"C\"",
                    [Value::from(opaque_id.to_owned())],
                ))
                .await
                .map_err(PORT.failing("reading a share link's serve set"))?;
            let mut serves = BTreeSet::new();
            for row in &rows {
                let address: String = row.try_get("", "address").map_err(&failed)?;
                serves.insert(address_from(&address)?);
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
                .execute(Statement::from_sql_and_values(
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
