//! Tell the two sidecar shapes apart **without reading either** (slice `S-D24`).
//!
//! The signed [`SidecarV1`](crate::sidecar::SidecarV1) carries its schema version at CBOR
//! integer key `0` and has no text key `version`; the retired unsigned pre-signed-path shape
//! carried a text key `version` and no integer key at all. The two are disjoint on the wire,
//! so a probe that looks at nothing but those two keys classifies a file exactly, and it
//! builds no model of either shape — which is what keeps it from being a second reader after
//! the unsigned one is deleted. The only consumer of a legacy record's *contents* is the
//! migration verb ([`Workspace::migrate_unsigned_sidecars`]), which owns its own private
//! decoder.
//!
//! [`Workspace::migrate_unsigned_sidecars`]: crate::lifecycle::Workspace::migrate_unsigned_sidecars

use ciborium::value::Value;

/// Which on-disk sidecar shape a `{uuid}.cbor` file has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SidecarShape {
    /// A signed `SidecarV1`-family record: integer key `0` is present and carries `schema`.
    /// Says nothing about whether the schema is one this build reads.
    Signed {
        /// The value at integer key `0`.
        schema: u16,
    },
    /// The retired unsigned pre-signed-path shape: a text `version` key and no key `0`.
    /// Readable only by the migration verb's private decoder.
    LegacyUnsigned,
    /// Neither: not CBOR, not a map, or a map carrying neither discriminating key.
    Unknown,
}

/// Classify sidecar bytes by their discriminating keys alone.
///
/// Decodes the outer CBOR value once and inspects only the map's keys; every value except
/// the one at integer key `0` is left unexamined. Never fails: undecodable bytes are
/// [`SidecarShape::Unknown`], and so is a `0` key whose value is not a `u16`.
pub(crate) fn probe(bytes: &[u8]) -> SidecarShape {
    let Ok(Value::Map(entries)) = ciborium::de::from_reader::<Value, _>(bytes) else {
        return SidecarShape::Unknown;
    };
    let mut has_version_key = false;
    for (key, value) in &entries {
        match key {
            Value::Integer(i) if i128::from(*i) == 0 => {
                return match value {
                    Value::Integer(v) => u16::try_from(i128::from(*v))
                        .map_or(SidecarShape::Unknown, |schema| SidecarShape::Signed {
                            schema,
                        }),
                    _ => SidecarShape::Unknown,
                };
            }
            Value::Text(t) if t == "version" => has_version_key = true,
            _ => {}
        }
    }
    if has_version_key {
        SidecarShape::LegacyUnsigned
    } else {
        SidecarShape::Unknown
    }
}

#[cfg(test)]
mod tests {
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
}
