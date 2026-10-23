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
/// [`extract_exif_from_bytes`] rather than constructing an [`ExifExtract`] by hand. That distinction is
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
    let extracted =
        extract_exif_from_bytes(&jpeg_with_date_time_original(b"2019:03:04 05:06:07\0"));

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
    let extracted =
        extract_exif_from_bytes(&jpeg_with_date_time_original(b"    :  :     :  :  \0"));
    assert_eq!(extracted.date_time_original, None);
}

/// A Live Photo's still carries its pairing UUID outside the EXIF segment, after the marker
/// key. The extractor must rewind the reader it has just consumed for the container parse,
/// read to the end, and lowercase what it finds.
#[test]
fn a_live_photo_content_identifier_is_read_after_the_exif_parse_rewinds() {
    let mut jpeg = jpeg_with_date_time_original(b"2019:03:04 05:06:07\0");
    jpeg.extend_from_slice(b"com.apple.quicktime.content.identifier\0");
    jpeg.extend_from_slice(b"550E8400-E29B-41D4-A716-446655440000 and some trailing text");

    let extracted = extract_exif_from_reader(&mut Cursor::new(jpeg));

    assert_eq!(
        extracted.content_identifier.as_deref(),
        Some("550e8400-e29b-41d4-a716-446655440000")
    );
    assert!(extracted.date_time_original.is_some());
}

#[test]
fn an_exif_container_without_the_marker_has_no_content_identifier() {
    let extracted =
        extract_exif_from_bytes(&jpeg_with_date_time_original(b"2019:03:04 05:06:07\0"));
    assert_eq!(extracted.content_identifier, None);
}

#[test]
fn bytes_that_are_not_an_exif_container_extract_to_nothing() {
    assert_eq!(
        extract_exif_from_bytes(b"not an image"),
        ExifExtract::default()
    );
}
