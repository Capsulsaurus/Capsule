//! The one suite every counter adapter must pass.
//!
//! The property that matters is that charging and deciding are one operation. A single-threaded
//! sequence cannot exhibit the race that makes read-then-write wrong — the same limit the index
//! conformance note records — so most of what is asserted here is the *observable consequence*:
//! the n-th hit inside a window is refused, and no sequence of calls admits more than the budget.
//! [`concurrent_hits_admit_exactly_the_budget`] is the one case that does race, on a
//! multi-threaded runtime, and it is the case a Valkey adapter built on read-then-`INCR` fails.
//!
//! Like [`crate::store::conformance`], this lives in `src/` because it is part of the contract:
//! the in-memory double is a legitimate stand-in for Valkey only to the extent it passes the
//! same cases the container-backed adapter does.
//!
//! # Time
//!
//! [`CounterStore::hit`] takes `at`, so the suite moves time by passing later instants rather
//! than through a harness clock. That is a property of the port worth keeping: the window a
//! limit measures is a fact about the caller's clock, and an adapter that measured it with a
//! clock of its own would be a second clock for one fact.
//!
//! # Reusing a store
//!
//! Every case scopes its key to itself and resets it first, so cases may share one store — and
//! [`run_all`] does — and may be re-run against a server that still holds the previous run.

use std::sync::Arc;

use jiff::{SignedDuration, Timestamp};

use super::{Budget, CounterKey, CounterStore, Verdict};
use crate::store::{StoreError, UserId, deadline};

/// Unwrap a store result, failing with the operation that was expected to work.
fn ok<T>(result: Result<T, StoreError>, operation: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("a conforming adapter must succeed at {operation}: {error}"),
    }
}

/// A key scoped to one case.
fn key(case: &str) -> CounterKey {
    CounterKey::LoginAttempts(UserId::new(format!("counter-conformance-{case}")))
}

/// Three hits per ten minutes.
fn budget() -> Budget {
    Budget::new(3, SignedDuration::from_mins(10))
}

/// `mins` minutes after the epoch.
fn at(mins: i64) -> Timestamp {
    deadline(Timestamp::UNIX_EPOCH, SignedDuration::from_mins(mins))
}

/// A fresh key for `case`: reset, so a store that remembers a previous run starts clean.
async fn fresh(counters: &dyn CounterStore, case: &str) -> CounterKey {
    let key = key(case);
    ok(counters.reset(&key).await, "reset");
    key
}

/// A window admits exactly its budget, then refuses and says when to come back.
pub async fn a_window_admits_exactly_its_budget_and_then_refuses(counters: &dyn CounterStore) {
    let key = fresh(counters, "budget").await;

    for expected_remaining in [2, 1, 0] {
        assert_eq!(
            ok(counters.hit(&key, budget(), at(0)).await, "hit"),
            Verdict::Admitted {
                remaining: expected_remaining
            }
        );
    }

    assert_eq!(
        ok(counters.hit(&key, budget(), at(0)).await, "hit"),
        Verdict::Limited {
            retry_after: at(10)
        },
        "the fourth hit is refused, and is told when to come back"
    );
}

/// A refused hit does not extend the window.
///
/// Otherwise an attacker who keeps hitting a limited key holds it limited forever, which turns a
/// rate limit into a denial of service against the legitimate user.
pub async fn a_refused_hit_does_not_extend_the_window(counters: &dyn CounterStore) {
    let key = fresh(counters, "no-extend").await;
    for _ in 0..3 {
        ok(counters.hit(&key, budget(), at(0)).await, "hit");
    }
    for minute in 1..8 {
        assert!(
            !ok(counters.hit(&key, budget(), at(minute)).await, "hit").admits(),
            "still limited at minute {minute}"
        );
    }

    assert!(
        ok(counters.hit(&key, budget(), at(11)).await, "hit").admits(),
        "the window still ends ten minutes after it opened, not ten after the last attempt"
    );
}

