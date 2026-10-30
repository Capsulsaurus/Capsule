//! [`PostgresActivity`] — the durable activity log.
//!
//! # One table, append-only
//!
//! `activity_events` holds one row per event, ordered by a `BIGSERIAL` `seq` that is the order
//! the events were recorded in — the order the port promises. `at` is the server's clock and is
//! not trusted to order anything: two events may share an instant. `event_id` is unique, so a
//! duplicate is refused by the same statement that would have written it.
//!
//! Nothing updates a row. Rows leave only by the retention bound, which
//! [`ActivityStore::record`] applies to the account it appends to inside the same transaction:
//! by age (`at` before `retain_from`) and by count (every row older than the newest
//! [`MAX_EVENTS_PER_ACCOUNT`]). Both deletes walk the `(user_id, seq)` index of one account.

use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, Statement,
    TransactionTrait, Value,
};

use super::{ActivityEvent, ActivityKind, ActivityStore, MAX_EVENTS_PER_ACCOUNT};
use crate::postgres::error::Port;
use crate::postgres::time::{from_micros, to_micros};
use crate::store::{SessionId, StoreError, StoreFuture, UserId};

/// Which port is speaking.
const PORT: Port = Port {
    store: "activity",
    record: "ActivityEvent",
};

/// The durable activity log.
#[derive(Debug, Clone)]
pub struct PostgresActivity {
    connection: DatabaseConnection,
}

impl PostgresActivity {
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

/// The bound as the `OFFSET` that finds the newest row past it.
fn bound() -> i64 {
    i64::try_from(MAX_EVENTS_PER_ACCOUNT).unwrap_or(i64::MAX)
}

impl ActivityStore for PostgresActivity {
    fn record(&self, event: ActivityEvent, retain_from: jiff::Timestamp) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let transaction = begin(&self.connection).await?;
            let inserted = transaction
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "INSERT INTO activity_events \
                       (event_id, user_id, kind, at, request_id, session_id, subject) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7) \
                     ON CONFLICT (event_id) DO NOTHING",
                    [
                        Value::from(event.event_id.clone()),
                        Value::from(event.user_id.as_str().to_owned()),
                        Value::from(event.kind.as_str().to_owned()),
                        Value::from(to_micros(event.at)),
                        Value::from(event.request_id.clone()),
                        Value::from(event.session_id.as_ref().map(|id| id.as_str().to_owned())),
                        Value::from(event.subject.clone()),
                    ],
                ))
                .await
                .map_err(PORT.failing("recording an activity event"))?
                .rows_affected();
            if inserted == 0 {
                // Dropping the transaction rolls it back; nothing was written.
                return Err(StoreError::Rejected {
                    store: PORT.store,
                    detail: format!("event {} is already recorded", event.event_id),
                });
            }
            // By age, then by count. `seq <= NULL` is never true, so an account inside its count
            // bound — whose `OFFSET` finds no row — deletes nothing by count.
            transaction
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "DELETE FROM activity_events WHERE user_id = $1 AND (at < $2 OR seq <= ( \
                       SELECT seq FROM activity_events WHERE user_id = $1 \
                       ORDER BY seq DESC OFFSET $3 LIMIT 1))",
                    [
                        Value::from(event.user_id.as_str().to_owned()),
                        Value::from(to_micros(retain_from)),
                        Value::from(bound()),
                    ],
                ))
                .await
                .map_err(PORT.failing("pruning an activity log to its bound"))?;
            transaction
                .commit()
                .await
                .map_err(PORT.failing("committing a transaction"))?;
            tracing::debug!(
                user_id = %event.user_id,
                kind = event.kind.as_str(),
                event_id = %event.event_id,
                "an activity event was stored"
            );
            Ok(())
        })
    }

    fn recent<'a>(
        &'a self,
        user: &'a UserId,
        since: jiff::Timestamp,
        limit: usize,
    ) -> StoreFuture<'a, Vec<ActivityEvent>> {
        Box::pin(async move {
            let rows = self
                .connection
                .query_all_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT event_id, kind, at, request_id, session_id, subject \
                     FROM activity_events WHERE user_id = $1 AND at >= $2 \
                     ORDER BY seq DESC LIMIT $3",
                    [
                        Value::from(user.as_str().to_owned()),
                        Value::from(to_micros(since)),
                        Value::from(i64::try_from(limit).unwrap_or(i64::MAX)),
                    ],
                ))
                .await
                .map_err(PORT.failing("reading an activity log"))?;
            let failed = PORT.failing("reading an activity log");
            let mut events = Vec::with_capacity(rows.len());
            for row in &rows {
                let kind: String = row.try_get("", "kind").map_err(&failed)?;
                let at: i64 = row.try_get("", "at").map_err(&failed)?;
                let session_id: Option<String> = row.try_get("", "session_id").map_err(&failed)?;
                events.push(ActivityEvent {
                    event_id: row.try_get("", "event_id").map_err(&failed)?,
                    user_id: user.clone(),
                    kind: ActivityKind::from_token(&kind).ok_or_else(|| {
                        PORT.undecodable(format!("`{kind}` is not an activity kind"))
                    })?,
                    at: from_micros(at).ok_or_else(|| {
                        PORT.undecodable(format!("{at}µs is not a representable instant"))
                    })?,
                    request_id: row.try_get("", "request_id").map_err(&failed)?,
                    session_id: session_id.map(SessionId::new),
                    subject: row.try_get("", "subject").map_err(&failed)?,
                });
            }
            Ok(events)
        })
    }
}

#[cfg(test)]
mod tests;
