use crate::domain::CaptureTzSource;
use crate::exif::ExifExtract;
use chrono::{FixedOffset, TimeZone};

#[derive(Debug, Clone, PartialEq)]
pub struct TimezoneResolution {
    pub capture_timestamp: Option<i64>, // local wall-clock as Unix epoch (no TZ applied)
    pub capture_utc: Option<i64>,       // UTC timestamp; None when floating
    pub capture_tz: Option<String>,     // IANA name or "+HH:MM"; None when floating
    pub capture_tz_source: Option<CaptureTzSource>,
    pub tz_db_version: Option<String>,
}

pub fn resolve_timezone(extract: &ExifExtract) -> TimezoneResolution {
    let capture_timestamp = extract
        .date_time_original
        .map(|dt| dt.and_utc().timestamp());

    // Case 1: OffsetTimeOriginal present
    if let (Some(dt), Some(offset_str)) =
        (extract.date_time_original, &extract.offset_time_original)
        && let Some(offset) = parse_offset(offset_str)
    {
        let local = offset.from_local_datetime(&dt).single();
        let capture_utc = local.map(|t| t.timestamp());
        return TimezoneResolution {
            capture_timestamp,
            capture_utc,
            capture_tz: Some(offset_str.clone()),
            capture_tz_source: Some(CaptureTzSource::OffsetExif),
            tz_db_version: None,
        };
    }

    // Case 2: GPS coordinates present → offline timezone lookup
    if let (Some(lat), Some(lon)) = (extract.gps_lat, extract.gps_lon)
        && let Some((tz_name, tz_db_ver)) = lookup_timezone(lat, lon)
    {
        return TimezoneResolution {
            capture_timestamp,
            capture_utc: None, // Would need chrono-tz to apply IANA tz; leave as None
            capture_tz: Some(tz_name),
            capture_tz_source: Some(CaptureTzSource::GpsLookup),
            tz_db_version: Some(tz_db_ver),
        };
    }

    // Case 3: Floating
    TimezoneResolution {
        capture_timestamp,
        capture_utc: None,
        capture_tz: None,
        capture_tz_source: Some(CaptureTzSource::Floating),
        tz_db_version: None,
    }
}

fn parse_offset(s: &str) -> Option<FixedOffset> {
    // Parse "+HH:MM" or "-HH:MM"
    let s = s.trim();
    if s.len() < 6 {
        return None;
    }
    let sign: i32 = if s.starts_with('-') { -1 } else { 1 };
    let parts: Vec<&str> = s[1..].split(':').collect();
    if parts.len() != 2 {
        return None;
    }
    let hours: i32 = parts[0].parse().ok()?;
    let minutes: i32 = parts[1].parse().ok()?;
    let total_secs = sign * (hours * 3600 + minutes * 60);
    FixedOffset::east_opt(total_secs)
}

fn lookup_timezone(lat: f64, lon: f64) -> Option<(String, String)> {
    use tzf_rs::DefaultFinder;
    let finder = DefaultFinder::new();
    let tz_name = finder.get_tz_name(lon, lat); // tzf-rs takes (lon, lat)
    if tz_name.is_empty() {
        return None;
    }
    let tz_db_version = finder.data_version().to_string();
    Some((tz_name.to_string(), tz_db_version))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::CaptureTzSource;
    use crate::exif::ExifExtract;
    use chrono::NaiveDateTime;

    fn extract_with_offset(dt: &str, offset: &str) -> ExifExtract {
        ExifExtract {
            date_time_original: NaiveDateTime::parse_from_str(dt, "%Y:%m:%d %H:%M:%S").ok(),
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
            date_time_original: NaiveDateTime::parse_from_str(dt, "%Y:%m:%d %H:%M:%S").ok(),
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
            date_time_original: NaiveDateTime::parse_from_str(dt, "%Y:%m:%d %H:%M:%S").ok(),
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
        assert_eq!(offset.utc_minus_local(), -9 * 3600);
    }

    #[test]
    fn test_parse_offset_negative() {
        let offset = parse_offset("-05:30").unwrap();
        assert_eq!(offset.utc_minus_local(), 5 * 3600 + 30 * 60);
    }

    #[test]
    fn test_parse_offset_invalid() {
        assert!(parse_offset("bad").is_none());
        assert!(parse_offset("").is_none());
        assert!(parse_offset("+09").is_none()); // missing minutes
    }
}
