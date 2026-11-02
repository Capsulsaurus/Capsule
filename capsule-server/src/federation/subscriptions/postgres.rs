//! [`PostgresSubscriptions`]: the durable [`SubscriptionStore`] over `federation_subscriptions`.
//!
//! # One statement decides a put
//!
//! `INSERT … ON CONFLICT DO UPDATE … WHERE held.expires_at < posted.expires_at RETURNING`, so
//! "replace only a shorter-lived token" is decided under the row lock the upsert takes and two
//! concurrent posts cannot both believe they replaced. `xmax = 0` on the returned row tells an
//! insert from an update. No returned row is the `WHERE` declining, and the held row is read
//! back to answer [`PutOutcome::Kept`].
//!
//! # A capped put counts under the subscriber's lock
//!
//! [`SubscriptionStore::put_within`] runs in a transaction that first takes
//! `pg_advisory_xact_lock(hashtext('federation_subscriptions:' || subscriber))`, then counts the
//! subscriber's rows and runs the same upsert, as the album key escrows bound an account. Two
//! capped puts by one subscriber are serialized, and the second's count, a fresh statement under
//! read committed, sees the first's committed row; capped puts by different subscribers never
//! wait on each other, and nothing else does either. The capped put is the only write that
//! records a key in production, so it is the only one that takes the lock:
//! [`SubscriptionStore::remove`] only lowers the count, and [`SubscriptionStore::renew`] never
//! records a key. Two subscribers whose names collide under `hashtext` share a lock, which costs
//! a wait and nothing else.
//!
//! # A renewal is one `UPDATE`
//!
//! [`SubscriptionStore::renew`] is `UPDATE … WHERE key AND expires_at < posted RETURNING`, so a
//! row removed before it runs is not re-created. No returned row is read back: a held row is
//! [`PutOutcome::Kept`], and none is the key no longer held.
//!
//! # Byte order, not the database's collation
//!
//! Every `ORDER BY` names `COLLATE "C"`, so the rows come back in the byte order the in-memory
//! double's `BTreeMap` and [`JobKey`]'s `Ord` give, whatever locale the cluster was initialised
//! under.

use jiff::Timestamp;
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, QueryResult, Statement,
    TransactionTrait as _, Value,
};

use super::{CapabilityToken, PutOutcome, Subscription, SubscriptionStore};
use crate::federation::PeerId;
use crate::federation::capability::Scope;
use crate::federation::pull::JobKey;
use crate::postgres::error::Port;
use crate::postgres::time::{from_micros, to_micros};
use crate::store::{AlbumId, StoreError, StoreFuture, UserId};

/// Which port is speaking, for every error the adapter raises.
const SUBSCRIPTIONS: Port = Port {
    store: "subscriptions",
    record: "Subscription",
};

/// The columns every read selects, in the order [`subscription_from`] decodes them.
const COLUMNS: &str = "subscriber_id, peer_id, album_id, scope, jti, token, issued_at, \
                       expires_at, min_protocol_version, subscribed_at, updated_at";

/// An instant as the port speaks it.
fn instant(micros: i64) -> Result<Timestamp, StoreError> {
    from_micros(micros).ok_or_else(|| {
        SUBSCRIPTIONS.undecodable(format!("{micros}µs is not a representable instant"))
    })
}

