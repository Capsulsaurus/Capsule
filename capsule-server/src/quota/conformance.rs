//! The one suite every [`QuotaStore`] adapter must pass.
//!
//! # What is here and what stayed in `tests.rs`
//!
//! Most of the quota module is a **pure** state machine — [`state_of`](super::state_of) and
//! [`admits`](super::admits) — and those cases read as a table with nothing to mock. They stay
//! in `quota/tests.rs`, because a suite generic over a store cannot say anything about a
//! function that takes no store.
//!
//! What is here is everything the *ledger* owes: the dedup rule, the two releases, the
//! over-limit clock, and the suspension. Every one of those was written against `InMemoryQuota` only, which made
//! the double an unproven stand-in for exactly the adapter that has to get concurrency right.
//!
//! # The rule the suite exists to protect
//!
//! Attribution is keyed on the **content address, globally**, and a blob shared between two
//! uploaders counts against only the first. That is not a courtesy: without it a malicious user
//! could exhaust another account's quota by re-uploading blobs whose addresses they already
//! know. [`charging_the_same_address_twice_debits_once`] is that rule, and it is the case an
//! adapter that reached for "check, then debit" would fail.
//!
//! # Reusing a harness
//!
//! Every case scopes its own users and addresses, so cases may share one ledger and [`run_all`]
//! does.

use jiff::{SignedDuration, Timestamp};

use super::{ChargeOutcome, QuotaLimits, QuotaStore};
use crate::blob::ContentAddress;
use crate::store::{StoreError, UserId};

/// The ledger under test.
///
/// One accessor and no time seam: nothing in this port expires, and the one instant it stores —
/// the moment an account crossed its hard limit — is an **argument** to
/// [`QuotaStore::charge`] rather than something read from a clock. That is deliberate in the
/// port and it is why this harness is one method: an adapter cannot get the crossing's timestamp
/// wrong by reading the wrong clock, because it never reads one.
pub trait Harness: Send + Sync {
    /// The ledger under test.
    fn quotas(&self) -> &dyn QuotaStore;
}

/// Unwrap a ledger result, failing with the operation that was expected to work.
#[track_caller]
fn ok<T>(result: Result<T, StoreError>, doing: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("a conforming ledger must succeed at {doing}: {error}"),
    }
}

/// A deployment with real limits: 100 bytes soft, 200 hard, a 14-day grace.
fn limits() -> QuotaLimits {
    QuotaLimits::new(100, 200, super::DEFAULT_GRACE_WINDOW)
}

/// An instant `days` after the epoch.
fn day(days: i64) -> Timestamp {
    Timestamp::UNIX_EPOCH + SignedDuration::from_hours(days * 24)
}

/// `case`'s own account `n`.
fn user(case: &str, n: u8) -> UserId {
    UserId::new(format!("{case}-user-{n}"))
}

/// A deterministic content address for `case`'s blob `n`.
///
/// Hashed rather than formatted because a content address is 64 lowercase-hex characters and
/// `ContentAddress::parse` is the only gate that says so.
fn address(case: &str, n: u8) -> ContentAddress {
    let mut seed = case.as_bytes().to_vec();
    seed.push(n);
    ContentAddress::parse(&capsule_core::crypto::hash::hash_bytes(&seed).to_hex())
        .expect("a digest is a content address")
}

// ===========================================================================================
// Attribution
// ===========================================================================================

/// A blob shared between two uploaders is charged to the first only.
///
/// The rule that stops a malicious user exhausting another account's quota by re-uploading blobs
/// whose addresses they already know. The second uploader is a merge, not a second copy.
pub async fn charging_the_same_address_twice_debits_once(h: &dyn Harness) {
    let first = user("shared", 1);
    let second = user("shared", 2);
    let shared = address("shared", 1);

    assert_eq!(
        ok(
            h.quotas()
                .charge(&first, &shared, 64, day(0), limits())
                .await,
            "charge an address",
        ),
        ChargeOutcome::Charged { used: 64 },
    );
    assert_eq!(
        ok(
            h.quotas()
                .charge(&second, &shared, 64, day(0), limits())
                .await,
            "charge an address a second account already holds",
        ),
        ChargeOutcome::AlreadyAttributed,
    );
    assert_eq!(
        ok(h.quotas().usage(&second).await, "read usage").used,
        0,
        "the second uploader is a merge, not a second copy"
    );
    assert_eq!(
        ok(h.quotas().usage(&first).await, "read usage").used,
        64,
        "and the first is still charged exactly once"
    );
}

