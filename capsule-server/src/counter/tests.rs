//! The counter port's own suite.
//!
//! The property that matters is that charging and deciding are one operation. A single-threaded
//! suite cannot exhibit the race that makes read-then-write wrong — the same limit `S-C21`'s
//! conformance note records — so what is asserted here is the *observable consequence*: the
//! n-th hit inside a window is refused, and no sequence of calls admits more than the budget.

use super::{budgets, *};

fn key() -> CounterKey {
    CounterKey::LoginAttempts(UserId::new("01937b7c-0000-7000-8000-000000000001"))
}

fn other() -> CounterKey {
    CounterKey::LoginAttempts(UserId::new("01937b7c-0000-7000-8000-0000000000ff"))
}

fn budget() -> Budget {
    Budget::new(3, SignedDuration::from_mins(10))
}

fn at(mins: i64) -> Timestamp {
    crate::store::deadline(Timestamp::UNIX_EPOCH, SignedDuration::from_mins(mins))
}

#[tokio::test]
async fn a_window_admits_exactly_its_budget_and_then_refuses() {
    let counters = InMemoryCounters::new();

    for expected_remaining in [2, 1, 0] {
        assert_eq!(
            counters
                .hit(&key(), budget(), at(0))
                .await
                .expect("the store answers"),
            Verdict::Admitted {
                remaining: expected_remaining
            }
        );
    }

    assert_eq!(
        counters
            .hit(&key(), budget(), at(0))
            .await
            .expect("the store answers"),
        Verdict::Limited {
            retry_after: at(10)
        },
        "the fourth hit is refused, and is told when to come back"
    );
}

#[tokio::test]
async fn a_refused_hit_does_not_extend_the_window() {
    // Otherwise an attacker who keeps hitting a limited key holds it limited forever, which
    // turns a rate limit into a denial of service against the legitimate user.
    let counters = InMemoryCounters::new();
    for _ in 0..3 {
        counters.hit(&key(), budget(), at(0)).await.expect("hits");
    }
    for minute in 1..8 {
        assert!(
            !counters
                .hit(&key(), budget(), at(minute))
                .await
                .expect("answers")
                .admits()
        );
    }

    assert!(
        counters
            .hit(&key(), budget(), at(11))
            .await
            .expect("answers")
            .admits(),
        "the window still ends ten minutes after it opened, not ten after the last attempt"
    );
}

#[tokio::test]
async fn a_window_measures_from_its_first_hit() {
    let counters = InMemoryCounters::new();
    counters.hit(&key(), budget(), at(0)).await.expect("hits");
    counters.hit(&key(), budget(), at(9)).await.expect("hits");

    assert!(
        counters
            .hit(&key(), budget(), at(11))
            .await
            .expect("answers")
            .admits(),
        "a fresh window opens once the first one passes"
    );
    // And that fresh window is a fresh budget, not a continuation.
    for _ in 0..2 {
        assert!(
            counters
                .hit(&key(), budget(), at(11))
                .await
                .expect("answers")
                .admits()
        );
    }
    assert!(
        !counters
            .hit(&key(), budget(), at(11))
            .await
            .expect("answers")
            .admits()
    );
}

#[tokio::test]
async fn counters_are_scoped_to_their_key() {
    let counters = InMemoryCounters::new();
    for _ in 0..3 {
        counters.hit(&key(), budget(), at(0)).await.expect("hits");
    }

    assert!(
        counters
            .hit(&other(), budget(), at(0))
            .await
            .expect("answers")
            .admits(),
        "one account's failed sign-ins are not another's"
    );
}

#[tokio::test]
async fn a_reset_ends_the_run() {
    // What a *successful* sign-in does to a failed-attempt counter: the policy counts
    // consecutive failures, so a success is not one more event, it ends the run.
    let counters = InMemoryCounters::new();
    for _ in 0..3 {
        counters.hit(&key(), budget(), at(0)).await.expect("hits");
    }
    assert!(
        !counters
            .hit(&key(), budget(), at(0))
            .await
            .expect("answers")
            .admits()
    );

    counters.reset(&key()).await.expect("resets");
    assert_eq!(
        counters
            .hit(&key(), budget(), at(0))
            .await
            .expect("answers"),
        Verdict::Admitted { remaining: 2 }
    );
}

#[tokio::test]
async fn peeking_charges_nothing_and_answers_a_verdict() {
    // It returns a `Verdict` rather than a count on purpose: handing back a number is handing
    // back the read half of a read-then-write, and somebody would eventually build a limiter
    // out of it.
    let counters = InMemoryCounters::new();
    for _ in 0..5 {
        assert!(
            counters
                .peek(&key(), budget(), at(0))
                .await
                .expect("answers")
                .admits(),
            "peeking does not spend the budget"
        );
    }

    for _ in 0..3 {
        counters.hit(&key(), budget(), at(0)).await.expect("hits");
    }
    assert!(
        !counters
            .peek(&key(), budget(), at(0))
            .await
            .expect("answers")
            .admits()
    );
}

