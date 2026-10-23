//! [`PostgresModeration`] — the durable moderation store.
//!
//! # Three tables
//!
//! - `moderation_standing`: one row per account anything has been done to, `suspended_since`
//!   `NULL` once reinstated. No row and a `NULL` both read as [`Standing::Active`].
//! - `moderation_events`: the record a user reads, append-only, ordered by a `BIGSERIAL` that is
//!   the order the actions were *applied* in — the order the port promises. `at` is the caller's
//!   instant and is not trusted to order anything: two actions may share one, and the in-memory
//!   adapter orders by arrival too.
//! - `federated_reports`: the report queue, keyed by `report_id`.
//!
//! # The event and its effect are one transaction
//!
//! [`ModerationStore::apply`] writes the standing and the event inside one transaction, so a
//! takedown that applied and failed to record itself — the silent operation the contract forbids
//! — cannot be produced by a crash between the two.
//!
//! # The durable queue keeps every report
//!
//! The in-memory adapter caps its queue at [`super::MAX_IN_MEMORY_REPORTS`] and evicts the
//! oldest, because process memory that never returns is its failure mode. This one does not:
//! the module docs name the durable adapter as where a queue that must not lose anything
//! belongs, and a dropped report is a moderation input nobody will ever see. Intake is already
//! bounded upstream by the per-peer report budget.
//!
//! The queue is read in `report_id` byte order (`COLLATE "C"`), which is the in-memory map's
//! order and — the id being a UUIDv7 in its canonical lowercase spelling — arrival order.

use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, Statement,
    TransactionTrait, Value,
};

use super::{FederatedReport, ModerationAction, ModerationEvent, ModerationStore, Standing};
use crate::postgres::error::Port;
use crate::postgres::time::{from_micros, to_micros};
use crate::store::{AlbumId, AssetId, StoreError, StoreFuture, UserId};

/// Which port is speaking about account standing and its record.
const PORT: Port = Port {
    store: "moderation",
    record: "ModerationEvent",
};

/// Which port is speaking about the federated report queue.
const REPORTS: Port = Port {
    store: "moderation",
    record: "FederatedReport",
};

/// The durable moderation store.
#[derive(Debug, Clone)]
pub struct PostgresModeration {
    connection: DatabaseConnection,
}

