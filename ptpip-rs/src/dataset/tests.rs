use super::*;

fn sample_device_info() -> DeviceInfo {
    DeviceInfo {
        standard_version: 100,
        vendor_extension_id: 0x11,
        vendor_extension_version: 100,
        vendor_extension_desc: "Sony PTP Extensions".into(),
        functional_mode: 0,
        operations_supported: vec![0x1001, 0x1002, 0x1009, 0x101B],
        events_supported: vec![0x4002],
        device_properties_supported: vec![],
        capture_formats: vec![0x3801],
        image_formats: vec![0x3801, 0xB101],
        manufacturer: "Sony Corporation".into(),
        model: "ILCE-7M4".into(),
        device_version: "2.00".into(),
        serial_number: "0123456789".into(),
    }
}

fn sample_object_info() -> ObjectInfo {
    ObjectInfo {
        storage_id: StorageId(0x0001_0001),
        format: ObjectFormat(0x3801),
        protection_status: 0,
        compressed_size: 4096,
        thumb_format: 0x3808,
        thumb_compressed_size: 512,
        thumb_pix_width: 160,
        thumb_pix_height: 120,
        image_pix_width: 6000,
        image_pix_height: 4000,
        image_bit_depth: 24,
        parent: ObjectHandle(0x10),
        association_type: 0,
        association_desc: 0,
        sequence_number: 0,
        filename: "DSC00001.JPG".into(),
        capture_date: "20260412T153012".into(),
        modification_date: "20260412T153012".into(),
        keywords: String::new(),
    }
}

#[test]
fn device_info_round_trips() {
    let info = sample_device_info();
    let bytes = info.encode().unwrap();
    assert_eq!(DeviceInfo::decode(&bytes).unwrap(), info);
}

#[test]
fn device_info_tolerates_vendor_trailing_fields() {
    let info = sample_device_info();
    let mut bytes = info.encode().unwrap();
    bytes.extend_from_slice(&[0xDE, 0xAD]);
    assert_eq!(DeviceInfo::decode(&bytes).unwrap(), info);
}

#[test]
fn device_info_supports_reads_the_operation_list() {
    let info = sample_device_info();
    assert!(info.supports(OperationCode::GET_PARTIAL_OBJECT));
    assert!(!info.supports(OperationCode::GET_OBJECT_HANDLES));
}

#[test]
fn object_info_round_trips() {
    let info = sample_object_info();
    let bytes = info.encode().unwrap();
    assert_eq!(ObjectInfo::decode(&bytes).unwrap(), info);
}

#[test]
fn truncated_object_info_is_an_error() {
    let bytes = sample_object_info().encode().unwrap();
    assert!(ObjectInfo::decode(&bytes[..bytes.len() - 3]).is_err());
}

#[test]
fn known_size_respects_the_four_gib_sentinel() {
    let mut info = sample_object_info();
    assert_eq!(info.known_size(), Some(4096));
    info.compressed_size = ObjectInfo::SIZE_UNKNOWN;
    assert_eq!(info.known_size(), None);
}

#[test]
fn ptp_datetime_parses_every_standard_form() {
    let plain = PtpDateTime::parse("20260412T153012").unwrap();
    assert_eq!(
        plain.civil,
        jiff::civil::date(2026, 4, 12).at(15, 30, 12, 0)
    );
    assert_eq!(plain.offset, None);

    let tenths = PtpDateTime::parse("20260412T153012.7").unwrap();
    assert_eq!(tenths.civil.subsec_nanosecond(), 700_000_000);

    let utc = PtpDateTime::parse("20260412T153012Z").unwrap();
    assert_eq!(utc.offset, Some(Offset::UTC));

    let east = PtpDateTime::parse("20260412T153012.5+0930").unwrap();
    assert_eq!(east.offset.unwrap().seconds(), 9 * 3600 + 30 * 60);

    let west = PtpDateTime::parse("20260412T153012-0500").unwrap();
    assert_eq!(west.offset.unwrap().seconds(), -5 * 3600);
}

#[test]
fn ptp_datetime_rejects_malformed_strings() {
    for raw in [
        "",
        "20260412",
        "20261312T000000",
        "20260412X153012",
        "20260412T256000",
        "20260412T153012+05",
        "20260412T153012Q",
    ] {
        assert_eq!(PtpDateTime::parse(raw), None, "{raw}");
    }
}

#[test]
fn ptp_datetime_orders_chronologically() {
    let early = PtpDateTime::parse("20260412T153012").unwrap();
    let late = PtpDateTime::parse("20260412T153013").unwrap();
    assert!(early < late);
}
