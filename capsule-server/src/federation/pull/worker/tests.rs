use std::collections::VecDeque;
use std::sync::Mutex;

use jiff::SignedDuration;

use super::*;
use crate::counter::{CounterStore, InMemoryCounters, budgets};
use crate::federation::pull::{BACKOFF_FLOOR, InMemoryPullJobs, PULL_INTERVAL};
use crate::federation::{InMemoryPeers, PeerId};
use crate::store::AlbumId;
use crate::store::memory::ManualClock;

/// Jitter that always draws the top of its bound, so every interval is its ceiling.
#[derive(Debug)]
struct Highest;

impl Jitter for Highest {
    fn below(&self, bound: u64) -> u64 {
        bound.saturating_sub(1)
    }
}

/// A [`Puller`] that answers a scripted sequence of reports and records every call.
#[derive(Debug, Default)]
struct Scripted {
    reports: Mutex<VecDeque<PullReport>>,
    calls: Mutex<Vec<(JobKey, PeerTier)>>,
}

impl Scripted {
    fn then(&self, report: PullReport) -> &Self {
        self.reports.lock().expect("lock").push_back(report);
        self
    }

    fn calls(&self) -> Vec<(JobKey, PeerTier)> {
        self.calls.lock().expect("lock").clone()
    }
}

impl Puller for Scripted {
    fn pull<'a>(
        &'a self,
        job: &'a JobKey,
        tier: PeerTier,
    ) -> Pin<Box<dyn Future<Output = PullReport> + Send + 'a>> {
        self.calls.lock().expect("lock").push((job.clone(), tier));
        let report = self
            .reports
            .lock()
            .expect("lock")
            .pop_front()
            .expect("a pull the test scripted");
        Box::pin(async move { report })
    }
}

#[derive(Debug)]
struct Listed(Vec<JobKey>);

impl JobSource for Listed {
    fn jobs(&self) -> StoreFuture<'_, Vec<JobKey>> {
        let jobs = self.0.clone();
        Box::pin(async move { Ok(jobs) })
    }
}

fn ok(validation_ms: u64) -> PullReport {
    PullReport {
        result: Ok(()),
        validation: Duration::from_millis(validation_ms),
    }
}

fn transport() -> PullReport {
    PullReport {
        result: Err(PullFailure::Transport {
            detail: "connection refused".to_owned(),
        }),
        validation: Duration::ZERO,
    }
}

fn malformed() -> PullReport {
    PullReport {
        result: Err(PullFailure::Validation {
            detail: "invariant 3: unknown crypto_suite_id".to_owned(),
        }),
        validation: Duration::from_millis(2),
    }
}

const PEER: &str = "home.test";

struct Rig {
    clock: ManualClock,
    jobs: Arc<InMemoryPullJobs>,
    peers: Arc<InMemoryPeers>,
    counters: Arc<InMemoryCounters>,
    puller: Arc<Scripted>,
    worker: PullWorker,
    job: JobKey,
}

fn start() -> Timestamp {
    Timestamp::from_second(1_800_000_000).expect("an instant")
}

impl Rig {
    fn new() -> Self {
        let clock = ManualClock::new(start());
        let shared: Arc<dyn Clock> = Arc::new(clock.clone());
        let jobs = Arc::new(InMemoryPullJobs::new());
        let peers = Arc::new(InMemoryPeers::new());
        let counters = Arc::new(InMemoryCounters::new());
        let puller = Arc::new(Scripted::default());
        let job = JobKey::new(PeerId::new(PEER), AlbumId::new("album-1"));
        let worker = PullWorker::with_holder(
            PullWorkerParts {
                jobs: jobs.clone(),
                source: Arc::new(Listed(vec![job.clone()])),
                puller: puller.clone(),
                peers: peers.clone(),
                counters: CounterContext::new(counters.clone(), shared.clone()),
                clock: shared,
                jitter: Arc::new(Highest),
            },
            "replica-a",
        );
        Self {
            clock,
            jobs,
            peers,
            counters,
            puller,
            worker,
            job,
        }
    }

    /// Pin the peer far enough back that it is past probation.
    async fn established(&self) {
        let pinned = start()
            .checked_sub(budgets::PEER_PROBATION + SignedDuration::from_hours(1))
            .expect("an instant");
        self.peers
            .pin(&self.job.peer, [7; 32], pinned)
            .await
            .expect("pinned");
    }

    fn now(&self) -> Timestamp {
        self.clock.now()
    }

    fn advance_to(&self, at: Timestamp) {
        let by = at.duration_since(self.now());
        if by.is_positive() {
            self.clock.advance(by);
        }
    }

