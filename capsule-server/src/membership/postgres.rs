//! [`PostgresMembership`] — the durable membership store (`S-C51`).
//!
//! # Two tables, one lock
//!
//! `album_rosters` holds the roster the server currently accepts for an album — one row per
//! album, the signed document verbatim — and `album_members` holds one row per account that
//! has ever been on one of that album's rosters. A revoked member keeps their row with
//! `revoked_at_version` and `revoked_epoch` set, because a deleted row would make the blob
//! route's `403` unrenderable (see the module docs).
//!
//! # Why the lock is an advisory lock and not `SELECT … FOR UPDATE`
//!
//! `apply_roster` has to be one critical section against a concurrent publish, and the row it
//! would lock does not exist yet for the album's **first** roster — two first publishes would
//! both read "no roster" and both upsert, and the loser would silently overwrite the winner's
//! row rather than answer `Stale`. A transaction-scoped advisory lock keyed on the album id
//! serialises both cases with one statement, is released by the commit or rollback, and needs
//! no row to exist. Every statement after it runs under the lock, so the read, the comparison
//! and the writes are one operation.

use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, Statement,
    TransactionTrait, Value,
};
use uuid::Uuid;

use super::{
    MemberRole, Membership, MembershipStore, Revocation, RosterOutcome, RosterRecord, precheck,
    role_from_token, role_token,
};
use crate::postgres::error::Port;
use crate::postgres::time::{from_micros, stored, to_micros};
use crate::store::{AlbumId, StoreError, StoreFuture, UserId};

/// Which port is speaking, for every error this adapter raises.
const PORT: Port = Port {
    store: "membership",
    record: "RosterRecord",
};

/// The durable membership store.
#[derive(Debug, Clone)]
pub struct PostgresMembership {
    connection: DatabaseConnection,
}

impl PostgresMembership {
    /// A store over `connection`.
    pub fn new(connection: DatabaseConnection) -> Self {
        Self { connection }
    }
}

/// A version or epoch as the column holds it.
fn counter_to_column(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::Rejected {
        store: PORT.store,
        detail: format!("{value} is past what a BIGINT column holds"),
    })
}

/// A version or epoch as the port speaks it.
fn counter_from(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| PORT.undecodable(format!("{value} is not a counter")))
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

/// The roster row for `album`, read through `connection`.
async fn roster_of<C: ConnectionTrait>(
    connection: &C,
    album: &AlbumId,
) -> Result<Option<RosterRecord>, StoreError> {
    let Some(row) = connection
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT roster_version, amk_epoch, attested_by_device, received_at, document \
             FROM album_rosters WHERE album_id = $1",
            [Value::from(album.as_str().to_owned())],
        ))
        .await
        .map_err(PORT.failing("reading an album's roster"))?
    else {
        return Ok(None);
    };
    let failed = PORT.failing("reading an album's roster");
    let roster_version: i64 = row.try_get("", "roster_version").map_err(&failed)?;
    let amk_epoch: i64 = row.try_get("", "amk_epoch").map_err(&failed)?;
    let attested_by_device: String = row.try_get("", "attested_by_device").map_err(&failed)?;
    let received_at: i64 = row.try_get("", "received_at").map_err(&failed)?;
    let document: Vec<u8> = row.try_get("", "document").map_err(&failed)?;
    Ok(Some(RosterRecord {
        album_id: album.clone(),
        roster_version: counter_from(roster_version)?,
        amk_epoch: counter_from(amk_epoch)?,
        attested_by_device: Uuid::parse_str(&attested_by_device)
            .map_err(|_| PORT.undecodable(format!("`{attested_by_device}` is not a device id")))?,
        received_at: from_micros(received_at).ok_or_else(|| {
            PORT.undecodable(format!("{received_at}µs is not a representable instant"))
        })?,
        document,
    }))
}

impl MembershipStore for PostgresMembership {
    fn apply_roster(
        &self,
        roster: RosterRecord,
        members: Vec<(UserId, MemberRole)>,
    ) -> StoreFuture<'_, RosterOutcome> {
        Box::pin(async move {
            let roster_version = counter_to_column(roster.roster_version)?;
            let amk_epoch = counter_to_column(roster.amk_epoch)?;
            let transaction = begin(&self.connection).await?;

            // The critical section starts here: everything below runs under the album's lock,
            // and the lock is released with the transaction.
            transaction
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT pg_advisory_xact_lock(hashtext($1))",
                    [Value::from(roster.album_id.as_str().to_owned())],
                ))
                .await
                .map_err(PORT.failing("locking an album's roster"))?;

            let held = roster_of(&transaction, &roster.album_id).await?;
            if let Some(outcome) = precheck(held.as_ref(), &roster) {
                // Nothing to write; the rollback releases the lock.
                return Ok(outcome);
            }