/// A window is measured from its first hit, and a fresh window is a fresh budget.
pub async fn a_window_measures_from_its_first_hit(counters: &dyn CounterStore) {
    let key = fresh(counters, "first-hit").await;
    ok(counters.hit(&key, budget(), at(0)).await, "hit");
    ok(counters.hit(&key, budget(), at(9)).await, "hit");

    assert!(
        ok(counters.hit(&key, budget(), at(11)).await, "hit").admits(),
        "a fresh window opens once the first one passes"
    );
    for _ in 0..2 {
        assert!(ok(counters.hit(&key, budget(), at(11)).await, "hit").admits());
    }
    assert!(
        !ok(counters.hit(&key, budget(), at(11)).await, "hit").admits(),
        "and that fresh window is a fresh budget, not a continuation"
    );
}

/// One account's failed sign-ins are not another's.
pub async fn counters_are_scoped_to_their_key(counters: &dyn CounterStore) {
    let key = fresh(counters, "scoped-a").await;
    let other = fresh(counters, "scoped-b").await;
    for _ in 0..3 {
        ok(counters.hit(&key, budget(), at(0)).await, "hit");
    }

    assert!(
        ok(counters.hit(&other, budget(), at(0)).await, "hit").admits(),
        "one account's failed sign-ins are not another's"
    );
    assert!(
        !ok(counters.hit(&key, budget(), at(0)).await, "hit").admits(),
        "and the first is still limited"
    );
}

/// A reset ends the run: what a successful sign-in does to a failed-attempt counter.
pub async fn a_reset_ends_the_run(counters: &dyn CounterStore) {
    let key = fresh(counters, "reset").await;
    for _ in 0..3 {
        ok(counters.hit(&key, budget(), at(0)).await, "hit");
    }
    assert!(!ok(counters.hit(&key, budget(), at(0)).await, "hit").admits());

    ok(counters.reset(&key).await, "reset");
    assert_eq!(
        ok(counters.hit(&key, budget(), at(0)).await, "hit"),
        Verdict::Admitted { remaining: 2 }
    );
}

/// Peeking charges nothing and answers a verdict, not a number.
pub async fn peeking_charges_nothing_and_answers_a_verdict(counters: &dyn CounterStore) {
    let key = fresh(counters, "peek").await;
    for _ in 0..5 {
        assert_eq!(
            ok(counters.peek(&key, budget(), at(0)).await, "peek"),
            Verdict::Admitted { remaining: 3 },
            "peeking does not spend the budget"
        );
    }

    for _ in 0..3 {
        ok(counters.hit(&key, budget(), at(0)).await, "hit");
    }
    assert_eq!(
        ok(counters.peek(&key, budget(), at(0)).await, "peek"),
        Verdict::Limited {
            retry_after: at(10)
        }
    );
    assert!(
        ok(counters.peek(&key, budget(), at(10)).await, "peek").admits(),
        "a peek past the window sees a fresh budget"
    );
}

/// No sequence of calls admits more than the budget, however peeks are interleaved.
pub async fn no_sequence_of_calls_admits_more_than_the_budget(counters: &dyn CounterStore) {
    let key = fresh(counters, "sequence").await;
    let mut admitted = 0;
    for _ in 0..50 {
        ok(counters.peek(&key, budget(), at(0)).await, "peek");
        if ok(counters.hit(&key, budget(), at(0)).await, "hit").admits() {
            admitted += 1;
        }
    }
    assert_eq!(admitted, 3);
}

/// Racing hits admit exactly the budget — the property read-then-write does not have.
///
/// Every task charges the same key at the same instant. An adapter that read the count, compared
/// it and then incremented would let a burst read the same under-limit value and admit them all;
/// one that charges and decides in one critical section (a mutex, a Lua script) admits three.
/// Meaningful on a multi-threaded runtime; on a single thread it degrades to the sequence case.
pub async fn concurrent_hits_admit_exactly_the_budget(counters: Arc<dyn CounterStore>) {
    let key = fresh(&*counters, "concurrent").await;
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..24 {
        let counters = Arc::clone(&counters);
        let key = key.clone();
        tasks.spawn(async move { ok(counters.hit(&key, budget(), at(0)).await, "hit") });
    }
    let mut admitted = 0;
    while let Some(verdict) = tasks.join_next().await {
        if verdict.expect("a hit task completes").admits() {
            admitted += 1;
        }
    }
    assert_eq!(admitted, 3, "racing hits must admit exactly the budget");
}

