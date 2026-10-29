//! [`PullWorker`]: one round of due jobs, and the failure policy applied to each.
//!
//! # One job, in order
//!
//! 1. **The peer.** A blocked peer's jobs do not run. Its [`PeerTier`] is read once, so every
//!    budget below holds it to one tier.
//! 2. **The circuit.** While the peer's circuit is open none of its jobs runs.
//! 3. **The claim.** [`PullJobs::claim`] admits the job if it is due and nobody holds it.
//! 4. **The CPU budget.** A peer whose validation-time budget is spent has its job queued to the
//!    window's end: deferred, not failed, and its back-off untouched.
//! 5. **The pull**, through the [`Puller`], bounded by [`PULL_DEADLINE`]: a pull past it is a
//!    transport failure, so a hung peer cannot hold the lease past [`super::PULL_LEASE`].
//! 6. **The meter.** The validation time the pull reports is charged to the CPU budget in one
//!    weighted call.
//! 7. **The schedule.** A success is next due on the jittered interval and clears the failure
//!    count. A transport failure backs off. A validation failure backs off **and** spends the
//!    peer's error budget, and a spent budget opens the peer's circuit for the back-off it
//!    earned. Nothing a failed pull fetched is written: that is the [`Puller`]'s contract, and
//!    nothing here asks it to undo anything.
//!
//! # Fails closed
//!
//! A peer store that cannot answer runs nothing for that peer. An error budget the counter
//! store cannot charge is read as spent, so a counter outage opens circuits rather than letting
//! a misbehaving peer's failures go uncounted. A CPU budget it cannot read defers the job.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jiff::Timestamp;
use tracing::instrument;

use super::jobs::{JobState, PullJobs};
use super::{
    HINT_FLOOR, Jitter, JobKey, PULL_LEASE, backoff, next_after_failure, next_after_success,
};
use crate::counter::{CounterContext, CounterKey, Verdict};
use crate::federation::{PeerRecord, PeerStore, PeerTier};
use crate::store::{Clock, StoreFuture, deadline};

/// The longest one pull may take before it is abandoned as a transport failure.
///
/// Five minutes, half of [`super::PULL_LEASE`], so a live holder always finishes before its
/// lease could lapse to another replica.
pub const PULL_DEADLINE: Duration = Duration::from_mins(5);

/// Where the jobs come from: every local album subscription and its source peer.
pub trait JobSource: std::fmt::Debug + Send + Sync {
    /// Every job this server should be pulling.
    fn jobs(&self) -> StoreFuture<'_, Vec<JobKey>>;
}

/// Why a pull failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullFailure {
    /// The peer could not be reached, timed out, or answered `5xx`. Backs off only.
    Transport {
        /// What went wrong, for the log.
        detail: String,
    },
    /// The peer answered and what it sent failed validation. Backs off and spends the peer's
    /// error budget.
    Validation {
        /// The invariant it failed, for the log.
        detail: String,
    },
}

impl PullFailure {
    /// The name this kind travels under, for a log field.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Transport { .. } => "transport",
            Self::Validation { .. } => "validation",
        }
    }

    fn detail(&self) -> &str {
        match self {
            Self::Transport { detail } | Self::Validation { detail } => detail,
        }
    }
}

/// What one pull did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullReport {
    /// Whether it succeeded.
    pub result: Result<(), PullFailure>,
    /// How long it spent validating the peer's input: the CPU the peer's budget is charged.
    pub validation: Duration,
}

/// One pull of one job: fetch, re-validate, and write only what verifies.
///
/// The contract every implementation owes: **nothing that failed validation is written**, and a
/// failure removes nothing already held. `tier` is the source peer's, so a peer on probation is
/// validated under the stricter rules.
pub trait Puller: std::fmt::Debug + Send + Sync {
    /// Pull `job` once.
    fn pull<'a>(
        &'a self,
        job: &'a JobKey,
        tier: PeerTier,
    ) -> Pin<Box<dyn Future<Output = PullReport> + Send + 'a>>;
}

