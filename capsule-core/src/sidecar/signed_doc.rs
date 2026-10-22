//! What the signed organization documents ([`StackDocV1`](super::StackDocV1) and
//! [`AlbumDocV1`](super::AlbumDocV1)) share with the sidecar (SSoT: [Organization — Labels]):
//! the schema version as integer **field 0**, every other field under a text key, unknown keys
//! preserved verbatim under the signature, and sealing under the album's AMK in the
//! [metadata-blob wire format] with the document's own id as the blob id.
//!
//! A sealed document and an asset's metadata blob derive their keys the same way. What stops one
//! being presented as the other is the payload itself: each decodes only as its own schema and
//! verifies only under its own signature.
//!
//! [Organization — Labels]: https://docs/design/organization/#labels
//! [metadata-blob wire format]: https://docs/design/cryptography/encryption/#metadata-blob-wire-format

use std::collections::BTreeMap;

use ciborium::value::Value;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::cbor;
use crate::crypto::CryptoError;
use crate::crypto::encryption::{blob_nonce, open_blob, seal_metadata_blob};
use crate::crypto::keys::Amk;

/// Why a signed organization document could not be decoded or opened.
#[derive(Debug, Error)]
pub enum DocError {
    /// The bytes are not a well-formed document.
    #[error("document decode: {0}")]
    Decode(String),
    /// The document's schema is newer than this build knows; it is refused rather than
    /// stripped and re-signed.
    #[error("document schema {found} is newer than max known {max_known}; refusing")]
    SchemaTooNew {
        /// The schema the document declares.
        found: u16,
        /// The newest schema this build implements.
        max_known: u16,
    },
    /// The document speaks for another id than the one it was filed or sealed under.
    #[error("document is for {found}, expected {expected}")]
    IdMismatch {
        /// The id the caller expected.
        expected: Uuid,
        /// The id the document carries.
        found: Uuid,
    },
    /// Sealing or opening failed.
    #[error(transparent)]
    Crypto(#[from] CryptoError),
}

pub(crate) fn to_value<T: Serialize>(v: &T) -> Value {
    let mut buf = Vec::new();
    ciborium::ser::into_writer(v, &mut buf).expect("document field serializes");
    ciborium::de::from_reader(buf.as_slice()).expect("document field re-reads")
}

pub(crate) fn from_value<T: for<'de> Deserialize<'de>>(v: Value) -> Result<T, DocError> {
    let mut buf = Vec::new();
    ciborium::ser::into_writer(&v, &mut buf).map_err(|e| DocError::Decode(e.to_string()))?;
    ciborium::de::from_reader(buf.as_slice()).map_err(|e| DocError::Decode(e.to_string()))
}

/// Split a document into its field-0 schema and its text-keyed fields, refusing a schema newer
/// than `max_known` before anything else is parsed.
pub(crate) fn split_fields(
    bytes: &[u8],
    max_known: u16,
) -> Result<(u16, BTreeMap<String, Value>), DocError> {
    let value: Value = ciborium::de::from_reader(bytes)
        .map_err(|e| DocError::Decode(format!("cbor decode: {e}")))?;
    let Value::Map(entries) = value else {
        return Err(DocError::Decode("document must be a CBOR map".into()));
    };
    let mut schema: Option<u16> = None;
    let mut text = BTreeMap::new();
    for (k, v) in entries {
        match k {
            Value::Integer(i) if i128::from(i) == 0 => schema = Some(from_value(v)?),
            Value::Text(key) => {
                text.insert(key, v);
            }
            _ => return Err(DocError::Decode("unexpected document map key".into())),
        }
    }
    let schema = schema.ok_or_else(|| DocError::Decode("missing schema (field 0)".into()))?;
    if schema > max_known {
        return Err(DocError::SchemaTooNew {
            found: schema,
            max_known,
        });
    }
    Ok((schema, text))
}

/// Remove and decode a required field.
pub(crate) fn take<T: for<'de> Deserialize<'de>>(
    fields: &mut BTreeMap<String, Value>,
    key: &str,
) -> Result<T, DocError> {
    from_value(
        fields
            .remove(key)
            .ok_or_else(|| DocError::Decode(format!("missing field: {key}")))?,
    )
}

/// Remove and decode an optional field (absent or `null` is `None`).
pub(crate) fn take_opt<T: for<'de> Deserialize<'de>>(
    fields: &mut BTreeMap<String, Value>,
    key: &str,
) -> Result<Option<T>, DocError> {
    match fields.remove(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => from_value(v).map(Some),
    }
}

/// Canonical encoding of a document's ordered entries.
pub(crate) fn encode(entries: Vec<(Value, Value)>) -> Vec<u8> {
    cbor::value_to_canonical_vec(&Value::Map(entries))
}

/// Fold `other`'s preserved unknown keys into `local`. Keys only one side holds are kept; where
/// both hold a key with different values, the value with the greater canonical encoding wins,
/// so the fold is commutative, associative and idempotent like the CRDT fields beside it.
pub(crate) fn merge_unknown(local: &mut BTreeMap<String, Value>, other: &BTreeMap<String, Value>) {
    for (key, theirs) in other {
        match local.get(key) {
            None => {
                local.insert(key.clone(), theirs.clone());
            }
            Some(ours) => {
                if cbor::value_to_canonical_vec(theirs) > cbor::value_to_canonical_vec(ours) {
                    local.insert(key.clone(), theirs.clone());
                }
            }
        }
    }
}

/// Seal `plaintext` under `amk` with a fresh nonce, `id` as the blob id.
pub(crate) fn seal(amk: &Amk, id: &Uuid, plaintext: &[u8]) -> Result<Vec<u8>, DocError> {
    let (wire, _key) = seal_metadata_blob(amk, id, plaintext, None)?;
    Ok(wire)
}

/// Open a document sealed by [`seal`].
pub(crate) fn open(amk: &Amk, id: &Uuid, wire: &[u8]) -> Result<Vec<u8>, DocError> {
    let nonce =
        blob_nonce(wire).ok_or_else(|| DocError::Decode("sealed document too short".into()))?;
    Ok(open_blob(&amk.derive_blob_key(id, &nonce), wire)?)
}
