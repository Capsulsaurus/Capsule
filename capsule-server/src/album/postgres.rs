//! [`PostgresAlbums`] — the durable album store.
//!
//! # One table, two kinds of critical section
//!
//! `albums` holds one row per provisioned album. **Provisioning** is decided by the primary key:
//! `INSERT … ON CONFLICT DO NOTHING` either binds the id or writes nothing, and a conflict is
//! answered from the row that won. That row cannot vanish between the two statements — nothing
//! deletes an album — so the read after a conflict is reading a settled fact rather than racing.
//!
//! **The upgrade ceremony** is a read-compare-write over a row that already exists, so it is a
//! transaction that takes the row with `SELECT … FOR UPDATE`, decides with the same rules the
//! in-memory adapter applies, and writes inside the same lock. A second proposer blocks on the
//! row until the first commits, then reads the ceremony the first one wrote.
//!
//! # The intent is stored as the bytes its proposer signed
//!
//! [`UpgradeIntent`](capsule_core::crypto::upgrade::UpgradeIntent) is kept as canonical CBOR, not
//! as one column per field. Its expiry is core's predicate over the decoded intent, and its
//! deadline is a `u64` that a `BIGINT` cannot hold without a rule this schema would have to make
//! up. A stored intent that does not decode is a corrupt row, reported as such.

use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, QueryResult, Statement,
    TransactionTrait, Value,
};
use uuid::Uuid;

use super::{AlbumRecord, AlbumStore, ProvisionOutcome, UpgradeOutcome, UpgradeQuiescence};
use crate::postgres::error::Port;
use crate::postgres::time::{from_micros, stored, to_micros};
use crate::store::{AlbumId, OwnerId, StoreError, StoreFuture};

/// Which port is speaking, for every error this adapter raises.
const PORT: Port = Port {
    store: "albums",
    record: "AlbumRecord",
};

/// The columns every read decodes, in one place so the two `SELECT`s cannot drift.
const COLUMNS: &str = "owner_id, protocol_version, created_at, upgrade_intent, upgrade_received_at";

/// The durable album store.
#[derive(Debug, Clone)]
pub struct PostgresAlbums {
    connection: DatabaseConnection,
}

impl PostgresAlbums {
    /// A store over `connection`.
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

/// Commit, or say why not.
async fn commit(transaction: DatabaseTransaction) -> Result<(), StoreError> {
    transaction
        .commit()
        .await
        .map_err(PORT.failing("committing a transaction"))
}

/// An instant column, decoded.
fn instant(micros: i64) -> Result<jiff::Timestamp, StoreError> {
    from_micros(micros)
        .ok_or_else(|| PORT.undecodable(format!("{micros}µs is not a representable instant")))
}

/// One `albums` row as the record it holds.
fn decode(album: &AlbumId, row: &QueryResult) -> Result<AlbumRecord, StoreError> {
    let failed = PORT.failing("reading an album");
    let owner_id: String = row.try_get("", "owner_id").map_err(&failed)?;
    let protocol_version: String = row.try_get("", "protocol_version").map_err(&failed)?;
    let created_at: i64 = row.try_get("", "created_at").map_err(&failed)?;
    let intent: Option<Vec<u8>> = row.try_get("", "upgrade_intent").map_err(&failed)?;
    let received_at: Option<i64> = row.try_get("", "upgrade_received_at").map_err(&failed)?;
    let upgrade = match (intent, received_at) {
        (Some(intent), Some(received_at)) => Some(UpgradeQuiescence {
            intent: capsule_core::cbor::from_slice(&intent).map_err(|error| {
                PORT.undecodable(format!(
                    "the stored upgrade intent does not decode: {error}"
                ))
            })?,
            received_at: instant(received_at)?,
        }),
        (None, None) => None,
        // The two ceremony columns are written and cleared together; one without the other is
        // a row this server did not write.
        _ => {
            return Err(PORT.undecodable("an album row carries half an upgrade ceremony"));
        }
    };
    Ok(AlbumRecord {
        album_id: album.clone(),
        owner_id: OwnerId::new(owner_id),
        protocol_version,
        created_at: instant(created_at)?,
        upgrade,
    })
}

/// The album row for `album`, read through `connection`, optionally locked for the transaction.
async fn read_through<C: ConnectionTrait>(
    connection: &C,
    album: &AlbumId,
    for_update: bool,
) -> Result<Option<AlbumRecord>, StoreError> {
    let lock = if for_update { " FOR UPDATE" } else { "" };
    let row = connection
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!("SELECT {COLUMNS} FROM albums WHERE album_id = $1{lock}"),
            [Value::from(album.as_str().to_owned())],
        ))
        .await
        .map_err(PORT.failing("reading an album"))?;
    row.map(|row| decode(album, &row)).transpose()
}

