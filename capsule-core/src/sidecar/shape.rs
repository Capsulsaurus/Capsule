//! Tell the two sidecar shapes apart **without reading either**.
//!
//! The signed [`SidecarV1`](crate::sidecar::SidecarV1) carries its schema version at CBOR
//! integer key `0` and has no text key `version`; the retired unsigned pre-signed-path shape
//! carried a text key `version` and no integer key at all. The two are disjoint on the wire,
//! so a probe that looks at nothing but those two keys classifies a file exactly, and it
//! builds no model of either shape — which is what keeps it from being a second reader after
//! the unsigned one is deleted. The only consumer of a legacy record's *contents* is the
//! migration verb (`capsule-library`'s `Workspace::migrate_unsigned_sidecars`), which owns its own private
//! decoder.

use ciborium::value::Value;

/// Which on-disk sidecar shape a `{uuid}.cbor` file has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarShape {
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
pub fn probe(bytes: &[u8]) -> SidecarShape {
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
mod tests;
