//! The quota module's own suite: the **pure** half.
//!
//! [`state_of`] and [`admits`] take no store, so the cases that matter read as a table of "this
//! much used, this long over, this kind of write" and there is nothing to mock. A suite generic
//! over an adapter cannot say anything about a function that takes none, which is why these
//! stayed here when the ledger's cases moved.
//!
//! Everything the *ledger* owes — the dedup rule, the two releases and the over-limit clock —
//! is in [`super::conformance`] (#402), so it runs against `InMemoryQuota` and against the
//! Postgres adapter from one list. Those cases used to live here against the double only, which
//! made the double an unproven stand-in for exactly the adapter that has to get concurrency
//! right.

use super::*;

/// A deployment with real limits: 100 bytes soft, 200 hard, a 14-day grace.
fn limits() -> QuotaLimits {
    QuotaLimits::new(100, 200, DEFAULT_GRACE_WINDOW)
}

/// An instant `days` after the epoch.
fn day(days: i64) -> Timestamp {
    Timestamp::UNIX_EPOCH + SignedDuration::from_hours(days * 24)
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
