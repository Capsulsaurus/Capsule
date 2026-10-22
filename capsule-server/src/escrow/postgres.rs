//! [`PostgresEscrow`] — the durable master-key escrow (`S-C12`, #446) and the sealed identity
//! beside it (#544).
//!
//! # The replace is one statement
//!
//! `escrows` holds one row per account, and so does `sealed_identities`; both are written by the
//! same statement shape, in `PostgresEscrow::upsert`. [`EscrowStore::store`] is a single
//! `INSERT … ON CONFLICT (user_id) DO UPDATE`, which is the atomicity the port owes: there is no
//! instant at which the account has no escrow, and none at which the secret being rotated away
//! from still unwraps anything this table holds. A delete followed by an insert would open the
//! first window; an insert beside the old row would open the second.
//!
//! Whether a row was displaced is read from the same statement — `RETURNING (xmax = 0)` is true
//! exactly when the row was freshly inserted — so the [`Replaced`] answer cannot disagree with
//! what was written, which a separate `SELECT` beforehand could under a concurrent store.
//!
//! `album_key_escrows` (#741) holds one row per `(user_id, album_id)` and is written by the same
//! statement shape over that composite key.

use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, Value};

use super::{AlbumKeyRecord, EscrowRecord, EscrowStore, Replaced};
use crate::postgres::error::Port;
use crate::postgres::time::{from_micros, to_micros};
use crate::store::{AlbumId, StoreError, StoreFuture, UserId};

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

/// One of the two one-row-per-account tables this adapter writes.
///
/// The escrow and the sealed identity obey the same rules — one per account, opaque, replaced by
/// one upsert — so they share one statement shape, and only the table and the log noun differ.
#[derive(Debug, Clone, Copy)]
struct Slot {
    /// The table: `escrows` or `sealed_identities`. A compile-time constant, never input.
    table: &'static str,
    /// The operation a failed write names, for the error line.
    storing: &'static str,
    /// The operation a failed read names, for the error line.
    reading: &'static str,
}

/// The wrapped master key.
const ESCROW: Slot = Slot {
    table: "escrows",
    storing: "storing an escrow",
    reading: "reading an escrow",
};

/// The user identity key sealed under the master key.
const SEALED_IDENTITY: Slot = Slot {
    table: "sealed_identities",
    storing: "storing a sealed identity",
    reading: "reading a sealed identity",
};

impl PostgresEscrow {
    /// Upsert `record` into `slot`'s table, reading from the same statement whether a row was
    /// displaced.
    async fn upsert(&self, slot: Slot, record: EscrowRecord) -> Result<Replaced, StoreError> {
        let user = record.user_id.clone();
        let bytes = record.blob.len();
        let doing = slot.storing;
        let row = self
            .connection
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!(
                    "INSERT INTO {} (user_id, blob, stored_at) VALUES ($1, $2, $3) \
                     ON CONFLICT (user_id) DO UPDATE SET \
                       blob = EXCLUDED.blob, stored_at = EXCLUDED.stored_at \
                     RETURNING (xmax = 0) AS inserted",
                    slot.table
                ),
                [
                    Value::from(user.as_str().to_owned()),
                    Value::from(record.blob),
                    Value::from(to_micros(record.stored_at)),
                ],
            ))
            .await
            .map_err(PORT.failing(doing))?
            .ok_or_else(|| PORT.undecodable("an upsert returned no row".to_owned()))?;
        let inserted: bool = row.try_get("", "inserted").map_err(PORT.failing(doing))?;
        if inserted {
            tracing::info!(%user, bytes, slot = slot.table, "stored for the first time");
            Ok(Replaced::No)
        } else {
            tracing::info!(
                %user,
                bytes,
                slot = slot.table,
                "replaced; the previous blob is gone"
            );
            Ok(Replaced::Yes)
        }
    }

    /// The row `slot`'s table holds for `user`, or `None`.
    async fn select(&self, slot: Slot, user: &UserId) -> Result<Option<EscrowRecord>, StoreError> {
        let doing = slot.reading;
        let Some(row) = self
            .connection
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!(
                    "SELECT blob, stored_at FROM {} WHERE user_id = $1",
                    slot.table
                ),
                [Value::from(user.as_str().to_owned())],
            ))
            .await
            .map_err(PORT.failing(doing))?
        else {
            return Ok(None);
        };
        let failed = PORT.failing(doing);
        let blob: Vec<u8> = row.try_get("", "blob").map_err(&failed)?;
        let stored_at: i64 = row.try_get("", "stored_at").map_err(&failed)?;
        Ok(Some(EscrowRecord {
            user_id: user.clone(),
            blob,
            stored_at: from_micros(stored_at).ok_or_else(|| {
                PORT.undecodable(format!("{stored_at}µs is not a representable instant"))
            })?,
        }))
    }
}

