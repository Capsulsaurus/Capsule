//! The chrono ⇄ jiff conversion seam.
//!
//! Per the [Dependencies design doc](../../../capsule-docs/src/content/docs/design/dependencies.md),
//! `jiff` is the canonical datetime library for all domain logic; `chrono` survives only
//! as the sea-orm column type inside the entity crates. Every value crossing the entity
//! boundary converts through these helpers, losslessly at second + nanosecond precision
//! for any real wall-clock value.

use jiff::Timestamp;
use sea_orm::prelude::{DateTimeUtc, DateTimeWithTimeZone};

/// Convert a sea-orm UTC column value to a jiff [`Timestamp`].
///
/// Falls back to the Unix epoch for values outside jiff's representable range
/// (year 9999+ / pre-(-9999) — unreachable for real row timestamps).
pub fn entity_to_ts(dt: DateTimeUtc) -> Timestamp {
    let nanos = i32::try_from(dt.timestamp_subsec_nanos()).unwrap_or(0);
    Timestamp::new(dt.timestamp(), nanos).unwrap_or(Timestamp::UNIX_EPOCH)
}

/// Convert a sea-orm timezone-aware column value to a jiff [`Timestamp`].
pub fn entity_tz_to_ts(dt: DateTimeWithTimeZone) -> Timestamp {
    entity_to_ts(dt.to_utc())
}

/// Convert a jiff [`Timestamp`] to a sea-orm UTC column value.
pub fn ts_to_entity(ts: Timestamp) -> DateTimeUtc {
    let nanos = u32::try_from(ts.subsec_nanosecond()).unwrap_or(0);
    DateTimeUtc::from_timestamp(ts.as_second(), nanos).unwrap_or_default()
}

/// Convert a jiff [`Timestamp`] to a sea-orm timezone-aware column value (UTC offset).
pub fn ts_to_entity_tz(ts: Timestamp) -> DateTimeWithTimeZone {
    ts_to_entity(ts).into()
}

/// The current instant as a sea-orm UTC column value — the sanctioned `now()` for
/// entity writes.
pub fn now_entity() -> DateTimeUtc {
    ts_to_entity(Timestamp::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_losslessly() {
        let ts: Timestamp = "2026-07-10T12:34:56.123456789Z".parse().expect("parses");
        assert_eq!(entity_to_ts(ts_to_entity(ts)), ts);
    }

    #[test]
    fn tz_round_trip_preserves_instant() {
        let ts: Timestamp = "2026-07-10T12:34:56.5Z".parse().expect("parses");
        assert_eq!(entity_tz_to_ts(ts_to_entity_tz(ts)), ts);
    }

    #[test]
    fn now_is_representable_both_ways() {
        let now = now_entity();
        assert!(entity_to_ts(now).as_second() > 0);
    }
}
