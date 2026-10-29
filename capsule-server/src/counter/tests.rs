//! The in-memory adapter against the counter port's own suite.
//!
//! One test per [`conformance`] case, on a fresh store each, so a failure names the property
//! that broke; and the whole suite in one pass, because that is the entry point the
//! container-backed adapter uses and it must be exercised where a failure is easiest to read.

use std::sync::Arc;

use super::{CounterStore, InMemoryCounters, budgets, ceilings, conformance, *};

fn counters() -> Arc<dyn CounterStore> {
    Arc::new(InMemoryCounters::new())
}

// The cases below the conformance block drive the adapter directly rather than through a
// conformance case, so they need a budget and a clock of their own. `conformance` has its own
// private pair; these are deliberately not shared, because a conformance helper that a
// non-conformance test also depends on is a helper nobody can change for the suite's sake.

fn key() -> CounterKey {
    CounterKey::LoginAttempts(UserId::new("01937b7c-0000-7000-8000-000000000001"))
}

fn budget() -> Budget {
    Budget::new(3, SignedDuration::from_mins(10))
}

fn at(mins: i64) -> Timestamp {
    crate::store::deadline(Timestamp::UNIX_EPOCH, SignedDuration::from_mins(mins))
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

/// The ceiling case, on a store whose every partition is bounded at the suite's own number.
#[tokio::test]
async fn a_full_partition_refuses_only_a_new_key_in_that_partition() {
    let counters = InMemoryCounters::new().with_ceiling(conformance::CEILING);
    conformance::a_full_partition_refuses_only_a_new_key_in_that_partition(&counters).await;
}

/// The whole suite, in one pass on one store.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_whole_suite_passes_in_one_pass() {
    conformance::run_all(counters()).await;
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
            CounterKey::EnrollmentRedemption(EnrollmentSpelling::Fallback),
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

/// The same property against the **real** `DropLink` ceiling rather than a test override.
///
/// `flooding_one_surface_does_not_deny_a_fresh_key_to_another` bounds every partition equally so
/// the boundary is legible; this one spends the twenty thousand the shipped constant actually
/// allows, so that a future edit which re-shares the ceilings — or sizes `DropLink` off some
/// other variant's number — is caught by the number a deployment really runs with.
#[tokio::test]
async fn the_real_drop_ceiling_is_the_drop_partition_and_nobody_else_s() {
    let counters = InMemoryCounters::new();

    for index in 0..ceilings::DROP_LINK {
        counters
            .hit(
                &CounterKey::DropLink(format!("{index:032x}")),
                budgets::DROP_LINK,
                at(0),
            )
            .await
            .expect("answers");
    }
    assert_eq!(
        counters.len_of(&CounterKey::DropLink(String::new())),
        ceilings::DROP_LINK
    );
    counters
        .hit(
            &CounterKey::DropLink("ffffffffffffffffffffffffffffffff".to_owned()),
            budgets::DROP_LINK,
            at(0),
        )
        .await
        .expect_err("the drop partition is at its shipped ceiling");

    // The two the finding named: a first-time share view and the first OIDC sign-in.
    assert!(
        counters
            .hit(
                &CounterKey::ShareLink("never-seen-share".to_owned()),
                budgets::SHARE_LINK,
                at(0),
            )
            .await
            .expect("a saturated drop partition denies nobody else")
            .admits()
    );
    assert!(
        counters
            .hit(
                &CounterKey::OidcAuthorize("app.example.test".to_owned()),
                budgets::OIDC_AUTHORIZE,
                at(0),
            )
            .await
            .expect("a saturated drop partition denies nobody else")
            .admits()
    );

    // And the flooded surface keeps counting the keys it already holds.
    assert!(
        counters
            .hit(
                &CounterKey::DropLink(format!("{0:032x}", 0)),
                budgets::DROP_LINK,
                at(0),
            )
            .await
            .expect("answers")
            .admits()
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
    // The two keys only one value of which can exist hold exactly one window.
    assert_eq!(CounterKey::OidcAuthorizeRefused.ceiling(), 1);
    assert_eq!(CounterKey::OidcDevice.ceiling(), 1);
    assert_eq!(CounterKey::EnrollmentRedemptionMalformed.ceiling(), 1);
    // The redemption key is one of two spellings (issue #528), so it holds exactly two.
    assert_eq!(
        CounterKey::EnrollmentRedemption(EnrollmentSpelling::Full).ceiling(),
        2
    );

    // The admitted OIDC host is structurally three values; the ceiling is headroom over that
    // and nothing like the caller-controlled partitions.
    assert_eq!(CounterKey::OidcAuthorize(String::new()).ceiling(), 16);
    assert!(
        CounterKey::OidcAuthorize(String::new()).ceiling()
            < CounterKey::ShareLink(String::new()).ceiling() / 100,
        "a bounded key must not be sized like an unbounded one"
    );

    // The two surfaces that key on a caller-supplied string are the ones that need room.
    for key in [
        CounterKey::ShareLink(String::new()),
        CounterKey::DropLink(String::new()),
    ] {
        assert!(key.ceiling() >= 5_000, "{key:?}");
    }

    // No two variants share a partition name, or one flood would reach two ceilings.
    let names = [
        CounterKey::LoginAttempts(UserId::new("u")),
        CounterKey::EnrollmentRedemption(EnrollmentSpelling::Full),
        CounterKey::EnrollmentRedemptionMalformed,
        CounterKey::ShareLink(String::new()),
        CounterKey::ShareSource(String::new()),
        CounterKey::DropLink(String::new()),
        CounterKey::DropSource(String::new()),
        CounterKey::DeepVerify(UserId::new("u")),
        CounterKey::UploadSessions(UserId::new("u")),
        CounterKey::UploadChunks(UserId::new("u")),
        CounterKey::UploadSessionsSource(String::new()),
        CounterKey::UploadChunksSource(String::new()),
        CounterKey::SecondFactor(String::new()),
        CounterKey::RegistrationSource(String::new()),
        CounterKey::OidcAuthorize(String::new()),
        CounterKey::OidcAuthorizeRefused,
        CounterKey::OidcDevice,
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
    assert_eq!(budgets::ENROLLMENT_REDEMPTION.window.as_mins(), 1);
    const { assert!(budgets::SHARE_SOURCE.limit > budgets::SHARE_LINK.limit) };
    const { assert!(budgets::DROP_SOURCE.limit > budgets::DROP_LINK.limit) };
    // One account must not reach its address's upload budget on its own: the route charges the
    // account first, so the address keeps room for the other accounts behind it.
    const { assert!(budgets::UPLOAD_SESSIONS_SOURCE.limit > budgets::UPLOAD_SESSIONS.limit) };
    const { assert!(budgets::UPLOAD_CHUNKS_SOURCE.limit > budgets::UPLOAD_CHUNKS.limit) };
    assert_eq!(budgets::DEEP_VERIFY.window.as_hours(), 1);
}

/// An account may spend its whole upload budget on distinct source keys, one fresh `/64` a
/// request, so each upload source partition must hold [`ceilings::HOSTILE_UPLOADERS`] accounts
/// doing that at once **and still** hold the legitimate address population every other source
/// partition is sized for. Sized against that population alone, the chunk partition filled at
/// four hostile accounts and refused every first-time uploader after them.
#[test]
fn the_upload_source_ceilings_hold_their_stated_hostile_accounts() {
    let accounts = ceilings::HOSTILE_UPLOADERS;
    let legitimate = ceilings::SOURCE_ADDRESS;
    let sessions = budgets::UPLOAD_SESSIONS.limit as usize;
    let chunks = budgets::UPLOAD_CHUNKS.limit as usize;
    assert!(
        CounterKey::UploadSessionsSource(String::new()).ceiling()
            >= sessions * accounts + legitimate
    );
    assert!(
        CounterKey::UploadChunksSource(String::new()).ceiling() >= chunks * accounts + legitimate
    );
}

/// The classifier must say "capacity" for the ceiling and **only** for the ceiling.
///
/// [`is_at_capacity`] decides capacity-versus-outage by matching
/// `StoreError::Rejected { store: COUNTER_STORE, .. }`. Today that is safe by construction:
/// `InMemoryCounters::hit`'s only error path *is* the ceiling, so no route can misclassify. It
/// stops being safe by construction the moment a second `CounterStore` exists — `COUNTER_STORE`
/// is `pub`, and a Valkey adapter (#460) that refuses under that same store name for an
/// unrelated reason would have a genuine outage rendered `429 error.*_at_capacity`. That tells a
/// caller to retry a store that is down and tells an operator nothing is wrong, which is a worse
/// failure than the `500`-for-everything this round replaced.
///
/// So the negative cases are pinned now, against the adapter that does not exist yet.
#[test]
fn only_the_counter_store_s_own_ceiling_reads_as_capacity() {
    use crate::store::StoreError;

    let ceiling = StoreError::Rejected {
        store: COUNTER_STORE,
        detail: "2 open windows is the ceiling for `share_link`".to_owned(),
    };
    assert!(is_at_capacity(&ceiling), "the ceiling is the capacity case");

    for outage in [
        // A store that could not answer at all — the `500` this must stay.
        StoreError::Unavailable {
            store: COUNTER_STORE,
            detail: "the connection was refused".to_owned(),
        },
        // A refusal from some *other* store that happens to travel the same channel.
        StoreError::Rejected {
            store: "something-else",
            detail: "a refusal that is not this port's ceiling".to_owned(),
        },
        StoreError::Unavailable {
            store: "something-else",
            detail: "an outage that is not this port's at all".to_owned(),
        },
    ] {
        assert!(
            !is_at_capacity(&outage),
            "only the counter store's own ceiling is a capacity refusal: {outage:?}"
        );
    }
}

/// And the method the routes actually call agrees with the predicate, including the deadline.
#[tokio::test]
async fn capacity_refusal_offers_a_deadline_for_the_ceiling_and_none_for_an_outage() {
    use std::sync::Arc;

    use crate::store::StoreError;
    use crate::store::memory::ManualClock;

    let clock = Arc::new(ManualClock::new(at(0)));
    let counters = CounterContext::new(Arc::new(InMemoryCounters::new()), clock.clone());

    // The ceiling: a deadline one window out, on the same clock the windows themselves use.
    let ceiling = StoreError::Rejected {
        store: COUNTER_STORE,
        detail: "full".to_owned(),
    };
    assert_eq!(
        counters.capacity_refusal(&ceiling, budget()),
        Some(at(10)),
        "one window from now, which is the bound on when a live window lapses"
    );

    // Everything else: no deadline, so the route renders its `500` rather than a `429` telling
    // a caller to retry a store that is down.
    for outage in [
        StoreError::Unavailable {
            store: COUNTER_STORE,
            detail: "the connection was refused".to_owned(),
        },
        StoreError::Rejected {
            store: "something-else",
            detail: "not this port's ceiling".to_owned(),
        },
    ] {
        assert_eq!(
            counters.capacity_refusal(&outage, budget()),
            None,
            "{outage:?}"
        );
    }
}

/// One key of every [`CounterKey`] variant, for the partition-mapping tests.
fn one_key_per_variant() -> Vec<CounterKey> {
    vec![
        CounterKey::LoginAttempts(UserId::new("u")),
        CounterKey::EnrollmentRedemption(EnrollmentSpelling::Full),
        CounterKey::EnrollmentRedemptionMalformed,
        CounterKey::ShareLink(String::new()),
        CounterKey::ShareSource(String::new()),
        CounterKey::DropLink(String::new()),
        CounterKey::DropSource(String::new()),
        CounterKey::DeepVerify(UserId::new("u")),
        CounterKey::UploadSessions(UserId::new("u")),
        CounterKey::UploadChunks(UserId::new("u")),
        CounterKey::UploadSessionsSource(String::new()),
        CounterKey::UploadChunksSource(String::new()),
        CounterKey::SecondFactor(String::new()),
        CounterKey::RegistrationSource(String::new()),
        CounterKey::OidcAuthorize(String::new()),
        CounterKey::OidcAuthorizeRefused,
        CounterKey::OidcDevice,
        CounterKey::PeerRequests(String::new()),
        CounterKey::FederatedReports(String::new()),
        CounterKey::PeerReports(String::new()),
        CounterKey::FederatedIntake(String::new()),
        CounterKey::FederatedReportNonce(String::new()),
        CounterKey::FederatedRequestNonce(String::new()),
        CounterKey::CapabilityMints(UserId::new("u")),
    ]
}

/// Drive one store future to completion on a runtime of its own, off any test runtime.
fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a runtime")
        .block_on(future)
}

#[test]
fn every_variant_has_a_partition_of_its_own() {
    let keys = one_key_per_variant();
    // No index can reach past the array: `PARTITION_COUNT` is counted from the `Slot`
    // declaration whose discriminants the indices are, so this holds for any variant, listed
    // here or not.
    let indices: std::collections::BTreeSet<usize> =
        keys.iter().map(CounterKey::partition_index).collect();
    assert!(
        indices.iter().all(|index| *index < PARTITION_COUNT),
        "{indices:?}"
    );

    // One slot per variant, no more and no fewer: a spare slot is a variant this list forgot,
    // and a shared one is two variants sharing a fate.
    assert_eq!(keys.len(), PARTITION_COUNT);
    assert_eq!(indices.len(), keys.len(), "two variants share a partition");

    // One name per variant too, so the partition a log line names is the partition it is.
    let names: std::collections::BTreeSet<&str> = keys.iter().map(CounterKey::as_str).collect();
    assert_eq!(names.len(), keys.len());
}

#[test]
fn a_held_partition_does_not_block_a_hit_on_another() {
    let counters = Arc::new(InMemoryCounters::new());
    let flooded = CounterKey::DropLink("held".to_owned());
    // A window that has lapsed by the time the other partition is hit, so the purge would
    // collect it if it could.
    block_on(counters.hit(&flooded, budget(), at(0))).expect("answers");

    let held = lock(&counters.partition(&flooded).windows);

    // Hit another variant from a second thread while the drop partition is held. Under one lock
    // for every partition this would block until `held` drops, which is never inside the bound.
    let (sent, received) = std::sync::mpsc::channel();
    let other = Arc::clone(&counters);
    let worker = std::thread::spawn(move || {
        let verdict =
            block_on(other.hit(&CounterKey::ShareLink("free".to_owned()), budget(), at(11)));
        sent.send(verdict).expect("the test is listening");
    });
    let verdict = received
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("a hit on another partition must not wait on a held one")
        .expect("answers");
    assert!(verdict.admits());

    // The held partition was skipped by the purge rather than waited on.
    assert_eq!(held.len(), 1, "a held partition is skipped, not purged");
    drop(held);
    worker.join().expect("the worker finished");

    // And the next write once it is released collects it.
    block_on(counters.hit(&CounterKey::ShareLink("next".to_owned()), budget(), at(11)))
        .expect("answers");
    assert_eq!(counters.len_of(&flooded), 0);
}

#[test]
fn a_flood_next_door_takes_no_lock_until_something_lapses() {
    let counters = Arc::new(InMemoryCounters::new());
    let quiet = CounterKey::ShareLink("quiet".to_owned());
    let flood = CounterKey::DropLink("flood".to_owned());
    block_on(counters.hit(&quiet, budget(), at(0))).expect("answers");

    // While nothing in the share partition has lapsed, a drop flood passes it over on the
    // lock-free hint: its lock is not taken however many drop hits arrive.
    for _ in 0..100 {
        block_on(counters.hit(&flood, budget(), at(5))).expect("answers");
    }
    assert_eq!(counters.purge_others(flood.partition_index(), at(5)), 0);

    // Held by a share caller, it is still not waited on once a window there lapses: the drop
    // hit answers within the bound and leaves the held partition to its holder.
    let held = lock(&counters.partition(&quiet).windows);
    let (sent, received) = std::sync::mpsc::channel();
    let other = Arc::clone(&counters);
    let worker = std::thread::spawn(move || {
        let verdict =
            block_on(other.hit(&CounterKey::DropLink("late".to_owned()), budget(), at(11)));
        sent.send(verdict).expect("the test is listening");
    });
    received
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("a drop hit must not wait on a held share partition")
        .expect("answers");
    assert_eq!(held.len(), 1, "a held partition is skipped, not purged");
    drop(held);
    worker.join().expect("the worker finished");

    // Released and lapsed, it is swept exactly once: the sweep re-derives the hint, so the
    // next drop hit passes it over again.
    assert_eq!(counters.purge_others(flood.partition_index(), at(11)), 1);
    assert_eq!(counters.len_of(&quiet), 0);
    assert_eq!(counters.purge_others(flood.partition_index(), at(11)), 0);
}
