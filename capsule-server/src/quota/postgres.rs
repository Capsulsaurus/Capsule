//! [`PostgresQuota`] — the durable quota ledger (#402).
//!
//! # The total is derived, and the clock is not
//!
//! `quota_attributions` holds one row per charged content address. A user's `used` is
//! `SUM(size)` over their rows and is **never** a stored column, for the reason
//! [`crate::index::AssetIndex::reference_count`] is a query: a stored total is a second copy of
//! a derivable fact, and one that drifts low hands somebody free storage.
//!
//! What cannot be derived is *when* an account crossed the hard limit and has not been under it
//! since — a current total says nothing about how long it has been that total. That instant is
//! one of the two columns of `quota_usage`; the other is when an operator suspended the account
//! (`suspended_since`, #705), which no charge or release touches.
//!
//! # Pulled blobs are read, not charged (#1162)
//!
//! An account's pulled metadata blobs ([`super`], "Pulled blobs") are part of the same derived
//! total: the usage statement sums the distinct addresses the federation pull holds for the albums the
//! account subscribes to (`federation_subscriptions` joined with
//! `federation_pulled_metadata_blobs`, along both tables' primary keys), less the ones it is
//! itself attributed here. No row of this ledger records them, so the charge is written by the
//! pulled page's own commit, released by the unsubscribe or forget that removes the holding, and
//! correct for every page committed before this ledger counted them, with nothing to backfill.
//!
//! # Why `charge` is a transaction and not one statement
//!
//! The port's requirement is that the check and the debit are atomic against two concurrent
//! sessions for one address, and the `ON CONFLICT (address) DO NOTHING` insert is exactly that on
//! its own: the primary key decides, and the row count is the answer. What needs the transaction
//! is the *second* half — after a successful debit the adapter has to re-total the account and
//! stamp `over_since` if that debit crossed the limit, and a crash between the two would leave an
//! account over its limit with no crossing recorded. The port models that state
//! (`state_of` treats "over with no recorded crossing" as newly over rather than expired,
//! deliberately, so the missing timestamp cannot lock somebody out of the writes that free
//! space) — but leaving it reachable when one statement away is a choice, not a fallback.
//!
//! The transaction also holds the account's `quota_usage` row (`FOR UPDATE`) from the debit to
//! the commit, so concurrent charges for one account total it one after another rather than
//! each in a snapshot that misses the others (#513).

use jiff::Timestamp;
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, Statement,
    TransactionTrait, Value,
};

use super::{ChargeOutcome, ExpiredReservation, QuotaLimits, QuotaStore, StoredUsage};
use crate::blob::ContentAddress;
use crate::postgres::error::Port;
use crate::postgres::time::{from_micros, to_micros};
use crate::store::{StoreError, StoreFuture, UserId};

/// Which port is speaking, for every error this adapter raises.
const PORT: Port = Port {
    store: "quota",
    record: "StoredUsage",
};

/// The durable quota ledger.
#[derive(Debug, Clone)]
pub struct PostgresQuota {
    connection: DatabaseConnection,
}

impl PostgresQuota {
    /// A ledger over `connection`.
    pub fn new(connection: DatabaseConnection) -> Self {
        Self { connection }
    }
}

/// A byte count as the column holds it.
fn size_to_column(size: u64) -> Result<i64, StoreError> {
    i64::try_from(size).map_err(|_| StoreError::Rejected {
        store: PORT.store,
        detail: format!("{size} bytes is past what a BIGINT column holds"),
    })
}

/// A byte count as the port speaks it.
fn size_from(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| PORT.undecodable(format!("{value} is not a byte count")))
}

