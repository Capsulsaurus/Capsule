//! [`PostgresDeviceDirectory`] — the durable device-directory store.
//!
//! # One row per account, one lock per publish
//!
//! `device_directories` holds the directory in force for each account: the signed bytes
//! verbatim, the version projected out of them, and the identity key the first publish anchored.
//!
//! [`DeviceDirectoryStore::publish`] is two comparisons and a write that must be one operation,
//! and the row it would lock does not exist for an account's **first** publish — the case the
//! anchor rule is most concerned with, because two concurrent first publishes under different
//! keys would each find no row and each believe it established the anchor. So, like
//! `membership/postgres.rs`, the critical section is a transaction-scoped advisory lock keyed on
//! the account, which needs no row to exist and is released by the commit or rollback. Every
//! statement after it — the read, both comparisons and the upsert — runs under it.
//!
//! The lock key is `hashtext('device_directories:' || user_id)`: prefixed so that an account id
//! and an album id of the same spelling do not share a key with `membership`'s lock. A 32-bit
//! collision with any other advisory-lock user costs serialization and never correctness, for the
//! reason `membership/postgres.rs` sets out.
//!
//! # A version past `BIGINT` is refused after the port's own rule, never before
//!
//! The comparisons are made in Rust on `u64`, exactly as the in-memory adapter makes them, so a
//! stale or mis-anchored publish is answered with its typed refusal whatever its version. Only a
//! publish that *would* be stored is then checked against the column's width, and a version past
//! `i64::MAX` is a [`StoreError::Rejected`]: no client increments a directory version nine
//! quintillion times, so that is a forged or corrupt document rather than a state worth a
//! wider column.

use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, Statement,
    TransactionTrait, Value,
};

use super::{DeviceDirectoryStore, PublishOutcome, PublishedDirectory};
use crate::postgres::error::Port;
use crate::postgres::time::{from_micros, stored, to_micros};
use crate::store::{StoreError, StoreFuture, UserId};

/// Which port is speaking, for every error this adapter raises.
const PORT: Port = Port {
    store: "device_directories",
    record: "PublishedDirectory",
};

/// The durable device-directory store.
#[derive(Debug, Clone)]
pub struct PostgresDeviceDirectory {
    connection: DatabaseConnection,
}

impl PostgresDeviceDirectory {
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

/// The directory published for `user`, read through `connection`.
async fn fetch_through<C: ConnectionTrait>(
    connection: &C,
    user: &UserId,
) -> Result<Option<PublishedDirectory>, StoreError> {
    let Some(row) = connection
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT directory_version, document, identity_key, published_at \
             FROM device_directories WHERE user_id = $1",
            [Value::from(user.as_str().to_owned())],
        ))
        .await
        .map_err(PORT.failing("reading a device directory"))?
    else {
        return Ok(None);
    };
    let failed = PORT.failing("reading a device directory");
    let directory_version: i64 = row.try_get("", "directory_version").map_err(&failed)?;
    let document: Vec<u8> = row.try_get("", "document").map_err(&failed)?;
    let identity_key: Vec<u8> = row.try_get("", "identity_key").map_err(&failed)?;
    let published_at: i64 = row.try_get("", "published_at").map_err(&failed)?;
    Ok(Some(PublishedDirectory {
        user_id: user.clone(),
        directory_version: u64::try_from(directory_version).map_err(|_| {
            PORT.undecodable(format!("{directory_version} is not a directory version"))
        })?,
        document,
        identity_key,
        published_at: from_micros(published_at).ok_or_else(|| {
            PORT.undecodable(format!("{published_at}µs is not a representable instant"))
        })?,
    }))
}

impl DeviceDirectoryStore for PostgresDeviceDirectory {
    fn publish(&self, record: PublishedDirectory) -> StoreFuture<'_, PublishOutcome> {
        Box::pin(async move {
            let transaction = begin(&self.connection).await?;

            // The critical section starts here and ends with the transaction.
            transaction
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT pg_advisory_xact_lock(hashtext('device_directories:' || $1))",
                    [Value::from(record.user_id.as_str().to_owned())],
                ))
                .await
                .map_err(PORT.failing("locking an account's device directory"))?;

            if let Some(held) = fetch_through(&transaction, &record.user_id).await? {
                // The anchor first, for the reason the in-memory adapter gives: a document under
                // the wrong key is refused for *that*, never for being stale.
                if held.identity_key != record.identity_key {
                    tracing::warn!(
                        user = %record.user_id,
                        "a device directory publish was refused: it is signed under a key that \
                         is not this account's identity anchor"
                    );
                    return Ok(PublishOutcome::IdentityMismatch);
                }
                if record.directory_version <= held.directory_version {
                    tracing::info!(
                        user = %record.user_id,
                        stored = held.directory_version,
                        submitted = record.directory_version,
                        "a device directory publish was refused: it does not advance"
                    );
                    return Ok(PublishOutcome::Stale {
                        stored: held.directory_version,
                    });
                }
            } else {
                tracing::info!(
                    user = %record.user_id,
                    "a first device directory establishes this account's identity anchor"
                );
            }

            let version =
                i64::try_from(record.directory_version).map_err(|_| StoreError::Rejected {
                    store: PORT.store,
                    detail: format!(
                        "directory version {} is past what a BIGINT column holds",
                        record.directory_version
                    ),
                })?;
            let published_at = stored(record.published_at);
            // The anchor column is written on the first publish and rewritten on later ones
            // with the value it already holds — the comparison above is what makes that true.
            transaction
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "INSERT INTO device_directories \
                       (user_id, directory_version, document, identity_key, published_at) \
                     VALUES ($1, $2, $3, $4, $5) \
                     ON CONFLICT (user_id) DO UPDATE SET \
                       directory_version = EXCLUDED.directory_version, \
                       document = EXCLUDED.document, \
                       published_at = EXCLUDED.published_at",
                    [
                        Value::from(record.user_id.as_str().to_owned()),
                        Value::from(version),
                        Value::from(record.document.clone()),
                        Value::from(record.identity_key.clone()),
                        Value::from(to_micros(published_at)),
                    ],
                ))
                .await
                .map_err(PORT.failing("publishing a device directory"))?;
            commit(transaction).await?;
            tracing::info!(
                user = %record.user_id,
                version = record.directory_version,
                bytes = record.document.len(),
                "a device directory was published"
            );
            Ok(PublishOutcome::Published {
                directory_version: record.directory_version,
            })
        })
    }

    fn fetch<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Option<PublishedDirectory>> {
        Box::pin(async move { fetch_through(&self.connection, user).await })
    }
}

#[cfg(test)]
mod tests;
