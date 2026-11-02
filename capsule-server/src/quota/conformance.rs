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

use super::{ChargeOutcome, ExpiredReservation, QuotaLimits, QuotaStore};
use crate::blob::ContentAddress;
use crate::store::{StoreError, StoreFuture, UserId};

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

    /// Make `user` a subscriber of the pulled album `album` and have that album hold `address`,
    /// of `size` bytes: what a committed pulled page leaves behind for the ledger to count.
    ///
    /// Setup rather than a port operation: the ledger never writes a pulled holding, it reads it.
    fn hold_pulled<'a>(
        &'a self,
        user: &'a UserId,
        album: &'a str,
        address: &'a ContentAddress,
        size: u64,
    ) -> StoreFuture<'a, ()>;

    /// End `user`'s subscription to the pulled album `album`.
    fn unsubscribe<'a>(&'a self, user: &'a UserId, album: &'a str) -> StoreFuture<'a, ()>;
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
// Reservations (#1059)
// ===========================================================================================

/// `case`'s own expired reservations as of `now`: the listing, narrowed to `case`'s accounts so
/// cases sharing one ledger do not see each other's.
async fn expired_of(h: &dyn Harness, case: &str, now: Timestamp) -> Vec<ExpiredReservation> {
    ok(
        h.quotas().expired_reservations(now, usize::MAX).await,
        "list expired reservations",
    )
    .into_iter()
    .filter(|reservation| reservation.user.as_str().starts_with(case))
    .collect()
}

/// A reservation is charged like any attribution, listed only once its deadline has passed, and
/// released as expired only by its own account, once.
pub async fn a_reservation_lapses_only_past_its_deadline(h: &dyn Harness) {
    let owner = user("lapse", 1);
    let other = user("lapse", 2);
    let held = address("lapse", 1);
    assert_eq!(
        ok(
            h.quotas()
                .reserve(&owner, &held, 64, day(0), day(1), limits())
                .await,
            "reserve an address",
        ),
        ChargeOutcome::Charged { used: 64 },
    );
    assert_eq!(
        ok(
            h.quotas()
                .reserve(&other, &held, 64, day(0), day(1), limits())
                .await,
            "reserve an attributed address",
        ),
        ChargeOutcome::AlreadyAttributed,
        "a reservation is an attribution, so the dedup rule holds for it"
    );

    assert!(expired_of(h, "lapse", day(0)).await.is_empty());
    assert!(!ok(
        h.quotas().release_expired(&owner, &held, day(0)).await,
        "release a reservation inside its deadline",
    ));
    assert_eq!(
        expired_of(h, "lapse", day(1)).await,
        vec![ExpiredReservation {
            user: owner.clone(),
            address: held.clone(),
            size: 64,
            reserved_until: day(1),
        }],
    );

    assert!(!ok(
        h.quotas().release_expired(&other, &held, day(1)).await,
        "release another account's reservation",
    ));
    assert!(ok(
        h.quotas().release_expired(&owner, &held, day(1)).await,
        "release an expired reservation",
    ));
    assert_eq!(ok(h.quotas().usage(&owner).await, "read usage").used, 0);
    assert!(
        !ok(
            h.quotas().release_expired(&owner, &held, day(1)).await,
            "release it again",
        ),
        "a second release must not credit the bytes twice"
    );
    assert!(expired_of(h, "lapse", day(9)).await.is_empty());
}

/// A settled reservation is an ordinary charge: never listed, never released as expired.
///
/// The property a completed upload's charge rests on.
pub async fn a_settled_reservation_is_never_released_as_expired(h: &dyn Harness) {
    let owner = user("settle", 1);
    let held = address("settle", 1);
    ok(
        h.quotas()
            .reserve(&owner, &held, 64, day(0), day(1), limits())
            .await,
        "reserve an address",
    );

    assert!(ok(h.quotas().settle(&held).await, "settle"));
    assert!(expired_of(h, "settle", day(9)).await.is_empty());
    assert!(!ok(
        h.quotas().release_expired(&owner, &held, day(9)).await,
        "release a settled charge as expired",
    ));
    assert_eq!(ok(h.quotas().usage(&owner).await, "read usage").used, 64);
    assert!(
        ok(h.quotas().settle(&held).await, "settle again"),
        "a settled attribution is still attributed"
    );
    assert!(!ok(
        h.quotas().extend_reservation(&held, day(9)).await,
        "extend a settled charge",
    ));

    let unattributed = address("settle", 2);
    assert!(
        !ok(h.quotas().settle(&unattributed).await, "settle nothing"),
        "an unattributed address is answered so the finalizer charges it"
    );

    // A plain charge pays for stored bytes, and is settled from the start.
    let stored = address("settle", 3);
    ok(
        h.quotas()
            .charge(&owner, &stored, 8, day(0), limits())
            .await,
        "charge an address",
    );
    assert!(expired_of(h, "settle", day(9)).await.is_empty());
}

