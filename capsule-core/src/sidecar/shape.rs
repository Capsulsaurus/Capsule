//! Tell a signed sidecar from anything else **without reading it**.
//!
//! The signed [`SidecarV1`](crate::sidecar::SidecarV1) carries its schema version at CBOR
//! integer key `0`. A probe that looks at nothing but that key classifies a file, and it
//! builds no model of the shape — which is what keeps it from being a second reader beside
//! the signed one. Every other byte string, including the retired unsigned pre-signed-path
//! record (text keys only, no key `0`), is [`SidecarShape::Unknown`]: no reader of that
//! record's contents remains anywhere.

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
    /// Not a signed sidecar: not CBOR, not a map, a map without integer key `0` (the
    /// retired unsigned pre-signed-path record among them), or a key `0` that is not a `u16`.
    Unknown,
}

/// Classify sidecar bytes by their discriminating key alone.
///
/// Decodes the outer CBOR value once and inspects only the map's keys; every value except
/// the one at integer key `0` is left unexamined. Never fails: undecodable bytes are
/// [`SidecarShape::Unknown`], and so is a `0` key whose value is not a `u16`.
pub fn probe(bytes: &[u8]) -> SidecarShape {
    let Ok(Value::Map(entries)) = ciborium::de::from_reader::<Value, _>(bytes) else {
        return SidecarShape::Unknown;
    };
    let schema_key = entries
        .iter()
        .find(|(key, _)| matches!(key, Value::Integer(i) if i128::from(*i) == 0));
    match schema_key {
        Some((_, Value::Integer(v))) => u16::try_from(i128::from(*v))
            .map_or(SidecarShape::Unknown, |schema| SidecarShape::Signed {
                schema,
            }),
        _ => SidecarShape::Unknown,
    }
}

#[cfg(test)]
mod tests;