impl PostgresModeration {
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

/// An action as its column holds it.
fn action_from(token: &str) -> Option<ModerationAction> {
    [
        ModerationAction::Suspended,
        ModerationAction::Reinstated,
        ModerationAction::TakenDown,
        ModerationAction::LegalHold,
        ModerationAction::HoldLifted,
    ]
    .into_iter()
    .find(|action| action.as_str() == token)
}

/// An instant column, decoded on behalf of `port`.
fn instant(port: Port, micros: i64) -> Result<jiff::Timestamp, StoreError> {
    from_micros(micros)
        .ok_or_else(|| port.undecodable(format!("{micros}µs is not a representable instant")))
}

impl ModerationStore for PostgresModeration {
    fn apply(&self, event: ModerationEvent, standing: Option<Standing>) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let transaction = begin(&self.connection).await?;
            if let Some(standing) = &standing {
                let since = match standing {
                    Standing::Active => None,
                    Standing::Suspended { since } => Some(to_micros(*since)),
                };
                transaction
                    .execute_raw(Statement::from_sql_and_values(
                        DbBackend::Postgres,
                        "INSERT INTO moderation_standing (user_id, suspended_since) \
                         VALUES ($1, $2) \
                         ON CONFLICT (user_id) DO UPDATE SET \
                           suspended_since = EXCLUDED.suspended_since",
                        [
                            Value::from(event.user_id.as_str().to_owned()),
                            Value::from(since),
                        ],
                    ))
                    .await
                    .map_err(PORT.failing("moving an account's standing"))?;
            }
            transaction
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "INSERT INTO moderation_events (user_id, action, asset_id, at, reason) \
                     VALUES ($1, $2, $3, $4, $5)",
                    [
                        Value::from(event.user_id.as_str().to_owned()),
                        Value::from(event.action.as_str().to_owned()),
                        Value::from(event.asset_id.as_ref().map(|id| id.as_str().to_owned())),
                        Value::from(to_micros(event.at)),
                        Value::from(event.reason.clone()),
                    ],
                ))
                .await
                .map_err(PORT.failing("recording a moderation action"))?;
            commit(transaction).await?;
            tracing::info!(
                user = %event.user_id,
                action = event.action.as_str(),
                asset = ?event.asset_id,
                standing = ?standing,
                "a moderation action was recorded"
            );
            Ok(())
        })
    }

    fn standing<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Standing> {
        Box::pin(async move {
            let Some(row) = self
                .connection
                .query_one_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT suspended_since FROM moderation_standing WHERE user_id = $1",
                    [Value::from(user.as_str().to_owned())],
                ))
                .await
                .map_err(PORT.failing("reading an account's standing"))?
            else {
                return Ok(Standing::Active);
            };
            let since: Option<i64> = row
                .try_get("", "suspended_since")
                .map_err(PORT.failing("reading an account's standing"))?;
            Ok(match since {
                None => Standing::Active,
                Some(micros) => Standing::Suspended {
                    since: instant(PORT, micros)?,
                },
            })
        })
    }

    fn events_for_user<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Vec<ModerationEvent>> {
        Box::pin(async move {
            let rows = self
                .connection
                .query_all_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT action, asset_id, at, reason FROM moderation_events \
                     WHERE user_id = $1 ORDER BY seq ASC",
                    [Value::from(user.as_str().to_owned())],
                ))
                .await
                .map_err(PORT.failing("reading an account's moderation record"))?;
            let failed = PORT.failing("reading an account's moderation record");
            let mut events = Vec::with_capacity(rows.len());
            for row in &rows {
                let action: String = row.try_get("", "action").map_err(&failed)?;
                let asset_id: Option<String> = row.try_get("", "asset_id").map_err(&failed)?;
                let at: i64 = row.try_get("", "at").map_err(&failed)?;
                let reason: Option<String> = row.try_get("", "reason").map_err(&failed)?;
                events.push(ModerationEvent {
                    user_id: user.clone(),
                    action: action_from(&action).ok_or_else(|| {
                        PORT.undecodable(format!("`{action}` is not a moderation action"))
                    })?,
                    asset_id: asset_id.map(AssetId::new),
                    at: instant(PORT, at)?,
                    reason,
                });
            }
            Ok(events)
        })
    }

    fn file_report(&self, report: FederatedReport) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            // The primary key decides a duplicate in the same statement that writes, so two
            // concurrent intakes of one id cannot both succeed.
            let inserted = self
                .connection
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "INSERT INTO federated_reports \
                       (report_id, reporting_server, reported_user, asset_hash, album_id, reason, \
                        reported_at, received_at, signature, signed) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
                     ON CONFLICT (report_id) DO NOTHING",
                    [
                        Value::from(report.report_id.clone()),
                        Value::from(report.reporting_server.clone()),
                        Value::from(report.reported_user.as_str().to_owned()),
                        Value::from(report.asset_hash.clone()),
                        Value::from(report.album_id.as_str().to_owned()),
                        Value::from(report.reason.clone()),
                        Value::from(to_micros(report.reported_at)),
                        Value::from(to_micros(report.received_at)),
                        Value::from(report.signature.clone()),
                        Value::from(report.signed.clone()),
                    ],
                ))
                .await
                .map_err(REPORTS.failing("filing a federated report"))?
                .rows_affected();
            if inserted == 0 {
                return Err(StoreError::Rejected {
                    store: REPORTS.store,
                    detail: format!("report {} is already on file", report.report_id),
                });
            }
            tracing::info!(
                report = %report.report_id,
                from = %report.reporting_server,
                about = %report.reported_user,
                album = %report.album_id,
                "a federated moderation report was filed"
            );
            Ok(())
        })
    }

    fn pending_reports(&self) -> StoreFuture<'_, Vec<FederatedReport>> {
        Box::pin(async move {
            let rows = self
                .connection
                .query_all_raw(Statement::from_string(
                    DbBackend::Postgres,
                    "SELECT report_id, reporting_server, reported_user, asset_hash, album_id, \
                       reason, reported_at, received_at, signature, signed \
                     FROM federated_reports ORDER BY report_id COLLATE \"C\" ASC",
                ))
                .await
                .map_err(REPORTS.failing("reading the federated report queue"))?;
            let failed = REPORTS.failing("reading the federated report queue");
            let mut reports = Vec::with_capacity(rows.len());
            for row in &rows {
                let reported_at: i64 = row.try_get("", "reported_at").map_err(&failed)?;
                let received_at: i64 = row.try_get("", "received_at").map_err(&failed)?;
                let reported_user: String = row.try_get("", "reported_user").map_err(&failed)?;
                let album_id: String = row.try_get("", "album_id").map_err(&failed)?;
                reports.push(FederatedReport {
                    report_id: row.try_get("", "report_id").map_err(&failed)?,
                    reporting_server: row.try_get("", "reporting_server").map_err(&failed)?,
                    reported_user: UserId::new(reported_user),
                    asset_hash: row.try_get("", "asset_hash").map_err(&failed)?,
                    album_id: AlbumId::new(album_id),
                    reason: row.try_get("", "reason").map_err(&failed)?,
                    reported_at: instant(REPORTS, reported_at)?,
                    received_at: instant(REPORTS, received_at)?,
                    signature: row.try_get("", "signature").map_err(&failed)?,
                    signed: row.try_get("", "signed").map_err(&failed)?,
                });
            }
            Ok(reports)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::action_from;
    use crate::moderation::ModerationAction;

    /// Every action round-trips through its column, and nothing else decodes.
    #[test]
    fn every_action_round_trips_through_its_column() {
        for action in [
            ModerationAction::Suspended,
            ModerationAction::Reinstated,
            ModerationAction::TakenDown,
            ModerationAction::LegalHold,
            ModerationAction::HoldLifted,
        ] {
            assert_eq!(action_from(action.as_str()), Some(action));
        }
        assert_eq!(action_from("banished"), None);
    }

    /// The suite, against a real Postgres.
    mod postgres_conformance {
        use super::super::PostgresModeration;
        use crate::moderation::ModerationStore;
        use crate::moderation::conformance::{self, Harness};
        use crate::postgres::testing;

        /// A store over one container.
        #[derive(Debug)]
        struct PostgresHarness {
            moderation: PostgresModeration,
        }

        impl Harness for PostgresHarness {
            fn moderation(&self) -> &dyn ModerationStore {
                &self.moderation
            }
        }

        #[tokio::test]
        async fn the_postgres_moderation_store_conforms() {
            let Some(database) = testing::start("the Postgres moderation store").await else {
                return;
            };
            let harness = PostgresHarness {
                moderation: PostgresModeration::new(database.connection().clone()),
            };
            conformance::run_all(&harness).await;
        }
    }
}
