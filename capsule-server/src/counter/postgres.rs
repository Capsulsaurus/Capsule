//! [`PostgresCounters`] — the counter port on `rate_counters`, one row per live window.
//!
//! # One transaction per hit, serialized per key
//!
//! A hit takes a transaction-scoped advisory lock on its key before it reads the window, so two
//! hits on one key are one after the other and neither reads a count the other is about to
//! change — the read-then-write the port forbids. Opening a **new** window additionally takes
//! the key's partition lock and counts the partition's live windows, which is how the ceiling
//! holds under racing first hits. The order is always key lock, then partition lock, and nothing
//! takes them the other way round, so no two hits can deadlock.
//!
//! Every counter here is security-relevant or low-rate: replay nonces, enrollment, TOTP, OIDC,
//! registration, share and drop links, federation budgets, and the per-create upload budgets.
//! The two per-chunk throughput budgets are routed past this adapter by
//! [`super::split::SplitCounters`] (ADR-0015), so commits here stay synchronous.

use jiff::Timestamp;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait, Value};

use super::{Budget, COUNTER_STORE, CounterKey, CounterStore, Verdict};
use crate::postgres::error::Port;
use crate::postgres::time::{from_micros, to_micros};
use crate::store::{StoreError, StoreFuture, deadline};

/// The name driver failures are reported under.
///
/// Deliberately **not** [`COUNTER_STORE`]: [`super::is_at_capacity`] reads that name as "the
/// partition is full", and a dropped connection must not be rendered to a client as `429`.
const PORT: Port = Port {
    store: "counter windows",
    record: "Window",
};

/// The PostgreSQL [`CounterStore`].
#[derive(Debug, Clone)]
pub struct PostgresCounters {
    connection: DatabaseConnection,
    ceiling_override: Option<usize>,
}

impl PostgresCounters {
    /// Counters over `connection`, each variant bounded by its own [`CounterKey::ceiling`].
    pub fn new(connection: DatabaseConnection) -> Self {
        Self {
            connection,
            ceiling_override: None,
        }
    }

    /// The same counters with **every** partition bounded at `ceiling` instead.
    #[must_use]
    pub fn with_ceiling(mut self, ceiling: usize) -> Self {
        self.ceiling_override = Some(ceiling);
        self
    }

    fn ceiling_for(&self, key: &CounterKey) -> usize {
        self.ceiling_override.unwrap_or_else(|| key.ceiling())
    }
}

