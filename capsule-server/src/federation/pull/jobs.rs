//! [`PullJobs`]: each job's schedule and lease, and each source peer's circuit, in volatile
//! state.
//!
//! # Why one port holds all three
//!
//! A replica deciding whether to pull a job asks three questions — is it due, does anyone else
//! hold it, is its peer's circuit open — and the first two must be answered and acted on as one
//! step, or two replicas that both read "due, unheld" both pull. So [`PullJobs::claim`] is that
//! step, and the schedule it reads lives beside the lease it writes. The circuit is the peer's,
//! not the job's, and is kept here because it is the same kind of fact: volatile, shared by
//! every replica, and lost harmlessly — a restarted Valkey forgets a back-off, and the worst
//! that costs is one early pull of a peer that will fail it again.
//!
//! Volatile rather than durable on purpose (Rust Architecture Decisions: upload-session and
//! authentication state are Valkey's, never PostgreSQL's). Nothing here is a record a restart
//! must keep: a job with no schedule is due, which is the right answer after a loss.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Mutex;

use jiff::{SignedDuration, Timestamp};

use super::JobKey;
use crate::federation::PeerId;
use crate::store::{StoreFuture, deadline};

/// One job's schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobState {
    /// The earliest instant the job may be claimed again.
    pub next_due: Timestamp,
    /// When its last pull began, whatever it ended in; `None` for a job never pulled.
    pub last_pull: Option<Timestamp>,
    /// Consecutive failed pulls, reset by a success. Drives [`super::backoff`].
    pub failures: u32,
}

impl JobState {
    /// The schedule of a job never pulled: due at `now`, no failures.
    pub fn fresh(now: Timestamp) -> Self {
        Self {
            next_due: now,
            last_pull: None,
            failures: 0,
        }
    }
}

/// The pull worker's volatile-state port.
pub trait PullJobs: fmt::Debug + Send + Sync {
    /// Claim `job` for `holder` until `now + lease`, if it is due at `now` and no lease on it is
    /// live at `now`.
    ///
    /// Answers the schedule the claim found — [`JobState::fresh`] at `now` for a job with none —
    /// or `None` when the job is not due or another lease holds it. One step: two claims racing
    /// for one due job admit exactly one. A lease that has lapsed is no lease, whoever held it.
    fn claim<'a>(
        &'a self,
        job: &'a JobKey,
        holder: &'a str,
        now: Timestamp,
        lease: SignedDuration,
    ) -> StoreFuture<'a, Option<JobState>>;

    /// Record `state` as `job`'s schedule and release `holder`'s lease, as one step.
    ///
    /// Only while `holder` is the job's last claimant: a holder whose lease lapsed and was then
    /// claimed by another writes nothing and answers `false`, so a slow replica cannot overwrite
    /// the schedule the faster one recorded. A holder whose lease lapsed with nobody claiming it
    /// since still finishes, because nobody else has an answer to record.
    fn finish<'a>(
        &'a self,
        job: &'a JobKey,
        holder: &'a str,
        state: JobState,
        now: Timestamp,
    ) -> StoreFuture<'a, bool>;

    /// Bring `job`'s next pull earlier on a hint received at `now`: to the later of `now` and
    /// `floor` past its last pull, when that is earlier than its next due.
    ///
    /// Answers the new due when it moved, `None` when it did not — a job with no schedule is
    /// already due, and a job due sooner keeps its time. Never moves a pull later.
    fn hint<'a>(
        &'a self,
        job: &'a JobKey,
        now: Timestamp,
        floor: SignedDuration,
    ) -> StoreFuture<'a, Option<Timestamp>>;

    /// Open `peer`'s circuit, at `now`, until `until`, or keep it open longer if it already is.
    ///
    /// Answers the instant it is open until. Never shortens an open circuit.
    fn open_circuit<'a>(
        &'a self,
        peer: &'a PeerId,
        now: Timestamp,
        until: Timestamp,
    ) -> StoreFuture<'a, Timestamp>;

    /// The instant `peer`'s circuit is open until, if it is open at `now`.
    fn circuit<'a>(
        &'a self,
        peer: &'a PeerId,
        now: Timestamp,
    ) -> StoreFuture<'a, Option<Timestamp>>;
}

/// One job's row in the double.
#[derive(Debug, Clone)]
struct Row {
    state: Option<JobState>,
    holder: Option<String>,
    lease_until: Option<Timestamp>,
}

/// The deterministic [`PullJobs`] double: one mutex, so every operation is one critical
/// section, as each Valkey script is one server-side step.
#[derive(Debug, Default)]
pub struct InMemoryPullJobs {
    jobs: Mutex<BTreeMap<JobKey, Row>>,
    circuits: Mutex<BTreeMap<PeerId, Timestamp>>,
}

impl InMemoryPullJobs {
    /// An empty store: every job due, every circuit closed.
    pub fn new() -> Self {
        Self::default()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl PullJobs for InMemoryPullJobs {
    fn claim<'a>(
        &'a self,
        job: &'a JobKey,
        holder: &'a str,
        now: Timestamp,
        lease: SignedDuration,
    ) -> StoreFuture<'a, Option<JobState>> {
        Box::pin(async move {
            let mut jobs = lock(&self.jobs);
            let row = jobs.entry(job.clone()).or_insert(Row {
                state: None,
                holder: None,
                lease_until: None,
            });
            if row.lease_until.is_some_and(|until| until > now) {
                return Ok(None);
            }
            if row.state.is_some_and(|state| state.next_due > now) {
                return Ok(None);
            }
            row.holder = Some(holder.to_owned());
            row.lease_until = Some(deadline(now, lease));
            Ok(Some(row.state.unwrap_or_else(|| JobState::fresh(now))))
        })
    }

    fn finish<'a>(
        &'a self,
        job: &'a JobKey,
        holder: &'a str,
        state: JobState,
        _now: Timestamp,
    ) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            let mut jobs = lock(&self.jobs);
            let Some(row) = jobs.get_mut(job) else {
                return Ok(false);
            };
            if row.holder.as_deref() != Some(holder) {
                return Ok(false);
            }
            row.state = Some(state);
            row.holder = None;
            row.lease_until = None;
            Ok(true)
        })
    }

    fn hint<'a>(
        &'a self,
        job: &'a JobKey,
        now: Timestamp,
        floor: SignedDuration,
    ) -> StoreFuture<'a, Option<Timestamp>> {
        Box::pin(async move {
            let mut jobs = lock(&self.jobs);
            let Some(state) = jobs.get_mut(job).and_then(|row| row.state.as_mut()) else {
                return Ok(None);
            };
            let candidate = state
                .last_pull
                .map_or(now, |last| deadline(last, floor).max(now));
            if candidate >= state.next_due {
                return Ok(None);
            }
            state.next_due = candidate;
            Ok(Some(candidate))
        })
    }

    fn open_circuit<'a>(
        &'a self,
        peer: &'a PeerId,
        _now: Timestamp,
        until: Timestamp,
    ) -> StoreFuture<'a, Timestamp> {
        Box::pin(async move {
            let mut circuits = lock(&self.circuits);
            let open = circuits
                .get(peer)
                .copied()
                .map_or(until, |current| current.max(until));
            circuits.insert(peer.clone(), open);
            Ok(open)
        })
    }

    fn circuit<'a>(
        &'a self,
        peer: &'a PeerId,
        now: Timestamp,
    ) -> StoreFuture<'a, Option<Timestamp>> {
        Box::pin(async move {
            Ok(lock(&self.circuits)
                .get(peer)
                .copied()
                .filter(|until| *until > now))
        })
    }
}