/// Decode one row of [`COLUMNS`].
fn subscription_from(row: &QueryResult) -> Result<Subscription, StoreError> {
    let failed = SUBSCRIPTIONS.failing("reading a subscription");
    let subscriber: String = row.try_get("", "subscriber_id").map_err(&failed)?;
    let peer: String = row.try_get("", "peer_id").map_err(&failed)?;
    let album: String = row.try_get("", "album_id").map_err(&failed)?;
    let scope: String = row.try_get("", "scope").map_err(&failed)?;
    let jti: String = row.try_get("", "jti").map_err(&failed)?;
    let token: String = row.try_get("", "token").map_err(&failed)?;
    let issued_at: i64 = row.try_get("", "issued_at").map_err(&failed)?;
    let expires_at: i64 = row.try_get("", "expires_at").map_err(&failed)?;
    let min_protocol_version: String = row.try_get("", "min_protocol_version").map_err(&failed)?;
    let subscribed_at: i64 = row.try_get("", "subscribed_at").map_err(&failed)?;
    let updated_at: i64 = row.try_get("", "updated_at").map_err(&failed)?;
    Ok(Subscription {
        subscriber: UserId::new(subscriber),
        peer: PeerId::new(peer),
        album: AlbumId::new(album),
        scope: Scope::from_token(&scope)
            .ok_or_else(|| SUBSCRIPTIONS.undecodable(format!("`{scope}` is not a scope")))?,
        jti,
        token: CapabilityToken::new(token),
        issued_at: instant(issued_at)?,
        expires_at: instant(expires_at)?,
        min_protocol_version,
        subscribed_at: instant(subscribed_at)?,
        updated_at: instant(updated_at)?,
    })
}

/// The durable subscription store.
#[derive(Debug, Clone)]
pub struct PostgresSubscriptions {
    connection: DatabaseConnection,
}

impl PostgresSubscriptions {
    /// A store over `connection`.
    pub fn new(connection: DatabaseConnection) -> Self {
        Self { connection }
    }

    /// Every row `sql` selects with `values`, decoded.
    async fn select(
        &self,
        sql: String,
        values: Vec<Value>,
        doing: &'static str,
    ) -> Result<Vec<Subscription>, StoreError> {
        select_on(&self.connection, sql, values, doing).await
    }
}

/// Every row `sql` selects with `values` on `connection`, decoded.
async fn select_on<C: ConnectionTrait>(
    connection: &C,
    sql: String,
    values: Vec<Value>,
    doing: &'static str,
) -> Result<Vec<Subscription>, StoreError> {
    let rows = connection
        .query_all_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            values,
        ))
        .await
        .map_err(SUBSCRIPTIONS.failing(doing))?;
    rows.iter().map(subscription_from).collect()
}

/// Whether `subscriber` may record a new key under `limit`: the count, read on `transaction`
/// after the subscriber's lock is taken, so no concurrent capped put moves it before the caller's
/// own.
async fn admits_new_key(
    transaction: &DatabaseTransaction,
    subscription: &Subscription,
    limit: u32,
) -> Result<bool, StoreError> {
    let doing = "counting an account's subscriptions";
    // The account's critical section starts here and ends with the transaction, so the count
    // below is read and the row written as one step against every concurrent capped put by the
    // same subscriber.
    transaction
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT pg_advisory_xact_lock(hashtext('federation_subscriptions:' || $1))",
            [Value::from(subscription.subscriber.as_str().to_owned())],
        ))
        .await
        .map_err(SUBSCRIPTIONS.failing("locking an account's subscriptions"))?;
    let row = transaction
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT \
             EXISTS (SELECT 1 FROM federation_subscriptions \
                     WHERE subscriber_id = $1 AND peer_id = $2 AND album_id = $3) AS held, \
             (SELECT COUNT(*) FROM federation_subscriptions WHERE subscriber_id = $1) AS count",
            [
                Value::from(subscription.subscriber.as_str().to_owned()),
                Value::from(subscription.peer.as_str().to_owned()),
                Value::from(subscription.album.as_str().to_owned()),
            ],
        ))
        .await
        .map_err(SUBSCRIPTIONS.failing(doing))?
        .ok_or_else(|| SUBSCRIPTIONS.undecodable("the count answered no row".to_owned()))?;
    let held: bool = row
        .try_get("", "held")
        .map_err(SUBSCRIPTIONS.failing(doing))?;
    let count: i64 = row
        .try_get("", "count")
        .map_err(SUBSCRIPTIONS.failing(doing))?;
    Ok(held || count < i64::from(limit))
}

