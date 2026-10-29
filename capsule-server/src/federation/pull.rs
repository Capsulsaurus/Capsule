//! The egress pull worker's engine: jobs, their schedule, their lease, and what a failure costs
//! the source peer (design/federation.md, Egress Pull Worker; #964).
//!
//! # The unit of work is a job
//!
//! A [`JobKey`] is one local album subscription and its source peer. Every job is pulled on a
//! schedule of its own; a peer's jobs share that peer's budgets and its circuit breaker, because
//! a peer is the blast-radius boundary everywhere else in federation.
//!
//! # What lives where
//!
//! - This file — the job's identity and the **policy**: when a job is next due after a success
//!   ([`next_after_success`]), after a failure ([`next_after_failure`], over [`backoff`]), and how
//!   far a hint may move it ([`HINT_FLOOR`]). Pure functions over values, with randomness
//!   injected through [`Jitter`] so every interval is asserted exactly.
//! - [`jobs`] — [`PullJobs`], the volatile-state port that holds each job's schedule, the lease
//!   that keeps two replicas from pulling one job, and each peer's circuit; and
//!   [`InMemoryPullJobs`], its deterministic double.
//! - [`valkey`] — [`ValkeyPullJobs`](valkey::ValkeyPullJobs), the production adapter.
//! - [`conformance`] — the suite both adapters pass.
//! - [`worker`] — [`PullWorker`], which drives one round of due jobs through a [`Puller`] and
//!   applies the failure policy: back-off, the error budget, the circuit, the CPU budget.
//!
//! # Times are the caller's
//!
//! Every port operation takes the instant it decides at, as the counter port does, so the
//! double and the Valkey adapter are driven through one suite with absolute instants and no
//! second clock decides anything.

use std::fmt;

use jiff::{SignedDuration, Timestamp};

use super::PeerId;
use crate::store::{AlbumId, deadline};

pub mod conformance;
pub mod jobs;
pub mod valkey;
pub mod worker;

pub use self::jobs::{InMemoryPullJobs, JobState, PullJobs};
pub use self::worker::{
    HintOutcome, JobOutcome, JobSource, PULL_DEADLINE, PullFailure, PullReport, PullWorker,
    PullWorkerParts, Puller, TickReport,
};

/// How often every job is pulled, before jitter (design/federation.md).
///
/// Fifteen minutes, the revocation list's cache bound: a pull that ran less often than the list
/// may be relied on would carry no fresher news of a revocation than the list already does.
pub const PULL_INTERVAL: SignedDuration = SignedDuration::from_mins(15);

/// How far either way of [`PULL_INTERVAL`] one pull is moved, in percent.
///
/// Twenty, so the jobs of one restart spread out over six minutes instead of arriving at a peer
/// together on every round.
pub const PULL_JITTER_PERCENT: i64 = 20;

/// The soonest a hint may bring a job's next pull after its previous one.
///
/// One minute, so a burst of hints is one pull.
pub const HINT_FLOOR: SignedDuration = SignedDuration::from_mins(1);

/// The first back-off after a failed pull, before jitter.
pub const BACKOFF_FLOOR: SignedDuration = SignedDuration::from_mins(1);

/// The longest back-off a run of failures reaches, before jitter.
pub const BACKOFF_CAP: SignedDuration = SignedDuration::from_hours(6);

/// How long a claimed job stays claimed if its holder never finishes it.
///
/// Ten minutes, twice [`worker::PULL_DEADLINE`]: a holder that is alive finishes inside the
/// deadline whatever the peer does, so a lease that lapses belongs to a holder that died, and
/// another replica may take the job.
pub const PULL_LEASE: SignedDuration = SignedDuration::from_mins(10);

/// How long a job's schedule is kept past the latest instant it names.
///
/// A job that leaves the source (its subscription ended) is never claimed again, so nothing
/// finishes it; its schedule is collected this long after it was last due rather than kept
/// forever. A week, so an outage of the subscription store does not reset every job's back-off.
pub const JOB_STATE_RETENTION: SignedDuration = SignedDuration::from_hours(24 * 7);