/// The partition ceiling every case above leaves alone, with its own key space.
pub const CEILING: usize = 3;

/// A full partition refuses a **new** key in that partition only, and only while it is full.
///
/// `counters` must be built with every partition bounded at [`CEILING`] — the adapter's
/// `with_ceiling` affordance — because the production ceilings are thousands of keys. The
/// contract is the one [the module docs](super) state: past its ceiling a key with no window yet
/// is refused with the error [`is_at_capacity`](super::is_at_capacity) recognises, a key already
/// being counted keeps counting, another variant's partition is not affected, and the partition
/// has room again once the windows filling it end.
///
/// Its instants sit far past every other case's, so the windows those cases left in the same
/// partition have all ended by the time this one hits — which is also what lets it run against a
/// server that still holds the previous run.
pub async fn a_full_partition_refuses_only_a_new_key_in_that_partition(
    counters: &dyn CounterStore,
) {
    let link = |n: usize| CounterKey::ShareLink(format!("counter-conformance-ceiling-{n}"));
    let other = CounterKey::DropLink("counter-conformance-ceiling-other".to_owned());
    for n in 0..=CEILING + 1 {
        ok(counters.reset(&link(n)).await, "reset");
    }
    ok(counters.reset(&other).await, "reset");
    let start = at(1_000_000);

    for n in 0..CEILING {
        assert!(
            ok(counters.hit(&link(n), budget(), start).await, "hit").admits(),
            "key {n} fits under the ceiling"
        );
    }

    match counters.hit(&link(CEILING), budget(), start).await {
        Err(error) => assert!(
            super::is_at_capacity(&error),
            "a full partition refuses as at-capacity, not as a broken store: {error}"
        ),
        Ok(verdict) => panic!("a new key past the ceiling must be refused, got {verdict:?}"),
    }
    assert_eq!(
        ok(counters.hit(&link(0), budget(), start).await, "hit"),
        Verdict::Admitted { remaining: 1 },
        "a key already being counted keeps counting in a full partition"
    );
    assert!(
        ok(counters.hit(&other, budget(), start).await, "hit").admits(),
        "another variant's partition is not this one's to fill"
    );

    ok(counters.reset(&link(1)).await, "reset");
    assert!(
        ok(counters.hit(&link(CEILING), budget(), start).await, "hit").admits(),
        "a reset gives its place in the partition back"
    );

    let later = deadline(start, budget().window);
    assert!(
        ok(
            counters.hit(&link(CEILING + 1), budget(), later).await,
            "hit"
        )
        .admits(),
        "once the windows filling it end, the partition has room again"
    );

    for n in 0..=CEILING + 1 {
        ok(counters.reset(&link(n)).await, "reset");
    }
    ok(counters.reset(&other).await, "reset");
}

/// Run every case above against one store, in order — all but
/// [`a_full_partition_refuses_only_a_new_key_in_that_partition`], which needs a store built
/// with the [`CEILING`] override and so is driven on one of its own.
///
/// The entry point for a backend where standing up a store is expensive. A unit-tier adapter
/// should prefer one test per case, so a failure names the property that broke.
pub async fn run_all(counters: Arc<dyn CounterStore>) {
    a_window_admits_exactly_its_budget_and_then_refuses(&*counters).await;
    a_refused_hit_does_not_extend_the_window(&*counters).await;
    a_window_measures_from_its_first_hit(&*counters).await;
    counters_are_scoped_to_their_key(&*counters).await;
    a_reset_ends_the_run(&*counters).await;
    peeking_charges_nothing_and_answers_a_verdict(&*counters).await;
    no_sequence_of_calls_admits_more_than_the_budget(&*counters).await;
    concurrent_hits_admit_exactly_the_budget(counters).await;
}
