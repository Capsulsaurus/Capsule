//! The Valkey [`PullJobs`] (#964).
//!
//! One hash per job — `capsule:pull_job:{peer}:{album}` holding `next_due`, `last_pull`,
//! `failures`, and while claimed `holder` and `lease_until` — and one string per open circuit,
//! `capsule:pull_circuit:{peer}`, holding the instant it closes. Every instant is integer
//! microseconds, as the other adapters store them.
//!
//! # Every operation is one script
//!
//! The claim reads the schedule and the lease and writes the lease in one server-side step,
//! which is what makes "two replicas never pull one job" a property of the store rather than of
//! the replicas' timing. The finish compares the holder and writes the schedule in one step for
//! the same reason. Instants are compared inside the scripts against the caller's `now`, as the
//! counter scripts compare against `at`; `PEXPIRE` is set only so a key nobody touches again is
//! collected, and never decides anything.
//!
//! A computed instant leaves a script through `string.format('%.0f', …)`: an integer below
//! 2^53 is exact in a Lua number, and formatting it that way keeps every digit.

use jiff::{SignedDuration, Timestamp};

use super::jobs::{JobState, PullJobs};
use super::{JOB_STATE_RETENTION, JobKey};
use crate::federation::PeerId;
use crate::store::valkey::{Lua, Valkey, from_micros, micros};
use crate::store::{StoreError, StoreFuture, deadline};

/// The port name, for the log line and the error.
const PULL_JOBS: &str = "pull_jobs";

/// The record type a corrupt field belongs to.
const RECORD: &str = "PullJob";

// KEYS: job. ARGV: now (µs), lease until (µs), holder, collector TTL (ms).
// Returns {'0'} when not due or leased, else {'1', next_due or '', last_pull or '', failures}.
static CLAIM: Lua = Lua::new(
    "local now = tonumber(ARGV[1])
local lease = redis.call('HGET', KEYS[1], 'lease_until')
if lease and tonumber(lease) > now then return {'0'} end
local due = redis.call('HGET', KEYS[1], 'next_due')
if due and tonumber(due) > now then return {'0'} end
redis.call('HSET', KEYS[1], 'holder', ARGV[3], 'lease_until', ARGV[2])
redis.call('PEXPIRE', KEYS[1], ARGV[4])
return {'1', due or '', redis.call('HGET', KEYS[1], 'last_pull') or '',
  redis.call('HGET', KEYS[1], 'failures') or '0'}",
);

// KEYS: job. ARGV: holder, next_due (µs), last_pull (µs or ''), failures, collector TTL (ms).
// Returns 1 when recorded, 0 when another holder claimed the job since.
static FINISH: Lua = Lua::new(
    "if redis.call('HGET', KEYS[1], 'holder') ~= ARGV[1] then return 0 end
redis.call('HSET', KEYS[1], 'next_due', ARGV[2], 'failures', ARGV[4])
if ARGV[3] == '' then
  redis.call('HDEL', KEYS[1], 'last_pull')
else
  redis.call('HSET', KEYS[1], 'last_pull', ARGV[3])
end
redis.call('HDEL', KEYS[1], 'holder', 'lease_until')
redis.call('PEXPIRE', KEYS[1], ARGV[5])
return 1",
);

// KEYS: job. ARGV: now (µs), floor (µs). Returns the new due (µs) when it moved, else ''.
static HINT: Lua = Lua::new(
    "local due = redis.call('HGET', KEYS[1], 'next_due')
if not due then return '' end
local candidate = tonumber(ARGV[1])
local last = redis.call('HGET', KEYS[1], 'last_pull')
if last then
  local floor = tonumber(last) + tonumber(ARGV[2])
  if floor > candidate then candidate = floor end
end
if candidate >= tonumber(due) then return '' end
local moved = string.format('%.0f', candidate)
redis.call('HSET', KEYS[1], 'next_due', moved)
return moved",
);

// KEYS: circuit. ARGV: until (µs), collector TTL (ms). Returns the instant it is open until.
static OPEN_CIRCUIT: Lua = Lua::new(
    "local current = redis.call('GET', KEYS[1])
if current and tonumber(current) >= tonumber(ARGV[1]) then return current end
redis.call('SET', KEYS[1], ARGV[1], 'PX', ARGV[2])
return ARGV[1]",
);

/// Valkey [`PullJobs`].
#[derive(Debug, Clone)]
pub struct ValkeyPullJobs {
    valkey: Valkey,
}

impl ValkeyPullJobs {
    /// The pull worker's state on `valkey`.
    pub fn new(valkey: Valkey) -> Self {
        Self { valkey }
    }

    async fn run<T: redis::FromRedisValue>(
        &self,
        script: &Lua,
        key: String,
        args: &[String],
    ) -> Result<T, StoreError> {
        let mut invocation = script.script().prepare_invoke();
        invocation.key(key);
        for arg in args {
            invocation.arg(arg.as_str());
        }
        self.valkey.invoke(PULL_JOBS, &invocation).await
    }
}

