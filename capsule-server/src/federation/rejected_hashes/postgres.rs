//! [`PostgresRejectedHashes`]: the durable [`RejectedHashes`] over `federation_rejected_hashes`.
//!
//! # A record is one transaction under one lock
//!
//! [`RejectedHashes::record`] opens a transaction and takes
//! `pg_advisory_xact_lock(hashtext('federation_rejected_hashes'))`, then, in order: deletes every
//! row past its age, upserts the refusal (`ON CONFLICT (hash) DO UPDATE` moves the last
//! reference forward with `GREATEST` and counts it), and deletes every row past capacity in
//! eviction order (`ORDER BY last_referenced_at DESC, hash DESC OFFSET capacity`). The lock
//! serializes records, so two concurrent ones cannot each count the other out and leave the
//! table past its capacity. Records are rare, one per refused event, so serializing them costs
//! nothing a pull would notice; a lookup takes no lock.
//!
//! # Encodings
//!
//! The hash is `BYTEA` of 32 bytes, whose byte order is the one the double's [`Hash32`] order
//! is, so both evict the same row on a tie. Instants are `BIGINT` microseconds
//! ([`crate::postgres::time`]).

use capsule_core::crypto::hash::Hash32;
use jiff::Timestamp;
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbBackend, QueryResult, Statement, TransactionTrait, Value,
};

use super::{RejectedHash, RejectedHashBounds, RejectedHashes, Rejection};
use crate::federation::PeerId;
use crate::postgres::error::Port;
use crate::postgres::time::{from_micros, to_micros};
use crate::store::{AlbumId, StoreError, StoreFuture};

/// Which port is speaking, for every error the adapter raises.
const PORT: Port = Port {
    store: "rejected hashes",
    record: "RejectedHash",
};

/// The durable rejected-hash table.
#[derive(Debug, Clone)]
pub struct PostgresRejectedHashes {
    connection: DatabaseConnection,
    bounds: RejectedHashBounds,
}

impl PostgresRejectedHashes {
    /// A table over `connection`, bounded by `bounds`.
    pub fn new(connection: DatabaseConnection, bounds: RejectedHashBounds) -> Self {
        Self { connection, bounds }
    }
}

fn statement(sql: &str, values: Vec<Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

fn row_from(row: &QueryResult) -> Result<RejectedHash, StoreError> {
    let failed = PORT.failing("reading a rejected hash");
    let hash: Vec<u8> = row.try_get("", "hash").map_err(&failed)?;
    let hash: [u8; 32] = hash
        .as_slice()
        .try_into()
        .map_err(|_| PORT.undecodable(format!("a {}-byte hash", hash.len())))?;
    let instant = |column: &str| -> Result<Timestamp, StoreError> {
        let micros: i64 = row.try_get("", column).map_err(&failed)?;
        from_micros(micros)
            .ok_or_else(|| PORT.undecodable(format!("{micros}µs is not a representable instant")))
    };
    let references: i64 = row.try_get("", "reference_count").map_err(&failed)?;
    Ok(RejectedHash {
        rejection: Rejection {
            hash: Hash32(hash),
            peer: PeerId::new(row.try_get::<String>("", "peer_id").map_err(&failed)?),
            album: AlbumId::new(row.try_get::<String>("", "album_id").map_err(&failed)?),
            asset_id: row.try_get("", "asset_id").map_err(&failed)?,
            rule: row.try_get("", "rule").map_err(&failed)?,
        },
        first_rejected_at: instant("first_rejected_at")?,
        last_referenced_at: instant("last_referenced_at")?,
        references: u64::try_from(references)
            .map_err(|_| PORT.undecodable(format!("{references} references")))?,
    })
}

impl RejectedHashes for PostgresRejectedHashes {
    fn record(&self, rejection: Rejection, now: Timestamp) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let horizon = to_micros(self.bounds.expired_at_or_before(now));
            let now = to_micros(now);
            let hash = rejection.hash;
            let transaction = self
                .connection
                .begin()
                .await
                .map_err(PORT.failing("opening a transaction"))?;
            transaction
                .execute_raw(statement(
                    "SELECT pg_advisory_xact_lock(hashtext('federation_rejected_hashes'))",
                    Vec::new(),
                ))
                .await
                .map_err(PORT.failing("locking the rejected-hash table"))?;
            let expired = transaction
                .execute_raw(statement(
                    "DELETE FROM federation_rejected_hashes WHERE last_referenced_at <= $1",
                    vec![Value::from(horizon)],
                ))
                .await
                .map_err(PORT.failing("forgetting aged rejected hashes"))?
                .rows_affected();
            let inserted = transaction
                .query_one_raw(statement(
                    "INSERT INTO federation_rejected_hashes (hash, peer_id, album_id, asset_id, \
                     rule, first_rejected_at, last_referenced_at, reference_count) \
                     VALUES ($1, $2, $3, $4, $5, $6, $6, 1) \
                     ON CONFLICT (hash) DO UPDATE SET \
                     last_referenced_at = GREATEST(federation_rejected_hashes.last_referenced_at, \
                     EXCLUDED.last_referenced_at), \
                     reference_count = CASE \
                     WHEN federation_rejected_hashes.reference_count < 9223372036854775807 \
                     THEN federation_rejected_hashes.reference_count + 1 \
                     ELSE federation_rejected_hashes.reference_count END \
                     RETURNING (xmax = 0) AS inserted",
                    vec![
                        Value::from(hash.0.to_vec()),
                        Value::from(rejection.peer.as_str().to_owned()),
                        Value::from(rejection.album.as_str().to_owned()),
                        Value::from(rejection.asset_id),
                        Value::from(rejection.rule),
                        Value::from(now),
                    ],
                ))
                .await
                .map_err(PORT.failing("recording a rejected hash"))?
                .map(|row| row.try_get::<bool>("", "inserted"))
                .transpose()
                .map_err(PORT.failing("recording a rejected hash"))?
                .unwrap_or(false);
            let evicted = transaction
                .execute_raw(statement(
                    "DELETE FROM federation_rejected_hashes WHERE hash IN (\
                     SELECT hash FROM federation_rejected_hashes \
                     ORDER BY last_referenced_at DESC, hash DESC OFFSET $1)",
                    vec![Value::from(i64::from(self.bounds.capacity()))],
                ))
                .await
                .map_err(PORT.failing("evicting rejected hashes past capacity"))?
                .rows_affected();
            transaction
                .commit()
                .await
                .map_err(PORT.failing("committing a rejected hash"))?;
            tracing::debug!(
                hash = %hash.to_hex(),
                referenced_again = !inserted,
                expired,
                evicted,
                "a rejected hash was recorded"
            );
            Ok(())
        })
    }

    fn lookup<'a>(
        &'a self,
        hash: &'a Hash32,
        now: Timestamp,
    ) -> StoreFuture<'a, Option<RejectedHash>> {
        Box::pin(async move {
            let horizon = to_micros(self.bounds.expired_at_or_before(now));
            self.connection
                .query_one_raw(statement(
                    "SELECT hash, peer_id, album_id, asset_id, rule, first_rejected_at, \
                     last_referenced_at, reference_count FROM federation_rejected_hashes \
                     WHERE hash = $1 AND last_referenced_at > $2",
                    vec![Value::from(hash.0.to_vec()), Value::from(horizon)],
                ))
                .await
                .map_err(PORT.failing("looking up a rejected hash"))?
                .as_ref()
                .map(row_from)
                .transpose()
        })
    }
}

#[cfg(test)]
mod tests;
