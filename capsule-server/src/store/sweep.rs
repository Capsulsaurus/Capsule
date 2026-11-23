//! [`ExpirySweeper`] — deletes the rows the state tables' own reads already treat as absent.
//!
//! # Why deleting is housekeeping, not correctness
//!
//! Every state adapter on Postgres ([`super::postgres`], [`crate::counter::postgres`],
//! [`crate::federation::pull::postgres`]) decides liveness on read against its clock, so a
//! row past its `expires_at` is already invisible the moment it expires. This sweeper only
//! reclaims the space: a missed pass leaves dead rows nobody can observe, never a live one
//! that should have died. That is also why it may run on every replica at once — two sweepers
//! deleting the same expired rows race to the same end state.
//!
//! # Bounded batches
//!
//! Each table is swept in batches of at most `batch` rows, each its own statement, so no
//! statement holds row locks on more than one batch and a backlog after downtime drains without
//! one long transaction blocking the request path.

use std::collections::BTreeMap;
use std::sync::Arc;

use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, Value};

use super::{Clock, StoreError};
use crate::postgres::error::Port;
use crate::postgres::time::to_micros;

const PORT: Port = Port {
    store: "expiry sweep",
    record: "expired row",
};

/// Every table the sweeper reclaims, with the column that holds its rows' expiry.
///
/// One list, so a new state table that is not added here shows up as a test failure
/// (`every_state_table_is_swept`) rather than as a table that grows forever.
pub const SWEPT_TABLES: [(&str, &str); 10] = [
    ("auth_sessions", "expires_at"),
    ("revoke_all_challenges", "expires_at"),
    ("enrollment_codes", "expires_at"),
    // The relay mailbox goes with its channel, by the foreign key's cascade.
    ("enrollment_channels", "expires_at"),
    ("oidc_authorizations", "expires_at"),
    ("oidc_device_authorizations", "expires_at"),
    ("upload_sessions", "expires_at"),
    ("rate_counters", "expires_at"),
    ("federation_pull_schedules", "expires_at"),
    ("federation_pull_circuits", "open_until"),
];

/// The default batch: large enough to drain a day's backlog in a few hundred statements, small
/// enough that one statement's locks are never noticeable.
pub const DEFAULT_SWEEP_BATCH: u64 = 1_000;

/// How often `serve` runs a sweep.
pub const SWEEP_TICK: std::time::Duration = std::time::Duration::from_mins(5);

/// What one sweep removed, per table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Rows deleted from each table, keyed by table name; tables with none are absent.
    pub removed: BTreeMap<&'static str, u64>,
}

impl SweepReport {
    /// Rows deleted across every table.
    pub fn total(&self) -> u64 {
        self.removed.values().sum()
    }
}

/// Deletes expired rows from every state table.
#[derive(Debug, Clone)]
pub struct ExpirySweeper {
    connection: DatabaseConnection,
    clock: Arc<dyn Clock>,
    batch: u64,
}

impl ExpirySweeper {
    /// A sweeper over `connection` that reads expiry against `clock`, in batches of
    /// [`DEFAULT_SWEEP_BATCH`].
    pub fn new(connection: DatabaseConnection, clock: Arc<dyn Clock>) -> Self {
        Self {
            connection,
            clock,
            batch: DEFAULT_SWEEP_BATCH,
        }
    }

    /// The same sweeper deleting at most `batch` rows per statement.
    #[must_use]
    pub fn with_batch(mut self, batch: u64) -> Self {
        self.batch = batch.max(1);
        self
    }

    /// Delete every row expired as of the clock's `now`, table by table.
    ///
    /// # Errors
    ///
    /// The first statement that fails; the tables already swept stay swept.
    pub async fn sweep(&self) -> Result<SweepReport, StoreError> {
        let now = to_micros(self.clock.now());
        let batch = i64::try_from(self.batch).unwrap_or(i64::MAX);
        let mut report = SweepReport::default();
        for (table, column) in SWEPT_TABLES {
            let mut removed = 0u64;
            loop {
                let result = self
                    .connection
                    .execute_raw(Statement::from_sql_and_values(
                        DbBackend::Postgres,
                        format!(
                            "DELETE FROM {table} WHERE ctid IN \
                               (SELECT ctid FROM {table} WHERE {column} <= $1 LIMIT $2)"
                        ),
                        [Value::from(now), Value::from(batch)],
                    ))
                    .await
                    .map_err(PORT.failing("deleting expired rows"))?;
                let deleted = result.rows_affected();
                removed += deleted;
                tracing::trace!(table, deleted, "swept a batch of expired rows");
                if deleted < self.batch {
                    break;
                }
            }
            if removed > 0 {
                tracing::debug!(table, removed, "swept expired rows");
                report.removed.insert(table, removed);
            }
        }
        tracing::info!(removed = report.total(), "expired state swept");
        Ok(report)
    }
}

#[cfg(test)]
mod tests;
