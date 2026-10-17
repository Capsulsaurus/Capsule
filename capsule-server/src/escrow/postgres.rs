//! [`PostgresEscrow`] — the durable master-key escrow (`S-C12`, #446).
//!
//! # The replace is one statement
//!
//! `escrows` holds one row per account. [`EscrowStore::store`] is a single
//! `INSERT … ON CONFLICT (user_id) DO UPDATE`, which is the atomicity the port owes: there is no
//! instant at which the account has no escrow, and none at which the secret being rotated away
//! from still unwraps anything this table holds. A delete followed by an insert would open the
//! first window; an insert beside the old row would open the second.
//!
//! Whether a row was displaced is read from the same statement — `RETURNING (xmax = 0)` is true
//! exactly when the row was freshly inserted — so the [`Replaced`] answer cannot disagree with
//! what was written, which a separate `SELECT` beforehand could under a concurrent store.

use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, Value};

use super::{EscrowRecord, EscrowStore, Replaced};
use crate::postgres::error::Port;
use crate::postgres::time::{from_micros, to_micros};
use crate::store::{StoreFuture, UserId};

/// Which port is speaking, for every error this adapter raises.
const PORT: Port = Port {
    store: "escrow",
    record: "EscrowRecord",
};

/// The durable escrow store.
#[derive(Debug, Clone)]
pub struct PostgresEscrow {
    connection: DatabaseConnection,
}

impl PostgresEscrow {
    /// A store over `connection`.
    pub fn new(connection: DatabaseConnection) -> Self {
        Self { connection }
    }
}

impl EscrowStore for PostgresEscrow {
    fn store(&self, record: EscrowRecord) -> StoreFuture<'_, Replaced> {
        Box::pin(async move {
            let user = record.user_id.clone();
            let bytes = record.blob.len();
            let row = self
                .connection
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "INSERT INTO escrows (user_id, blob, stored_at) VALUES ($1, $2, $3) \
                     ON CONFLICT (user_id) DO UPDATE SET \
                       blob = EXCLUDED.blob, stored_at = EXCLUDED.stored_at \
                     RETURNING (xmax = 0) AS inserted",
                    [
                        Value::from(user.as_str().to_owned()),
                        Value::from(record.blob),
                        Value::from(to_micros(record.stored_at)),
                    ],
                ))
                .await
                .map_err(PORT.failing("storing an escrow"))?
                .ok_or_else(|| PORT.undecodable("an upsert returned no row".to_owned()))?;
            let inserted: bool = row
                .try_get("", "inserted")
                .map_err(PORT.failing("storing an escrow"))?;
            if inserted {
                tracing::info!(%user, bytes, "an escrow was stored for the first time");
                Ok(Replaced::No)
            } else {
                tracing::info!(
                    %user,
                    bytes,
                    "an escrow was replaced; the previous wrapped master key is gone"
                );
                Ok(Replaced::Yes)
            }
        })
    }

    fn fetch<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Option<EscrowRecord>> {
        Box::pin(async move {
            let Some(row) = self
                .connection
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT blob, stored_at FROM escrows WHERE user_id = $1",
                    [Value::from(user.as_str().to_owned())],
                ))
                .await
                .map_err(PORT.failing("reading an escrow"))?
            else {
                return Ok(None);
            };
            let failed = PORT.failing("reading an escrow");
            let blob: Vec<u8> = row.try_get("", "blob").map_err(&failed)?;
            let stored_at: i64 = row.try_get("", "stored_at").map_err(&failed)?;
            Ok(Some(EscrowRecord {
                user_id: user.clone(),
                blob,
                stored_at: from_micros(stored_at).ok_or_else(|| {
                    PORT.undecodable(format!("{stored_at}µs is not a representable instant"))
                })?,
            }))
        })
    }
}

#[cfg(test)]
mod tests {
    /// The suite, against a real Postgres.
    mod postgres_conformance {
        use super::super::PostgresEscrow;
        use crate::escrow::EscrowStore;
        use crate::escrow::conformance::{self, Harness};
        use crate::postgres::testing;

        /// A store over one container.
        #[derive(Debug)]
        struct PostgresHarness {
            escrows: PostgresEscrow,
        }

        impl Harness for PostgresHarness {
            fn escrows(&self) -> &dyn EscrowStore {
                &self.escrows
            }
        }

        #[tokio::test]
        async fn the_postgres_escrow_store_conforms() {
            let Some(database) = testing::start("the Postgres escrow store").await else {
                return;
            };
            let harness = PostgresHarness {
                escrows: PostgresEscrow::new(database.connection().clone()),
            };
            conformance::run_all(&harness).await;
        }
    }
}
