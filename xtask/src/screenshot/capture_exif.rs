//! A minimal EXIF (TIFF) block carrying only a capture date.
//!
//! Seed photos are re-encoded without their original metadata — camera, GPS and editing history
//! have no place in a screenshot fixture — and get exactly the tags Photos needs to date them:
//! `DateTimeOriginal` and `DateTimeDigitized`, plus `Orientation = 1` because the pixels are
//! already rotated upright. Hand-built because the layout is fixed and tiny; a general EXIF
//! writer would be a dependency for ~40 bytes of structure.

use jiff::civil::DateTime;

const TAG_ORIENTATION: u16 = 0x0112;
const TAG_EXIF_IFD: u16 = 0x8769;
const TAG_EXIF_VERSION: u16 = 0x9000;
const TAG_DATE_TIME_ORIGINAL: u16 = 0x9003;
const TAG_DATE_TIME_DIGITIZED: u16 = 0x9004;

const TYPE_ASCII: u16 = 2;
const TYPE_SHORT: u16 = 3;
const TYPE_LONG: u16 = 4;
const TYPE_UNDEFINED: u16 = 7;

/// `YYYY:MM:DD HH:MM:SS` plus its NUL terminator.
const DATE_LEN: u32 = 20;

/// Raw TIFF bytes (big-endian) for the JPEG APP1 segment; the encoder adds the `Exif\0\0` prefix.
pub(crate) fn capture_date_block(captured_at: DateTime) -> Vec<u8> {
    let date = captured_at.strftime("%Y:%m:%d %H:%M:%S").to_string();
    debug_assert_eq!(date.len() + 1, DATE_LEN as usize);

    // Layout: header (8) | IFD0: 2 entries (30) | Exif IFD: 3 entries (42) | two date strings.
    let ifd0_offset: u32 = 8;
    let exif_ifd_offset = ifd0_offset + ifd_len(2);
    let original_offset = exif_ifd_offset + ifd_len(3);
    let digitized_offset = original_offset + DATE_LEN;

    let mut out = Vec::with_capacity((digitized_offset + DATE_LEN) as usize);
    out.extend_from_slice(b"MM");
    out.extend_from_slice(&42u16.to_be_bytes());
    out.extend_from_slice(&ifd0_offset.to_be_bytes());

    // Entries within an IFD must be sorted by tag.
    out.extend_from_slice(&2u16.to_be_bytes());
    entry(&mut out, TAG_ORIENTATION, TYPE_SHORT, 1, [0, 1, 0, 0]);
    entry(
        &mut out,
        TAG_EXIF_IFD,
        TYPE_LONG,
        1,
        exif_ifd_offset.to_be_bytes(),
    );
    out.extend_from_slice(&0u32.to_be_bytes());

    out.extend_from_slice(&3u16.to_be_bytes());
    entry(&mut out, TAG_EXIF_VERSION, TYPE_UNDEFINED, 4, *b"0232");
    entry(
        &mut out,
        TAG_DATE_TIME_ORIGINAL,
        TYPE_ASCII,
        DATE_LEN,
        original_offset.to_be_bytes(),
    );
    entry(
        &mut out,
        TAG_DATE_TIME_DIGITIZED,
        TYPE_ASCII,
        DATE_LEN,
        digitized_offset.to_be_bytes(),
    );
    out.extend_from_slice(&0u32.to_be_bytes());

    for _ in 0..2 {
        out.extend_from_slice(date.as_bytes());
        out.push(0);
    }
    out
}

/// Byte length of an IFD with `entries` entries: count + 12 bytes each + next-IFD pointer.
const fn ifd_len(entries: u32) -> u32 {
    2 + 12 * entries + 4
}

fn entry(out: &mut Vec<u8>, tag: u16, kind: u16, count: u32, value: [u8; 4]) {
    out.extend_from_slice(&tag.to_be_bytes());
    out.extend_from_slice(&kind.to_be_bytes());
    out.extend_from_slice(&count.to_be_bytes());
    out.extend_from_slice(&value);
}

#[cfg(test)]
mod tests {
    use exif::{In, Tag, Value};

    use super::*;

    fn read(block: Vec<u8>) -> exif::Exif {
        exif::Reader::new()
            .read_raw(block)
            .expect("well-formed TIFF")
    }

    #[test]
    fn round_trips_the_capture_date_through_a_standard_reader() {
        let block = capture_date_block(jiff::civil::date(2026, 9, 20).at(17, 40, 5, 0));
        let exif = read(block);
        for tag in [Tag::DateTimeOriginal, Tag::DateTimeDigitized] {
            let field = exif.get_field(tag, In::PRIMARY).expect("date tag present");
            assert_eq!(field.display_value().to_string(), "2026-09-20 17:40:05");
        }
    }

    #[test]
    fn declares_upright_orientation_and_nothing_identifying() {
        let exif = read(capture_date_block(
            jiff::civil::date(2026, 1, 2).at(3, 4, 5, 0),
        ));
        let orientation = exif.get_field(Tag::Orientation, In::PRIMARY).unwrap();
        assert!(matches!(orientation.value, Value::Short(ref v) if v == &[1]));
        let tags: Vec<Tag> = exif.fields().map(|f| f.tag).collect();
        // The reader resolves the Exif IFD pointer rather than listing it as a field.
        assert_eq!(
            tags,
            [
                Tag::Orientation,
                Tag::ExifVersion,
                Tag::DateTimeOriginal,
                Tag::DateTimeDigitized
            ]
        );
    }
}
