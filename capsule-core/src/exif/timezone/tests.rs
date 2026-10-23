use jiff::civil;

use super::*;
use crate::domain::CaptureTzSource;
use crate::exif::ExifExtract;

fn extract_with_offset(dt: &str, offset: &str) -> ExifExtract {
    ExifExtract {
        date_time_original: civil::DateTime::strptime("%Y:%m:%d %H:%M:%S", dt).ok(),
        offset_time_original: Some(offset.to_string()),
        gps_lat: None,
        gps_lon: None,
        make: None,
        model: None,
        width: None,
        height: None,
        duration_ms: None,
        content_identifier: None,
    }
}

fn extract_with_gps(dt: &str, lat: f64, lon: f64) -> ExifExtract {
    ExifExtract {
        date_time_original: civil::DateTime::strptime("%Y:%m:%d %H:%M:%S", dt).ok(),
        offset_time_original: None,
        gps_lat: Some(lat),
        gps_lon: Some(lon),
        make: None,
        model: None,
        width: None,
        height: None,
        duration_ms: None,
        content_identifier: None,
    }
}

fn extract_floating(dt: &str) -> ExifExtract {
    ExifExtract {
        date_time_original: civil::DateTime::strptime("%Y:%m:%d %H:%M:%S", dt).ok(),
        offset_time_original: None,
        gps_lat: None,
        gps_lon: None,
        make: None,
        model: None,
        width: None,
        height: None,
        duration_ms: None,
        content_identifier: None,
    }
}

#[test]
fn test_case1_offset_exif() {
    let extract = extract_with_offset("2024:07:15 10:30:00", "+09:00");
    let result = resolve_timezone(&extract);
    assert_eq!(result.capture_tz_source, Some(CaptureTzSource::OffsetExif));
    assert_eq!(result.capture_tz, Some("+09:00".to_string()));
    assert!(result.capture_utc.is_some());
    // 10:30 +09:00 = 01:30 UTC
    assert_eq!(
        result.capture_utc.unwrap(),
        result.capture_timestamp.unwrap() - 9 * 3600
    );
    assert!(result.tz_db_version.is_none());
}

#[test]
fn test_case1_negative_offset() {
    let extract = extract_with_offset("2024:07:15 10:30:00", "-05:00");
    let result = resolve_timezone(&extract);
    assert_eq!(result.capture_tz_source, Some(CaptureTzSource::OffsetExif));
    // 10:30 -05:00 = 15:30 UTC
    assert_eq!(
        result.capture_utc.unwrap(),
        result.capture_timestamp.unwrap() + 5 * 3600
    );
}

#[test]
fn test_case2_gps_lookup() {
    // New York City coordinates
    let extract = extract_with_gps("2024:07:15 10:30:00", 40.7128, -74.0060);
    let result = resolve_timezone(&extract);
    assert_eq!(result.capture_tz_source, Some(CaptureTzSource::GpsLookup));
    assert!(result.capture_tz.is_some());
    // Should return America/New_York or similar
    let tz = result.capture_tz.unwrap();
    assert!(
        tz.contains("New_York") || tz.contains("America"),
        "Expected NYC timezone, got: {tz}"
    );
    assert!(result.tz_db_version.is_some());
}

#[test]
fn test_case3_floating() {
    let extract = extract_floating("2024:07:15 10:30:00");
    let result = resolve_timezone(&extract);
    assert_eq!(result.capture_tz_source, Some(CaptureTzSource::Floating));
    assert!(result.capture_utc.is_none());
    assert!(result.capture_tz.is_none());
    assert!(result.tz_db_version.is_none());
    assert!(result.capture_timestamp.is_some());
}

#[test]
fn test_no_datetime_at_all() {
    let extract = ExifExtract {
        date_time_original: None,
        offset_time_original: None,
        gps_lat: None,
        gps_lon: None,
        make: None,
        model: None,
        width: None,
        height: None,
        duration_ms: None,
        content_identifier: None,
    };
    let result = resolve_timezone(&extract);
    assert_eq!(result.capture_tz_source, Some(CaptureTzSource::Floating));
    assert!(result.capture_timestamp.is_none());
    assert!(result.capture_utc.is_none());
}

#[test]
fn test_parse_offset_positive() {
    let offset = parse_offset("+09:00").unwrap();
    assert_eq!(offset.seconds(), 9 * 3600);
}

#[test]
fn test_parse_offset_negative() {
    let offset = parse_offset("-05:30").unwrap();
    assert_eq!(offset.seconds(), -(5 * 3600 + 30 * 60));
}

#[test]
fn test_parse_offset_invalid() {
    assert!(parse_offset("bad").is_none());
    assert!(parse_offset("").is_none());
    assert!(parse_offset("+09").is_none()); // missing minutes
}
