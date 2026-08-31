//! The quota port's own suite.
//!
//! Most of it is the state machine, which is pure — so the cases that matter read as a table of
//! "this much used, this long over, this kind of write" and there is nothing to mock.

use super::*;

/// A deployment with real limits: 100 bytes soft, 200 hard, a 14-day grace.
fn limits() -> QuotaLimits {
    QuotaLimits::new(100, 200, DEFAULT_GRACE_WINDOW)
}

/// An instant `days` after the epoch.
fn day(days: i64) -> Timestamp {
    Timestamp::UNIX_EPOCH + SignedDuration::from_hours(days * 24)
}

fn user() -> UserId {
    UserId::new("01937b7c-0000-7000-8000-000000000001")
}

fn address(seed: u8) -> ContentAddress {
    ContentAddress::parse(&capsule_core::crypto::hash::hash_bytes(&[seed; 8]).to_hex())
        .expect("a content address")
}

#[test]
fn the_states_follow_the_thresholds() {
    let now = day(0);
    assert_eq!(state_of(0, None, now, limits()), QuotaState::Ok);
    assert_eq!(state_of(99, None, now, limits()), QuotaState::Ok);
    assert_eq!(state_of(100, None, now, limits()), QuotaState::SoftWarning);
    assert_eq!(state_of(199, None, now, limits()), QuotaState::SoftWarning);
    assert_eq!(state_of(200, None, now, limits()), QuotaState::HardExceeded);
}

#[test]
fn the_grace_window_is_measured_from_the_crossing() {
    assert_eq!(
        state_of(250, Some(day(0)), day(13), limits()),
        QuotaState::HardExceeded,
        "inside the window, only uploads are refused"
    );
    assert_eq!(
        state_of(250, Some(day(0)), day(15), limits()),
        QuotaState::GraceExpired,
    );
}

#[test]
fn being_over_with_no_recorded_crossing_is_not_expired() {
    assert_eq!(
        state_of(250, None, day(400), limits()),
        QuotaState::HardExceeded,
        "refusing metadata growth on the strength of a missing timestamp would lock a user out \
         of the writes that free space, which is the one thing quota must never do"
    );
}

#[test]
fn a_lifecycle_write_is_admitted_in_every_state() {
    for state in [
        QuotaState::Ok,
        QuotaState::SoftWarning,
        QuotaState::HardExceeded,
        QuotaState::GraceExpired,
    ] {
        assert!(
            admits(state, WriteClass::Lifecycle, 10_000, 0, limits()),
            "a user must be able to delete their way back under quota, and a delete is a write"
        );
    }
}

#[test]
fn metadata_growth_stops_only_when_the_grace_expires() {
    for (state, expected) in [
        (QuotaState::Ok, true),
        (QuotaState::SoftWarning, true),
        (QuotaState::HardExceeded, true),
        (QuotaState::GraceExpired, false),
    ] {
        assert_eq!(
            admits(state, WriteClass::MetadataGrowth, 250, 1, limits()),
            expected,
            "{state:?} decided metadata growth wrongly"
        );
    }
}

#[test]
fn an_upload_is_checked_against_its_projected_total() {
    assert!(
        admits(QuotaState::Ok, WriteClass::Upload, 150, 49, limits()),
        "199 is under the hard limit"
    );
    assert!(
        !admits(QuotaState::Ok, WriteClass::Upload, 150, 50, limits()),
        "the declared size is what becomes the cap, so it is what the check has to include"
    );
}

#[test]
fn an_unlimited_deployment_never_leaves_ok() {
    let limits = QuotaLimits::unlimited();
    assert_eq!(state_of(u64::MAX - 1, None, day(0), limits), QuotaState::Ok);
    assert!(admits(
        QuotaState::Ok,
        WriteClass::Upload,
        u64::MAX / 2,
        u64::MAX / 2,
        limits
    ));
}

#[tokio::test]
async fn a_shared_blob_is_charged_to_its_first_uploader_only() {
    let quotas = InMemoryQuota::new();
    let first = user();
    let second = UserId::new("01937b7c-0000-7000-8000-000000000002");
    let shared = address(1);

    assert_eq!(
        quotas
            .charge(&first, &shared, 64, day(0), limits())
            .await
            .expect("charge"),
        ChargeOutcome::Charged { used: 64 },
    );
    assert_eq!(
        quotas
            .charge(&second, &shared, 64, day(0), limits())
            .await
            .expect("charge"),
        ChargeOutcome::AlreadyAttributed,
        "without this a malicious user could exhaust another account's quota by re-uploading \
         blobs whose addresses they already know",
    );
    assert_eq!(
        quotas.usage(&second).await.expect("usage").used,
        0,
        "the second uploader is a merge, not a second copy"
    );
}

