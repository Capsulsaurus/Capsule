//! `StackDocV1` — a stack's signed, versioned label record (SSoT: [Organization — Labels]).
//!
//! Keyed by `stack_id` and sealed under the AMK of the stack's container album — every member
//! of a stack belongs to that one album, and the record names it so a holder knows which AMK
//! seals it. It carries the stack's labels as an OR-set. Like the sidecar, it is CRDT fields
//! only, its schema is field 0, and unknown keys survive under the signature.
//!
//! [Organization — Labels]: https://docs/design/organization/#labels

use std::collections::BTreeMap;

use ciborium::value::Value;
use uuid::Uuid;

use super::signed_doc::{self, DocError};
use crate::crypto::keys::{Amk, HybridSignature, HybridSigningKey, HybridVerifyingKey};
use crate::metadata::crdt::OrSet;

/// The current stack-document schema version.
pub const STACK_DOC_SCHEMA_V1: u16 = 1;

/// A stack's signed label record.
#[derive(Debug, Clone, PartialEq)]
pub struct StackDocV1 {
    /// Schema version — integer **field 0**.
    pub stack_doc_schema: u16,
    /// The stack this record speaks for (UUIDv7).
    pub stack_id: Uuid,
    /// The container album every member of the stack belongs to; its AMK seals this record.
    pub album_id: Uuid,
    /// The stack's labels.
    pub labels: OrSet<String>,
    /// Unknown CBOR keys preserved verbatim (covered by the signature).
    pub unknown: BTreeMap<String, Value>,
    /// Hybrid signature over every other field.
    pub signature: Option<HybridSignature>,
}

impl StackDocV1 {
    /// An empty, unsigned record for `stack_id` in `album_id`.
    pub fn new(stack_id: Uuid, album_id: Uuid) -> Self {
        Self {
            stack_doc_schema: STACK_DOC_SCHEMA_V1,
            stack_id,
            album_id,
            labels: OrSet::new(),
            unknown: BTreeMap::new(),
            signature: None,
        }
    }

    fn to_entries(&self, include_signature: bool) -> Vec<(Value, Value)> {
        let mut m = vec![
            (
                Value::Integer(0u8.into()),
                Value::Integer(self.stack_doc_schema.into()),
            ),
            ("stack_id".into(), signed_doc::to_value(&self.stack_id)),
            ("album_id".into(), signed_doc::to_value(&self.album_id)),
            ("labels".into(), signed_doc::to_value(&self.labels)),
        ];
        for (k, v) in &self.unknown {
            m.push((Value::Text(k.clone()), v.clone()));
        }
        if include_signature && let Some(sig) = &self.signature {
            m.push(("signature".into(), signed_doc::to_value(sig)));
        }
        m
    }

    /// The canonical bytes the signature covers (everything except `signature`).
    pub fn signing_bytes(&self) -> Vec<u8> {
        signed_doc::encode(self.to_entries(false))
    }

    /// The full canonical encoding, signature included.
    pub fn to_canonical_vec(&self) -> Vec<u8> {
        signed_doc::encode(self.to_entries(true))
    }

    /// Sign with the user identity key.
    pub fn sign(&mut self, ik: &HybridSigningKey) {
        self.signature = Some(ik.sign(&self.signing_bytes()));
    }

    /// Verify against the user identity key. An unsigned record never verifies.
    pub fn verify(&self, ik_public: &HybridVerifyingKey) -> bool {
        self.signature
            .as_ref()
            .is_some_and(|sig| ik_public.verify(&self.signing_bytes(), sig))
    }

    /// Decode from canonical CBOR, refusing a schema newer than `max_known`.
    ///
    /// # Errors
    ///
    /// [`DocError::SchemaTooNew`] or [`DocError::Decode`].
    pub fn from_canonical_slice(bytes: &[u8], max_known: u16) -> Result<Self, DocError> {
        let (stack_doc_schema, mut fields) = signed_doc::split_fields(bytes, max_known)?;
        let stack_id = signed_doc::take(&mut fields, "stack_id")?;
        let album_id = signed_doc::take(&mut fields, "album_id")?;
        let labels = signed_doc::take(&mut fields, "labels")?;
        let signature = signed_doc::take_opt(&mut fields, "signature")?;
        Ok(Self {
            stack_doc_schema,
            stack_id,
            album_id,
            labels,
            unknown: fields,
            signature,
        })
    }

    /// Merge another replica of the same stack's record: labels by OR-set union, unknown keys
    /// by a deterministic fold. The signature is dropped when the merge changed anything.
    ///
    /// # Errors
    ///
    /// [`DocError::IdMismatch`] when `other` speaks for another stack, or places the stack in
    /// another album; nothing is merged.
    pub fn merge(&mut self, other: &Self) -> Result<(), DocError> {
        if other.stack_id != self.stack_id {
            return Err(DocError::IdMismatch {
                expected: self.stack_id,
                found: other.stack_id,
            });
        }
        if other.album_id != self.album_id {
            return Err(DocError::IdMismatch {
                expected: self.album_id,
                found: other.album_id,
            });
        }
        let before = self.signing_bytes();
        self.labels.merge(&other.labels);
        signed_doc::merge_unknown(&mut self.unknown, &other.unknown);
        if self.signing_bytes() != before {
            self.signature = None;
        }
        Ok(())
    }

    /// Seal the full record under `amk` (its album's current epoch) with a fresh nonce.
    ///
    /// # Errors
    ///
    /// [`DocError::Crypto`] if sealing fails.
    pub fn seal(&self, amk: &Amk) -> Result<Vec<u8>, DocError> {
        signed_doc::seal(amk, &self.stack_id, &self.to_canonical_vec())
    }

    /// Open a record sealed by [`seal`](Self::seal) for `stack_id`. The signature is **not**
    /// checked here; the caller verifies against the signer it trusts.
    ///
    /// # Errors
    ///
    /// [`DocError::Crypto`] when `amk` does not open it, [`DocError::IdMismatch`] when it speaks
    /// for another stack, and the decode errors of
    /// [`from_canonical_slice`](Self::from_canonical_slice).
    pub fn open_sealed(
        wire: &[u8],
        amk: &Amk,
        stack_id: &Uuid,
        max_known: u16,
    ) -> Result<Self, DocError> {
        let plaintext = signed_doc::open(amk, stack_id, wire)?;
        let doc = Self::from_canonical_slice(&plaintext, max_known)?;
        if doc.stack_id != *stack_id {
            return Err(DocError::IdMismatch {
                expected: *stack_id,
                found: doc.stack_id,
            });
        }
        Ok(doc)
    }
}

#[cfg(test)]
mod tests;