    async fn round(&self) -> JobOutcome {
        let report = self.worker.tick().await;
        assert_eq!(report.outcomes.len(), 1);
        report.outcomes[0].1.clone()
    }
}

fn plus(at: Timestamp, span: SignedDuration) -> Timestamp {
    at.checked_add(span).expect("an instant")
}

#[tokio::test]
async fn a_due_job_is_pulled_and_rescheduled_on_the_jittered_interval() {
    let rig = Rig::new();
    rig.established().await;
    rig.puller.then(ok(5));
    let interval_ceiling = SignedDuration::from_mins(18);
    assert_eq!(
        rig.round().await,
        JobOutcome::Pulled {
            next: plus(start(), interval_ceiling)
        }
    );
    assert_eq!(
        rig.puller.calls(),
        vec![(rig.job.clone(), PeerTier::Established)]
    );
    assert_eq!(rig.round().await, JobOutcome::NotDue, "not due again yet");

    rig.advance_to(plus(start(), interval_ceiling));
    rig.puller.then(ok(5));
    assert!(matches!(rig.round().await, JobOutcome::Pulled { .. }));
    assert!(interval_ceiling > PULL_INTERVAL);
}

#[tokio::test]
async fn a_job_another_replica_holds_is_not_pulled() {
    let rig = Rig::new();
    let claimed = rig
        .jobs
        .claim(
            &rig.job,
            "replica-b",
            start(),
            crate::federation::pull::PULL_LEASE,
        )
        .await
        .expect("claimed");
    assert!(claimed.is_some());
    assert_eq!(rig.round().await, JobOutcome::NotDue);
    assert!(rig.puller.calls().is_empty());
}

#[tokio::test]
async fn a_blocked_peers_job_does_not_run() {
    let rig = Rig::new();
    rig.peers
        .block(&rig.job.peer, start(), None)
        .await
        .expect("blocked");
    assert_eq!(rig.round().await, JobOutcome::PeerBlocked);
    assert!(rig.puller.calls().is_empty());
}

#[tokio::test]
async fn an_unknown_peer_is_pulled_on_probation() {
    let rig = Rig::new();
    rig.puller.then(ok(1));
    assert!(matches!(rig.round().await, JobOutcome::Pulled { .. }));
    assert_eq!(rig.puller.calls()[0].1, PeerTier::Probation);
}

#[tokio::test]
async fn a_transport_failure_backs_off_and_spends_no_error_budget() {
    let rig = Rig::new();
    for failures in 1..=4_u32 {
        rig.puller.then(transport());
        let at = rig.now();
        assert_eq!(
            rig.round().await,
            JobOutcome::Failed {
                kind: "transport",
                failures,
                next: plus(at, backoff(failures)),
                circuit: None,
            }
        );
        rig.advance_to(plus(at, backoff(failures)));
    }
    let errors = CounterKey::PeerPullErrors(PEER.to_owned());
    assert_eq!(
        rig.counters
            .peek(&errors, budgets::PEER_PULL_ERRORS_PROBATION, rig.now())
            .await
            .expect("peeked"),
        Verdict::Admitted {
            remaining: budgets::PEER_PULL_ERRORS_PROBATION.limit
        },
        "an unreachable peer is backed off, not distrusted"
    );

    rig.puller.then(ok(1));
    assert!(matches!(rig.round().await, JobOutcome::Pulled { .. }));
    let state = rig
        .jobs
        .claim(
            &rig.job,
            "probe",
            plus(rig.now(), BACKOFF_CAP_PROBE),
            PULL_LEASE,
        )
        .await
        .expect("claimed")
        .expect("due by then");
    assert_eq!(state.failures, 0, "a success clears the run of failures");
}

/// Far enough ahead that any job is due.
const BACKOFF_CAP_PROBE: SignedDuration = SignedDuration::from_hours(7);

#[tokio::test]
async fn validation_failures_spend_the_error_budget_and_open_the_circuit() {
    let rig = Rig::new();
    let limit = budgets::PEER_PULL_ERRORS_PROBATION.limit;
    for failures in 1..limit {
        rig.puller.then(malformed());
        let at = rig.now();
        assert!(
            matches!(
                rig.round().await,
                JobOutcome::Failed {
                    kind: "validation",
                    circuit: None,
                    ..
                }
            ),
            "failure {failures} is inside the budget"
        );
        rig.advance_to(plus(at, backoff(failures)));
    }

    rig.puller.then(malformed());
    let at = rig.now();
    let open_until = plus(at, backoff(limit));
    assert_eq!(
        rig.round().await,
        JobOutcome::Failed {
            kind: "validation",
            failures: limit,
            next: open_until,
            circuit: Some(open_until),
        },
        "the failure that spends the budget opens the circuit for its back-off"
    );

    rig.advance_to(plus(at, SignedDuration::from_secs(1)));
    assert_eq!(
        rig.round().await,
        JobOutcome::CircuitOpen { until: open_until }
    );
    assert_eq!(
        rig.worker.run_job(&rig.job).await,
        JobOutcome::CircuitOpen { until: open_until },
        "none of the peer's jobs runs"
    );
    assert_eq!(rig.puller.calls().len() as u32, limit);
}