/// `AlreadyAttributed` is one value whoever holds the address.
///
/// Telling a caller "somebody else already holds these bytes" would answer, from a quota
/// endpoint, the cross-tenant question `AssetIndex::find_by_address` is owner-scoped to avoid.
/// Structural — the variant has no payload — and asserted here so a richer one cannot be added
/// without a case going red.
pub async fn the_already_attributed_answer_says_nothing_about_who_holds_it(h: &dyn Harness) {
    let mine = user("silent", 1);
    let stranger = user("silent", 2);
    let held = address("silent", 1);
    ok(
        h.quotas().charge(&mine, &held, 32, day(0), limits()).await,
        "charge an address",
    );

    let outcome = ok(
        h.quotas()
            .charge(&stranger, &held, 32, day(0), limits())
            .await,
        "charge an address another account holds",
    );
    let rendered = format!("{outcome:?}");
    assert!(
        !rendered.contains(mine.as_str()),
        "the refusal named the account that holds the address: {rendered}"
    );
}

/// Usage is per account, and an account that has been charged nothing owes nothing.
pub async fn an_uncharged_account_owes_nothing(h: &dyn Harness) {
    let stranger = user("fresh", 1);
    let usage = ok(h.quotas().usage(&stranger).await, "read usage");
    assert_eq!(usage.used, 0);
    assert_eq!(usage.over_since, None);
}

// ===========================================================================================
// Releases
// ===========================================================================================

/// Releasing a reservation credits the bytes back, once, and frees the address.
pub async fn releasing_a_reservation_credits_the_bytes_back(h: &dyn Harness) {
    let owner = user("release", 1);
    let held = address("release", 1);
    ok(
        h.quotas().charge(&owner, &held, 64, day(0), limits()).await,
        "charge an address",
    );

    assert!(ok(h.quotas().release(&owner, &held).await, "release"));
    assert_eq!(ok(h.quotas().usage(&owner).await, "read usage").used, 0);
    assert!(
        !ok(h.quotas().release(&owner, &held).await, "release again"),
        "a second release must not credit the bytes twice"
    );

    // And the address is free again, so a later uploader is charged for it.
    assert_eq!(
        ok(
            h.quotas().charge(&owner, &held, 64, day(0), limits()).await,
            "charge a released address",
        ),
        ChargeOutcome::Charged { used: 64 },
    );
}

/// Another account's reservation is not releasable.
///
/// Releasing somebody else's attribution would let one account free bytes off another's ledger —
/// and, worse, tell them the address was attributed.
pub async fn another_accounts_reservation_is_not_releasable(h: &dyn Harness) {
    let owner = user("steal", 1);
    let other = user("steal", 2);
    let held = address("steal", 1);
    ok(
        h.quotas().charge(&owner, &held, 64, day(0), limits()).await,
        "charge an address",
    );

    assert!(!ok(
        h.quotas().release(&other, &held).await,
        "release another account's attribution",
    ));
    assert_eq!(ok(h.quotas().usage(&owner).await, "read usage").used, 64);
}

/// The collector releases by address, and the ledger names the account.
///
/// A sweep knows an address and nothing else — attribution is global by content address, so the
/// blob it is deleting may be charged to an account with no remaining connection to the asset
/// whose purge exposed it. The collector cannot supply the user and must not guess one.
pub async fn the_collector_releases_by_address_and_the_ledger_names_the_account(h: &dyn Harness) {
    let owner = user("sweep", 1);
    let swept = address("sweep", 1);
    ok(
        h.quotas()
            .charge(
                &owner,
                &swept,
                1_024,
                Timestamp::UNIX_EPOCH,
                QuotaLimits::unlimited(),
            )
            .await,
        "charge an address",
    );

    let released = ok(
        h.quotas().release_attribution(&swept).await,
        "release by address",
    );
    assert_eq!(released, Some((owner.clone(), 1_024)));
    assert_eq!(ok(h.quotas().usage(&owner).await, "read usage").used, 0);
}