/// [`SubscriptionStore::put`] on `connection`: the pool, or [`put_within`]'s transaction.
///
/// [`put_within`]: SubscriptionStore::put_within
async fn put_on<C: ConnectionTrait>(
    connection: &C,
    subscription: Subscription,
) -> Result<PutOutcome, StoreError> {
    let row = connection
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "INSERT INTO federation_subscriptions ({COLUMNS}) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
                 ON CONFLICT (subscriber_id, peer_id, album_id) DO UPDATE SET \
                 scope = EXCLUDED.scope, jti = EXCLUDED.jti, token = EXCLUDED.token, \
                 issued_at = EXCLUDED.issued_at, expires_at = EXCLUDED.expires_at, \
                 min_protocol_version = EXCLUDED.min_protocol_version, \
                 updated_at = EXCLUDED.updated_at \
                 WHERE federation_subscriptions.expires_at < EXCLUDED.expires_at \
                 RETURNING (xmax = 0) AS inserted, {COLUMNS}"
            ),
            [
                Value::from(subscription.subscriber.as_str().to_owned()),
                Value::from(subscription.peer.as_str().to_owned()),
                Value::from(subscription.album.as_str().to_owned()),
                Value::from(subscription.scope.as_str().to_owned()),
                Value::from(subscription.jti.clone()),
                Value::from(subscription.token.expose().to_owned()),
                Value::from(to_micros(subscription.issued_at)),
                Value::from(to_micros(subscription.expires_at)),
                Value::from(subscription.min_protocol_version.clone()),
                Value::from(to_micros(subscription.subscribed_at)),
                Value::from(to_micros(subscription.updated_at)),
            ],
        ))
        .await
        .map_err(SUBSCRIPTIONS.failing("recording a subscription"))?;
    if let Some(row) = row {
        let inserted: bool = row
            .try_get("", "inserted")
            .map_err(SUBSCRIPTIONS.failing("recording a subscription"))?;
        let stored = subscription_from(&row)?;
        return Ok(if inserted {
            PutOutcome::Created(stored)
        } else {
            PutOutcome::Replaced(stored)
        });
    }
    // The upsert declined because a row held the key, so the row is there unless a removal
    // raced in between: the outcome is then unknown, and saying so is the honest answer.
    held_on(connection, &subscription)
        .await?
        .map(PutOutcome::Kept)
        .ok_or_else(|| StoreError::Rejected {
            store: SUBSCRIPTIONS.store,
            detail: "the subscription was removed while it was being recorded".to_owned(),
        })
}

/// The row under `subscription`'s key on `connection`, if one is held.
async fn held_on<C: ConnectionTrait>(
    connection: &C,
    subscription: &Subscription,
) -> Result<Option<Subscription>, StoreError> {
    let held = select_on(
        connection,
        format!(
            "SELECT {COLUMNS} FROM federation_subscriptions \
             WHERE subscriber_id = $1 AND peer_id = $2 AND album_id = $3"
        ),
        vec![
            Value::from(subscription.subscriber.as_str().to_owned()),
            Value::from(subscription.peer.as_str().to_owned()),
            Value::from(subscription.album.as_str().to_owned()),
        ],
        "reading a kept subscription",
    )
    .await?;
    Ok(held.into_iter().next())
}

