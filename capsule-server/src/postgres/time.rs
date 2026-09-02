//! `jiff::Timestamp` ⇄ `BIGINT` microseconds since the Unix epoch.
//!
//! # Why an integer and not `TIMESTAMPTZ`
//!
//! Binding a `TIMESTAMPTZ` needs one of sea-orm's datetime features, and both are refused. With
//! `with-chrono` the server joins the chrono path that design/dependencies.md makes a
//! review-blocking gate; with `with-time` the workspace gains a third datetime crate with no row
//! in that table. Without either there is no Rust type for the column at all, so the choice is
//! an integer column or a banned dependency.
//!
//! This is the server's exact analogue of the CLI's "chrono only at the entity boundary" rule:
//! the conversion happens here, at the adapter's edge, and every layer above speaks
//! [`jiff::Timestamp`]. design/dependencies.md already blesses integer epochs as a serialized
//! form.
//!
//! # What it costs
//!
//! No SQL date arithmetic. Every expiry and retention comparison in the durable ports is an
//! ordering on one column — `updated_at`, `first_seen`, `retention_until` — and integers order
//! identically, so nothing in scope wants it. A future port that needs `now() - interval` gets
//! the arithmetic in Rust, above the boundary, where the clock is already injected.
//!
//! # Microseconds, not nanoseconds
//!
//! `i64` nanoseconds run out in 2262 and `jiff` represents instants well past that, so the
//! conversion would be lossy at the wrong end. Microseconds cover the whole of `Timestamp`'s
//! range in an `i64` and are PostgreSQL's own `timestamp` resolution, so a column that is later
//! migrated to `TIMESTAMPTZ` loses nothing.

use jiff::Timestamp;

/// The instant `at`, as microseconds since the Unix epoch.
pub(crate) fn to_micros(at: Timestamp) -> i64 {
    at.as_microsecond()
}

/// The instant `micros` microseconds after the Unix epoch, or `None` if it is not one.
///
/// `None` is a **corrupt row**, not a missing value: the column is `NOT NULL` wherever it is
/// required, and a value outside `Timestamp`'s range can only come from something that did not
/// write it through [`to_micros`]. The adapters map it onto
/// [`StoreError::Corrupt`](crate::store::StoreError::Corrupt).
pub(crate) fn from_micros(micros: i64) -> Option<Timestamp> {
    Timestamp::from_microsecond(micros).ok()
}

#[cfg(test)]
mod tests {
    use jiff::{SignedDuration, Timestamp};

    use super::{from_micros, to_micros};

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
    fn the_whole_representable_range_survives_and_nothing_outside_it_is_invented() {
        // Microseconds rather than nanoseconds precisely so this holds: an `i64` of nanoseconds
        // overflows in 2262, well inside `Timestamp`'s range.
        assert_eq!(from_micros(to_micros(Timestamp::MAX)), Some(Timestamp::MAX));
        assert_eq!(from_micros(to_micros(Timestamp::MIN)), Some(Timestamp::MIN));
        assert_eq!(from_micros(i64::MAX), None);
        assert_eq!(from_micros(i64::MIN), None);
    }

    #[test]
    fn a_negative_instant_is_before_the_epoch_rather_than_a_failure() {
        let before = Timestamp::UNIX_EPOCH - SignedDuration::from_secs(1);
        assert_eq!(to_micros(before), -1_000_000);
        assert_eq!(from_micros(-1_000_000), Some(before));
    }
}
