use jiff::{SignedDuration, Timestamp};

use super::{from_micros, stored, to_micros};

#[test]
fn an_instant_survives_the_round_trip() {
    let at = Timestamp::UNIX_EPOCH + SignedDuration::from_secs(1_700_000_000);
    assert_eq!(from_micros(to_micros(at)), Some(at));
}

#[test]
fn the_epoch_is_zero_so_a_column_is_readable_by_a_human_with_a_calculator() {
    assert_eq!(to_micros(Timestamp::UNIX_EPOCH), 0);
    assert_eq!(from_micros(0), Some(Timestamp::UNIX_EPOCH));
}

#[test]
fn instants_order_the_way_their_integers_do() {
    // The whole justification for giving up SQL date arithmetic: every comparison these
    // tables make is an ordering, and an ordering is preserved.
    let earlier = Timestamp::UNIX_EPOCH + SignedDuration::from_secs(10);
    let later = Timestamp::UNIX_EPOCH + SignedDuration::from_secs(20);
    assert!(earlier < later);
    assert!(to_micros(earlier) < to_micros(later));
}

#[test]
fn every_instant_this_module_writes_can_be_read_back() {
    // The property the adapters actually depend on, and it is *not* "MAX round-trips".
    // `Timestamp::MAX` is `9999-12-30T22:00:00.999999999Z` and
    // `Timestamp::from_microsecond` refuses anything past the whole second below it, so an
    // unclamped conversion would put a number in a `NOT NULL` column that this server
    // cannot decode — a corrupt row of its own making. `to_micros` clamps instead.
    assert!(from_micros(to_micros(Timestamp::MAX)).is_some());
    assert!(from_micros(to_micros(Timestamp::MIN)).is_some());
    // Microseconds rather than nanoseconds precisely so the *bottom* end survives: an `i64`
    // of nanoseconds overflows in 2262, well inside `Timestamp`'s range.
    assert_eq!(from_micros(to_micros(Timestamp::MIN)), Some(Timestamp::MIN));
    // And an integer no adapter wrote is refused rather than turned into some other instant.
    assert_eq!(from_micros(i64::MAX), None);
    assert_eq!(from_micros(i64::MIN), None);
}

#[test]
fn sub_microsecond_precision_is_dropped_and_stored_says_so() {
    // A `BIGINT` of microseconds cannot carry nanoseconds. What matters is that an adapter
    // knows: a record it builds in Rust and returns without re-reading has to go through
    // `stored`, or the value it hands a caller differs from the value the next read
    // produces.
    let precise = Timestamp::from_nanosecond(1_700_000_000_123_456_789).expect("an instant");
    let truncated = stored(precise);
    assert_ne!(
        truncated, precise,
        "the nanoseconds are gone, and that is the point"
    );
    assert_eq!(
        truncated,
        Timestamp::from_microsecond(1_700_000_000_123_456).expect("an instant"),
    );
    assert_eq!(stored(truncated), truncated, "and `stored` is idempotent");
}

#[test]
fn the_clamp_is_a_clamp_and_not_a_wrap() {
    // The one thing worse than losing the last second of year 9999 would be storing an
    // instant that reads back as some *other* instant.
    let clamped = stored(Timestamp::MAX);
    assert!(clamped <= Timestamp::MAX);
    assert_eq!(stored(clamped), clamped);
    assert_eq!(from_micros(to_micros(clamped)), Some(clamped));
}

#[test]
fn a_negative_instant_is_before_the_epoch_rather_than_a_failure() {
    let before = Timestamp::UNIX_EPOCH - SignedDuration::from_secs(1);
    assert_eq!(to_micros(before), -1_000_000);
    assert_eq!(from_micros(-1_000_000), Some(before));
}
