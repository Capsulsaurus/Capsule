//! The in-memory adapter against the counter port's own suite.
//!
//! One test per [`conformance`] case, on a fresh store each, so a failure names the property
//! that broke; and the whole suite in one pass, because that is the entry point the
//! container-backed adapter uses and it must be exercised where a failure is easiest to read.

use std::sync::Arc;

use super::{CounterStore, InMemoryCounters, budgets, conformance};

fn counters() -> Arc<dyn CounterStore> {
    Arc::new(InMemoryCounters::new())
}

/// Declares one `#[tokio::test]` per conformance case taking a borrowed store.
macro_rules! conformance_cases {
    ($($case:ident),+ $(,)?) => {
        $(
            #[tokio::test]
            async fn $case() {
                conformance::$case(&*counters()).await;
            }
        )+
    };
}

conformance_cases! {
    a_window_admits_exactly_its_budget_and_then_refuses,
    a_refused_hit_does_not_extend_the_window,
    a_window_measures_from_its_first_hit,
    counters_are_scoped_to_their_key,
    a_reset_ends_the_run,
    peeking_charges_nothing_and_answers_a_verdict,
    no_sequence_of_calls_admits_more_than_the_budget,
}

/// The one case that races, on the runtime that lets it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_hits_admit_exactly_the_budget() {
    conformance::concurrent_hits_admit_exactly_the_budget(counters()).await;
}

/// The whole suite, in one pass on one store.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_whole_suite_passes_in_one_pass() {
    conformance::run_all(counters()).await;
}

#[test]
fn every_budget_is_declared_in_one_place() {
    // A budget written inline at its call site is a budget nobody can review against the threat
    // model, and the two pairs the contracts call "the same two limiters" would drift the first
    // time one was tuned.
    assert_eq!(budgets::LOGIN_ATTEMPTS.limit, 5);
    assert_eq!(budgets::ENROLLMENT_REDEMPTION.window.as_mins(), 10);
    assert!(budgets::SHARE_SOURCE.limit > budgets::SHARE_LINK.limit);
    assert!(budgets::DROP_SOURCE.limit > budgets::DROP_LINK.limit);
    assert_eq!(budgets::DEEP_VERIFY.window.as_hours(), 1);
}