#[tokio::test]
async fn releasing_a_reservation_credits_the_bytes_back() {
    let quotas = InMemoryQuota::new();
    let user = user();
    let held = address(2);
    quotas
        .charge(&user, &held, 64, day(0), limits())
        .await
        .expect("charge");

    assert!(quotas.release(&user, &held).await.expect("release"));
    assert_eq!(quotas.usage(&user).await.expect("usage").used, 0);
    assert!(
        !quotas.release(&user, &held).await.expect("release"),
        "a second release must not credit the bytes twice"
    );

    // And the address is free again, so a later uploader is charged for it.
    assert_eq!(
        quotas
            .charge(&user, &held, 64, day(0), limits())
            .await
            .expect("charge"),
        ChargeOutcome::Charged { used: 64 },
    );
}

#[tokio::test]
async fn another_users_reservation_is_not_releasable() {
    let quotas = InMemoryQuota::new();
    let owner = user();
    let other = UserId::new("01937b7c-0000-7000-8000-000000000002");
    let held = address(3);
    quotas
        .charge(&owner, &held, 64, day(0), limits())
        .await
        .expect("charge");

    assert!(
        !quotas.release(&other, &held).await.expect("release"),
        "releasing somebody else's attribution would let one account free bytes off another's \
         ledger — and, worse, tell them the address was attributed"
    );
    assert_eq!(quotas.usage(&owner).await.expect("usage").used, 64);
}

#[tokio::test]
async fn the_crossing_is_stamped_once_and_cleared_by_going_under() {
    let quotas = InMemoryQuota::new();
    let user = user();
    quotas
        .charge(&user, &address(4), 150, day(0), limits())
        .await
        .expect("charge");
    assert_eq!(quotas.usage(&user).await.expect("usage").over_since, None);

    quotas
        .charge(&user, &address(5), 100, day(1), limits())
        .await
        .expect("charge");
    let over = quotas.usage(&user).await.expect("usage");
    assert_eq!(over.used, 250);
    assert_eq!(over.over_since, Some(day(1)));

    // A later charge while still over must not restamp the clock, or the grace window would
    // never expire for an account that keeps trying to upload.
    quotas
        .charge(&user, &address(6), 10, day(9), limits())
        .await
        .expect("charge");
    assert_eq!(
        quotas.usage(&user).await.expect("usage").over_since,
        Some(day(1)),
    );

    // Going back under stops the clock, so a later crossing gets a fresh window rather than
    // inheriting an expired one.
    quotas.release(&user, &address(5)).await.expect("release");
    assert_eq!(quotas.usage(&user).await.expect("usage").over_since, None);
}

#[tokio::test]
async fn the_collector_releases_by_address_and_the_ledger_names_the_account() {
    // `S-C44`. A sweep knows an address and nothing else — attribution is global by content
    // address, so the blob it is deleting may be charged to an account with no remaining
    // connection to the asset whose purge exposed it. The collector must not guess a user.
    let ledger = InMemoryQuota::new();
    let address = address(41);
    ledger
        .charge(
            &user(),
            &address,
            1_024,
            Timestamp::UNIX_EPOCH,
            QuotaLimits::unlimited(),
        )
        .await
        .expect("the ledger charges");

    let released = ledger
        .release_attribution(&address)
        .await
        .expect("the ledger answers")
        .expect("the address was attributed");
    assert_eq!(released, (user(), 1_024));
    assert_eq!(ledger.usage(&user()).await.expect("usage").used, 0);
}

#[tokio::test]
async fn releasing_an_unattributed_address_is_none_rather_than_an_error() {
    // The ordinary case for a blob the ledger never saw. A sweep that treated it as a failure
    // would stall on the first one.
    let ledger = InMemoryQuota::new();
    assert_eq!(
        ledger
            .release_attribution(&address(42))
            .await
            .expect("the ledger answers"),
        None
    );
}

#[tokio::test]
async fn a_collector_release_clears_the_over_limit_clock_like_a_user_release() {
    // Both releases credit the same way, which is why they share one helper: a second copy
    // would eventually forget `over_since`, leaving an account back under its limit still
    // carrying the clock that decides when a soft limit becomes a hard one.
    let ledger = InMemoryQuota::new();
    let limits = QuotaLimits::new(1_000, 1_500, SignedDuration::from_hours(24));
    ledger
        .charge(&user(), &address(43), 2_000, Timestamp::UNIX_EPOCH, limits)
        .await
        .expect("the ledger charges");
    assert!(
        ledger
            .usage(&user())
            .await
            .expect("usage")
            .over_since
            .is_some()
    );

    ledger
        .release_attribution(&address(43))
        .await
        .expect("the ledger answers")
        .expect("attributed");

    assert_eq!(
        ledger.usage(&user()).await.expect("usage").over_since,
        None,
        "back under the limit, so a later crossing gets a fresh window"
    );
}
