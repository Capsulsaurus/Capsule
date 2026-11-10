//! [`PostgresPullJobs`] — the pull worker's schedule and leases on `federation_pull_schedules`
//! and `federation_pull_circuits`.
//!
//! Every operation is one statement. A claim is an `INSERT … ON CONFLICT DO UPDATE … WHERE` that
//! takes the lease only while none is live and the job is due: Postgres locks the conflicting
//! row and re-evaluates the condition against its latest version, so two replicas claiming one
//! due job admit exactly one. A schedule past its retention (`expires_at`) is treated as never
//! written, which is what the Valkey key's expiry did: a job with no schedule is due, and that
//! loss is harmless by the port's own contract.

use jiff::{SignedDuration, Timestamp};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, QueryResult, Statement, Value};

use super::JOB_STATE_RETENTION;
use super::jobs::{JobState, PullJobs};
use super::JobKey;
use crate::federation::PeerId;
use crate::postgres::error::Port;
use crate::postgres::time::{from_micros, to_micros};
use crate::store::{StoreError, StoreFuture, deadline};

const PORT: Port = Port {
    store: "pull_jobs",
    record: "PullJob",
};

/// The PostgreSQL [`PullJobs`].
#[derive(Debug, Clone)]
pub struct PostgresPullJobs {
    connection: DatabaseConnection,
}

impl PostgresPullJobs {
    /// Pull jobs over `connection`.
    pub fn new(connection: DatabaseConnection) -> Self {
        Self { connection }
    }
}

