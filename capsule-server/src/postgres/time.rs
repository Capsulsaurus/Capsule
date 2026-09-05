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
//! conversion would be lossy at the wrong end. Microseconds cover `Timestamp`'s whole range in
//! an `i64` and are PostgreSQL's own `timestamp` resolution, so a column later migrated to
//! `TIMESTAMPTZ` loses nothing.
//!
//! # Two lossy edges, and what is done about each
//!
//! **Sub-microsecond precision is dropped.** `Timestamp` carries nanoseconds and a `BIGINT` of
//! microseconds cannot, so an instant that goes into a column comes back truncated toward zero.
//! That matters for one thing only: an adapter that builds a record in Rust and returns it
//! *without* reading it back would hand a caller a value the next read does not produce.
//! [`stored`] is what such an adapter puts the instant through, so the record it returns is the
//! record the database holds.
//!
//! **`jiff` will not read back its own maximum.** `Timestamp::MAX` is
//! `9999-12-30T22:00:00.999999999Z`, and `Timestamp::from_microsecond` accepts nothing past the
//! whole second below it — so the obvious round trip is *not* total, and a naive
//! `to_micros`-then-`from_micros` on `MAX` is a column no adapter can decode. [`to_micros`]
//! therefore clamps, exactly as [`crate::store::deadline`] clamps for the same reason: an
//! instant that far out is indistinguishable from never, and clamping is what gives that. The
//! bounds are derived from `jiff`'s own constants rather than written down, so a repin cannot
//! silently move them out from under this.

use jiff::Timestamp;

/// The largest microsecond value [`from_micros`] will read back.
///
/// Derived rather than hardcoded: `Timestamp::MAX` carries a fractional second that
/// `Timestamp::from_microsecond` refuses, and the whole second below it is the boundary.
fn storable_max() -> i64 {
    Timestamp::MAX.as_second().saturating_mul(1_000_000)
}

/// The smallest microsecond value [`from_micros`] will read back.
///
/// `Timestamp::MIN` lands on a whole second, so it needs no adjustment — and deriving it anyway
/// is what keeps this pair honest if a repin changes either end.
fn storable_min() -> i64 {
    Timestamp::MIN.as_microsecond()
}

/// The instant `at`, as microseconds since the Unix epoch, clamped to what can be read back.
///
/// The clamp is reachable only in the last second of representable time, and it is the same
/// choice [`crate::store::deadline`] makes there. Writing the unclamped value would put a number
/// in the column that [`from_micros`] rejects — a row this server wrote and cannot decode.
pub(crate) fn to_micros(at: Timestamp) -> i64 {
    at.as_microsecond().clamp(storable_min(), storable_max())
}

/// The instant `micros` microseconds after the Unix epoch, or `None` if it is not one.
///
/// `None` is a **corrupt row**, not a missing value: the column is `NOT NULL` wherever it is
/// required, and [`to_micros`] cannot produce a value outside the range, so an unreadable one
/// came from something that did not write it through this module. The adapters map it onto
/// [`StoreError::Corrupt`](crate::store::StoreError::Corrupt).
pub(crate) fn from_micros(micros: i64) -> Option<Timestamp> {
    Timestamp::from_microsecond(micros).ok()
}

/// `at` as the schema will hold it.
///
/// What an adapter puts an incoming instant through before keeping it in a record it returns
/// without re-reading. Idempotent, and total: [`to_micros`] clamps, so the value always reads
/// back.
pub(crate) fn stored(at: Timestamp) -> Timestamp {
    // The `unwrap_or` is unreachable while `to_micros` clamps, and is a clamp rather than a
    // panic because a composition root has no business dying over an instant.
    from_micros(to_micros(at)).unwrap_or(at)
}

#[cfg(test)]
mod tests {
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
}