fn statement(sql: &str, values: impl IntoIterator<Item = Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

fn key_values(key: &CounterKey) -> [Value; 2] {
    [
        Value::from(key.as_str().to_owned()),
        Value::from(key.scope().to_owned()),
    ]
}

/// The open window `key` holds at `at` under `budget`: its hit count and when it ends.
async fn open_window(
    connection: &impl ConnectionTrait,
    key: &CounterKey,
    budget: Budget,
    at: Timestamp,
) -> Result<Option<(i64, Timestamp)>, StoreError> {
    let row = connection
        .query_one_raw(statement(
            "SELECT opened_at, hits FROM rate_counters WHERE kind = $1 AND scope = $2",
            key_values(key),
        ))
        .await
        .map_err(PORT.failing("reading a counter window"))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let opened: i64 = row
        .try_get("", "opened_at")
        .map_err(PORT.failing("reading a window's start"))?;
    let hits: i64 = row
        .try_get("", "hits")
        .map_err(PORT.failing("reading a window's hits"))?;
    let opened = from_micros(opened)
        .ok_or_else(|| PORT.undecodable(format!("{opened}µs is not a representable instant")))?;
    let ends = deadline(opened, budget.window);
    if ends <= at {
        return Ok(None);
    }
    Ok(Some((hits, ends)))
}

fn admitted(limit: u32, hits: i64) -> Verdict {
    Verdict::Admitted {
        remaining: limit.saturating_sub(u32::try_from(hits).unwrap_or(u32::MAX)),
    }
}

impl CounterStore for PostgresCounters {
    fn charge<'a>(
        &'a self,
        key: &'a CounterKey,
        units: u32,
        budget: Budget,
        at: Timestamp,
    ) -> StoreFuture<'a, Verdict> {
        if units == 0 {
            return self.peek(key, budget, at);
        }
        Box::pin(async move {
            let transaction = self
                .connection
                .begin()
                .await
                .map_err(PORT.failing("beginning a counter hit"))?;
            transaction
                .execute_raw(statement(
                    "SELECT pg_advisory_xact_lock(hashtext('rate_counter'), \
                                                  hashtext($1 || ':' || $2))",
                    key_values(key),
                ))
                .await
                .map_err(PORT.failing("locking a counter key"))?;

            if let Some((hits, ends)) = open_window(&transaction, key, budget, at).await? {
                if hits + i64::from(units) > i64::from(budget.limit) {
                    tracing::info!(counter = key.as_str(), units, retry_after = %ends, "a rate limit engaged");
                    return Ok(Verdict::Limited { retry_after: ends });
                }
                let row = transaction
                    .query_one_raw(statement(
                        "UPDATE rate_counters SET hits = hits + $3 \
                         WHERE kind = $1 AND scope = $2 RETURNING hits",
                        [
                            Value::from(key.as_str().to_owned()),
                            Value::from(key.scope().to_owned()),
                            Value::from(i64::from(units)),
                        ],
                    ))
                    .await
                    .map_err(PORT.failing("charging a counter window"))?
                    .ok_or_else(|| StoreError::Rejected {
                        store: PORT.store,
                        detail: "a locked window vanished".to_owned(),
                    })?;
                let hits: i64 = row
                    .try_get("", "hits")
                    .map_err(PORT.failing("reading a charged window"))?;
                transaction
                    .commit()
                    .await
                    .map_err(PORT.failing("committing a counter hit"))?;
                tracing::trace!(counter = key.as_str(), units, hits, "charged a counter");
                return Ok(admitted(budget.limit, hits));
            }

            // A new window: the partition's ceiling decides whether it may open.
            transaction
                .execute_raw(statement(
                    "SELECT pg_advisory_xact_lock(hashtext('rate_counter_partition'), \
                                                  hashtext($1))",
                    [Value::from(key.as_str().to_owned())],
                ))
                .await
                .map_err(PORT.failing("locking a counter partition"))?;
            let ceiling = self.ceiling_for(key);
            let occupancy = transaction
                .query_one_raw(statement(
                    "SELECT count(*) AS live FROM rate_counters \
                     WHERE kind = $1 AND scope <> $2 AND expires_at > $3",
                    [
                        Value::from(key.as_str().to_owned()),
                        Value::from(key.scope().to_owned()),
                        Value::from(to_micros(at)),
                    ],
                ))
                .await
                .map_err(PORT.failing("counting a partition's windows"))?
                .map(|row| row.try_get::<i64>("", "live"))
                .transpose()
                .map_err(PORT.failing("reading a partition's occupancy"))?
                .unwrap_or(0);
            if usize::try_from(occupancy).unwrap_or(usize::MAX) >= ceiling {
                tracing::warn!(
                    counter = key.as_str(),
                    windows = occupancy,
                    ceiling,
                    "a counter partition is full; a hit was refused rather than counted"
                );
                return Err(StoreError::Rejected {
                    store: COUNTER_STORE,
                    detail: format!(
                        "{ceiling} open windows is the ceiling for `{}`",
                        key.as_str()
                    ),
                });
            }
            transaction
                .execute_raw(statement(
                    "INSERT INTO rate_counters (kind, scope, opened_at, hits, expires_at) \
                     VALUES ($1, $2, $3, $4, $5) \
                     ON CONFLICT (kind, scope) DO UPDATE SET opened_at = EXCLUDED.opened_at, \
                       hits = EXCLUDED.hits, expires_at = EXCLUDED.expires_at",
                    [
                        Value::from(key.as_str().to_owned()),
                        Value::from(key.scope().to_owned()),
                        Value::from(to_micros(at)),
                        Value::from(i64::from(units)),
                        Value::from(to_micros(deadline(at, budget.window))),
                    ],
                ))
                .await
                .map_err(PORT.failing("opening a counter window"))?;
            transaction
                .commit()
                .await
                .map_err(PORT.failing("committing a counter hit"))?;
            tracing::trace!(counter = key.as_str(), units, "opened a counter window");
            Ok(admitted(budget.limit, i64::from(units)))
        })
    }

    fn peek<'a>(
        &'a self,
        key: &'a CounterKey,
        budget: Budget,
        at: Timestamp,
    ) -> StoreFuture<'a, Verdict> {
        Box::pin(async move {
            match open_window(&self.connection, key, budget, at).await? {
                None => Ok(admitted(budget.limit, 0)),
                Some((hits, ends)) if hits >= i64::from(budget.limit) => {
                    Ok(Verdict::Limited { retry_after: ends })
                }
                Some((hits, _)) => Ok(admitted(budget.limit, hits)),
            }
        })
    }

    fn reset<'a>(&'a self, key: &'a CounterKey) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let removed = self
                .connection
                .execute_raw(statement(
                    "DELETE FROM rate_counters WHERE kind = $1 AND scope = $2",
                    key_values(key),
                ))
                .await
                .map_err(PORT.failing("clearing a counter window"))?;
            if removed.rows_affected() > 0 {
                tracing::debug!(counter = key.as_str(), "a counter window was cleared");
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests;
