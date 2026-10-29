//! The one suite every [`PullJobs`] adapter must pass.
//!
//! # The rules the suite exists to protect
//!
//! - **One holder per job.** A due, unheld job is claimed once; a live lease refuses every other
//!   claim; a lapsed lease is no lease.
//! - **The schedule is honoured.** A finished job is not claimable before the due it recorded,
//!   and is at it.
//! - **A lost lease writes nothing.** A holder overtaken by another claimant cannot overwrite
//!   the schedule the other records.
//! - **A hint only ever brings a pull earlier**, and never inside the floor after the last one.
//! - **A circuit only ever opens wider** until it lapses.
//!
//! # Reusing a store
//!
//! Every case scopes its own job and peer, so cases may share one store and [`run_all`] does.
//! Instants are absolute and passed in; no adapter clock is read.

use jiff::{SignedDuration, Timestamp};

use super::JobKey;
use super::jobs::{JobState, PullJobs};
use crate::federation::PeerId;
use crate::store::{AlbumId, StoreError};

/// Unwrap a store result, failing with the operation that was expected to work.
#[track_caller]
fn ok<T>(result: Result<T, StoreError>, doing: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("a conforming pull-job store must succeed at {doing}: {error}"),
    }
}

/// The instant every case starts at.
fn start() -> Timestamp {
    Timestamp::from_second(1_800_000_000).unwrap_or(Timestamp::UNIX_EPOCH)
}

fn at(offset: SignedDuration) -> Timestamp {
    start().checked_add(offset).unwrap_or(Timestamp::UNIX_EPOCH)
}

fn mins(n: i64) -> SignedDuration {
    SignedDuration::from_mins(n)
}

/// A job scoped to `case` within `run`.
fn job(run: &str, case: &str) -> JobKey {
    JobKey::new(
        PeerId::new(format!("{case}.conformance.test")),
        AlbumId::new(format!("album-{run}")),
    )
}

/// Run every case against `store`, under a run id of its own.
///
/// The id scopes every job and peer the cases touch, so the suite can be re-run against a
/// server that still holds the previous run's keys.
pub async fn run_all(store: &dyn PullJobs) {
    let run = uuid::Uuid::now_v7().to_string();
    a_due_job_is_claimed_by_one_holder(store, &run).await;
    a_lapsed_lease_is_claimable_by_another(store, &run).await;
    a_finished_job_is_next_claimable_at_its_due(store, &run).await;
    a_holder_overtaken_by_another_claim_writes_nothing(store, &run).await;
    a_finish_round_trips_the_whole_schedule(store, &run).await;
    a_hint_brings_a_pull_earlier_but_not_inside_the_floor(store, &run).await;
    a_hint_never_moves_a_pull_later_or_creates_one(store, &run).await;
    a_circuit_opens_widens_and_lapses(store, &run).await;
}

/// A due, unheld job is claimed once; a second holder is refused while the lease is live.
pub async fn a_due_job_is_claimed_by_one_holder(store: &dyn PullJobs, run: &str) {
    let job = job(run, "claim-once");
    let claimed = ok(store.claim(&job, "a", start(), mins(10)).await, "claim");
    assert_eq!(
        claimed,
        Some(JobState::fresh(start())),
        "a new job is due now"
    );
    assert_eq!(
        ok(store.claim(&job, "b", at(mins(9)), mins(10)).await, "claim"),
        None,
        "a live lease refuses another holder"
    );
    assert_eq!(
        ok(store.claim(&job, "a", at(mins(9)), mins(10)).await, "claim"),
        None,
        "a live lease refuses its own holder too: a claim is not a renewal"
    );
}

/// A lease that has lapsed is no lease, whoever held it.
pub async fn a_lapsed_lease_is_claimable_by_another(store: &dyn PullJobs, run: &str) {
    let job = job(run, "lapsed");
    assert!(ok(store.claim(&job, "a", start(), mins(10)).await, "claim").is_some());
    assert!(
        ok(
            store.claim(&job, "b", at(mins(10)), mins(10)).await,
            "claim"
        )
        .is_some(),
        "a lease ends at its own instant"
    );
}

/// A finished job is not claimable before the due it recorded, and is at it.
pub async fn a_finished_job_is_next_claimable_at_its_due(store: &dyn PullJobs, run: &str) {
    let job = job(run, "schedule");
    assert!(ok(store.claim(&job, "a", start(), mins(10)).await, "claim").is_some());
    let state = JobState {
        next_due: at(mins(15)),
        last_pull: Some(start()),
        failures: 0,
    };
    assert!(ok(
        store.finish(&job, "a", state, at(mins(1))).await,
        "finish"
    ));
    assert_eq!(
        ok(
            store.claim(&job, "b", at(mins(14)), mins(10)).await,
            "claim"
        ),
        None,
        "not due yet"
    );
    assert_eq!(
        ok(
            store.claim(&job, "b", at(mins(15)), mins(10)).await,
            "claim"
        ),
        Some(state),
        "due at its own instant, and the claim answers the schedule recorded"
    );
    assert!(
        !ok(store.finish(&job, "a", state, at(mins(16))).await, "finish"),
        "a finish is once per claim"
    );
}