impl SubscriptionStore for PostgresSubscriptions {
    fn put(&self, subscription: Subscription) -> StoreFuture<'_, PutOutcome> {
        Box::pin(async move { put_on(&self.connection, subscription).await })
    }

    fn put_within(
        &self,
        subscription: Subscription,
        limit: u32,
    ) -> StoreFuture<'_, Option<PutOutcome>> {
        Box::pin(async move {
            let transaction = self
                .connection
                .begin()
                .await
                .map_err(SUBSCRIPTIONS.failing("opening a capped subscription write"))?;
            if !admits_new_key(&transaction, &subscription, limit).await? {
                transaction
                    .rollback()
                    .await
                    .map_err(SUBSCRIPTIONS.failing("closing a refused subscription write"))?;
                return Ok(None);
            }
            let outcome = put_on(&transaction, subscription).await?;
            transaction
                .commit()
                .await
                .map_err(SUBSCRIPTIONS.failing("committing a subscription"))?;
            Ok(Some(outcome))
        })
    }

    fn renew(&self, subscription: Subscription) -> StoreFuture<'_, Option<PutOutcome>> {
        Box::pin(async move {
            let doing = "renewing a subscription";
            let row = self
                .connection
                .query_one_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    format!(
                        "UPDATE federation_subscriptions SET \
                         scope = $4, jti = $5, token = $6, issued_at = $7, expires_at = $8, \
                         min_protocol_version = $9, updated_at = $10 \
                         WHERE subscriber_id = $1 AND peer_id = $2 AND album_id = $3 \
                         AND expires_at < $8 \
                         RETURNING {COLUMNS}"
                    ),
                    [
                        Value::from(subscription.subscriber.as_str().to_owned()),
                        Value::from(subscription.peer.as_str().to_owned()),
                        Value::from(subscription.album.as_str().to_owned()),
                        Value::from(subscription.scope.as_str().to_owned()),
                        Value::from(subscription.jti.clone()),
                        Value::from(subscription.token.expose().to_owned()),
                        Value::from(to_micros(subscription.issued_at)),
                        Value::from(to_micros(subscription.expires_at)),
                        Value::from(subscription.min_protocol_version.clone()),
                        Value::from(to_micros(subscription.updated_at)),
                    ],
                ))
                .await
                .map_err(SUBSCRIPTIONS.failing(doing))?;
            if let Some(row) = row {
                return Ok(Some(PutOutcome::Replaced(subscription_from(&row)?)));
            }
            Ok(held_on(&self.connection, &subscription)
                .await?
                .map(PutOutcome::Kept))
        })
    }

    fn of_subscriber<'a>(&'a self, subscriber: &'a UserId) -> StoreFuture<'a, Vec<Subscription>> {
        Box::pin(async move {
            self.select(
                format!(
                    "SELECT {COLUMNS} FROM federation_subscriptions WHERE subscriber_id = $1 \
                     ORDER BY peer_id COLLATE \"C\", album_id COLLATE \"C\""
                ),
                vec![Value::from(subscriber.as_str().to_owned())],
                "listing an account's subscriptions",
            )
            .await
        })
    }

    fn remove<'a>(
        &'a self,
        subscriber: &'a UserId,
        peer: &'a PeerId,
        album: &'a AlbumId,
    ) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let deleted = self
                .connection
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "DELETE FROM federation_subscriptions \
                     WHERE subscriber_id = $1 AND peer_id = $2 AND album_id = $3",
                    [
                        Value::from(subscriber.as_str().to_owned()),
                        Value::from(peer.as_str().to_owned()),
                        Value::from(album.as_str().to_owned()),
                    ],
                ))
                .await
                .map_err(SUBSCRIPTIONS.failing("removing a subscription"))?;
            Ok(deleted.rows_affected() > 0)
        })
    }

    fn of_job<'a>(&'a self, job: &'a JobKey) -> StoreFuture<'a, Vec<Subscription>> {
        Box::pin(async move {
            self.select(
                format!(
                    "SELECT {COLUMNS} FROM federation_subscriptions \
                     WHERE peer_id = $1 AND album_id = $2 ORDER BY subscriber_id COLLATE \"C\""
                ),
                vec![
                    Value::from(job.peer.as_str().to_owned()),
                    Value::from(job.album.as_str().to_owned()),
                ],
                "listing a job's subscriptions",
            )
            .await
        })
    }

    fn live_jobs(&self, now: Timestamp) -> StoreFuture<'_, Vec<JobKey>> {
        Box::pin(async move {
            let rows = self
                .connection
                .query_all_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT peer_id, album_id FROM federation_subscriptions \
                     WHERE expires_at > $1 \
                     GROUP BY peer_id, album_id \
                     ORDER BY peer_id COLLATE \"C\", album_id COLLATE \"C\"",
                    [Value::from(to_micros(now))],
                ))
                .await
                .map_err(SUBSCRIPTIONS.failing("listing the live jobs"))?;
            let failed = SUBSCRIPTIONS.failing("listing the live jobs");
            rows.iter()
                .map(|row| {
                    let peer: String = row.try_get("", "peer_id").map_err(&failed)?;
                    let album: String = row.try_get("", "album_id").map_err(&failed)?;
                    Ok(JobKey::new(PeerId::new(peer), AlbumId::new(album)))
                })
                .collect()
        })
    }
}

#[cfg(test)]
mod tests;
