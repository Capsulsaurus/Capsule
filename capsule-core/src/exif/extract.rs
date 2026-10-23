use std::io::{BufRead, Cursor, Read, Seek, SeekFrom};

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

/// Extract the EXIF fields Capsule indexes from a file's bytes.
///
/// The in-memory form of [`extract_exif_from_reader`]; see it for the contract.
pub fn extract_exif_from_bytes(bytes: &[u8]) -> ExifExtract {
    extract_exif_from_reader(&mut Cursor::new(bytes))
}

/// Extract the EXIF fields Capsule indexes from a seekable reader over a file's contents.
///
/// Generic over the reader so opening the file stays the caller's business (`capsule-library`
/// hands a buffered file handle here). Contents that are not an EXIF container yield an
/// all-`None` extract rather than an error — a file without EXIF is ordinary, not a failure — and
/// are read only as far as the container sniff needs, so a large non-EXIF original is never held
/// whole. Only an EXIF container is rewound and read to its end, for the Live Photo content
/// identifier; a read failure there leaves that one field `None`.
pub fn extract_exif_from_reader<R: BufRead + Seek>(reader: &mut R) -> ExifExtract {
    let Ok(exif) = Reader::new().read_from_container(reader) else {
        // Not a valid EXIF container — return all-None result
        return ExifExtract {
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
    let content_identifier =
        read_whole(reader).and_then(|bytes| extract_content_identifier(&bytes));

    ExifExtract {
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
    }
}

fn strip_quotes(s: &str) -> String {
    let s = s.trim();
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// The reader's whole contents from the start, or `None` if rewinding or reading fails.
fn read_whole<R: Read + Seek>(reader: &mut R) -> Option<Vec<u8>> {
    reader.seek(SeekFrom::Start(0)).ok()?;
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

fn extract_content_identifier(bytes: &[u8]) -> Option<String> {
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
mod tests;