/// The account's total and crossing, read through `connection`.
///
/// One statement with scalar subqueries rather than several round trips, so the total and the
/// clock come from one snapshot: read separately, a concurrent release between them could report
/// a total that is already under the limit beside a crossing that has already been cleared.
///
/// The total is the account's attributions plus its pulled holdings: the distinct addresses in
/// `federation_pulled_metadata_blobs` of every album it subscribes to, live or lapsed, that it is
/// not itself attributed (the module's docs, "Pulled blobs").
async fn usage_of<C: ConnectionTrait>(
    connection: &C,
    user: &UserId,
) -> Result<StoredUsage, StoreError> {
    let read = connection
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT \
               (SELECT COALESCE(SUM(size), 0)::bigint FROM quota_attributions WHERE user_id = $1) \
                 AS attributed, \
               (SELECT COALESCE(SUM(held.size), 0)::bigint FROM ( \
                  SELECT b.address, MAX(b.size) AS size \
                  FROM federation_subscriptions s \
                  JOIN federation_pulled_metadata_blobs b \
                    ON b.peer_id = s.peer_id AND b.album_id = s.album_id \
                  WHERE s.subscriber_id = $1 \
                    AND NOT EXISTS (SELECT 1 FROM quota_attributions a \
                                    WHERE a.address = b.address AND a.user_id = $1) \
                  GROUP BY b.address) held) AS pulled, \
               (SELECT over_since FROM quota_usage WHERE user_id = $1) AS over_since, \
               (SELECT suspended_since FROM quota_usage WHERE user_id = $1) AS suspended_since",
            [Value::from(user.as_str().to_owned())],
        ))
        .await
        .map_err(PORT.failing("reading an account's usage"))?
        .ok_or_else(|| StoreError::Rejected {
            store: PORT.store,
            detail: "the usage query returned no row".to_owned(),
        })?;
    let failed = PORT.failing("reading an account's usage");
    let attributed: i64 = read.try_get("", "attributed").map_err(&failed)?;
    let pulled: i64 = read.try_get("", "pulled").map_err(&failed)?;
    let over_since: Option<i64> = read.try_get("", "over_since").map_err(&failed)?;
    let suspended_since: Option<i64> = read.try_get("", "suspended_since").map_err(&failed)?;
    let (attributed, pulled) = (size_from(attributed)?, size_from(pulled)?);
    tracing::trace!(%user, attributed, pulled, "an account's usage");
    Ok(StoredUsage {
        used: attributed.saturating_add(pulled),
        over_since: over_since.map(instant_from).transpose()?,
        suspended_since: suspended_since.map(instant_from).transpose()?,
    })
}

/// An instant as the port speaks it.
fn instant_from(micros: i64) -> Result<Timestamp, StoreError> {
    from_micros(micros)
        .ok_or_else(|| PORT.undecodable(format!("{micros}µs is not a representable instant")))
}

/// Give an account's bytes back, and stop its over-limit clock.
///
/// The clock is cleared **unconditionally**, exactly as the in-memory ledger's `credit` does, and
/// that is the contract rather than an approximation: an account that is still over after a
/// release gets a *fresh* window rather than inheriting a running one. Two copies of this rule
/// would eventually disagree, which is why the in-memory adapter has one helper and this has one
/// statement.
async fn stop_the_clock(
    transaction: &DatabaseTransaction,
    user: &UserId,
) -> Result<(), StoreError> {
    transaction
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE quota_usage SET over_since = NULL WHERE user_id = $1",
            [Value::from(user.as_str().to_owned())],
        ))
        .await
        .map_err(PORT.failing("clearing an account's over-limit clock"))?;
    Ok(())
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

