//! The one suite every [`QuotaStore`] adapter must pass.
//!
//! # What is here and what stayed in `tests.rs`
//!
//! Most of the quota module is a **pure** state machine — [`state_of`](super::state_of) and
//! [`admits`](super::admits) — and those cases read as a table with nothing to mock. They stay
//! in `quota/tests.rs`, because a suite generic over a store cannot say anything about a
//! function that takes no store.
//!
//! What is here is everything the *ledger* owes: the dedup rule, the two releases, and the
//! over-limit clock. Every one of those was written against `InMemoryQuota` only, which made
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

/// The collector releases by address, and the ledger names the account (`S-C44`).
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
    releasing_an_unattributed_address_is_none_rather_than_an_error(h).await;

    the_crossing_is_stamped_once_and_cleared_by_going_under(h).await;
    a_collector_release_clears_the_over_limit_clock(h).await;
}

#[cfg(test)]
mod tests {
    use super::{Harness, run_all};
    use crate::quota::{InMemoryQuota, QuotaStore};

    /// The deterministic ledger.
    #[derive(Debug, Default)]
    struct MemoryHarness {
        quotas: InMemoryQuota,
    }

    impl Harness for MemoryHarness {
        fn quotas(&self) -> &dyn QuotaStore {
            &self.quotas
        }
    }

    /// Declares one `#[tokio::test]` per conformance case.
    ///
    /// One test each, on a fresh ledger each: a failure names the property that broke, and no
    /// case can pass because a previous one left the ledger in a convenient state.
    macro_rules! conformance_cases {
        ($($case:ident),+ $(,)?) => {
            $(
                #[tokio::test]
                async fn $case() {
                    super::$case(&MemoryHarness::default()).await;
                }
            )+
        };
    }

    conformance_cases! {
        charging_the_same_address_twice_debits_once,
        the_already_attributed_answer_says_nothing_about_who_holds_it,
        an_uncharged_account_owes_nothing,
        releasing_a_reservation_credits_the_bytes_back,
        another_accounts_reservation_is_not_releasable,
        the_collector_releases_by_address_and_the_ledger_names_the_account,
        releasing_an_unattributed_address_is_none_rather_than_an_error,
        the_crossing_is_stamped_once_and_cleared_by_going_under,
        a_collector_release_clears_the_over_limit_clock,
    }

    /// The whole suite, in one pass on one ledger.
    ///
    /// The entry point a container-backed adapter uses, so it is exercised here too — otherwise
    /// the first time anyone ran it would be against Postgres, where a failure is hardest to
    /// read. It also proves the cases really are independent.
    #[tokio::test]
    async fn the_in_memory_ledger_conforms() {
        run_all(&MemoryHarness::default()).await;
    }
}