/// A ceremony as the two columns hold it.
fn encode_upgrade(
    upgrade: Option<&UpgradeQuiescence>,
) -> Result<(Option<Vec<u8>>, Option<i64>), StoreError> {
    let Some(quiescence) = upgrade else {
        return Ok((None, None));
    };
    let intent = capsule_core::cbor::to_canonical_vec(&quiescence.intent).map_err(|error| {
        StoreError::Rejected {
            store: PORT.store,
            detail: format!("the upgrade intent could not be encoded: {error}"),
        }
    })?;
    Ok((Some(intent), Some(to_micros(quiescence.received_at))))
}

/// Write `upgrade` as `album`'s ceremony (or clear it), inside `transaction`.
async fn write_upgrade(
    transaction: &DatabaseTransaction,
    album: &AlbumId,
    upgrade: Option<&UpgradeQuiescence>,
) -> Result<(), StoreError> {
    let (intent, received_at) = encode_upgrade(upgrade)?;
    transaction
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE albums SET upgrade_intent = $2, upgrade_received_at = $3 WHERE album_id = $1",
            [
                Value::from(album.as_str().to_owned()),
                Value::from(intent),
                Value::from(received_at),
            ],
        ))
        .await
        .map_err(PORT.failing("writing an album's upgrade ceremony"))?;
    Ok(())
}

impl AlbumStore for PostgresAlbums {
    fn provision(&self, record: AlbumRecord) -> StoreFuture<'_, ProvisionOutcome> {
        Box::pin(async move {
            // Stored exactly as given, ceremony included, as the in-memory adapter stores it —
            // with every instant at the precision its column holds.
            let record = AlbumRecord {
                created_at: stored(record.created_at),
                upgrade: record.upgrade.map(|quiescence| UpgradeQuiescence {
                    received_at: stored(quiescence.received_at),
                    ..quiescence
                }),
                ..record
            };
            let (intent, received_at) = encode_upgrade(record.upgrade.as_ref())?;
            let inserted = self
                .connection
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "INSERT INTO albums \
                       (album_id, owner_id, protocol_version, created_at, upgrade_intent, \
                        upgrade_received_at) \
                     VALUES ($1, $2, $3, $4, $5, $6) \
                     ON CONFLICT (album_id) DO NOTHING",
                    [
                        Value::from(record.album_id.as_str().to_owned()),
                        Value::from(record.owner_id.as_str().to_owned()),
                        Value::from(record.protocol_version.clone()),
                        Value::from(to_micros(record.created_at)),
                        Value::from(intent),
                        Value::from(received_at),
                    ],
                ))
                .await
                .map_err(PORT.failing("provisioning an album"))?
                .rows_affected();
            if inserted == 1 {
                tracing::info!(
                    album = %record.album_id,
                    owner = %record.owner_id,
                    pin = %record.protocol_version,
                    "an album was provisioned"
                );
                return Ok(ProvisionOutcome::Created(record));
            }

