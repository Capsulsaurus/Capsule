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

/// The finding decision 22 answers: one shared ceiling makes four unauthenticated surfaces share
/// a fate.
///
/// The drop path keys on a caller-supplied id over an hour-long window, which makes it the
/// cheapest partition to hold saturated. Under one global ceiling, saturating it would refuse a
/// *first* key to every other surface — a first-time share view, a first enrollment redemption,
/// the first OIDC sign-in after a reboot — and every one of those maps a counter error to a
/// fail-closed 500/503. Partitioned, the flood costs the flooded surface its new keys and costs
/// the others nothing.
#[tokio::test]
async fn flooding_one_surface_does_not_deny_a_fresh_key_to_another() {
    // The override bounds every partition equally, so the partition *boundary* is what this
    // test observes rather than the size of any one of them. The real numbers are asserted in
    // `every_ceiling_is_sized_from_its_own_window`.
    let counters = InMemoryCounters::new().with_ceiling(50);

    // Fill the drop path to its ceiling with fabricated but well-formed ids.
    for index in 0..50 {
        let key = CounterKey::DropLink(format!("{index:032x}"));
        assert!(
            counters
                .hit(&key, budgets::DROP_LINK, at(0))
                .await
                .expect("answers")
                .admits()
        );
    }
    assert_eq!(counters.len_of(&CounterKey::DropLink(String::new())), 50);

    // Saturated: a fifty-first drop id is refused, which is the bound doing its job.
    counters
        .hit(
            &CounterKey::DropLink("ffffffffffffffffffffffffffffffff".to_owned()),
            budgets::DROP_LINK,
            at(0),
        )
        .await
        .expect_err("the drop partition is full");

    // And every other surface is untouched. A never-seen key on each of the three that a shared
    // ceiling would have denied:
    for (key, budget) in [
        (
            CounterKey::ShareLink("never-seen-share".to_owned()),
            budgets::SHARE_LINK,
        ),
        (
            CounterKey::OidcAuthorize("app.example.test".to_owned()),
            budgets::OIDC_AUTHORIZE,
        ),
        (
            CounterKey::EnrollmentRedemption("00000000".to_owned()),
            budgets::ENROLLMENT_REDEMPTION,
        ),
        (
            CounterKey::SecondFactor("challenge-1".to_owned()),
            budgets::SECOND_FACTOR,
        ),
    ] {
        assert!(
            counters
                .hit(&key, budget, at(0))
                .await
                .expect("a full drop partition is not another surface's problem")
                .admits(),
            "{} was denied by a flood against drop_link",
            key.as_str()
        );
    }

    // The flooded surface's own existing keys keep counting, up to their own budget.
    let held = CounterKey::DropLink(format!("{0:032x}", 0));
    assert!(
        counters
            .hit(&held, budgets::DROP_LINK, at(0))
            .await
            .expect("answers")
            .admits(),
        "a key already being counted keeps being counted"
    );
    assert_eq!(
        counters.len_of(&CounterKey::DropLink(String::new())),
        50,
        "and counting it minted nothing"
    );
}

#[tokio::test]
async fn a_partition_is_dropped_when_its_last_window_lapses() {
    let counters = InMemoryCounters::new();
    let key = CounterKey::ShareLink("a".to_owned());
    counters.hit(&key, budget(), at(0)).await.expect("answers");
    assert_eq!(counters.len_of(&key), 1);

    // A hit on a *different* partition purges the lapsed one rather than leaving it behind.
    counters
        .hit(&CounterKey::DropLink("b".to_owned()), budget(), at(11))
        .await
        .expect("answers");
    assert_eq!(counters.len_of(&key), 0, "the emptied partition is gone");
    assert_eq!(counters.len(), 1);
}

#[test]
fn every_ceiling_is_sized_from_its_own_window() {
    use crate::counter::ceilings;

    // The two keys only one value of which can exist hold exactly one window.
    assert_eq!(CounterKey::OidcAuthorizeRefused.ceiling(), 1);
    assert_eq!(CounterKey::EnrollmentRedemptionMalformed.ceiling(), 1);

    // The admitted OIDC host is structurally three values; the ceiling is headroom over that
    // and nothing like the caller-controlled partitions.
    assert_eq!(CounterKey::OidcAuthorize(String::new()).ceiling(), 16);
    assert!(
        CounterKey::OidcAuthorize(String::new()).ceiling()
            < CounterKey::ShareLink(String::new()).ceiling() / 100,
        "a bounded key must not be sized like an unbounded one"
    );

    // The three surfaces that key on a caller-supplied string are the ones that need room.
    for key in [
        CounterKey::ShareLink(String::new()),
        CounterKey::DropLink(String::new()),
        CounterKey::EnrollmentRedemption(String::new()),
    ] {
        assert!(key.ceiling() >= ceilings::ENROLLMENT_REDEMPTION, "{key:?}");
    }

    // No two variants share a partition name, or one flood would reach two ceilings.
    let names = [
        CounterKey::LoginAttempts(UserId::new("u")),
        CounterKey::EnrollmentRedemption(String::new()),
        CounterKey::EnrollmentRedemptionMalformed,
        CounterKey::ShareLink(String::new()),
        CounterKey::ShareSource(String::new()),
        CounterKey::DropLink(String::new()),
        CounterKey::DropSource(String::new()),
        CounterKey::DeepVerify(UserId::new("u")),
        CounterKey::SecondFactor(String::new()),
        CounterKey::RegistrationSource(String::new()),
        CounterKey::OidcAuthorize(String::new()),
        CounterKey::OidcAuthorizeRefused,
    ]
    .map(|key| key.as_str());
    let unique: std::collections::BTreeSet<&str> = names.iter().copied().collect();
    assert_eq!(unique.len(), names.len(), "{names:?}");
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
