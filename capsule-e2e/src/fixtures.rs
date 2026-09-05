//! Byte-built fixtures, so the repository carries no binary test asset.

/// A real 8×8 grayscale baseline JPEG carrying an EXIF APP1 segment, built byte by byte.
///
/// The same construction as the CLI's import round trip
/// (`capsule-cli/tests/import_round_trip.rs`), which a test crate cannot import; the EXIF block
/// is a big-endian TIFF structure with three IFDs — IFD0 (make/model + pointers), the Exif
/// SubIFD (`DateTimeOriginal`, `OffsetTimeOriginal`, pixel dimensions) and the GPS IFD — and the
/// image is a genuine baseline JPEG a conformant decoder accepts, which is what lets the media
/// stack produce a derivative for it.
#[must_use]
pub fn synthetic_jpeg() -> Vec<u8> {
    const ASCII: u16 = 2;
    const LONG: u16 = 4;
    const RATIONAL: u16 = 5;

    const MAKE: &[u8] = b"Capsule\0";
    const MODEL: &[u8] = b"Synth\0";
    const DATE_TIME_ORIGINAL: &[u8] = b"2019:03:04 05:06:07\0";
    const OFFSET_TIME_ORIGINAL: &[u8] = b"+00:00\0";

    // Each IFD here holds four entries: 2 count bytes + 4×12 entry bytes + 4 next-IFD bytes.
    const IFD_LEN: u32 = 2 + 4 * 12 + 4;
    const IFD0_AT: u32 = 8;
    const EXIF_IFD_AT: u32 = IFD0_AT + IFD_LEN;
    const GPS_IFD_AT: u32 = EXIF_IFD_AT + IFD_LEN;
    const DATA_AT: u32 = GPS_IFD_AT + IFD_LEN;
    const MAKE_AT: u32 = DATA_AT;
    const MODEL_AT: u32 = MAKE_AT + MAKE.len() as u32;
    const DTO_AT: u32 = MODEL_AT + MODEL.len() as u32;
    const OTO_AT: u32 = DTO_AT + DATE_TIME_ORIGINAL.len() as u32;
    // Rationals are 4-byte quantities; one pad byte keeps them aligned.
    const LAT_AT: u32 = OTO_AT + OFFSET_TIME_ORIGINAL.len() as u32 + 1;
    const LON_AT: u32 = LAT_AT + 24;

    /// One 12-byte IFD entry whose value is an offset into the TIFF block.
    fn at(tag: u16, kind: u16, count: u32, offset: u32) -> Vec<u8> {
        let mut e = Vec::with_capacity(12);
        e.extend_from_slice(&tag.to_be_bytes());
        e.extend_from_slice(&kind.to_be_bytes());
        e.extend_from_slice(&count.to_be_bytes());
        e.extend_from_slice(&offset.to_be_bytes());
        e
    }

    /// One 12-byte IFD entry whose value fits in the 4 inline bytes.
    fn inline(tag: u16, kind: u16, count: u32, value: [u8; 4]) -> Vec<u8> {
        let mut e = Vec::with_capacity(12);
        e.extend_from_slice(&tag.to_be_bytes());
        e.extend_from_slice(&kind.to_be_bytes());
        e.extend_from_slice(&count.to_be_bytes());
        e.extend_from_slice(&value);
        e
    }

    fn rational(numerator: u32, denominator: u32) -> Vec<u8> {
        let mut r = Vec::with_capacity(8);
        r.extend_from_slice(&numerator.to_be_bytes());
        r.extend_from_slice(&denominator.to_be_bytes());
        r
    }

    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"MM");
    tiff.extend_from_slice(&42u16.to_be_bytes());
    tiff.extend_from_slice(&IFD0_AT.to_be_bytes());

    // IFD0: Make, Model, and the pointers to the two sub-IFDs.
    tiff.extend_from_slice(&4u16.to_be_bytes());
    tiff.extend(at(0x010F, ASCII, MAKE.len() as u32, MAKE_AT));
    tiff.extend(at(0x0110, ASCII, MODEL.len() as u32, MODEL_AT));
    tiff.extend(at(0x8769, LONG, 1, EXIF_IFD_AT));
    tiff.extend(at(0x8825, LONG, 1, GPS_IFD_AT));
    tiff.extend_from_slice(&0u32.to_be_bytes());

    // Exif SubIFD: capture time, its UTC offset, and the pixel dimensions.
    tiff.extend_from_slice(&4u16.to_be_bytes());
    tiff.extend(at(0x9003, ASCII, DATE_TIME_ORIGINAL.len() as u32, DTO_AT));
    tiff.extend(at(0x9011, ASCII, OFFSET_TIME_ORIGINAL.len() as u32, OTO_AT));
    tiff.extend(inline(0xA002, LONG, 1, 8u32.to_be_bytes()));
    tiff.extend(inline(0xA003, LONG, 1, 8u32.to_be_bytes()));
    tiff.extend_from_slice(&0u32.to_be_bytes());

    // GPS IFD: 48°51'29.6"N, 2°17'40.2"W.
    tiff.extend_from_slice(&4u16.to_be_bytes());
    tiff.extend(inline(0x0001, ASCII, 2, *b"N\0\0\0"));
    tiff.extend(at(0x0002, RATIONAL, 3, LAT_AT));
    tiff.extend(inline(0x0003, ASCII, 2, *b"W\0\0\0"));
    tiff.extend(at(0x0004, RATIONAL, 3, LON_AT));
    tiff.extend_from_slice(&0u32.to_be_bytes());

    // The out-of-line values, in the order the offsets above declare.
    tiff.extend_from_slice(MAKE);
    tiff.extend_from_slice(MODEL);
    tiff.extend_from_slice(DATE_TIME_ORIGINAL);
    tiff.extend_from_slice(OFFSET_TIME_ORIGINAL);
    tiff.push(0);
    for (numerator, denominator) in [(48, 1), (51, 1), (296, 10), (2, 1), (17, 1), (402, 10)] {
        tiff.extend(rational(numerator, denominator));
    }
    assert_eq!(
        tiff.len() as u32,
        LON_AT + 24,
        "the TIFF block must be exactly as long as its own offsets claim"
    );

    let mut app1 = b"Exif\0\0".to_vec();
    app1.extend_from_slice(&tiff);

    let mut jpeg = vec![0xFF, 0xD8]; // SOI
    jpeg.extend_from_slice(&[0xFF, 0xE1]); // APP1
    jpeg.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
    jpeg.extend_from_slice(&app1);

    // DQT: one flat 8-bit luminance table.
    jpeg.extend_from_slice(&[0xFF, 0xDB]);
    jpeg.extend_from_slice(&(2u16 + 1 + 64).to_be_bytes());
    jpeg.push(0x00);
    jpeg.extend(std::iter::repeat_n(1u8, 64));

    // SOF0: baseline, 8-bit, 8×8, one component with no subsampling.
    jpeg.extend_from_slice(&[0xFF, 0xC0]);
    jpeg.extend_from_slice(&11u16.to_be_bytes());
    jpeg.extend_from_slice(&[0x08]);
    jpeg.extend_from_slice(&8u16.to_be_bytes());
    jpeg.extend_from_slice(&8u16.to_be_bytes());
    jpeg.extend_from_slice(&[0x01, 0x01, 0x11, 0x00]);

    // DHT: a DC and an AC table each holding a single 1-bit code for symbol 0.
    for class_and_id in [0x00u8, 0x10] {
        jpeg.extend_from_slice(&[0xFF, 0xC4]);
        jpeg.extend_from_slice(&(2u16 + 1 + 16 + 1).to_be_bytes());
        jpeg.push(class_and_id);
        jpeg.push(1);
        jpeg.extend(std::iter::repeat_n(0u8, 15));
        jpeg.push(0x00);
    }

    // SOS, then the entropy-coded data: one all-zero block, padded to a byte with 1 bits.
    jpeg.extend_from_slice(&[0xFF, 0xDA]);
    jpeg.extend_from_slice(&8u16.to_be_bytes());
    jpeg.extend_from_slice(&[0x01, 0x01, 0x00, 0x00, 0x3F, 0x00]);
    jpeg.push(0x3F);

    jpeg.extend_from_slice(&[0xFF, 0xD9]); // EOI
    jpeg
}
