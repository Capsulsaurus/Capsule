//! `AlbumDocV1` — an album's signed, versioned label record (SSoT: [Organization — Labels]).
//!
//! Keyed by `album_id` and sealed under the album's current AMK, it carries the album's labels
//! (an OR-set) and **its name** (an LWW register). The name used to live only in the device's
//! local keystore, so a restored or newly joined device saw `Recovered album`; carrying it here
//! lets every member converge on it. Like the sidecar, it is CRDT fields only, its schema is
//! field 0, and unknown keys survive under the signature.
//!
//! [Organization — Labels]: https://docs/design/organization/#labels

use std::collections::BTreeMap;

use ciborium::value::Value;
use uuid::Uuid;

use super::signed_doc::{self, DocError};
use crate::crypto::keys::{Amk, HybridSignature, HybridSigningKey, HybridVerifyingKey};
use crate::metadata::crdt::{Lww, OrSet};

/// The current album-document schema version.
pub const ALBUM_DOC_SCHEMA_V1: u16 = 1;

/// An album's signed label record.
#[derive(Debug, Clone, PartialEq)]
pub struct AlbumDocV1 {
    /// Schema version — integer **field 0**.
    pub album_doc_schema: u16,
    /// The album this record speaks for (UUIDv7).
    pub album_id: Uuid,
    /// The album's labels.
    pub labels: OrSet<String>,
    /// The album's display name.
    pub name: Lww<String>,
    /// Unknown CBOR keys preserved verbatim (covered by the signature).
    pub unknown: BTreeMap<String, Value>,
    /// Hybrid signature over every other field.
    pub signature: Option<HybridSignature>,
}

impl AlbumDocV1 {
    /// An empty, unsigned record for `album_id`.
    pub fn new(album_id: Uuid) -> Self {
        Self {
            album_doc_schema: ALBUM_DOC_SCHEMA_V1,
            album_id,
            labels: OrSet::new(),
            name: Lww::new(),
            unknown: BTreeMap::new(),
            signature: None,
        }
    }

    fn to_entries(&self, include_signature: bool) -> Vec<(Value, Value)> {
        let mut m = vec![
            (
                Value::Integer(0u8.into()),
                Value::Integer(self.album_doc_schema.into()),
            ),
            ("album_id".into(), signed_doc::to_value(&self.album_id)),
            ("labels".into(), signed_doc::to_value(&self.labels)),
            ("name".into(), signed_doc::to_value(&self.name)),
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
        let (album_doc_schema, mut fields) = signed_doc::split_fields(bytes, max_known)?;
        let album_id = signed_doc::take(&mut fields, "album_id")?;
        let labels = signed_doc::take(&mut fields, "labels")?;
        let name = signed_doc::take(&mut fields, "name")?;
        let signature = signed_doc::take_opt(&mut fields, "signature")?;
        Ok(Self {
            album_doc_schema,
            album_id,
            labels,
            name,
            unknown: fields,
            signature,
        })
    }

    /// The album's current name, if one was ever written.
    pub fn current_name(&self) -> Option<&str> {
        self.name.get().map(String::as_str)
    }

    /// Merge another replica of the same album's record: labels by OR-set union, the name by
    /// LWW, unknown keys by a deterministic fold. The signature is dropped when the merge changed
    /// anything, so the caller re-signs what it now holds.
    ///
    /// # Errors
    ///
    /// [`DocError::IdMismatch`] when `other` speaks for another album; nothing is merged.
    pub fn merge(&mut self, other: &Self) -> Result<(), DocError> {
        if other.album_id != self.album_id {
            return Err(DocError::IdMismatch {
                expected: self.album_id,
                found: other.album_id,
            });
        }
        let before = self.signing_bytes();
        self.labels.merge(&other.labels);
        self.name.merge(&other.name);
        signed_doc::merge_unknown(&mut self.unknown, &other.unknown);
        if self.signing_bytes() != before {
            self.signature = None;
        }
        Ok(())
    }

    /// Seal the full record under `amk` (the album's current epoch) with a fresh nonce.
    ///
    /// # Errors
    ///
    /// [`DocError::Crypto`] if sealing fails.
    pub fn seal(&self, amk: &Amk) -> Result<Vec<u8>, DocError> {
        signed_doc::seal(amk, &self.album_id, &self.to_canonical_vec())
    }

    /// Open a record sealed by [`seal`](Self::seal) for `album_id`. The signature is **not**
    /// checked here; the caller verifies against the signer it trusts.
    ///
    /// # Errors
    ///
    /// [`DocError::Crypto`] when `amk` does not open it, [`DocError::IdMismatch`] when it speaks
    /// for another album, and the decode errors of
    /// [`from_canonical_slice`](Self::from_canonical_slice).
    pub fn open_sealed(
        wire: &[u8],
        amk: &Amk,
        album_id: &Uuid,
        max_known: u16,
    ) -> Result<Self, DocError> {
        let plaintext = signed_doc::open(amk, album_id, wire)?;
        let doc = Self::from_canonical_slice(&plaintext, max_known)?;
        if doc.album_id != *album_id {
            return Err(DocError::IdMismatch {
                expected: *album_id,
                found: doc.album_id,
            });
        }
        Ok(doc)
    }
}

#[cfg(test)]
mod tests;
