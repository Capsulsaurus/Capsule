//! [`PostgresCollection`] — the durable mark store the collector writes (`S-C11`, #446).
//!
//! # Why the marks have to survive a restart at all
//!
//! [`crate::gc::collect`] marks a blob on one pass and sweeps it on a later one, once the grace
//! window has passed. Over a store that forgets, every process starts with no marks, so a
//! collector run from cron — one process per pass — can only ever mark and never sweep. One
//! table, one row per marked address, is the whole of what fixes that.
//!
//! # Every operation is one statement
//!
//! Each method is a single statement whose row count or `RETURNING` value decides the answer,
//! so none of them has a read-then-write window: `mark` is an upsert, `unmark` a `DELETE`, and
//! the two reads are plain `SELECT`s. Nothing here needs a transaction.
//!
//! # Re-marking re-stamps, as the in-memory store does
//!
//! [`CollectionStore::mark`] on an address that is already marked answers `false` and moves the
//! mark to the new instant. The collector never does that — it marks only what
//! [`CollectionStore::marked_since`] said was unmarked — so the case is a caller outside the
//! collector, and the later instant is the one that delays the sweep. Delaying a sweep keeps
//! bytes longer, which is the direction `gc`'s module docs say a mistake here must lean.
//!
//! # Addresses order by their bytes
//!
//! [`CollectionStore::marks`] is "in address order", which for the in-memory store is a
//! `BTreeMap` over the hex — byte order. The query pins `COLLATE "C"` so a database whose
//! default collation is a locale's cannot reorder it. Lowercase hex sorts identically under
//! every collation in practice; pinning it is what makes that a guarantee rather than a hope.

use jiff::Timestamp;
use sea_orm::{ConnectionTrait as _, DatabaseConnection, DbBackend, Statement, Value};

use super::CollectionStore;
use crate::blob::ContentAddress;
use crate::postgres::error::Port;
use crate::postgres::time::{from_micros, to_micros};
use crate::store::{StoreError, StoreFuture};

/// Which port is speaking, for every error this adapter raises.
const PORT: Port = Port {
    store: "collection",
    record: "CollectionMark",
};

/// The durable mark store.
#[derive(Debug, Clone)]
pub struct PostgresCollection {
    connection: DatabaseConnection,
}

impl PostgresCollection {
    /// A store over `connection`.
    pub fn new(connection: DatabaseConnection) -> Self {
        Self { connection }
    }
}

/// A stored content address, read back.
fn address_from(text: &str) -> Result<ContentAddress, StoreError> {
    ContentAddress::parse(text)
        .map_err(|error| PORT.undecodable(format!("a marked address is malformed ({error})")))
}

/// A stored mark instant, read back.
fn instant_from(micros: i64) -> Result<Timestamp, StoreError> {
    from_micros(micros)
        .ok_or_else(|| PORT.undecodable(format!("{micros}µs is not a representable instant")))
}

impl CollectionStore for PostgresCollection {
    fn mark(&self, address: &ContentAddress, at: Timestamp) -> StoreFuture<'_, bool> {
        let address = address.clone();
        Box::pin(async move {
            // `xmax = 0` is true exactly for a row this statement inserted rather than updated,
            // which is the "was it already marked" answer, in the same statement as the write.
            let row = self
                .connection
                .query_one_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "INSERT INTO collection_marks (address, marked_at) VALUES ($1, $2) \
                     ON CONFLICT (address) DO UPDATE SET marked_at = EXCLUDED.marked_at \
                     RETURNING (xmax = 0) AS inserted",
                    [
                        Value::from(address.as_str().to_owned()),
                        Value::from(to_micros(at)),
                    ],
                ))
                .await
                .map_err(PORT.failing("marking a blob"))?
                .ok_or_else(|| PORT.undecodable("an upsert returned no row".to_owned()))?;
            let inserted: bool = row
                .try_get("", "inserted")
                .map_err(PORT.failing("marking a blob"))?;
            if inserted {
                tracing::info!(%address, marked_at = %at, "a blob was marked collectable");
            } else {
                tracing::debug!(%address, marked_at = %at, "an already-marked blob was re-stamped");
            }
            Ok(inserted)
        })
    }

    fn unmark<'a>(&'a self, address: &'a ContentAddress) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let result = self
                .connection
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "DELETE FROM collection_marks WHERE address = $1",
                    [Value::from(address.as_str().to_owned())],
                ))
                .await
                .map_err(PORT.failing("unmarking a blob"))?;
            let removed = result.rows_affected() > 0;
            if removed {
                tracing::info!(%address, "a blob's collection mark was removed");
            }
            Ok(removed)
        })
    }

    fn marked_since<'a>(
        &'a self,
        address: &'a ContentAddress,
    ) -> StoreFuture<'a, Option<Timestamp>> {
        Box::pin(async move {
            let Some(row) = self
                .connection
                .query_one_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT marked_at FROM collection_marks WHERE address = $1",
                    [Value::from(address.as_str().to_owned())],
                ))
                .await
                .map_err(PORT.failing("reading a blob's mark"))?
            else {
                return Ok(None);
            };
            let micros: i64 = row
                .try_get("", "marked_at")
                .map_err(PORT.failing("reading a blob's mark"))?;
            instant_from(micros).map(Some)
        })
    }

    fn marks(&self) -> StoreFuture<'_, Vec<(ContentAddress, Timestamp)>> {
        Box::pin(async move {
            let rows = self
                .connection
                .query_all_raw(Statement::from_string(
                    DbBackend::Postgres,
                    "SELECT address, marked_at FROM collection_marks \
                     ORDER BY address COLLATE \"C\"",
                ))
                .await
                .map_err(PORT.failing("listing the collection marks"))?;
            let failed = PORT.failing("listing the collection marks");
            let mut marks = Vec::with_capacity(rows.len());
            for row in &rows {
                let address: String = row.try_get("", "address").map_err(&failed)?;
                let micros: i64 = row.try_get("", "marked_at").map_err(&failed)?;
                marks.push((address_from(&address)?, instant_from(micros)?));
            }
            Ok(marks)
        })
    }
}

#[cfg(test)]
mod tests {
    /// The suite, against a real Postgres.
    mod postgres_conformance {
        use super::super::PostgresCollection;
        use crate::gc::CollectionStore;
        use crate::gc::conformance::{self, Harness};
        use crate::postgres::testing;

        /// A store over one container.
        #[derive(Debug)]
        struct PostgresHarness {
            marks: PostgresCollection,
        }

        impl Harness for PostgresHarness {
            fn marks(&self) -> &dyn CollectionStore {
                &self.marks
            }
        }

        #[tokio::test]
        async fn the_postgres_collection_store_conforms() {
            let Some(database) = testing::start("the Postgres collection store").await else {
                return;
            };
            let harness = PostgresHarness {
                marks: PostgresCollection::new(database.connection().clone()),
            };
            conformance::run_all(&harness).await;
        }
    }
}