impl PostgresQuota {
    /// Attribute `size` bytes at `address` to `user`, unless the address is attributed already,
    /// carrying `reserved_until` as the row's reservation deadline (`None` for a settled charge).
    ///
    /// The one body [`QuotaStore::charge`] and [`QuotaStore::reserve`] share.
    async fn attribute(
        &self,
        user: &UserId,
        address: &ContentAddress,
        size: u64,
        at: Timestamp,
        reserved_until: Option<Timestamp>,
        limits: QuotaLimits,
    ) -> Result<ChargeOutcome, StoreError> {
        let size = size_to_column(size)?;
        let transaction = begin(&self.connection).await?;

        // The primary key decides, and the row count is the answer. Two concurrent sessions
        // for one address cannot both read "unattributed" and both debit, because neither
        // reads: they both insert, and one of them affects no row.
        let debited = transaction
            .execute_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO quota_attributions (address, user_id, size, charged_at, reserved_until) \
                 VALUES ($1, $2, $3, $4, $5) ON CONFLICT (address) DO NOTHING",
                [
                    Value::from(address.as_str().to_owned()),
                    Value::from(user.as_str().to_owned()),
                    Value::from(size),
                    Value::from(to_micros(at)),
                    Value::from(reserved_until.map(to_micros)),
                ],
            ))
            .await
            .map_err(PORT.failing("charging an address"))?;
        if debited.rows_affected() == 0 {
            // Already attributed — to this account or to another, and the port answers one
            // value for both so a quota endpoint cannot become a cross-tenant oracle.
            return Ok(ChargeOutcome::AlreadyAttributed);
        }

        // The account's row in `quota_usage` exists from its first charge onwards, and holds
        // nothing but the clock.
        transaction
            .execute_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO quota_usage (user_id, over_since) VALUES ($1, NULL) \
                 ON CONFLICT (user_id) DO NOTHING",
                [Value::from(user.as_str().to_owned())],
            ))
            .await
            .map_err(PORT.failing("opening an account's usage row"))?;

        // Serialise this account's charges on its usage row (#513). Without the lock, two
        // charges for different addresses each total the account in a snapshot that misses
        // the other's uncommitted debit: both report a total under the limit, the upload
        // path admits both, and neither stamps the crossing their sum made. Holding the row
        // until commit means the total below is read only after every earlier charge for
        // this account has committed — a READ COMMITTED statement takes its snapshot when it
        // starts, which is after this lock is granted.
        //
        // Taken *after* the attribution insert, and that order is what keeps it free of
        // deadlock: the insert can wait on another transaction's uncommitted attribution, but
        // a transaction holding this lock waits on nothing further, so no cycle can form.
        transaction
            .execute_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT 1 FROM quota_usage WHERE user_id = $1 FOR UPDATE",
                [Value::from(user.as_str().to_owned())],
            ))
            .await
            .map_err(PORT.failing("locking an account's usage row"))?;
        tracing::trace!(%user, "holding an account's usage row for a charge");

        let used = usage_of(&transaction, user).await?.used;
        if used >= limits.hard_limit {
            // `WHERE over_since IS NULL` is what stamps the crossing **once**: a later charge
            // while still over must not restamp it, or the grace window would never expire
            // for an account that keeps trying to upload.
            let stamped = transaction
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "UPDATE quota_usage SET over_since = $2 \
                     WHERE user_id = $1 AND over_since IS NULL",
                    [
                        Value::from(user.as_str().to_owned()),
                        Value::from(to_micros(at)),
                    ],
                ))
                .await
                .map_err(PORT.failing("stamping an account's hard-limit crossing"))?;
            if stamped.rows_affected() == 1 {
                tracing::info!(%user, used, "an account crossed its hard quota limit");
            }
        }

        commit(transaction).await?;
        Ok(ChargeOutcome::Charged { used })
    }
}

impl QuotaStore for PostgresQuota {
    fn usage<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, StoredUsage> {
        Box::pin(async move { usage_of(&self.connection, user).await })
    }