/// What became of one job in one round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobOutcome {
    /// Not due, or another holder has it.
    NotDue,
    /// Its peer is blocked.
    PeerBlocked,
    /// Its peer's circuit is open until the instant named.
    CircuitOpen {
        /// When the circuit closes.
        until: Timestamp,
    },
    /// Its peer's CPU budget is spent; the job is queued to the instant named.
    Deferred {
        /// When it is next due.
        until: Timestamp,
    },
    /// Pulled; next due at the instant named.
    Pulled {
        /// When it is next due.
        next: Timestamp,
    },
    /// The pull failed.
    Failed {
        /// Transport or validation.
        kind: &'static str,
        /// Consecutive failures, this one included.
        failures: u32,
        /// When it is next due.
        next: Timestamp,
        /// When the peer's circuit closes, where this failure opened or widened it.
        circuit: Option<Timestamp>,
    },
    /// A store could not answer; nothing ran.
    Unavailable,
}

/// One round.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TickReport {
    /// Every job the source listed, and what became of it.
    pub outcomes: Vec<(JobKey, JobOutcome)>,
}

/// What a hint did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintOutcome {
    /// The job's next pull moved to the instant named.
    Moved(Timestamp),
    /// The job was already due as soon as a hint may bring it.
    Unchanged,
    /// The peer is on probation or blocked; its hints are ignored.
    Ignored,
    /// A store could not answer.
    Unavailable,
}

/// The egress pull worker.
#[derive(Debug)]
pub struct PullWorker {
    jobs: Arc<dyn PullJobs>,
    source: Arc<dyn JobSource>,
    puller: Arc<dyn Puller>,
    peers: Arc<dyn PeerStore>,
    counters: CounterContext,
    clock: Arc<dyn Clock>,
    jitter: Arc<dyn Jitter>,
    holder: String,
}

/// What [`PullWorker::new`] is built from.
#[derive(Debug)]
pub struct PullWorkerParts {
    /// The schedule, lease and circuit port.
    pub jobs: Arc<dyn PullJobs>,
    /// The subscriptions.
    pub source: Arc<dyn JobSource>,
    /// What performs one pull.
    pub puller: Arc<dyn Puller>,
    /// The peers, for the block and the tier.
    pub peers: Arc<dyn PeerStore>,
    /// The error and CPU budgets.
    pub counters: CounterContext,
    /// The instant every decision is made at.
    pub clock: Arc<dyn Clock>,
    /// The schedule's randomness.
    pub jitter: Arc<dyn Jitter>,
}

impl PullWorker {
    /// A worker over `parts`, holding its leases under a UUIDv7 minted for this process.
    pub fn new(parts: PullWorkerParts) -> Self {
        Self::with_holder(parts, uuid::Uuid::now_v7().to_string())
    }

    /// A worker that holds its leases as `holder`: the seam a test uses to stage two replicas.
    pub fn with_holder(parts: PullWorkerParts, holder: impl Into<String>) -> Self {
        let PullWorkerParts {
            jobs,
            source,
            puller,
            peers,
            counters,
            clock,
            jitter,
        } = parts;
        Self {
            jobs,
            source,
            puller,
            peers,
            counters,
            clock,
            jitter,
            holder: holder.into(),
        }
    }

    /// The name this worker holds its leases under.
    pub fn holder(&self) -> &str {
        &self.holder
    }

    /// Run one round: every job the source lists, in order.
    #[instrument(skip(self), fields(holder = %self.holder))]
    pub async fn tick(&self) -> TickReport {
        let started = Instant::now();
        let jobs = match self.source.jobs().await {
            Ok(jobs) => jobs,
            Err(error) => {
                tracing::error!(%error, "the subscriptions could not be listed; no job runs this round");
                return TickReport::default();
            }
        };
        let mut report = TickReport::default();
        for job in jobs {
            let outcome = self.run_job(&job).await;
            report.outcomes.push((job, outcome));
        }
        let pulled = report
            .outcomes
            .iter()
            .filter(|(_, outcome)| matches!(outcome, JobOutcome::Pulled { .. }))
            .count();
        let failed = report
            .outcomes
            .iter()
            .filter(|(_, outcome)| matches!(outcome, JobOutcome::Failed { .. }))
            .count();
        tracing::info!(
            jobs = report.outcomes.len(),
            pulled,
            failed,
            elapsed_ms = started.elapsed().as_millis(),
            "a pull round finished"
        );
        report
    }

    /// The peer's record, or `None` when the peer store cannot answer.
    async fn peer(&self, job: &JobKey) -> Option<Option<PeerRecord>> {
        match self.peers.read(&job.peer).await {
            Ok(record) => Some(record),
            Err(error) => {
                tracing::error!(%error, peer = %job.peer, "the peer store could not answer; the job does not run");
                None
            }
        }
    }