/// An extension moves a reservation's deadline later, never earlier.
pub async fn an_extension_moves_the_deadline_and_never_shortens_it(h: &dyn Harness) {
    let owner = user("extend", 1);
    let held = address("extend", 1);
    ok(
        h.quotas()
            .reserve(&owner, &held, 64, day(0), day(1), limits())
            .await,
        "reserve an address",
    );

    assert!(ok(
        h.quotas().extend_reservation(&held, day(3)).await,
        "extend",
    ));
    assert!(expired_of(h, "extend", day(2)).await.is_empty());
    ok(
        h.quotas().extend_reservation(&held, day(2)).await,
        "extend to an earlier deadline",
    );
    assert!(
        expired_of(h, "extend", day(2)).await.is_empty(),
        "an earlier deadline must not shorten the reservation"
    );
    assert!(!ok(
        h.quotas().release_expired(&owner, &held, day(2)).await,
        "release an extended reservation before its new deadline",
    ));
    assert_eq!(expired_of(h, "extend", day(3)).await.len(), 1);
    assert!(!ok(
        h.quotas()
            .extend_reservation(&address("extend", 2), day(3))
            .await,
        "extend an unattributed address",
    ));

    assert!(ok(h.quotas().settle(&held).await, "settle"));
}

/// The listing is earliest deadline first, and bounded.
pub async fn expired_reservations_are_listed_earliest_first_and_bounded(h: &dyn Harness) {
    let owner = user("order", 1);
    for (n, deadline) in [(1, 30), (2, 10), (3, 20)] {
        ok(
            h.quotas()
                .reserve(
                    &owner,
                    &address("order", n),
                    1,
                    day(0),
                    day(deadline),
                    limits(),
                )
                .await,
            "reserve an address",
        );
    }

    let deadlines: Vec<Timestamp> = expired_of(h, "order", day(40))
        .await
        .into_iter()
        .map(|reservation| reservation.reserved_until)
        .collect();
    assert_eq!(deadlines, vec![day(10), day(20), day(30)]);
    let first = ok(
        h.quotas().expired_reservations(day(25), 1).await,
        "list one expired reservation",
    );
    assert_eq!(first.len(), 1, "the limit bounds the listing");
    assert!(
        first[0].reserved_until <= day(10),
        "the earliest deadline comes first"
    );

    for n in 1..=3 {
        ok(h.quotas().settle(&address("order", n)).await, "settle");
    }
}

// ===========================================================================================
// Pulled blobs
// ===========================================================================================

/// The usage of `owner`, as the ledger reports it.
async fn used(h: &dyn Harness, owner: &UserId) -> u64 {
    ok(h.quotas().usage(owner).await, "read usage").used
}

/// A pulled blob is charged in full to every subscriber of the album holding it, and the
/// charge ends with the subscription.
///
/// A pulled album is shared by its subscribers, so none of them is "first": each one holds the
/// bytes and each one pays for them. Unsubscribing is what releases them, and releases one
/// subscriber without touching the other.
pub async fn a_pulled_blob_counts_in_full_against_each_subscriber(h: &dyn Harness) {
    let first = user("pulledshared", 1);
    let second = user("pulledshared", 2);
    let blob = address("pulledshared", 1);
    for subscriber in [&first, &second] {
        ok(
            h.hold_pulled(subscriber, "pulledshared-album", &blob, 40)
                .await,
            "hold a pulled blob",
        );
    }
    assert_eq!(used(h, &first).await, 40);
    assert_eq!(used(h, &second).await, 40, "charged in full, never split");

    ok(
        h.unsubscribe(&first, "pulledshared-album").await,
        "unsubscribe",
    );
    assert_eq!(used(h, &first).await, 0, "the unsubscribe released it");
    assert_eq!(
        used(h, &second).await,
        40,
        "and released it for that subscriber only"
    );
}