    fn charge<'a>(
        &'a self,
        user: &'a UserId,
        address: &'a ContentAddress,
        size: u64,
        at: Timestamp,
        limits: QuotaLimits,
    ) -> StoreFuture<'a, ChargeOutcome> {
        Box::pin(self.attribute(user, address, size, at, None, limits))
    }

    fn reserve<'a>(
        &'a self,
        user: &'a UserId,
        address: &'a ContentAddress,
        size: u64,
        at: Timestamp,
        until: Timestamp,
        limits: QuotaLimits,
    ) -> StoreFuture<'a, ChargeOutcome> {
        Box::pin(self.attribute(user, address, size, at, Some(until), limits))
    }

    fn settle<'a>(&'a self, address: &'a ContentAddress) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let address = Value::from(address.as_str().to_owned());
            // Two statements, deliberately not one. The update waits on a sweep's delete of the
            // same reservation and, once that commits, finds no row — but a read in the same
            // statement would still see the row in the snapshot taken before the wait, and
            // answer "attributed" for a charge that is gone. The read below is a new statement,
            // so it sees every delete the update waited on.
            let settled = self
                .connection
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "UPDATE quota_attributions SET reserved_until = NULL \
                     WHERE address = $1 AND reserved_until IS NOT NULL",
                    [address.clone()],
                ))
                .await
                .map_err(PORT.failing("settling a reservation"))?;
            if settled.rows_affected() == 1 {
                return Ok(true);
            }
            // Nothing reserved: already settled, which no sweep releases, or not attributed.
            let held = self
                .connection
                .query_one_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT 1 AS held FROM quota_attributions WHERE address = $1",
                    [address],
                ))
                .await
                .map_err(PORT.failing("reading a settled attribution"))?;
            Ok(held.is_some())
        })
    }

    fn expired_reservations(
        &self,
        now: Timestamp,
        limit: usize,
    ) -> StoreFuture<'_, Vec<ExpiredReservation>> {
        Box::pin(async move {
            // A limit past what a BIGINT holds is no limit at all.
            let limit = i64::try_from(limit).unwrap_or(i64::MAX);
            let rows = self
                .connection
                .query_all_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT user_id, address, size, reserved_until FROM quota_attributions \
                     WHERE reserved_until IS NOT NULL AND reserved_until <= $1 \
                     ORDER BY reserved_until, address LIMIT $2",
                    [Value::from(to_micros(now)), Value::from(limit)],
                ))
                .await
                .map_err(PORT.failing("listing expired reservations"))?;
            let failed = PORT.failing("reading an expired reservation");
            rows.into_iter()
                .map(|row| {
                    let user: String = row.try_get("", "user_id").map_err(&failed)?;
                    let address: String = row.try_get("", "address").map_err(&failed)?;
                    let size: i64 = row.try_get("", "size").map_err(&failed)?;
                    let until: i64 = row.try_get("", "reserved_until").map_err(&failed)?;
                    Ok(ExpiredReservation {
                        user: UserId::new(user),
                        address: ContentAddress::parse(&address).map_err(|error| {
                            PORT.undecodable(format!("{address} is not a content address: {error}"))
                        })?,
                        size: size_from(size)?,
                        reserved_until: instant_from(until)?,
                    })
                })
                .collect()
        })
    }

    fn extend_reservation<'a>(
        &'a self,
        address: &'a ContentAddress,
        until: Timestamp,
    ) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let extended = self
                .connection
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "UPDATE quota_attributions SET reserved_until = GREATEST(reserved_until, $2) \
                     WHERE address = $1 AND reserved_until IS NOT NULL",
                    [
                        Value::from(address.as_str().to_owned()),
                        Value::from(to_micros(until)),
                    ],
                ))
                .await
                .map_err(PORT.failing("extending a reservation"))?;
            Ok(extended.rows_affected() == 1)
        })
    }

    fn release_expired<'a>(
        &'a self,
        user: &'a UserId,
        address: &'a ContentAddress,
        now: Timestamp,
    ) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let transaction = begin(&self.connection).await?;
            // The condition is the whole guard: a row a finalizer settled, or a sweep extended,
            // no longer matches, and a row already released matches nothing. So the release is
            // idempotent and never frees a charge a committed blob holds.
            let released = transaction
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "DELETE FROM quota_attributions \
                     WHERE address = $1 AND user_id = $2 \
                       AND reserved_until IS NOT NULL AND reserved_until <= $3",
                    [
                        Value::from(address.as_str().to_owned()),
                        Value::from(user.as_str().to_owned()),
                        Value::from(to_micros(now)),
                    ],
                ))
                .await
                .map_err(PORT.failing("releasing an expired reservation"))?;
            if released.rows_affected() == 0 {
                return Ok(false);
            }
            stop_the_clock(&transaction, user).await?;
            commit(transaction).await?;
            Ok(true)
        })
    }

    fn release<'a>(
        &'a self,
        user: &'a UserId,
        address: &'a ContentAddress,
    ) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let transaction = begin(&self.connection).await?;
            // Scoped to the account in the `DELETE` itself. Releasing somebody else's
            // attribution would let one account free bytes off another's ledger — and a
            // read-then-check would answer whether the address was attributed at all, which is
            // the disclosure the port refuses.
            let released = transaction
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "DELETE FROM quota_attributions WHERE address = $1 AND user_id = $2",
                    [
                        Value::from(address.as_str().to_owned()),
                        Value::from(user.as_str().to_owned()),
                    ],
                ))
                .await
                .map_err(PORT.failing("releasing an attribution"))?;
            if released.rows_affected() == 0 {
                return Ok(false);
            }
            stop_the_clock(&transaction, user).await?;
            commit(transaction).await?;
            Ok(true)
        })
    }

    fn release_attribution<'a>(
        &'a self,
        address: &'a ContentAddress,
    ) -> StoreFuture<'a, Option<(UserId, u64)>> {
        Box::pin(async move {
            let transaction = begin(&self.connection).await?;
            // The collector's release: a sweep knows an address and nothing else, so
            // the ledger names the account rather than the caller guessing one. `RETURNING` is
            // what makes the delete and the answer one operation.
            let released = transaction
                .query_one_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "DELETE FROM quota_attributions WHERE address = $1 RETURNING user_id, size",
                    [Value::from(address.as_str().to_owned())],
                ))
                .await
                .map_err(PORT.failing("releasing an attribution by address"))?;
            let Some(released) = released else {
                // The ordinary case for a blob the ledger never saw, and not an error: a sweep
                // that treated it as one would stall on the first.
                return Ok(None);
            };
            let failed = PORT.failing("reading a released attribution");
            let owner: String = released.try_get("", "user_id").map_err(&failed)?;
            let size: i64 = released.try_get("", "size").map_err(&failed)?;
            let owner = UserId::new(owner);
            stop_the_clock(&transaction, &owner).await?;
            commit(transaction).await?;
            let size = size_from(size)?;
            tracing::info!(
                user = %owner,
                %address,
                size,
                "a swept blob's bytes were credited back to the account they were charged to"
            );
            Ok(Some((owner, size)))
        })
    }

    fn suspend<'a>(&'a self, user: &'a UserId, at: Timestamp) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            // One statement: the row is created for an account the ledger has never charged,
            // and an existing suspension keeps its instant because the update's `WHERE` skips
            // it — so the row count is the answer, as `charge`'s is.
            let suspended = self
                .connection
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "INSERT INTO quota_usage (user_id, over_since, suspended_since) \
                     VALUES ($1, NULL, $2) \
                     ON CONFLICT (user_id) DO UPDATE SET suspended_since = EXCLUDED.suspended_since \
                     WHERE quota_usage.suspended_since IS NULL",
                    [
                        Value::from(user.as_str().to_owned()),
                        Value::from(to_micros(at)),
                    ],
                ))
                .await
                .map_err(PORT.failing("suspending an account's quota"))?;
            Ok(suspended.rows_affected() == 1)
        })
    }

    fn unsuspend<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let lifted = self
                .connection
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "UPDATE quota_usage SET suspended_since = NULL \
                     WHERE user_id = $1 AND suspended_since IS NOT NULL",
                    [Value::from(user.as_str().to_owned())],
                ))
                .await
                .map_err(PORT.failing("lifting an account's quota suspension"))?;
            Ok(lifted.rows_affected() == 1)
        })
    }
}

#[cfg(test)]
mod tests;