#[tokio::test]
async fn no_sequence_of_calls_admits_more_than_the_budget() {
    // The observable consequence of charging and deciding together. A single-threaded suite
    // cannot exhibit the race that makes read-then-write wrong, so what is asserted is that the
    // count admitted never exceeds the limit however the calls are interleaved with peeks.
    let counters = InMemoryCounters::new();
    let mut admitted = 0;
    for step in 0..50 {
        counters.peek(&key(), budget(), at(0)).await.expect("peeks");
        if counters
            .hit(&key(), budget(), at(0))
            .await
            .expect("answers")
            .admits()
        {
            admitted += 1;
        }
        let _ = step;
    }
    assert_eq!(admitted, 3);
}

#[tokio::test]
async fn a_lapsed_window_is_dropped_rather_than_kept_as_a_row_nobody_reads() {
    // The map is process-wide and shared by every limiter, and several keys are derived from
    // something a caller sent. A window that decides nothing must not still occupy a row.
    let counters = InMemoryCounters::new();
    for index in 0..50 {
        let key = CounterKey::ShareLink(format!("link-{index}"));
        assert!(
            counters
                .hit(&key, budget(), at(0))
                .await
                .expect("answers")
                .admits()
        );
    }
    assert_eq!(counters.len(), 50);

    // One hit after every window has lapsed, and the fifty are collected with it.
    let key = CounterKey::ShareLink("link-fresh".to_owned());
    assert!(
        counters
            .hit(&key, budget(), at(11))
            .await
            .expect("answers")
            .admits()
    );
    assert_eq!(counters.len(), 1, "only the live window survives");
}

#[tokio::test]
async fn a_full_store_refuses_a_new_key_and_keeps_counting_the_ones_it_holds() {
    let counters = InMemoryCounters::new().with_ceiling(2);
    let first = CounterKey::ShareLink("a".to_owned());
    let second = CounterKey::ShareLink("b".to_owned());

    for key in [&first, &second] {
        assert!(
            counters
                .hit(key, budget(), at(0))
                .await
                .expect("answers")
                .admits()
        );
    }

    // A third key finds no room. An error and not a verdict: every caller treats a counter
    // failure as a refusal, so this fails closed.
    let refusal = counters
        .hit(&CounterKey::ShareLink("c".to_owned()), budget(), at(0))
        .await
        .expect_err("the ceiling refuses");
    assert!(
        matches!(refusal, crate::store::StoreError::Rejected { store, .. } if store == "counters"),
        "{refusal:?}"
    );
    assert_eq!(counters.len(), 2, "the refused key wrote nothing");

    // A key the store already holds keeps counting: a flood of new keys must not switch off a
    // limiter that is already tracking somebody.
    assert!(
        counters
            .hit(&first, budget(), at(0))
            .await
            .expect("answers")
            .admits()
    );
    // Right up to its own budget, which is still the thing that limits it.
    assert!(
        counters
            .hit(&first, budget(), at(0))
            .await
            .expect("answers")
            .admits()
    );
    assert!(
        !counters
            .hit(&first, budget(), at(0))
            .await
            .expect("answers")
            .admits(),
        "the budget still ends the run"
    );

    // And the ceiling is not a one-way door: once the windows lapse, a new key fits again.
    assert!(
        counters
            .hit(&CounterKey::ShareLink("c".to_owned()), budget(), at(11))
            .await
            .expect("answers")
            .admits()
    );
}

#[tokio::test]
async fn purging_does_not_change_a_verdict() {
    // The purge hint is recorded from the budget in force when a window opened; admission is
    // still recomputed from `opened_at` against the budget the caller supplies. A budget that
    // was re-tuned between two hits must decide by the new one.
    let counters = InMemoryCounters::new();
    let wide = Budget::new(3, SignedDuration::from_mins(60));
    assert!(
        counters
            .hit(&key(), wide, at(0))
            .await
            .expect("answers")
            .admits()
    );
    // Re-tuned to ten minutes: at minute eleven the window has lapsed under the new budget, so
    // the hit opens a fresh one with the full allowance, exactly as before this field existed.
    let narrow = Budget::new(3, SignedDuration::from_mins(10));
    assert_eq!(
        counters.hit(&key(), narrow, at(11)).await.expect("answers"),
        Verdict::Admitted { remaining: 2 }
    );
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