/// One job: a local album subscription and the peer it is pulled from.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JobKey {
    /// The album's home server, which every pull of this job fetches from.
    pub peer: PeerId,
    /// The album, as its home server names it.
    pub album: AlbumId,
}

impl JobKey {
    /// The job pulling `album` from `peer`.
    pub fn new(peer: PeerId, album: AlbumId) -> Self {
        Self { peer, album }
    }
}

impl fmt::Debug for JobKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "JobKey({}/{})", self.peer, self.album)
    }
}

impl fmt::Display for JobKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.peer, self.album)
    }
}

/// Where the schedule's randomness comes from.
///
/// Injected so a test asserts an exact interval and production draws from the operating
/// system. Jitter needs no secrecy; the system source is used because the crate already links
/// one and a second generator would be a dependency for nothing.
pub trait Jitter: fmt::Debug + Send + Sync {
    /// A value drawn uniformly from `0..bound`, or `0` when `bound` is `0`.
    fn below(&self, bound: u64) -> u64;
}

/// [`Jitter`] from the operating system's generator.
#[derive(Debug)]
pub struct SystemJitter {
    random: ring::rand::SystemRandom,
}

impl SystemJitter {
    /// Jitter from the operating system's generator.
    pub fn new() -> Self {
        Self {
            random: ring::rand::SystemRandom::new(),
        }
    }
}

impl Default for SystemJitter {
    fn default() -> Self {
        Self::new()
    }
}

impl Jitter for SystemJitter {
    fn below(&self, bound: u64) -> u64 {
        use ring::rand::SecureRandom as _;
        if bound == 0 {
            return 0;
        }
        let mut bytes = [0_u8; 8];
        if self.random.fill(&mut bytes).is_err() {
            // The midpoint: a schedule with no jitter this once is still a schedule, and the
            // interval it lands on is the one the design names.
            tracing::warn!("the system generator failed; this interval is not jittered");
            return bound / 2;
        }
        // The modulo bias is below one part in 2^40 for any bound this module asks for, which
        // is no bias a schedule can show.
        u64::from_le_bytes(bytes) % bound
    }
}

/// A span in whole microseconds, saturating.
fn span_micros(span: SignedDuration) -> i64 {
    i64::try_from(span.as_micros()).unwrap_or(i64::MAX)
}

/// When a job whose pull succeeded at `now` is next due: [`PULL_INTERVAL`], moved up to
/// [`PULL_JITTER_PERCENT`] either way.
pub fn next_after_success(now: Timestamp, jitter: &dyn Jitter) -> Timestamp {
    let interval = span_micros(PULL_INTERVAL);
    let spread = interval * PULL_JITTER_PERCENT / 100;
    let drawn = jitter.below(u64::try_from(2 * spread + 1).unwrap_or(0));
    let offset = i64::try_from(drawn).unwrap_or(0) - spread;
    deadline(now, SignedDuration::from_micros(interval + offset))
}

/// The back-off ceiling after `failures` consecutive failed pulls, before jitter.
///
/// [`BACKOFF_FLOOR`] after the first, doubling with each one after it, never past
/// [`BACKOFF_CAP`]. Zero failures is read as one: the function is only asked after a failure.
pub fn backoff(failures: u32) -> SignedDuration {
    let doublings = failures.saturating_sub(1);
    let floor = span_micros(BACKOFF_FLOOR);
    let cap = span_micros(BACKOFF_CAP);
    let scaled = 1_i64
        .checked_shl(doublings)
        .filter(|factor| *factor > 0)
        .and_then(|factor| floor.checked_mul(factor))
        .unwrap_or(cap);
    SignedDuration::from_micros(scaled.min(cap))
}

/// When a job whose `failures`-th consecutive pull failed at `now` is next due: anywhere from
/// `now` to [`backoff`] past it, uniformly (full jitter).
pub fn next_after_failure(now: Timestamp, failures: u32, jitter: &dyn Jitter) -> Timestamp {
    let ceiling = span_micros(backoff(failures));
    let drawn = jitter.below(u64::try_from(ceiling).unwrap_or(0).saturating_add(1));
    deadline(
        now,
        SignedDuration::from_micros(i64::try_from(drawn).unwrap_or(ceiling)),
    )
}

#[cfg(test)]
mod tests;
