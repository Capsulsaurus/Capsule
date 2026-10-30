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
//! # Byte order, not the database's collation
//!
//! Every `ORDER BY` names `COLLATE "C"`, so the rows come back in the byte order the in-memory
//! double's `BTreeMap` and [`JobKey`]'s `Ord` give, whatever locale the cluster was initialised
//! under.

use jiff::Timestamp;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, QueryResult, Statement, Value};

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
        let rows = self
            .connection
            .query_all_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                values,
            ))
            .await
            .map_err(SUBSCRIPTIONS.failing(doing))?;
        rows.iter().map(subscription_from).collect()
    }
}

impl SubscriptionStore for PostgresSubscriptions {
    fn put(&self, subscription: Subscription) -> StoreFuture<'_, PutOutcome> {
        Box::pin(async move {
            let row = self
                .connection
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
            let held = self
                .select(
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
            // The upsert declined because a row held the key, so the row is there unless a
            // removal raced in between: the outcome is then unknown, and saying so is the
            // honest answer.
            held.into_iter()
                .next()
                .map(PutOutcome::Kept)
                .ok_or_else(|| StoreError::Rejected {
                    store: SUBSCRIPTIONS.store,
                    detail: "the subscription was removed while it was being recorded".to_owned(),
                })
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
