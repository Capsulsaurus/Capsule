//! [`PostgresCohorts`] — the durable device-cohort map (`S-C13`, #402).
//!
//! # Why this one port in `store` is Postgres and the other five are not
//!
//! Everything else in [`super`] is volatile TTL state whose production adapter is Valkey. The
//! cohort map is the exception the port's own docs argue for: *"A session store forgets a cohort
//! exactly when the 'have I seen this device before?' question becomes worth asking"* — the user
//! reinstalls, gets a new `device_id` by design, and the sessions that carried the old one have
//! long expired. A map that expired with them would answer the question it exists for with
//! "no, never" every time.
//!
//! That is why [`super::conformance`] splits `CohortHarness` out of `Harness`: this adapter
//! implements one port, and a suite that made it implement six to run four cases would make it
//! invent five adapters it will never have.
//!
//! # The whole adapter is one upsert
//!
//! `observe` is `INSERT … ON CONFLICT (user_id, cohort_hash) DO UPDATE SET last_seen = …
//! RETURNING`, and the composite primary key **is** the idempotence the port states: seeing the
//! same cohort twice is one row, not two. A read-then-branch would be the same statement with a
//! race in the middle, and the race is two devices of one account signing in at once.
//!
//! `first_seen` is untouched by the update, which is the half that matters: it is what lets a
//! client say *"a device you've used before (last seen March)"* rather than presenting a
//! stranger.

use jiff::Timestamp;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, Value};

use super::auth::{CohortRecord, CohortStore};
use super::{StoreFuture, UserId};
use crate::postgres::error::Port;
use crate::postgres::time::{from_micros, to_micros};

/// Which port is speaking, for every error this adapter raises.
const PORT: Port = Port {
    store: "device-cohorts",
    record: "CohortRecord",
};

/// The durable device-cohort map.
#[derive(Debug, Clone)]
pub struct PostgresCohorts {
    connection: DatabaseConnection,
}

impl PostgresCohorts {
    /// A cohort map over `connection`.
    pub fn new(connection: DatabaseConnection) -> Self {
        Self { connection }
    }
}

/// Read one `device_cohorts` row back.
fn record_from(row: &sea_orm::QueryResult) -> Result<CohortRecord, crate::store::StoreError> {
    let failed = PORT.failing("reading a cohort row");
    let user_id: String = row.try_get("", "user_id").map_err(&failed)?;
    let cohort_hash: String = row.try_get("", "cohort_hash").map_err(&failed)?;
    let first_seen: i64 = row.try_get("", "first_seen").map_err(&failed)?;
    let last_seen: i64 = row.try_get("", "last_seen").map_err(&failed)?;
    let instant = |micros: i64| {
        from_micros(micros)
            .ok_or_else(|| PORT.undecodable(format!("{micros}µs is not a representable instant")))
    };
    Ok(CohortRecord {
        user_id: UserId::new(user_id),
        cohort_hash,
        first_seen: instant(first_seen)?,
        last_seen: instant(last_seen)?,
    })
}

impl CohortStore for PostgresCohorts {
    fn observe<'a>(
        &'a self,
        user: &'a UserId,
        cohort_hash: &'a str,
        at: Timestamp,
    ) -> StoreFuture<'a, CohortRecord> {
        Box::pin(async move {
            let observed = self
                .connection
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "INSERT INTO device_cohorts (user_id, cohort_hash, first_seen, last_seen) \
                     VALUES ($1, $2, $3, $3) \
                     ON CONFLICT (user_id, cohort_hash) \
                     DO UPDATE SET last_seen = EXCLUDED.last_seen \
                     RETURNING user_id, cohort_hash, first_seen, last_seen, \
                               (xmax = 0) AS inserted",
                    [
                        Value::from(user.as_str().to_owned()),
                        Value::from(cohort_hash.to_owned()),
                        Value::from(to_micros(at)),
                    ],
                ))
                .await
                .map_err(PORT.failing("observing a device cohort"))?
                .ok_or_else(|| crate::store::StoreError::Rejected {
                    store: PORT.store,
                    detail: "the cohort upsert returned no row".to_owned(),
                })?;
            let record = record_from(&observed)?;
            // `xmax = 0` is how an upsert says which half it took: PostgreSQL leaves the
            // deleting-transaction id at zero on a freshly inserted tuple and sets it on one the
            // `DO UPDATE` rewrote. Derived rather than inferred from `first_seen == last_seen`,
            // which is the same thing being said by a coincidence — a device observed twice at
            // one instant, which a test clock does routinely, would log a new cohort twice.
            let inserted: bool = observed
                .try_get("", "inserted")
                .map_err(PORT.failing("reading whether a cohort row was new"))?;
            if inserted {
                tracing::info!(%user, "an account was seen under a new device cohort");
            }
            Ok(record)
        })
    }

    fn cohorts_for_user<'a>(&'a self, user: &'a UserId) -> StoreFuture<'a, Vec<CohortRecord>> {
        Box::pin(async move {
            // Oldest first sighting first, ties broken by the hash so the order is total. The
            // order is part of the contract rather than an accident of the backend: a
            // user-visible listing whose order depends on the storage engine is a listing that
            // reshuffles itself between page loads. `COLLATE "C"` on the tie-break so the total
            // order is the same one the deterministic double's `BTreeMap` produces — a cohort
            // hash is client-asserted text, and a locale collation orders it differently.
            let found = self
                .connection
                .query_all(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT user_id, cohort_hash, first_seen, last_seen FROM device_cohorts \
                     WHERE user_id = $1 \
                     ORDER BY first_seen, cohort_hash COLLATE \"C\"",
                    [Value::from(user.as_str().to_owned())],
                ))
                .await
                .map_err(PORT.failing("listing an account's device cohorts"))?;
            found.iter().map(record_from).collect()
        })
    }
}

#[cfg(test)]
mod tests {
    /// The suite, against a real Postgres.
    mod postgres_conformance {
        use jiff::SignedDuration;

        use super::super::PostgresCohorts;
        use crate::postgres::testing;
        use crate::store::conformance::{self, CohortHarness};
        use crate::store::{CohortStore, StoreFuture};

        /// A harness over one container.
        ///
        /// `advance` is a **no-op**, and that is the honest implementation rather than a stub:
        /// the cohort map has no TTL at all, so there is no clock to move. The one case that
        /// calls it — `the_cohort_map_does_not_expire` — asserts a record survives a year, and a
        /// store that expires nothing passes it whatever the clock says.
        #[derive(Debug)]
        struct Harness {
            cohorts: PostgresCohorts,
        }

        impl CohortHarness for Harness {
            fn cohorts(&self) -> &dyn CohortStore {
                &self.cohorts
            }

            fn advance(&self, _by: SignedDuration) -> StoreFuture<'_, ()> {
                Box::pin(async { Ok(()) })
            }
        }

        #[tokio::test]
        async fn the_postgres_cohort_map_conforms() {
            let Some(database) = testing::start("the Postgres device-cohort map").await else {
                return;
            };
            let harness = Harness {
                cohorts: PostgresCohorts::new(database.connection().clone()),
            };
            conformance::run_all_cohorts(&harness).await;
        }
    }
}
