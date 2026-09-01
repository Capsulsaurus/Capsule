use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use exif::{DateTime as ExifDateTime, In, Reader, Tag, Value};
use jiff::civil;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ExifExtract {
    pub date_time_original: Option<civil::DateTime>,
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

    let Ok(exif) = Reader::new().read_from_container(&mut reader) else {
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
    };

    // DateTimeOriginal.
    //
    // Parsed from the **raw ASCII value**, never from `display_value()`. The EXIF wire format is
    // `YYYY:MM:DD HH:MM:SS` (colons throughout), but kamadak-exif's `Display` deliberately
    // reformats it to `YYYY-MM-DD HH:MM:SS` (`tiff.rs`'s `impl fmt::Display for DateTime`). This
    // code previously applied the wire pattern `%Y:%m:%d %H:%M:%S` to the display string, so the
    // parse could never succeed and `date_time_original` was **always** `None` for well-formed
    // EXIF — which silently made every import fall back to `Timestamp::now()`, stamping and
    // bucketing photos by import time instead of capture time.
    //
    // `ExifDateTime::from_ascii` is the crate's own parser for the wire format. It also rejects
    // the all-blank value the spec allows, which a `strptime` on the raw bytes would not.
    let date_time_original = exif
        .get_field(Tag::DateTimeOriginal, In::PRIMARY)
        .and_then(|field| match &field.value {
            Value::Ascii(values) => values.first().map(Vec::as_slice),
            _ => None,
        })
        .and_then(|raw| ExifDateTime::from_ascii(raw).ok())
        .and_then(|dt| {
            civil::DateTime::new(
                i16::try_from(dt.year).ok()?,
                i8::try_from(dt.month).ok()?,
                i8::try_from(dt.day).ok()?,
                i8::try_from(dt.hour).ok()?,
                i8::try_from(dt.minute).ok()?,
                i8::try_from(dt.second).ok()?,
                0,
            )
            .ok()
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

    /// A JPEG carrying nothing but a valid EXIF APP1 segment with `DateTimeOriginal`.
    ///
    /// Hand-built rather than committed, and deliberately routed through the real
    /// [`extract_exif`] rather than constructing an [`ExifExtract`] by hand. That distinction is
    /// the whole point of this test: the parsing bug it guards survived because
    /// `extract.rs`'s tests never fed real EXIF through the extractor, and `timezone.rs`'s tests
    /// built `ExifExtract` values directly using the same wire format the extractor expected —
    /// so both sides agreed on a spelling the EXIF crate never produces, and nothing compared
    /// them against reality.
    ///
    /// The bytes are a minimal but structurally valid container: SOI, one APP1 holding
    /// `Exif\0\0` plus a big-endian TIFF header, IFD0 with only an Exif-SubIFD pointer, the
    /// SubIFD with only `DateTimeOriginal` (tag `0x9003`), the ASCII value, then EOI. No image
    /// data — `Reader::read_from_container` only needs the APP1.
    fn jpeg_with_date_time_original(value: &[u8; 20]) -> Vec<u8> {
        // Offsets are relative to the start of the TIFF header.
        const IFD0: u32 = 8; // straight after the 8-byte TIFF header
        const SUB_IFD: u32 = 26; // IFD0 is 2 + 12 + 4 = 18 bytes
        const ASCII: u32 = 44; // the SubIFD is another 18

        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"MM"); // big-endian
        tiff.extend_from_slice(&0x002Au16.to_be_bytes());
        tiff.extend_from_slice(&IFD0.to_be_bytes());

        tiff.extend_from_slice(&1u16.to_be_bytes()); // IFD0: one entry
        tiff.extend_from_slice(&0x8769u16.to_be_bytes()); // ExifIFDPointer
        tiff.extend_from_slice(&4u16.to_be_bytes()); // LONG
        tiff.extend_from_slice(&1u32.to_be_bytes());
        tiff.extend_from_slice(&SUB_IFD.to_be_bytes());
        tiff.extend_from_slice(&0u32.to_be_bytes()); // no next IFD

        tiff.extend_from_slice(&1u16.to_be_bytes()); // SubIFD: one entry
        tiff.extend_from_slice(&0x9003u16.to_be_bytes()); // DateTimeOriginal
        tiff.extend_from_slice(&2u16.to_be_bytes()); // ASCII
        tiff.extend_from_slice(&20u32.to_be_bytes());
        tiff.extend_from_slice(&ASCII.to_be_bytes());
        tiff.extend_from_slice(&0u32.to_be_bytes());

        tiff.extend_from_slice(value);

        let mut app1 = Vec::from(*b"Exif\0\0");
        app1.extend_from_slice(&tiff);

        let mut jpeg = vec![0xFF, 0xD8]; // SOI
        jpeg.extend_from_slice(&[0xFF, 0xE1]);
        // Segment length counts itself but not the marker.
        let len = u16::try_from(app1.len() + 2).expect("fixture segment fits in a u16");
        jpeg.extend_from_slice(&len.to_be_bytes());
        jpeg.extend_from_slice(&app1);
        jpeg.extend_from_slice(&[0xFF, 0xD9]); // EOI
        jpeg
    }

    /// The regression guard: a real `DateTimeOriginal` reaches the caller.
    ///
    /// Before the fix this asserted `None`, because the extractor parsed
    /// `display_value()` — which kamadak-exif renders with **dashes** — using the EXIF wire
    /// pattern, which uses **colons**. The consequence was not a missing field in isolation:
    /// `capture_utc` went unset, and `import_asset_with` fell back to `Timestamp::now()`, so every
    /// imported photo was stamped and date-bucketed by when it was imported rather than when it
    /// was taken.
    #[test]
    fn date_time_original_is_parsed_from_a_real_exif_segment() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("fixture.jpg");
        std::fs::write(
            &path,
            jpeg_with_date_time_original(b"2019:03:04 05:06:07\0"),
        )
        .expect("write fixture");

        let extracted = extract_exif(&path).expect("the fixture is a readable EXIF container");

        assert_eq!(
            extracted.date_time_original,
            Some(civil::DateTime::new(2019, 3, 4, 5, 6, 7, 0).expect("valid civil datetime")),
            "DateTimeOriginal must survive extraction; a None here silently reroutes import to \
             Timestamp::now() and buckets photos by import date"
        );
    }

    /// The spec permits an all-blank `DateTimeOriginal`, and it must read as absent rather than
    /// as some epoch-adjacent date. `ExifDateTime::from_ascii` rejects it explicitly; a plain
    /// `strptime` over the raw bytes would not.
    #[test]
    fn a_blank_date_time_original_is_absent_not_a_bogus_date() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("blank.jpg");
        std::fs::write(
            &path,
            jpeg_with_date_time_original(b"    :  :     :  :  \0"),
        )
        .expect("write fixture");

        let extracted = extract_exif(&path).expect("the fixture is a readable EXIF container");
        assert_eq!(extracted.date_time_original, None);
    }

    #[test]
    fn test_extract_exif_nonexistent_file_returns_io_error() {
        let result = extract_exif(Path::new("/nonexistent/path/to/file.jpg"));
        assert!(result.is_err());
    }
}
