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
