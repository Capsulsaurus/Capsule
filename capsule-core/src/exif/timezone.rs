use jiff::tz::{Offset, TimeZone};

use crate::domain::CaptureTzSource;
use crate::exif::ExifExtract;

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
        .and_then(|dt| dt.to_zoned(TimeZone::UTC).ok())
        .map(|z| z.timestamp().as_second());

    // Case 1: OffsetTimeOriginal present
    if let (Some(dt), Some(offset_str)) =
        (extract.date_time_original, &extract.offset_time_original)
        && let Some(offset) = parse_offset(offset_str)
    {
        // A fixed offset is never ambiguous, so this either resolves or the datetime
        // itself is out of range.
        let local = dt.to_zoned(TimeZone::fixed(offset)).ok();
        let capture_utc = local.map(|t| t.timestamp().as_second());
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
            capture_utc: None, // Applying the IANA tz is deferred; leave as None
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

fn parse_offset(s: &str) -> Option<Offset> {
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
    Offset::from_seconds(total_secs).ok()
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
mod tests;