/// A blob two of a subscriber's albums hold is charged once, and stays charged until the last
/// of them is gone; the usage never goes below what is held.
pub async fn a_pulled_blob_counts_once_across_a_subscribers_albums(h: &dyn Harness) {
    let owner = user("pulledonce", 1);
    let blob = address("pulledonce", 1);
    let other = address("pulledonce", 2);
    ok(
        h.hold_pulled(&owner, "pulledonce-a", &blob, 30).await,
        "hold a pulled blob",
    );
    ok(
        h.hold_pulled(&owner, "pulledonce-b", &blob, 30).await,
        "hold the same blob through a second album",
    );
    ok(
        h.hold_pulled(&owner, "pulledonce-b", &other, 5).await,
        "hold a second blob",
    );
    assert_eq!(used(h, &owner).await, 35, "the shared blob counts once");

    ok(h.unsubscribe(&owner, "pulledonce-a").await, "unsubscribe");
    assert_eq!(
        used(h, &owner).await,
        35,
        "still held through the other album, so still charged"
    );
    ok(h.unsubscribe(&owner, "pulledonce-b").await, "unsubscribe");
    assert_eq!(used(h, &owner).await, 0);
    ok(
        h.unsubscribe(&owner, "pulledonce-b").await,
        "unsubscribe twice",
    );
    assert_eq!(used(h, &owner).await, 0, "a second release credits nothing");
}

/// A blob the subscriber is itself attributed in the ledger counts once, and the collector's
/// release of that attribution leaves it charged as pulled rather than crediting it.
pub async fn a_pulled_blob_the_subscriber_uploaded_counts_once(h: &dyn Harness) {
    let owner = user("pulledmine", 1);
    let blob = address("pulledmine", 1);
    ok(
        h.quotas().charge(&owner, &blob, 50, day(0), limits()).await,
        "charge an address",
    );
    ok(
        h.hold_pulled(&owner, "pulledmine-album", &blob, 50).await,
        "hold the same blob pulled",
    );
    assert_eq!(
        used(h, &owner).await,
        50,
        "attributed and pulled is one copy"
    );

    assert_eq!(
        ok(
            h.quotas().release_attribution(&blob).await,
            "release by address"
        ),
        Some((owner.clone(), 50)),
    );
    assert_eq!(
        used(h, &owner).await,
        50,
        "the bytes are still held through the album, so they are still charged"
    );
    ok(
        h.unsubscribe(&owner, "pulledmine-album").await,
        "unsubscribe",
    );
    assert_eq!(used(h, &owner).await, 0);
}

/// A blob another account is attributed is still charged to the receiver, so the receiver's
/// usage says nothing about who else on the server holds those bytes.
pub async fn a_pulled_blob_another_account_uploaded_still_counts_for_the_receiver(h: &dyn Harness) {
    let uploader = user("pulledtheirs", 1);
    let receiver = user("pulledtheirs", 2);
    let blob = address("pulledtheirs", 1);
    ok(
        h.quotas()
            .charge(&uploader, &blob, 25, day(0), limits())
            .await,
        "charge an address",
    );
    ok(
        h.hold_pulled(&receiver, "pulledtheirs-album", &blob, 25)
            .await,
        "hold the same blob pulled",
    );
    assert_eq!(used(h, &receiver).await, 25);
    assert_eq!(
        used(h, &uploader).await,
        25,
        "the uploader's charge is its own"
    );
}

/// Pulled bytes count toward the hard limit a charge is measured against: the total a charge
/// reports includes them, and a charge that crosses because of them stamps the crossing.
pub async fn pulled_bytes_count_toward_the_hard_limit(h: &dyn Harness) {
    let owner = user("pulledlimit", 1);
    ok(
        h.hold_pulled(&owner, "pulledlimit-album", &address("pulledlimit", 1), 150)
            .await,
        "hold a pulled blob",
    );
    assert_eq!(
        ok(
            h.quotas()
                .charge(&owner, &address("pulledlimit", 2), 60, day(3), limits())
                .await,
            "charge an address",
        ),
        ChargeOutcome::Charged { used: 210 },
    );
    let usage = ok(h.quotas().usage(&owner).await, "read usage");
    assert_eq!(usage.used, 210);
    assert_eq!(usage.over_since, Some(day(3)));
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

    a_reservation_lapses_only_past_its_deadline(h).await;
    a_settled_reservation_is_never_released_as_expired(h).await;
    an_extension_moves_the_deadline_and_never_shortens_it(h).await;
    expired_reservations_are_listed_earliest_first_and_bounded(h).await;

    a_pulled_blob_counts_in_full_against_each_subscriber(h).await;
    a_pulled_blob_counts_once_across_a_subscribers_albums(h).await;
    a_pulled_blob_the_subscriber_uploaded_counts_once(h).await;
    a_pulled_blob_another_account_uploaded_still_counts_for_the_receiver(h).await;
    pulled_bytes_count_toward_the_hard_limit(h).await;
}

#[cfg(test)]
mod tests;