/// A contributor stays charged until the collector releases the bytes (issue #473).
///
/// The ledger's half of design/quota.md's "Departed album members" decision: removing a writer
/// member from an album's roster reclaims nothing, by design. The member's upload is charged to
/// them while the album owner holds it, and nothing the owner can say to the ledger moves it.
/// Charging the same address is a merge, and releasing it is another account's reservation. What
/// takes the bytes off the member's ledger is the collector, once the owner or a current writer
/// member has deleted the asset and the last reference is gone. An adapter that let the owner "adopt" the bytes, or let
/// the owner's release clear the member's charge, would let one account's quota pressure act on
/// another account's photos.
pub async fn a_contributors_attribution_survives_until_the_collector_releases_it(h: &dyn Harness) {
    let member = user("departed", 1);
    let owner = user("departed", 2);
    let contributed = address("departed", 1);
    ok(
        h.quotas()
            .charge(&member, &contributed, 96, day(0), limits())
            .await,
        "charge a member's upload",
    );

    assert_eq!(
        ok(
            h.quotas()
                .charge(&owner, &contributed, 96, day(1), limits())
                .await,
            "charge the owner for bytes a member already holds",
        ),
        ChargeOutcome::AlreadyAttributed,
        "the owner cannot adopt a member's bytes by charging them",
    );
    assert!(
        !ok(
            h.quotas().release(&owner, &contributed).await,
            "release a member's attribution as the owner",
        ),
        "the owner cannot free a member's bytes off the member's ledger",
    );
    assert_eq!(ok(h.quotas().usage(&owner).await, "read usage").used, 0);
    assert_eq!(
        ok(h.quotas().usage(&member).await, "read usage").used,
        96,
        "the member stays charged while the owner still holds the bytes",
    );

    assert_eq!(
        ok(
            h.quotas().release_attribution(&contributed).await,
            "release by address",
        ),
        Some((member.clone(), 96)),
        "the collector credits the member, not the account whose delete exposed the blob",
    );
    assert_eq!(ok(h.quotas().usage(&member).await, "read usage").used, 0);
}

/// Releasing an address the ledger never saw is `None` rather than an error.
///
/// The ordinary case for a blob the ledger never saw. A sweep that treated it as a failure would
/// stall on the first one.
pub async fn releasing_an_unattributed_address_is_none_rather_than_an_error(h: &dyn Harness) {
    assert_eq!(
        ok(
            h.quotas().release_attribution(&address("unseen", 1)).await,
            "release an unattributed address",
        ),
        None,
    );
}

// ===========================================================================================
// The over-limit clock
// ===========================================================================================

/// The crossing is stamped once and cleared by going under.
///
/// `over_since` is kept by the store rather than derived, because "how long have you been over"
/// cannot be computed from a current total. A later charge while still over must not restamp it,
/// or the grace window would never expire for an account that keeps trying to upload.
pub async fn the_crossing_is_stamped_once_and_cleared_by_going_under(h: &dyn Harness) {
    let owner = user("clock", 1);
    ok(
        h.quotas()
            .charge(&owner, &address("clock", 1), 150, day(0), limits())
            .await,
        "charge an address",
    );
    assert_eq!(
        ok(h.quotas().usage(&owner).await, "read usage").over_since,
        None,
        "150 is over the soft limit and under the hard one"
    );

    ok(
        h.quotas()
            .charge(&owner, &address("clock", 2), 100, day(1), limits())
            .await,
        "charge an address",
    );
    let over = ok(h.quotas().usage(&owner).await, "read usage");
    assert_eq!(over.used, 250);
    assert_eq!(over.over_since, Some(day(1)));

    ok(
        h.quotas()
            .charge(&owner, &address("clock", 3), 10, day(9), limits())
            .await,
        "charge an address while over",
    );
    assert_eq!(
        ok(h.quotas().usage(&owner).await, "read usage").over_since,
        Some(day(1)),
        "a later charge while still over must not restamp the clock"
    );

    // Going back under stops the clock, so a later crossing gets a fresh window rather than
    // inheriting an expired one.
    ok(
        h.quotas().release(&owner, &address("clock", 2)).await,
        "release",
    );
    assert_eq!(
        ok(h.quotas().usage(&owner).await, "read usage").over_since,
        None,
    );
}

