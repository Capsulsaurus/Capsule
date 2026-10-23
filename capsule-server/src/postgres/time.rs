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
//! `stored` is what such an adapter puts the instant through, so the record it returns is the
//! record the database holds.
//!
//! **`jiff` will not read back its own maximum.** `Timestamp::MAX` is
//! `9999-12-30T22:00:00.999999999Z`, and `Timestamp::from_microsecond` accepts nothing past the
//! whole second below it — so the obvious round trip is *not* total, and a naive
//! `to_micros`-then-`from_micros` on `MAX` is a column no adapter can decode. `to_micros`
//! therefore clamps, exactly as `crate::store::deadline` clamps for the same reason: an
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
mod tests;
