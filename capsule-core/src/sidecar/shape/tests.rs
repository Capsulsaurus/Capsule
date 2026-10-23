use super::*;

fn encode(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    ciborium::ser::into_writer(value, &mut out).unwrap();
    out
}

fn text(s: &str) -> Value {
    Value::Text(s.to_string())
}

#[test]
fn a_signed_sidecar_is_recognised_by_integer_key_zero() {
    let bytes = encode(&Value::Map(vec![
        (Value::Integer(0.into()), Value::Integer(1.into())),
        (text("uuid"), text("0195...")),
        (text("hash"), Value::Bytes(vec![0; 32])),
    ]));
    assert_eq!(probe(&bytes), SidecarShape::Signed { schema: 1 });
}

/// The probe reports the schema it finds rather than judging it, so a reader can refuse a
/// too-new sidecar with the version in hand.
#[test]
fn a_newer_signed_schema_is_still_signed() {
    let bytes = encode(&Value::Map(vec![(
        Value::Integer(0.into()),
        Value::Integer(7.into()),
    )]));
    assert_eq!(probe(&bytes), SidecarShape::Signed { schema: 7 });
}

#[test]
fn a_legacy_unsigned_sidecar_is_recognised_by_its_version_key() {
    let bytes = encode(&Value::Map(vec![
        (text("version"), Value::Integer(1.into())),
        (text("uuid"), text("aabbccdd-0000-0000-0000-000000000001")),
        (text("hash_sha256"), text(&"a".repeat(64))),
    ]));
    assert_eq!(probe(&bytes), SidecarShape::LegacyUnsigned);
}

/// Key `0` wins even when a `version` text key is also present: the signed schema field
/// is the authoritative discriminator, and a legacy map never carries key `0`.
#[test]
fn key_zero_outranks_a_stray_version_key() {
    let bytes = encode(&Value::Map(vec![
        (text("version"), Value::Integer(1.into())),
        (Value::Integer(0.into()), Value::Integer(1.into())),
    ]));
    assert_eq!(probe(&bytes), SidecarShape::Signed { schema: 1 });
}

#[test]
fn everything_else_is_unknown() {
    assert_eq!(probe(b"not cbor at all"), SidecarShape::Unknown);
    assert_eq!(probe(&[]), SidecarShape::Unknown);
    // A CBOR array, not a map.
    assert_eq!(
        probe(&encode(&Value::Array(vec![Value::Integer(0.into())]))),
        SidecarShape::Unknown
    );
    // A map with neither discriminating key.
    assert_eq!(
        probe(&encode(&Value::Map(vec![(text("uuid"), text("x"))]))),
        SidecarShape::Unknown
    );
    // Key 0 whose value is not an integer schema.
    assert_eq!(
        probe(&encode(&Value::Map(vec![(
            Value::Integer(0.into()),
            text("one")
        )]))),
        SidecarShape::Unknown
    );
    // Key 0 whose value does not fit a u16.
    assert_eq!(
        probe(&encode(&Value::Map(vec![(
            Value::Integer(0.into()),
            Value::Integer(70_000.into())
        )]))),
        SidecarShape::Unknown
    );
}