/// A collector release clears the over-limit clock exactly like a user release.
///
/// Both releases credit the same way, which is why the in-memory adapter shares one helper: a
/// second copy would eventually forget `over_since`, leaving an account back under its limit
/// still carrying the clock that decides when a soft limit becomes a hard one.
pub async fn a_collector_release_clears_the_over_limit_clock(h: &dyn Harness) {
    let owner = user("sweepclock", 1);
    let held = address("sweepclock", 1);
    let tight = QuotaLimits::new(1_000, 1_500, SignedDuration::from_hours(24));
    ok(
        h.quotas()
            .charge(&owner, &held, 2_000, Timestamp::UNIX_EPOCH, tight)
            .await,
        "charge an address",
    );
    assert!(
        ok(h.quotas().usage(&owner).await, "read usage")
            .over_since
            .is_some()
    );

    ok(
        h.quotas().release_attribution(&held).await,
        "release by address",
    );
    assert_eq!(
        ok(h.quotas().usage(&owner).await, "read usage").over_since,
        None,
        "back under the limit, so a later crossing gets a fresh window"
    );
}

// ===========================================================================================
// Concurrency
// ===========================================================================================

/// Parallel charges for one account are serialised: none is admitted past the hard limit, and
/// the crossing is stamped once, by the charge that made it (issue #513).
///
/// Eight sessions for one account, each a distinct 50-byte blob, all in flight at once against
/// a 200-byte hard limit. A ledger that totals the account without serialising its charges lets
/// each one read a total that misses the others' uncommitted debits: several report the same
/// `used`, all of them under the limit, so the upload path admits every one and the account
/// ends at 400 bytes with no `over_since` at all. Serialised, the totals each charge reports are
/// exactly 50, 100, ... 400 — one per charge — and the clock holds the instant of the charge
/// that reported 200.
///
/// The futures are joined on one task, which is enough: each one holds its own connection, so
/// the store sees eight concurrent transactions whichever task polls them.
pub async fn parallel_charges_cannot_overshoot_the_hard_limit(h: &dyn Harness) {
    let owner = user("parallel", 1);
    let addresses: Vec<ContentAddress> = (0..8).map(|n| address("parallel", n)).collect();
    let charge = |n: u8| {
        h.quotas().charge(
            &owner,
            &addresses[usize::from(n)],
            50,
            day(i64::from(n)),
            limits(),
        )
    };
    let outcomes = tokio::join!(
        charge(0),
        charge(1),
        charge(2),
        charge(3),
        charge(4),
        charge(5),
        charge(6),
        charge(7),
    );
    let outcomes = [
        outcomes.0, outcomes.1, outcomes.2, outcomes.3, outcomes.4, outcomes.5, outcomes.6,
        outcomes.7,
    ];

    let mut totals: Vec<(u64, Timestamp)> = outcomes
        .into_iter()
        .zip(0_i64..)
        .map(
            |(outcome, n)| match ok(outcome, "charge an address in parallel") {
                ChargeOutcome::Charged { used } => (used, day(n)),
                ChargeOutcome::AlreadyAttributed => {
                    panic!("every parallel charge is a distinct address, so none is a merge")
                }
            },
        )
        .collect();
    totals.sort_unstable();
    assert_eq!(
        totals.iter().map(|(used, _)| *used).collect::<Vec<_>>(),
        [50, 100, 150, 200, 250, 300, 350, 400],
        "each charge must see every charge serialised before it"
    );

    let admitted: u64 = totals
        .iter()
        .map(|(used, _)| *used)
        .filter(|used| *used < limits().hard_limit)
        .count()
        .try_into()
        .expect("eight fits in a u64");
    assert!(
        admitted * 50 < limits().hard_limit,
        "the upload path admitted {admitted} charges, past the hard limit"
    );

    let crossing = totals
        .iter()
        .find(|(used, _)| *used >= limits().hard_limit)
        .map(|(_, at)| *at);
    let usage = ok(h.quotas().usage(&owner).await, "read usage");
    assert_eq!(usage.used, 400);
    assert_eq!(
        usage.over_since, crossing,
        "the clock is stamped once, by the charge that crossed the limit"
    );
}

// ===========================================================================================
// Suspension (#705)
// ===========================================================================================