impl PostgresEscrow {
    /// Upsert one album's key escrow into `album_key_escrows`, keyed `(user_id, album_id)`, by
    /// the same one-statement replace as the per-account slots.
    async fn upsert_album_keys(&self, record: AlbumKeyRecord) -> Result<Replaced, StoreError> {
        const DOING: &str = "storing an album key escrow";
        let user = record.user_id.clone();
        let album = record.album_id.as_str().to_owned();
        let bytes = record.blob.len();
        let row = self
            .connection
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO album_key_escrows (user_id, album_id, blob, stored_at) \
                 VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (user_id, album_id) DO UPDATE SET \
                   blob = EXCLUDED.blob, stored_at = EXCLUDED.stored_at \
                 RETURNING (xmax = 0) AS inserted",
                [
                    Value::from(user.as_str().to_owned()),
                    Value::from(album.clone()),
                    Value::from(record.blob),
                    Value::from(to_micros(record.stored_at)),
                ],
            ))
            .await
            .map_err(PORT.failing(DOING))?
            .ok_or_else(|| PORT.undecodable("an upsert returned no row".to_owned()))?;
        let inserted: bool = row.try_get("", "inserted").map_err(PORT.failing(DOING))?;
        if inserted {
            tracing::info!(%user, %album, bytes, "album key escrow stored for the first time");
            Ok(Replaced::No)
        } else {
            tracing::info!(%user, %album, bytes, "album key escrow replaced");
            Ok(Replaced::Yes)
        }
    }

    /// Every album key escrow `user` holds, in album order.
    async fn select_album_keys(&self, user: &UserId) -> Result<Vec<AlbumKeyRecord>, StoreError> {
        const DOING: &str = "listing album key escrows";
        let rows = self
            .connection
            .query_all_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT album_id, blob, stored_at FROM album_key_escrows \
                 WHERE user_id = $1 ORDER BY album_id COLLATE \"C\"",
                [Value::from(user.as_str().to_owned())],
            ))
            .await
            .map_err(PORT.failing(DOING))?;
        let failed = PORT.failing(DOING);
        rows.into_iter()
            .map(|row| {
                let album_id: String = row.try_get("", "album_id").map_err(&failed)?;
                let blob: Vec<u8> = row.try_get("", "blob").map_err(&failed)?;
                let stored_at: i64 = row.try_get("", "stored_at").map_err(&failed)?;
                Ok(AlbumKeyRecord {
                    user_id: user.clone(),
                    album_id: AlbumId::new(album_id),
                    blob,
                    stored_at: from_micros(stored_at).ok_or_else(|| {
                        PORT.undecodable(format!("{stored_at}µs is not a representable instant"))
                    })?,
                })
            })
            .collect()
    }
}

impl EscrowStore for PostgresEscrow {
    fn store(&self, record: EscrowRecord) -> StoreFuture<'_, Replaced> {
        Box::pin(self.upsert(ESCROW, record))
    }

    fn fetch<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Option<EscrowRecord>> {
        Box::pin(self.select(ESCROW, user))
    }

    fn store_identity(&self, record: EscrowRecord) -> StoreFuture<'_, Replaced> {
        Box::pin(self.upsert(SEALED_IDENTITY, record))
    }

    fn fetch_identity<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Option<EscrowRecord>> {
        Box::pin(self.select(SEALED_IDENTITY, user))
    }

    fn store_album_keys(&self, record: AlbumKeyRecord) -> StoreFuture<'_, Replaced> {
        Box::pin(self.upsert_album_keys(record))
    }

    fn list_album_keys<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Vec<AlbumKeyRecord>> {
        Box::pin(self.select_album_keys(user))
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
