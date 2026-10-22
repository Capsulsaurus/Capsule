use super::*;

fn sample() -> EnrollmentPayload {
    EnrollmentPayload::Commitment([7u8; KEY_COMMITMENT_LEN])
}

#[test]
fn a_payload_is_lowercase_hex_and_round_trips() {
    let encoded = sample().encode().unwrap();
    assert!(
        encoded
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    assert_eq!(EnrollmentPayload::decode(&encoded).unwrap(), sample());
}

/// The commitment is a CBOR byte string, not an array of integers — the canonical form any
/// other implementation must produce.
#[test]
fn the_commitment_encodes_as_a_byte_string() {
    let bytes = hex::decode(sample().encode().unwrap()).unwrap();
    let value: ciborium::Value = crate::cbor::from_slice(&bytes).unwrap();
    let payload = value
        .as_map()
        .unwrap()
        .iter()
        .find(|(k, _)| k.as_text() == Some("payload"))
        .map(|(_, v)| v.clone())
        .unwrap();
    let inner = payload.as_map().unwrap()[0].clone();
    assert_eq!(inner.0.as_text(), Some("commitment"));
    assert!(inner.1.is_bytes());
}

#[test]
fn uppercase_hex_is_refused_as_non_canonical() {
    let encoded = sample().encode().unwrap().to_uppercase();
    assert_eq!(
        EnrollmentPayload::decode(&encoded),
        Err(EnrollmentPayloadError::NonCanonical)
    );
}

#[test]
fn a_non_canonical_cbor_spelling_is_refused() {
    // The same envelope with its two keys in non-canonical order.
    let value = ciborium::Value::Map(vec![
        (
            ciborium::Value::Text("payload".into()),
            ciborium::Value::Map(vec![(
                ciborium::Value::Text("commitment".into()),
                ciborium::Value::Bytes(vec![7u8; KEY_COMMITMENT_LEN]),
            )]),
        ),
        (
            ciborium::Value::Text("v".into()),
            ciborium::Value::Integer(1.into()),
        ),
    ]);
    let mut bytes = Vec::new();
    ciborium::into_writer(&value, &mut bytes).unwrap();
    assert_ne!(hex::encode(&bytes), sample().encode().unwrap());
    assert_eq!(
        EnrollmentPayload::decode(&hex::encode(&bytes)),
        Err(EnrollmentPayloadError::NonCanonical)
    );
}

#[test]
fn another_version_is_refused_by_number() {
    let envelope = ciborium::Value::Map(vec![
        (
            ciborium::Value::Text("v".into()),
            ciborium::Value::Integer(2.into()),
        ),
        (
            ciborium::Value::Text("payload".into()),
            ciborium::Value::Text("from the future".into()),
        ),
    ]);
    let encoded = hex::encode(crate::cbor::value_to_canonical_vec(&envelope));
    assert_eq!(
        EnrollmentPayload::decode(&encoded),
        Err(EnrollmentPayloadError::UnsupportedVersion(2))
    );
}

#[test]
fn malformed_and_oversized_payloads_are_refused() {
    for bad in ["zz", "0", "a0", "ff"] {
        assert!(
            matches!(
                EnrollmentPayload::decode(bad),
                Err(EnrollmentPayloadError::Malformed(_))
            ),
            "{bad}"
        );
    }
    assert_eq!(
        EnrollmentPayload::decode(""),
        Err(EnrollmentPayloadError::Size(0))
    );
    let huge = "0".repeat(MAX_ENROLLMENT_PAYLOAD_CHARS + 2);
    assert_eq!(
        EnrollmentPayload::decode(&huge),
        Err(EnrollmentPayloadError::Size(huge.len()))
    );
}

/// A commitment of the wrong length is not a commitment.
#[test]
fn a_short_commitment_is_refused() {
    let envelope = ciborium::Value::Map(vec![
        (
            ciborium::Value::Text("v".into()),
            ciborium::Value::Integer(1.into()),
        ),
        (
            ciborium::Value::Text("payload".into()),
            ciborium::Value::Map(vec![(
                ciborium::Value::Text("commitment".into()),
                ciborium::Value::Bytes(vec![7u8; KEY_COMMITMENT_LEN - 1]),
            )]),
        ),
    ]);
    let encoded = hex::encode(crate::cbor::value_to_canonical_vec(&envelope));
    assert!(matches!(
        EnrollmentPayload::decode(&encoded),
        Err(EnrollmentPayloadError::Malformed(_))
    ));
}
