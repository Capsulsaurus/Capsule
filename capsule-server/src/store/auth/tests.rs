use super::*;

fn at(seconds: i64) -> Timestamp {
    Timestamp::from_second(seconds).expect("in range")
}

fn days(n: i64) -> SignedDuration {
    SignedDuration::from_hours(n * 24)
}

#[test]
fn the_defaults_are_the_documented_180_day_window_and_365_day_cap() {
    assert_eq!(DEFAULT_SESSION_TTL, days(180));
    assert_eq!(DEFAULT_SESSION_HARD_CAP, days(365));
}

#[test]
fn a_fresh_sign_in_lives_for_the_sliding_window() {
    let now = at(1_000_000);
    assert_eq!(
        session_deadline(now, now, days(180), days(365)),
        now + days(180),
        "inside the cap, the sliding window governs"
    );
}

#[test]
fn the_sliding_window_is_measured_from_the_latest_issuance() {
    let signed_in = at(1_000_000);
    let now = signed_in + days(100);
    assert_eq!(
        session_deadline(signed_in, now, days(180), days(365)),
        now + days(180),
        "a rotation 100 days in buys another full window"
    );
}

#[test]
fn the_hard_cap_clips_the_window_near_the_end_of_the_lineage() {
    let signed_in = at(1_000_000);
    let now = signed_in + days(300);
    assert_eq!(
        session_deadline(signed_in, now, days(180), days(365)),
        signed_in + days(365),
        "a rotation 300 days in is clipped to the cap, not given 180 more"
    );
}

#[test]
fn a_lineage_past_its_cap_is_already_expired() {
    let signed_in = at(1_000_000);
    let now = signed_in + days(400);
    assert!(
        session_deadline(signed_in, now, days(180), days(365)) <= now,
        "a session past its cap has no life left to open with"
    );
}

#[test]
fn a_cap_shorter_than_the_window_governs_from_the_start() {
    let now = at(1_000_000);
    assert_eq!(session_deadline(now, now, days(30), days(7)), now + days(7));
}

#[test]
fn an_unrepresentable_deadline_saturates_rather_than_panicking() {
    assert_eq!(
        session_deadline(Timestamp::MAX, Timestamp::MAX, days(1), days(1)),
        Timestamp::MAX
    );
}