/// A holder whose lease lapsed and was claimed by another records nothing.
pub async fn a_holder_overtaken_by_another_claim_writes_nothing(store: &dyn PullJobs, run: &str) {
    let job = job(run, "overtaken");
    assert!(ok(store.claim(&job, "slow", start(), mins(10)).await, "claim").is_some());
    assert!(
        ok(
            store.claim(&job, "fast", at(mins(11)), mins(10)).await,
            "claim"
        )
        .is_some()
    );
    let fast = JobState {
        next_due: at(mins(30)),
        last_pull: Some(at(mins(11))),
        failures: 0,
    };
    let slow = JobState {
        next_due: at(mins(12)),
        last_pull: Some(start()),
        failures: 4,
    };
    assert!(
        !ok(
            store.finish(&job, "slow", slow, at(mins(12))).await,
            "finish"
        ),
        "the overtaken holder is refused"
    );
    assert!(ok(
        store.finish(&job, "fast", fast, at(mins(12))).await,
        "finish"
    ));
    assert_eq!(
        ok(
            store.claim(&job, "next", at(mins(30)), mins(10)).await,
            "claim"
        ),
        Some(fast),
        "the schedule is the one the current holder recorded"
    );
}

/// Every field of the schedule survives the store, a missing last pull and a failure count
/// included.
pub async fn a_finish_round_trips_the_whole_schedule(store: &dyn PullJobs, run: &str) {
    let job = job(run, "round-trip");
    assert!(ok(store.claim(&job, "a", start(), mins(10)).await, "claim").is_some());
    let state = JobState {
        next_due: at(SignedDuration::from_micros(123_456_789)),
        last_pull: None,
        failures: 7,
    };
    assert!(ok(store.finish(&job, "a", state, start()).await, "finish"));
    assert_eq!(
        ok(
            store.claim(&job, "b", state.next_due, mins(10)).await,
            "claim"
        ),
        Some(state),
        "microsecond instants and counts are exact"
    );
}

/// A hint brings the next pull to `now`, or to the floor past the last pull when that is later.
pub async fn a_hint_brings_a_pull_earlier_but_not_inside_the_floor(
    store: &dyn PullJobs,
    run: &str,
) {
    let job = job(run, "hint");
    assert!(ok(store.claim(&job, "a", start(), mins(10)).await, "claim").is_some());
    let state = JobState {
        next_due: at(mins(15)),
        last_pull: Some(start()),
        failures: 0,
    };
    assert!(ok(store.finish(&job, "a", state, start()).await, "finish"));

    assert_eq!(
        ok(
            store
                .hint(&job, at(SignedDuration::from_secs(10)), mins(1))
                .await,
            "hint"
        ),
        Some(at(mins(1))),
        "inside the floor, the pull moves to the floor"
    );
    assert_eq!(
        ok(
            store
                .hint(&job, at(SignedDuration::from_secs(20)), mins(1))
                .await,
            "hint"
        ),
        None,
        "a burst of hints is one pull"
    );
    assert_eq!(
        ok(store.claim(&job, "b", at(mins(1)), mins(10)).await, "claim").map(|s| s.next_due),
        Some(at(mins(1))),
        "the hinted job is claimable at its new due"
    );
}

/// A hint never moves a pull later, and a job with no schedule is left as it is: already due.
pub async fn a_hint_never_moves_a_pull_later_or_creates_one(store: &dyn PullJobs, run: &str) {
    let unknown = job(run, "hint-unknown");
    assert_eq!(
        ok(store.hint(&unknown, start(), mins(1)).await, "hint"),
        None
    );
    assert_eq!(
        ok(store.claim(&unknown, "a", start(), mins(10)).await, "claim"),
        Some(JobState::fresh(start())),
        "a hint wrote no schedule"
    );

    let soon = job(run, "hint-soon");
    assert!(ok(store.claim(&soon, "a", start(), mins(10)).await, "claim").is_some());
    let state = JobState {
        next_due: at(mins(2)),
        last_pull: Some(start()),
        failures: 1,
    };
    assert!(ok(store.finish(&soon, "a", state, start()).await, "finish"));
    assert_eq!(
        ok(store.hint(&soon, at(mins(3)), mins(1)).await, "hint"),
        None,
        "a pull already due sooner keeps its time"
    );
}

/// A circuit opens, only ever widens, and is closed at its own instant.
pub async fn a_circuit_opens_widens_and_lapses(store: &dyn PullJobs, run: &str) {
    let peer = PeerId::new(format!("circuit-{run}.conformance.test"));
    assert_eq!(ok(store.circuit(&peer, start()).await, "circuit"), None);
    assert_eq!(
        ok(
            store.open_circuit(&peer, start(), at(mins(10))).await,
            "open"
        ),
        at(mins(10))
    );
    assert_eq!(
        ok(
            store.open_circuit(&peer, start(), at(mins(5))).await,
            "open"
        ),
        at(mins(10)),
        "a shorter opening does not shorten the circuit"
    );
    assert_eq!(
        ok(store.circuit(&peer, at(mins(9))).await, "circuit"),
        Some(at(mins(10)))
    );
    assert_eq!(
        ok(
            store.open_circuit(&peer, at(mins(9)), at(mins(20))).await,
            "open"
        ),
        at(mins(20)),
        "a longer opening widens it"
    );
    assert_eq!(
        ok(store.circuit(&peer, at(mins(20))).await, "circuit"),
        None,
        "closed at its own instant"
    );
}