            // The id was already bound. Nothing deletes an album, so the row that won the
            // conflict is still there to be read.
            let Some(existing) = read_through(&self.connection, &record.album_id, false).await?
            else {
                return Err(StoreError::Rejected {
                    store: PORT.store,
                    detail: "an album conflicted on insert and then could not be read".to_owned(),
                });
            };
            Ok(if existing.owner_id == record.owner_id {
                ProvisionOutcome::AlreadyProvisioned(existing)
            } else {
                tracing::info!(
                    album = %record.album_id,
                    "an album provision was refused: the id is bound elsewhere"
                );
                ProvisionOutcome::NotAvailable
            })
        })
    }

    fn read<'a>(&'a self, album: &'a AlbumId) -> StoreFuture<'a, Option<AlbumRecord>> {
        Box::pin(async move { read_through(&self.connection, album, false).await })
    }

    fn begin_upgrade<'a>(
        &'a self,
        album: &'a AlbumId,
        owner: &'a OwnerId,
        quiescence: UpgradeQuiescence,
        now: jiff::Timestamp,
    ) -> StoreFuture<'a, UpgradeOutcome> {
        Box::pin(async move {
            let transaction = begin(&self.connection).await?;
            let Some(mut record) = read_through(&transaction, album, true).await? else {
                return Ok(UpgradeOutcome::NotFound);
            };
            // Not this caller's album is the same answer as no album: the id is client-derived,
            // so a guess must buy nothing.
            if &record.owner_id != owner {
                return Ok(UpgradeOutcome::NotFound);
            }
            if let Some(live) = &record.upgrade
                && !live.is_expired(now)
                && live.intent.intent_id != quiescence.intent.intent_id
            {
                tracing::info!(
                    %album,
                    live = %live.intent.intent_id,
                    proposed = %quiescence.intent.intent_id,
                    "an upgrade proposal was refused: another ceremony is in flight"
                );
                return Ok(UpgradeOutcome::Conflict {
                    intent_id: live.intent.intent_id,
                });
            }
            let quiescence = UpgradeQuiescence {
                received_at: stored(quiescence.received_at),
                ..quiescence
            };
            write_upgrade(&transaction, album, Some(&quiescence)).await?;
            commit(transaction).await?;
            tracing::info!(
                %album,
                intent = %quiescence.intent.intent_id,
                to = %quiescence.intent.to_protocol_version,
                "an album entered upgrade quiescence"
            );
            record.upgrade = Some(quiescence);
            Ok(UpgradeOutcome::Quiescing(Box::new(record)))
        })
    }

    fn end_upgrade<'a>(
        &'a self,
        album: &'a AlbumId,
        owner: &'a OwnerId,
        intent_id: Uuid,
        now: jiff::Timestamp,
    ) -> StoreFuture<'a, UpgradeOutcome> {
        Box::pin(async move {
            let transaction = begin(&self.connection).await?;
            let Some(mut record) = read_through(&transaction, album, true).await? else {
                return Ok(UpgradeOutcome::NotFound);
            };
            if &record.owner_id != owner {
                return Ok(UpgradeOutcome::NotFound);
            }
            if let Some(live) = &record.upgrade
                && !live.is_expired(now)
                && live.intent.intent_id != intent_id
            {
                return Ok(UpgradeOutcome::Conflict {
                    intent_id: live.intent.intent_id,
                });
            }
            if record.upgrade.take().is_some() {
                write_upgrade(&transaction, album, None).await?;
                commit(transaction).await?;
                tracing::info!(%album, intent = %intent_id, "an album left upgrade quiescence");
            }
            Ok(UpgradeOutcome::Cleared(Box::new(record)))
        })
    }
}

#[cfg(test)]
mod tests {
    /// The suite, against a real Postgres.
    mod postgres_conformance {
        use super::super::PostgresAlbums;
        use crate::album::AlbumStore;
        use crate::album::conformance::{self, Harness};
        use crate::postgres::testing;

        /// A store over one container.
        #[derive(Debug)]
        struct PostgresHarness {
            albums: PostgresAlbums,
        }

        impl Harness for PostgresHarness {
            fn albums(&self) -> &dyn AlbumStore {
                &self.albums
            }
        }

        #[tokio::test]
        async fn the_postgres_album_store_conforms() {
            let Some(database) = testing::start("the Postgres album store").await else {
                return;
            };
            let harness = PostgresHarness {
                albums: PostgresAlbums::new(database.connection().clone()),
            };
            conformance::run_all(&harness).await;
        }
    }
}