#[tokio::test]
async fn a_spent_cpu_budget_queues_the_job_without_failing_it() {
    let rig = Rig::new();
    let cpu = CounterKey::PeerPullCpu(PEER.to_owned());
    let budget = budgets::PEER_PULL_CPU_MILLIS_PROBATION;
    rig.counters
        .charge(&cpu, budget.limit, budget, start())
        .await
        .expect("charged");
    let window_end = plus(start(), budget.window);
    assert_eq!(
        rig.round().await,
        JobOutcome::Deferred { until: window_end }
    );
    assert!(rig.puller.calls().is_empty(), "nothing was pulled");
    assert_eq!(
        rig.round().await,
        JobOutcome::NotDue,
        "queued to the window's end"
    );

    rig.advance_to(window_end);
    rig.puller.then(ok(1));
    assert!(matches!(rig.round().await, JobOutcome::Pulled { .. }));
}

#[tokio::test]
async fn validation_time_is_metered_and_a_pull_that_spends_it_waits_out_the_window() {
    let rig = Rig::new();
    let budget = budgets::PEER_PULL_CPU_MILLIS_PROBATION;
    rig.puller.then(ok(u64::from(budget.limit)));
    assert_eq!(
        rig.round().await,
        JobOutcome::Pulled {
            next: plus(start(), budget.window)
        },
        "the next pull waits for the window to turn, not just the interval"
    );
    let cpu = CounterKey::PeerPullCpu(PEER.to_owned());
    assert!(
        !rig.counters
            .peek(&cpu, budget, start())
            .await
            .expect("peeked")
            .admits()
    );
}

#[tokio::test]
async fn a_sub_millisecond_validation_still_costs_a_millisecond() {
    let rig = Rig::new();
    rig.puller.then(PullReport {
        result: Ok(()),
        validation: Duration::from_micros(10),
    });
    rig.round().await;
    let cpu = CounterKey::PeerPullCpu(PEER.to_owned());
    let budget = budgets::PEER_PULL_CPU_MILLIS_PROBATION;
    assert_eq!(
        rig.counters
            .peek(&cpu, budget, start())
            .await
            .expect("peeked"),
        Verdict::Admitted {
            remaining: budget.limit - 1
        }
    );
}

#[tokio::test]
async fn a_hint_brings_an_established_peers_pull_earlier_but_not_inside_a_minute() {
    let rig = Rig::new();
    rig.established().await;
    rig.puller.then(ok(1));
    rig.round().await;

    rig.advance_to(plus(start(), SignedDuration::from_secs(5)));
    assert_eq!(
        rig.worker.hint(&rig.job).await,
        HintOutcome::Moved(plus(start(), HINT_FLOOR))
    );
    assert_eq!(rig.worker.hint(&rig.job).await, HintOutcome::Unchanged);
    rig.advance_to(plus(start(), HINT_FLOOR));
    rig.puller.then(ok(1));
    assert!(matches!(rig.round().await, JobOutcome::Pulled { .. }));
}

#[tokio::test]
async fn a_hint_from_a_peer_on_probation_is_ignored() {
    let rig = Rig::new();
    rig.peers
        .pin(&rig.job.peer, [7; 32], start())
        .await
        .expect("pinned");
    rig.puller.then(ok(1));
    rig.round().await;
    assert_eq!(rig.worker.hint(&rig.job).await, HintOutcome::Ignored);
    assert_eq!(rig.round().await, JobOutcome::NotDue);
}

#[tokio::test]
async fn a_hint_from_a_blocked_peer_is_ignored() {
    let rig = Rig::new();
    rig.established().await;
    rig.peers
        .block(&rig.job.peer, start(), None)
        .await
        .expect("blocked");
    assert_eq!(rig.worker.hint(&rig.job).await, HintOutcome::Ignored);
}

#[test]
fn the_deadline_is_half_the_lease() {
    assert_eq!(
        SignedDuration::try_from(PULL_DEADLINE).expect("a span") * 2,
        PULL_LEASE
    );
    assert!(BACKOFF_FLOOR < PULL_LEASE);
}