    /// Run `job` if it is due, and apply the failure policy to what the pull answered.
    #[instrument(skip(self), fields(job = %job, holder = %self.holder))]
    pub async fn run_job(&self, job: &JobKey) -> JobOutcome {
        let now = self.clock.now();
        let Some(record) = self.peer(job).await else {
            return JobOutcome::Unavailable;
        };
        if record.as_ref().is_some_and(PeerRecord::is_blocked) {
            tracing::debug!("the peer is blocked; the job does not run");
            return JobOutcome::PeerBlocked;
        }
        let tier = PeerTier::of(record.as_ref(), now);

        match self.jobs.circuit(&job.peer, now).await {
            Ok(Some(until)) => {
                tracing::debug!(%until, "the peer's circuit is open");
                return JobOutcome::CircuitOpen { until };
            }
            Ok(None) => {}
            Err(error) => {
                tracing::error!(%error, "the circuit could not be read; the job does not run");
                return JobOutcome::Unavailable;
            }
        }

        let state = match self.jobs.claim(job, &self.holder, now, PULL_LEASE).await {
            Ok(Some(state)) => state,
            Ok(None) => return JobOutcome::NotDue,
            Err(error) => {
                tracing::error!(%error, "the job could not be claimed");
                return JobOutcome::Unavailable;
            }
        };
        tracing::debug!(tier = tier.as_str(), failures = state.failures, "claimed");

        let cpu = CounterKey::PeerPullCpu(job.peer.as_str().to_owned());
        let cpu_budget = tier.pull_cpu_millis();
        let spent_until = match self.counters.peek(&cpu, cpu_budget).await {
            Ok(Verdict::Limited { retry_after }) => Some(retry_after),
            Ok(Verdict::Admitted { .. }) => None,
            Err(error) => {
                tracing::error!(%error, "the CPU budget could not be read; the job is deferred");
                Some(deadline(now, super::BACKOFF_FLOOR))
            }
        };
        if let Some(until) = spent_until {
            tracing::info!(%until, "the peer's CPU budget is spent; the job is queued");
            let deferred = JobState {
                next_due: until,
                ..state
            };
            self.finish(job, deferred, now).await;
            return JobOutcome::Deferred { until };
        }

        let report = match tokio::time::timeout(PULL_DEADLINE, self.puller.pull(job, tier)).await {
            Ok(report) => report,
            Err(_) => PullReport {
                result: Err(PullFailure::Transport {
                    detail: format!("the pull did not finish within {PULL_DEADLINE:?}"),
                }),
                validation: Duration::ZERO,
            },
        };
        let ended = self.clock.now();

        let cpu_queue = self.meter(&cpu, cpu_budget, report.validation).await;

        match report.result {
            Ok(()) => {
                let mut next = next_after_success(ended, self.jitter.as_ref());
                if let Some(until) = cpu_queue {
                    next = next.max(until);
                }
                tracing::info!(%next, validation_ms = report.validation.as_millis(), "pulled");
                self.finish(
                    job,
                    JobState {
                        next_due: next,
                        last_pull: Some(now),
                        failures: 0,
                    },
                    ended,
                )
                .await;
                JobOutcome::Pulled { next }
            }
            Err(failure) => {
                self.failed(job, tier, state, now, ended, failure, cpu_queue)
                    .await
            }
        }
    }

    /// Charge `validation` to the CPU budget, answering the window's end when it is spent.
    async fn meter(
        &self,
        key: &CounterKey,
        budget: crate::counter::Budget,
        validation: Duration,
    ) -> Option<Timestamp> {
        if validation.is_zero() {
            return None;
        }
        // Any validation at all costs at least a millisecond, so a peer cannot have its input
        // validated in sub-millisecond pulls for free.
        let millis = u32::try_from(validation.as_millis().max(1)).unwrap_or(u32::MAX);
        match self.counters.charge(key, millis, budget).await {
            Ok(Verdict::Admitted { remaining: 0 }) => {
                tracing::info!(millis, "the peer's CPU budget is now spent");
                self.counters
                    .peek(key, budget)
                    .await
                    .ok()
                    .and_then(|verdict| match verdict {
                        Verdict::Limited { retry_after } => Some(retry_after),
                        Verdict::Admitted { .. } => None,
                    })
            }
            Ok(Verdict::Admitted { .. }) => None,
            Ok(Verdict::Limited { retry_after }) => {
                tracing::info!(millis, %retry_after, "the peer's CPU budget was overrun");
                Some(retry_after)
            }
            Err(error) => {
                tracing::error!(%error, millis, "the CPU budget could not be charged");
                None
            }
        }
    }