/// A suspension is recorded once, keeps its first instant, and is lifted by unsuspending.
///
/// The account has never been charged: an operator's suspension does not wait for the first
/// upload, so the ledger has to hold one for an account it otherwise knows nothing about.
pub async fn a_suspension_is_recorded_once_and_lifted(h: &dyn Harness) {
    let account = user("suspend", 1);
    assert_eq!(
        ok(h.quotas().usage(&account).await, "read usage").suspended_since,
        None
    );

    assert!(
        ok(h.quotas().suspend(&account, day(1)).await, "suspend"),
        "the first suspension is a change"
    );
    assert!(
        !ok(h.quotas().suspend(&account, day(2)).await, "suspend again"),
        "suspending a suspended account changes nothing"
    );
    assert_eq!(
        ok(h.quotas().usage(&account).await, "read usage").suspended_since,
        Some(day(1)),
        "the record says when the suspension began, not when it was last repeated"
    );

    assert!(ok(h.quotas().unsuspend(&account).await, "unsuspend"));
    assert_eq!(
        ok(h.quotas().usage(&account).await, "read usage").suspended_since,
        None
    );
    assert!(
        !ok(h.quotas().unsuspend(&account).await, "unsuspend again"),
        "lifting a suspension that is not there changes nothing"
    );
    assert!(
        !ok(h.quotas().unsuspend(&user("suspend", 2)).await, "unsuspend"),
        "an account the ledger has never seen is not suspended"
    );
}

/// A suspension is independent of the usage: no charge or release sets or clears it, and it
/// belongs to one account only.
pub async fn a_suspension_is_independent_of_the_usage(h: &dyn Harness) {
    let account = user("suspend-usage", 1);
    let bystander = user("suspend-usage", 2);
    ok(
        h.quotas()
            .charge(&account, &address("suspend-usage", 1), 50, day(0), limits())
            .await,
        "charge an address",
    );
    ok(h.quotas().suspend(&account, day(1)).await, "suspend");

    // Crossing the hard limit stamps the clock and leaves the suspension alone.
    ok(
        h.quotas()
            .charge(
                &account,
                &address("suspend-usage", 2),
                300,
                day(2),
                limits(),
            )
            .await,
        "charge past the hard limit",
    );
    let over = ok(h.quotas().usage(&account).await, "read usage");
    assert_eq!(over.used, 350);
    assert_eq!(over.over_since, Some(day(2)));
    assert_eq!(over.suspended_since, Some(day(1)));

    // Going back under stops the clock and leaves the suspension alone.
    ok(
        h.quotas()
            .release(&account, &address("suspend-usage", 2))
            .await,
        "release",
    );
    let under = ok(h.quotas().usage(&account).await, "read usage");
    assert_eq!(under.over_since, None);
    assert_eq!(
        under.suspended_since,
        Some(day(1)),
        "only an operator lifts a suspension; freeing space does not"
    );

    // And so does the collector's release.
    ok(
        h.quotas()
            .release_attribution(&address("suspend-usage", 1))
            .await,
        "release by address",
    );
    assert_eq!(
        ok(h.quotas().usage(&account).await, "read usage").suspended_since,
        Some(day(1))
    );

    assert_eq!(
        ok(h.quotas().usage(&bystander).await, "read usage").suspended_since,
        None,
        "a suspension is one account's"
    );

    // Unsuspending leaves the usage alone.
    ok(
        h.quotas()
            .charge(&account, &address("suspend-usage", 3), 70, day(3), limits())
            .await,
        "charge an address",
    );
    ok(h.quotas().unsuspend(&account).await, "unsuspend");
    assert_eq!(ok(h.quotas().usage(&account).await, "read usage").used, 70);
}

// ===========================================================================================
// The whole suite
// ===========================================================================================

/// Run every case above against one harness, in order.
pub async fn run_all(h: &dyn Harness) {
    charging_the_same_address_twice_debits_once(h).await;
    the_already_attributed_answer_says_nothing_about_who_holds_it(h).await;
    an_uncharged_account_owes_nothing(h).await;

    releasing_a_reservation_credits_the_bytes_back(h).await;
    another_accounts_reservation_is_not_releasable(h).await;
    the_collector_releases_by_address_and_the_ledger_names_the_account(h).await;
    a_contributors_attribution_survives_until_the_collector_releases_it(h).await;
    releasing_an_unattributed_address_is_none_rather_than_an_error(h).await;

    the_crossing_is_stamped_once_and_cleared_by_going_under(h).await;
    a_collector_release_clears_the_over_limit_clock(h).await;

    parallel_charges_cannot_overshoot_the_hard_limit(h).await;

    a_suspension_is_recorded_once_and_lifted(h).await;
    a_suspension_is_independent_of_the_usage(h).await;
}

#[cfg(test)]
mod tests;