            let roster = RosterRecord {
                received_at: stored(roster.received_at),
                ..roster
            };
            transaction
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "INSERT INTO album_rosters \
                       (album_id, roster_version, amk_epoch, attested_by_device, received_at, \
                        document) \
                     VALUES ($1, $2, $3, $4, $5, $6) \
                     ON CONFLICT (album_id) DO UPDATE SET \
                       roster_version = EXCLUDED.roster_version, \
                       amk_epoch = EXCLUDED.amk_epoch, \
                       attested_by_device = EXCLUDED.attested_by_device, \
                       received_at = EXCLUDED.received_at, \
                       document = EXCLUDED.document",
                    [
                        Value::from(roster.album_id.as_str().to_owned()),
                        Value::from(roster_version),
                        Value::from(amk_epoch),
                        Value::from(roster.attested_by_device.to_string()),
                        Value::from(to_micros(roster.received_at)),
                        Value::from(roster.document.clone()),
                    ],
                ))
                .await
                .map_err(PORT.failing("replacing an album's roster"))?;

            // Everyone live who is not on the new list vanished at this version and epoch. The
            // list is bound as a text array so one statement covers any roster size.
            let listed: Vec<String> = members
                .iter()
                .map(|(user, _)| user.as_str().to_owned())
                .collect();
            transaction
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "UPDATE album_members \
                     SET revoked_at_version = $2, revoked_epoch = $3 \
                     WHERE album_id = $1 AND revoked_at_version IS NULL \
                       AND NOT (user_id = ANY($4))",
                    [
                        Value::from(roster.album_id.as_str().to_owned()),
                        Value::from(roster_version),
                        Value::from(amk_epoch),
                        Value::from(listed),
                    ],
                ))
                .await
                .map_err(PORT.failing("revoking the members a roster omits"))?;

            // One statement per listed member, so a user listed twice is taken once, last entry
            // winning, exactly as the in-memory store's map does. A continuing member keeps
            // their grant and takes the new role; a new or re-admitted one gets a fresh grant.
            for (user, role) in members {
                transaction
                    .execute(Statement::from_sql_and_values(
                        DbBackend::Postgres,
                        "INSERT INTO album_members \
                           (album_id, user_id, role, since_version, granted_epoch, \
                            revoked_at_version, revoked_epoch) \
                         VALUES ($1, $2, $3, $4, $5, NULL, NULL) \
                         ON CONFLICT (album_id, user_id) DO UPDATE SET \
                           role = EXCLUDED.role, \
                           since_version = CASE \
                             WHEN album_members.revoked_at_version IS NULL \
                             THEN album_members.since_version ELSE EXCLUDED.since_version END, \
                           granted_epoch = CASE \
                             WHEN album_members.revoked_at_version IS NULL \
                             THEN album_members.granted_epoch ELSE EXCLUDED.granted_epoch END, \
                           revoked_at_version = NULL, \
                           revoked_epoch = NULL",
                        [
                            Value::from(roster.album_id.as_str().to_owned()),
                            Value::from(user.as_str().to_owned()),
                            Value::from(role_token(role).to_owned()),
                            Value::from(roster_version),
                            Value::from(amk_epoch),
                        ],
                    ))
                    .await
                    .map_err(PORT.failing("recording a roster member"))?;
            }

            commit(transaction).await?;
            tracing::info!(
                album = %roster.album_id,
                roster_version = roster.roster_version,
                amk_epoch = roster.amk_epoch,
                "an album roster was applied"
            );
            Ok(RosterOutcome::Applied(roster))
        })
    }

    fn membership<'a>(
        &'a self,
        album: &'a AlbumId,
        user: &'a UserId,
    ) -> StoreFuture<'a, Membership> {
        Box::pin(async move {
            let Some(row) = self
                .connection
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT role, granted_epoch, revoked_at_version, revoked_epoch \
                     FROM album_members WHERE album_id = $1 AND user_id = $2",
                    [
                        Value::from(album.as_str().to_owned()),
                        Value::from(user.as_str().to_owned()),
                    ],
                ))
                .await
                .map_err(PORT.failing("reading a membership"))?
            else {
                return Ok(Membership::Never);
            };
            let failed = PORT.failing("reading a membership");
            let role: String = row.try_get("", "role").map_err(&failed)?;
            // Validated on every row, revoked ones included: a stored fact is read as a fact,
            // and a token no version of this server wrote is corruption whichever row holds it.
            let role = role_from_token(&role)
                .ok_or_else(|| PORT.undecodable(format!("`{role}` is not a member role")))?;
            let granted_epoch: i64 = row.try_get("", "granted_epoch").map_err(&failed)?;
            let revoked_at_version: Option<i64> =
                row.try_get("", "revoked_at_version").map_err(&failed)?;
            let revoked_epoch: Option<i64> = row.try_get("", "revoked_epoch").map_err(&failed)?;
            Ok(match (revoked_at_version, revoked_epoch) {
                (Some(at_version), Some(at_epoch)) => Membership::Revoked(Revocation {
                    at_version: counter_from(at_version)?,
                    at_epoch: counter_from(at_epoch)?,
                }),
                (None, None) => Membership::Member {
                    role,
                    granted_epoch: counter_from(granted_epoch)?,
                },
                // The two revocation columns are written together; one without the other is a
                // row this server did not write.
                _ => {
                    return Err(
                        PORT.undecodable("a member row carries half a revocation".to_owned())
                    );
                }
            })
        })
    }

    fn current_roster<'a>(&'a self, album: &'a AlbumId) -> StoreFuture<'a, Option<RosterRecord>> {
        Box::pin(async move { roster_of(&self.connection, album).await })
    }
}

#[cfg(test)]
mod tests {
    /// The suite, against a real Postgres.
    mod postgres_conformance {
        use super::super::PostgresMembership;
        use crate::membership::MembershipStore;
        use crate::membership::conformance::{self, Harness};
        use crate::postgres::testing;

        /// A store over one container.
        #[derive(Debug)]
        struct PostgresHarness {
            members: PostgresMembership,
        }

        impl Harness for PostgresHarness {
            fn members(&self) -> &dyn MembershipStore {
                &self.members
            }
        }

        #[tokio::test]
        async fn the_postgres_membership_store_conforms() {
            let Some(database) = testing::start("the Postgres membership store").await else {
                return;
            };
            let harness = PostgresHarness {
                members: PostgresMembership::new(database.connection().clone()),
            };
            conformance::run_all(&harness).await;
        }
    }
}
