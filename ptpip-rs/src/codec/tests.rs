use super::*;

#[test]
fn integers_round_trip_little_endian() {
    let mut w = Writer::new();
    w.u8(0xAB)
        .u16(0x1234)
        .u32(0xDEAD_BEEF)
        .u64(0x0102_0304_0506_0708);
    let bytes = w.into_bytes();
    assert_eq!(&bytes[1..3], &[0x34, 0x12]);
    let mut r = Reader::new(&bytes);
    assert_eq!(r.u8().unwrap(), 0xAB);
    assert_eq!(r.u16().unwrap(), 0x1234);
    assert_eq!(r.u32().unwrap(), 0xDEAD_BEEF);
    assert_eq!(r.u64().unwrap(), 0x0102_0304_0506_0708);
    assert_eq!(r.remaining(), 0);
}

#[test]
fn truncated_integer_is_an_error_naming_the_offset() {
    let mut r = Reader::new(&[1, 2, 3]);
    assert_eq!(r.u16().unwrap(), 0x0201);
    assert_eq!(
        r.u32(),
        Err(DecodeError::Truncated {
            offset: 2,
            needed: 4,
            available: 1
        })
    );
}

#[test]
fn string_counts_units_including_the_nul() {
    let mut w = Writer::new();
    w.string("DSC").unwrap();
    let bytes = w.into_bytes();
    assert_eq!(bytes, vec![4, b'D', 0, b'S', 0, b'C', 0, 0, 0]);
    assert_eq!(Reader::new(&bytes).string().unwrap(), "DSC");
}

#[test]
fn empty_string_is_a_single_zero_byte() {
    let mut w = Writer::new();
    w.string("").unwrap();
    assert_eq!(w.into_bytes(), vec![0]);
    assert_eq!(Reader::new(&[0]).string().unwrap(), "");
}

#[test]
fn non_ascii_string_round_trips() {
    let mut w = Writer::new();
    w.string("café 写真").unwrap();
    let bytes = w.into_bytes();
    assert_eq!(Reader::new(&bytes).string().unwrap(), "café 写真");
}

#[test]
fn string_without_trailing_nul_is_rejected() {
    // Count 1, one unit that is not NUL.
    assert_eq!(
        Reader::new(&[1, b'A', 0]).string(),
        Err(DecodeError::InvalidString { offset: 0 })
    );
}

#[test]
fn overlong_string_refuses_to_encode() {
    let long = "x".repeat(255);
    assert_eq!(
        Writer::new().string(&long).err(),
        Some(DecodeError::StringTooLong { units: 255 })
    );
    assert!(Writer::new().string(&"x".repeat(254)).is_ok());
}

#[test]
fn nul_terminated_utf16_round_trips_and_requires_the_terminator() {
    let mut w = Writer::new();
    w.nul_terminated_utf16("Capsule");
    let bytes = w.into_bytes();
    let mut r = Reader::new(&bytes);
    assert_eq!(r.nul_terminated_utf16().unwrap(), "Capsule");
    assert_eq!(r.remaining(), 0);
    assert!(
        Reader::new(&[b'A', 0, b'B', 0])
            .nul_terminated_utf16()
            .is_err()
    );
}

#[test]
fn arrays_round_trip() {
    let mut w = Writer::new();
    w.u16_array(&[0x1001, 0x1009]).u32_array(&[7, 8, 9]);
    let bytes = w.into_bytes();
    let mut r = Reader::new(&bytes);
    assert_eq!(r.u16_array().unwrap(), vec![0x1001, 0x1009]);
    assert_eq!(r.u32_array().unwrap(), vec![7, 8, 9]);
}

#[test]
fn array_count_beyond_the_input_is_rejected_before_allocating() {
    let mut w = Writer::new();
    w.u32(u32::MAX);
    let bytes = w.into_bytes();
    assert_eq!(
        Reader::new(&bytes).u32_array(),
        Err(DecodeError::ArrayTooLong {
            offset: 0,
            count: u32::MAX
        })
    );
}