fn statement(sql: &str, values: impl IntoIterator<Item = Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

fn instant(row: &QueryResult, name: &str) -> Result<Option<Timestamp>, StoreError> {
    let micros: Option<i64> = row
        .try_get("", name)
        .map_err(|error| PORT.undecodable(format!("`{name}`: {error}")))?;
    micros
        .map(|micros| {
            from_micros(micros)
                .ok_or_else(|| PORT.undecodable(format!("`{name}` = {micros}µs is out of range")))
        })
        .transpose()
}

/// How long a schedule whose latest instant is `latest` is kept, measured from `now`.
fn retained_until(now: Timestamp, latest: Timestamp) -> i64 {
    to_micros(deadline(latest.max(now), JOB_STATE_RETENTION))
}

fn job_values(job: &JobKey) -> [Value; 2] {
    [
        Value::from(job.peer.as_str().to_owned()),
        Value::from(job.album.as_str().to_owned()),
    ]
}

impl PullJobs for PostgresPullJobs {
    fn claim<'a>(
        &'a self,
        job: &'a JobKey,
        holder: &'a str,
        now: Timestamp,
        lease: SignedDuration,
    ) -> StoreFuture<'a, Option<JobState>> {
        Box::pin(async move {
            let lease_until = deadline(now, lease);
            let [peer, album] = job_values(job);
            let row = self
                .connection
                .query_one_raw(statement(
                    "INSERT INTO federation_pull_schedules AS s \
                       (peer_id, album_id, holder, lease_until, expires_at) \
                     VALUES ($1, $2, $3, $4, $5) \
                     ON CONFLICT (peer_id, album_id) DO UPDATE SET \
                       next_due = CASE WHEN s.expires_at <= $6 THEN NULL ELSE s.next_due END, \
                       last_pull = CASE WHEN s.expires_at <= $6 THEN NULL ELSE s.last_pull END, \
                       failures = CASE WHEN s.expires_at <= $6 THEN 0 ELSE s.failures END, \
                       holder = EXCLUDED.holder, lease_until = EXCLUDED.lease_until, \
                       expires_at = GREATEST(s.expires_at, EXCLUDED.expires_at) \
                     WHERE s.expires_at <= $6 \
                        OR ((s.lease_until IS NULL OR s.lease_until <= $6) \
                            AND (s.next_due IS NULL OR s.next_due <= $6)) \
                     RETURNING next_due, last_pull, failures",
                    [
                        peer,
                        album,
                        Value::from(holder.to_owned()),
                        Value::from(to_micros(lease_until)),
                        Value::from(retained_until(now, lease_until)),
                        Value::from(to_micros(now)),
                    ],
                ))
                .await
                .map_err(PORT.failing("claiming a pull job"))?;
            let Some(row) = row else {
                tracing::trace!(%job, "a pull job was not due or is leased");
                return Ok(None);
            };
            let state = match instant(&row, "next_due")? {
                None => JobState::fresh(now),
                Some(next_due) => {
                    let failures: i32 = row
                        .try_get("", "failures")
                        .map_err(|error| PORT.undecodable(format!("`failures`: {error}")))?;
                    JobState {
                        next_due,
                        last_pull: instant(&row, "last_pull")?,
                        failures: u32::try_from(failures).unwrap_or(0),
                    }
                }
            };
            tracing::debug!(%job, holder, %lease_until, "claimed a pull job");
            Ok(Some(state))
        })
    }

    fn finish<'a>(
        &'a self,
        job: &'a JobKey,
        holder: &'a str,
        state: JobState,
        now: Timestamp,
    ) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let [peer, album] = job_values(job);
            let result = self
                .connection
                .execute_raw(statement(
                    "UPDATE federation_pull_schedules SET next_due = $4, last_pull = $5, \
                       failures = $6, holder = NULL, lease_until = NULL, expires_at = $7 \
                     WHERE peer_id = $1 AND album_id = $2 AND holder = $3 AND expires_at > $8",
                    [
                        peer,
                        album,
                        Value::from(holder.to_owned()),
                        Value::from(to_micros(state.next_due)),
                        Value::from(state.last_pull.map(to_micros)),
                        Value::from(i32::try_from(state.failures).unwrap_or(i32::MAX)),
                        Value::from(retained_until(now, state.next_due)),
                        Value::from(to_micros(now)),
                    ],
                ))
                .await
                .map_err(PORT.failing("finishing a pull job"))?;
            let finished = result.rows_affected() == 1;
            tracing::debug!(%job, holder, finished, "finished a pull job");
            Ok(finished)
        })
    }

    fn hint<'a>(
        &'a self,
        job: &'a JobKey,
        now: Timestamp,
        floor: SignedDuration,
    ) -> StoreFuture<'a, Option<Timestamp>> {
        Box::pin(async move {
            let [peer, album] = job_values(job);
            let floor = i64::try_from(floor.as_micros()).unwrap_or(i64::MAX);
            let row = self
                .connection
                .query_one_raw(statement(
                    "UPDATE federation_pull_schedules \
                     SET next_due = GREATEST($3, COALESCE(last_pull + $4, $3)) \
                     WHERE peer_id = $1 AND album_id = $2 AND expires_at > $3 \
                       AND next_due IS NOT NULL AND failures = 0 \
                       AND GREATEST($3, COALESCE(last_pull + $4, $3)) < next_due \
                     RETURNING next_due",
                    [peer, album, Value::from(to_micros(now)), Value::from(floor)],
                ))
                .await
                .map_err(PORT.failing("moving a pull job earlier"))?;
            let moved = match row {
                Some(row) => instant(&row, "next_due")?,
                None => None,
            };
            tracing::debug!(%job, moved = moved.is_some(), "applied a pull hint");
            Ok(moved)
        })
    }

    fn open_circuit<'a>(
        &'a self,
        peer: &'a PeerId,
        _now: Timestamp,
        until: Timestamp,
    ) -> StoreFuture<'a, Timestamp> {
        Box::pin(async move {
            let row = self
                .connection
                .query_one_raw(statement(
                    "INSERT INTO federation_pull_circuits AS c (peer_id, open_until) \
                     VALUES ($1, $2) \
                     ON CONFLICT (peer_id) DO UPDATE \
                       SET open_until = GREATEST(c.open_until, EXCLUDED.open_until) \
                     RETURNING open_until",
                    [
                        Value::from(peer.as_str().to_owned()),
                        Value::from(to_micros(until)),
                    ],
                ))
                .await
                .map_err(PORT.failing("opening a peer's circuit"))?
                .ok_or_else(|| StoreError::Rejected {
                    store: PORT.store,
                    detail: "the circuit upsert returned no row".to_owned(),
                })?;
            let open = instant(&row, "open_until")?.unwrap_or(until);
            tracing::info!(%peer, until = %open, "opened a peer's pull circuit");
            Ok(open)
        })
    }

    fn circuit<'a>(
        &'a self,
        peer: &'a PeerId,
        now: Timestamp,
    ) -> StoreFuture<'a, Option<Timestamp>> {
        Box::pin(async move {
            let row = self
                .connection
                .query_one_raw(statement(
                    "SELECT open_until FROM federation_pull_circuits \
                     WHERE peer_id = $1 AND open_until > $2",
                    [
                        Value::from(peer.as_str().to_owned()),
                        Value::from(to_micros(now)),
                    ],
                ))
                .await
                .map_err(PORT.failing("reading a peer's circuit"))?;
            match row {
                Some(row) => instant(&row, "open_until"),
                None => Ok(None),
            }
        })
    }
}

#[cfg(test)]
mod tests;
