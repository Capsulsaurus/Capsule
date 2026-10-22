use ciborium::value::Value;

use super::*;
use crate::cbor::{from_slice, value_to_canonical_vec};

/// A record with one required and one optional field, decoded the way a signed core is.
#[derive(Debug, PartialEq)]
struct Probe {
    a: u64,
    b: Option<u64>,
    unknown: BTreeMap<String, Value>,
}

impl<'de> Deserialize<'de> for Probe {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let mut m = OpenMap::deserialize("test record", d)?;
        Ok(Self {
            a: m.take("a")?,
            b: m.take_opt("b")?,
            unknown: m.into_unknown(),
        })
    }
}

fn decode(entries: Vec<(Value, Value)>) -> Result<Probe, String> {
    from_slice(&value_to_canonical_vec(&Value::Map(entries))).map_err(|e| e.to_string())
}

#[test]
fn known_fields_are_taken_and_the_rest_is_unknown() {
    let future = Value::Tag(42, Box::new(Value::Bool(true)));
    let probe = decode(vec![
        ("a".into(), Value::Integer(7.into())),
        ("b".into(), Value::Null),
        ("future".into(), future.clone()),
    ])
    .unwrap();
    assert_eq!(
        probe,
        Probe {
            a: 7,
            b: None,
            unknown: BTreeMap::from([("future".to_owned(), future)]),
        }
    );
}

#[test]
fn an_absent_optional_reads_as_none_and_a_present_one_decodes() {
    assert_eq!(
        decode(vec![("a".into(), Value::Integer(1.into()))])
            .unwrap()
            .b,
        None
    );
    assert_eq!(
        decode(vec![
            ("a".into(), Value::Integer(1.into())),
            ("b".into(), Value::Integer(2.into())),
        ])
        .unwrap()
        .b,
        Some(2)
    );
}

#[test]
fn a_missing_required_field_is_an_error() {
    let err = decode(vec![("b".into(), Value::Null)]).unwrap_err();
    assert!(err.contains("missing field"), "{err}");
}

#[test]
fn a_mistyped_field_names_the_record_and_key() {
    let err = decode(vec![("a".into(), "text".into())]).unwrap_err();
    assert!(err.contains("test record field a"), "{err}");
}

#[test]
fn malformed_maps_are_refused() {
    let not_a_map: Result<Probe, _> =
        from_slice(&value_to_canonical_vec(&Value::Integer(1.into())));
    assert!(
        not_a_map
            .unwrap_err()
            .to_string()
            .contains("test record must be a map")
    );
    let err = decode(vec![(Value::Integer(0.into()), Value::Null)]).unwrap_err();
    assert!(err.contains("test record has only text keys"), "{err}");
    let err = decode(vec![
        ("a".into(), Value::Integer(1.into())),
        ("a".into(), Value::Integer(2.into())),
    ])
    .unwrap_err();
    assert!(err.contains("duplicate test record field a"), "{err}");
}