/// `capsule:pull_job:{peer}:{album}`. A peer is a host name and holds no `:`, so no pair of
/// values spells another pair's key.
fn job_key(job: &JobKey) -> String {
    format!("capsule:pull_job:{}:{}", job.peer, job.album)
}

/// `capsule:pull_circuit:{peer}`.
fn circuit_key(peer: &PeerId) -> String {
    format!("capsule:pull_circuit:{peer}")
}

/// The collector's TTL for a key whose latest meaningful instant is `latest`, seen at `now`:
/// that far ahead plus [`JOB_STATE_RETENTION`], in whole milliseconds, at least one.
fn retention_millis(now: Timestamp, latest: Timestamp) -> String {
    let until = deadline(latest.max(now), JOB_STATE_RETENTION);
    until.duration_since(now).as_millis().max(1).to_string()
}

/// A stored microsecond field back to an instant.
fn instant(field: &'static str, value: &str) -> Result<Timestamp, StoreError> {
    let micros: i64 = value.parse().map_err(|_| StoreError::Corrupt {
        store: PULL_JOBS,
        record: RECORD,
        detail: format!("`{field}` is not an integer: {value:?}"),
    })?;
    from_micros(PULL_JOBS, RECORD, micros)
}

impl PullJobs for ValkeyPullJobs {
    fn claim<'a>(
        &'a self,
        job: &'a JobKey,
        holder: &'a str,
        now: Timestamp,
        lease: SignedDuration,
    ) -> StoreFuture<'a, Option<JobState>> {
        Box::pin(async move {
            let lease_until = deadline(now, lease);
            let reply: Vec<String> = self
                .run(
                    &CLAIM,
                    job_key(job),
                    &[
                        micros(now).to_string(),
                        micros(lease_until).to_string(),
                        holder.to_owned(),
                        retention_millis(now, lease_until),
                    ],
                )
                .await?;
            let [claimed, due, last, failures] = reply.as_slice() else {
                tracing::trace!(%job, "the job is not due or is held");
                return Ok(None);
            };
            if claimed != "1" {
                return Ok(None);
            }
            let state = JobState {
                next_due: if due.is_empty() {
                    now
                } else {
                    instant("next_due", due)?
                },
                last_pull: if last.is_empty() {
                    None
                } else {
                    Some(instant("last_pull", last)?)
                },
                failures: failures.parse().map_err(|_| StoreError::Corrupt {
                    store: PULL_JOBS,
                    record: RECORD,
                    detail: format!("`failures` is not a count: {failures:?}"),
                })?,
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
            let recorded: i64 = self
                .run(
                    &FINISH,
                    job_key(job),
                    &[
                        holder.to_owned(),
                        micros(state.next_due).to_string(),
                        state
                            .last_pull
                            .map(|at| micros(at).to_string())
                            .unwrap_or_default(),
                        state.failures.to_string(),
                        retention_millis(now, state.next_due),
                    ],
                )
                .await?;
            Ok(recorded == 1)
        })
    }

    fn hint<'a>(
        &'a self,
        job: &'a JobKey,
        now: Timestamp,
        floor: SignedDuration,
    ) -> StoreFuture<'a, Option<Timestamp>> {
        Box::pin(async move {
            let floor = i64::try_from(floor.as_micros()).unwrap_or(i64::MAX);
            let moved: String = self
                .run(
                    &HINT,
                    job_key(job),
                    &[micros(now).to_string(), floor.to_string()],
                )
                .await?;
            if moved.is_empty() {
                return Ok(None);
            }
            Ok(Some(instant("next_due", &moved)?))
        })
    }

    fn open_circuit<'a>(
        &'a self,
        peer: &'a PeerId,
        now: Timestamp,
        until: Timestamp,
    ) -> StoreFuture<'a, Timestamp> {
        Box::pin(async move {
            // The collector's TTL only; whether the circuit is open is read from the value.
            let ttl = until.duration_since(now).as_millis().max(1).to_string();
            let open: String = self
                .run(
                    &OPEN_CIRCUIT,
                    circuit_key(peer),
                    &[micros(until).to_string(), ttl],
                )
                .await?;
            instant("circuit", &open)
        })
    }

    fn circuit<'a>(
        &'a self,
        peer: &'a PeerId,
        now: Timestamp,
    ) -> StoreFuture<'a, Option<Timestamp>> {
        Box::pin(async move {
            let mut get = redis::cmd("GET");
            get.arg(circuit_key(peer));
            let open: Option<String> = self.valkey.command(PULL_JOBS, get).await?;
            match open {
                Some(value) => {
                    let until = instant("circuit", &value)?;
                    Ok((until > now).then_some(until))
                }
                None => Ok(None),
            }
        })
    }
}