    /// Apply the failure policy.
    #[allow(clippy::too_many_arguments)]
    async fn failed(
        &self,
        job: &JobKey,
        tier: PeerTier,
        state: JobState,
        started: Timestamp,
        ended: Timestamp,
        failure: PullFailure,
        cpu_queue: Option<Timestamp>,
    ) -> JobOutcome {
        let failures = state.failures.saturating_add(1);
        let mut next = next_after_failure(ended, failures, self.jitter.as_ref());
        if let Some(until) = cpu_queue {
            next = next.max(until);
        }
        let circuit = match &failure {
            PullFailure::Transport { .. } => None,
            PullFailure::Validation { .. } => {
                self.spend_error_budget(job, tier, ended, failures).await
            }
        };
        tracing::warn!(
            kind = failure.kind(),
            detail = failure.detail(),
            failures,
            %next,
            circuit = ?circuit,
            "a pull failed"
        );
        self.finish(
            job,
            JobState {
                next_due: next,
                last_pull: Some(started),
                failures,
            },
            ended,
        )
        .await;
        JobOutcome::Failed {
            kind: failure.kind(),
            failures,
            next,
            circuit,
        }
    }

    /// Charge one unit of `job`'s peer's error budget; open its circuit when that spends it.
    async fn spend_error_budget(
        &self,
        job: &JobKey,
        tier: PeerTier,
        now: Timestamp,
        failures: u32,
    ) -> Option<Timestamp> {
        let key = CounterKey::PeerPullErrors(job.peer.as_str().to_owned());
        let spent = match self.counters.hit(&key, tier.pull_errors()).await {
            Ok(Verdict::Admitted { remaining }) => remaining == 0,
            Ok(Verdict::Limited { .. }) => true,
            Err(error) => {
                tracing::error!(%error, "the error budget could not be charged; read as spent");
                true
            }
        };
        if !spent {
            return None;
        }
        let until = deadline(now, backoff(failures));
        match self.jobs.open_circuit(&job.peer, now, until).await {
            Ok(open) => {
                tracing::warn!(peer = %job.peer, until = %open, "the peer's error budget is spent; its circuit is open");
                Some(open)
            }
            Err(error) => {
                tracing::error!(%error, "the circuit could not be opened");
                None
            }
        }
    }

    /// Record `state` and release the lease, logging a lease lost to another replica.
    async fn finish(&self, job: &JobKey, state: JobState, now: Timestamp) {
        match self.jobs.finish(job, &self.holder, state, now).await {
            Ok(true) => {}
            Ok(false) => tracing::warn!(
                "the lease lapsed and another replica claimed the job; this pull's schedule is dropped"
            ),
            Err(error) => tracing::error!(
                %error,
                "the job's schedule could not be recorded; its lease lapses on its own"
            ),
        }
    }

    /// Act on a hint that `job`'s album has news: bring its next pull earlier, unless its peer
    /// is on probation or blocked.
    #[instrument(skip(self), fields(job = %job))]
    pub async fn hint(&self, job: &JobKey) -> HintOutcome {
        let now = self.clock.now();
        let Some(record) = self.peer(job).await else {
            return HintOutcome::Unavailable;
        };
        if record.as_ref().is_some_and(PeerRecord::is_blocked)
            || !PeerTier::of(record.as_ref(), now).honours_hints()
        {
            tracing::debug!("a hint from a peer on probation or blocked was ignored");
            return HintOutcome::Ignored;
        }
        match self.jobs.hint(job, now, HINT_FLOOR).await {
            Ok(Some(at)) => {
                tracing::debug!(%at, "a hint brought the next pull earlier");
                HintOutcome::Moved(at)
            }
            Ok(None) => HintOutcome::Unchanged,
            Err(error) => {
                tracing::error!(%error, "the hint could not be recorded");
                HintOutcome::Unavailable
            }
        }
    }
}

#[cfg(test)]
mod tests;
