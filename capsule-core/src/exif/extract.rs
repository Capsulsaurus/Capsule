use chrono::NaiveDateTime;
use exif::{In, Reader, Tag, Value};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ExifExtract {
    pub date_time_original: Option<NaiveDateTime>,
    pub offset_time_original: Option<String>, // e.g. "+09:00"
    pub gps_lat: Option<f64>,
    pub gps_lon: Option<f64>,
    pub make: Option<String>,
    pub model: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>, // For video; not from EXIF — always None from this extractor
    pub content_identifier: Option<String>, // Apple Live Photo UUID
}

pub fn extract_exif(path: &Path) -> Result<ExifExtract, Box<dyn std::error::Error + Send + Sync>> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);

    let exif = match Reader::new().read_from_container(&mut reader) {
        Ok(e) => e,
        Err(_) => {
            // Not a valid EXIF container — return all-None result
            return Ok(ExifExtract {
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
            });
        }
    };

    // DateTimeOriginal
    let date_time_original = exif
        .get_field(Tag::DateTimeOriginal, In::PRIMARY)
        .and_then(|field| {
            let dt_str = field.display_value().to_string();
            NaiveDateTime::parse_from_str(&dt_str, "%Y:%m:%d %H:%M:%S").ok()
        });

    // OffsetTimeOriginal
    let offset_time_original = exif
        .get_field(Tag::OffsetTimeOriginal, In::PRIMARY)
        .map(|field| field.display_value().to_string())
        .map(|s| {
            // kamadak-exif may add surrounding quotes; strip them
            let s = s.trim();
            if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
                s[1..s.len() - 1].to_string()
            } else {
                s.to_string()
            }
        });

    // GPS Latitude
    let gps_lat_decimal = exif
        .get_field(Tag::GPSLatitude, In::PRIMARY)
        .and_then(|field| {
            if let Value::Rational(ref rationals) = field.value {
                if rationals.len() >= 3 {
                    let deg = rationals[0].to_f64();
                    let min = rationals[1].to_f64();
                    let sec = rationals[2].to_f64();
                    Some(deg + min / 60.0 + sec / 3600.0)
                } else {
                    None
                }
            } else {
                None
            }
        });

    let gps_lat = gps_lat_decimal.map(|decimal| {
        let ref_str = exif
            .get_field(Tag::GPSLatitudeRef, In::PRIMARY)
            .map(|f| f.display_value().to_string())
            .unwrap_or_default();
        if ref_str.to_uppercase().contains('S') {
            -decimal
        } else {
            decimal
        }
    });

    // GPS Longitude
    let gps_lon_decimal = exif
        .get_field(Tag::GPSLongitude, In::PRIMARY)
        .and_then(|field| {
            if let Value::Rational(ref rationals) = field.value {
                if rationals.len() >= 3 {
                    let deg = rationals[0].to_f64();
                    let min = rationals[1].to_f64();
                    let sec = rationals[2].to_f64();
                    Some(deg + min / 60.0 + sec / 3600.0)
                } else {
                    None
                }
            } else {
                None
            }
        });

    let gps_lon = gps_lon_decimal.map(|decimal| {
        let ref_str = exif
            .get_field(Tag::GPSLongitudeRef, In::PRIMARY)
            .map(|f| f.display_value().to_string())
            .unwrap_or_default();
        if ref_str.to_uppercase().contains('W') {
            -decimal
        } else {
            decimal
        }
    });

    // Make
    let make = exif
        .get_field(Tag::Make, In::PRIMARY)
        .map(|field| field.display_value().to_string())
        .map(|s| strip_quotes(&s));

    // Model
    let model = exif
        .get_field(Tag::Model, In::PRIMARY)
        .map(|field| field.display_value().to_string())
        .map(|s| strip_quotes(&s));

    // Width (PixelXDimension)
    let width = exif
        .get_field(Tag::PixelXDimension, In::PRIMARY)
        .and_then(|field| match field.value {
            Value::Long(ref v) if !v.is_empty() => Some(v[0]),
            Value::Short(ref v) if !v.is_empty() => Some(u32::from(v[0])),
            _ => None,
        });

    // Height (PixelYDimension)
    let height = exif
        .get_field(Tag::PixelYDimension, In::PRIMARY)
        .and_then(|field| match field.value {
            Value::Long(ref v) if !v.is_empty() => Some(v[0]),
            Value::Short(ref v) if !v.is_empty() => Some(u32::from(v[0])),
            _ => None,
        });

    // content_identifier — Apple Live Photo UUID (byte search)
    let content_identifier = extract_content_identifier(path);

    Ok(ExifExtract {
        date_time_original,
        offset_time_original,
        gps_lat,
        gps_lon,
        make,
        model,
        width,
        height,
        duration_ms: None,
        content_identifier,
    })
}

fn strip_quotes(s: &str) -> String {
    let s = s.trim();
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

fn extract_content_identifier(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let marker = b"com.apple.quicktime.content.identifier";
    let pos = bytes.windows(marker.len()).position(|w| w == marker)?;
    // After the marker, find a UUID-like string (36 chars: 8-4-4-4-12 hex with hyphens)
    let after = &bytes[pos + marker.len()..];
    // Scan for UUID pattern in the next 200 bytes
    let search_region = &after[..after.len().min(200)];
    let s = std::str::from_utf8(search_region).ok()?;
    find_uuid_in_str(s)
}

fn find_uuid_in_str(s: &str) -> Option<String> {
    // Simple scan: find 8-4-4-4-12 hex pattern
    for start in 0..s.len().saturating_sub(36) {
        let candidate = &s[start..start + 36];
        if is_uuid_format(candidate) {
            return Some(candidate.to_ascii_lowercase());
        }
    }
    None
}

fn is_uuid_format(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    let expected_hyphens = [8, 13, 18, 23];
    for (i, &b) in bytes.iter().enumerate() {
        if expected_hyphens.contains(&i) {
            if b != b'-' {
                return false;
            }
        } else if !b.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_uuid_format_valid() {
        assert!(is_uuid_format("550e8400-e29b-41d4-a716-446655440000"));
        assert!(is_uuid_format("6ba7b810-9dad-11d1-80b4-00c04fd430c8"));
    }

    #[test]
    fn test_is_uuid_format_invalid() {
        assert!(!is_uuid_format("not-a-uuid"));
        assert!(!is_uuid_format("550e8400-e29b-41d4-a716-44665544000")); // 35 chars
        assert!(!is_uuid_format("550e8400-e29b-41d4-a716-4466554400000")); // 37 chars
        assert!(!is_uuid_format("550e8400xe29b-41d4-a716-446655440000")); // wrong hyphen pos
        assert!(!is_uuid_format("550e8400-e29b-41d4-a716-44665544zzzz")); // non-hex chars
    }

    #[test]
    fn test_find_uuid_in_str() {
        let s = "some prefix 550e8400-e29b-41d4-a716-446655440000 suffix";
        assert_eq!(
            find_uuid_in_str(s),
            Some("550e8400-e29b-41d4-a716-446655440000".to_string())
        );
    }

    #[test]
    fn test_find_uuid_uppercase_lowercased() {
        let s = "prefix 550E8400-E29B-41D4-A716-446655440000 suffix";
        assert_eq!(
            find_uuid_in_str(s),
            Some("550e8400-e29b-41d4-a716-446655440000".to_string())
        );
    }

    #[test]
    fn test_extract_exif_nonexistent_file_returns_io_error() {
        let result = extract_exif(Path::new("/nonexistent/path/to/file.jpg"));
        assert!(result.is_err());
    }
}
