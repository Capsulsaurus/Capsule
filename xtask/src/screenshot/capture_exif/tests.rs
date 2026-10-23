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
