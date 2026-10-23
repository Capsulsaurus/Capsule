//! Append-only, hash-chained provenance log per asset (SSoT: [Cryptography — Provenance
//! § Provenance of Library Modifications]).
//!
//! Each non-create record references its predecessor by SHA-256 hash; a rewrite of any
//! past record breaks the chain at that point and is detectable by any reader walking
//! forward from `create`. This is the structure that lets a key-holding attacker be
//! detected after the fact: history is read-only.
//!
//! [Cryptography — Provenance § Provenance of Library Modifications]: https://docs/design/cryptography/provenance/#provenance-of-library-modifications

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use super::manifest::AssetManifest;
use crate::cbor;
use crate::crypto::hash::{self, Hash32};

/// The provenance-record version this build writes, and the newest it reads.
pub const PROVENANCE_RECORD_VERSION: u16 = 1;

/// One link in an asset's provenance chain.
///
/// The record leads with its [`version`](Self::version) ([Versioning — invariant 2]): the key
/// sorts first in canonical CBOR, and [`decode`](Self::decode) reads it before anything else,
/// so a record from a newer writer is recognised as such rather than misread as this version.
/// Every other decode path refuses it too, because the field's own decoder rejects a version
/// this build does not implement.
///
/// [Versioning — invariant 2]: https://docs/design/versioning/#system-invariants
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceRecord {
    /// The record schema version; [`PROVENANCE_RECORD_VERSION`] on every record this build
    /// writes.
    #[serde(deserialize_with = "known_record_version")]
    pub version: u16,
    /// The asset this chain belongs to.
    pub asset_id: Uuid,
    /// The signed manifest for this transition.
    pub manifest: AssetManifest,
    /// SHA-256 of the previous record; null only for `action = create`. Mirrors the
    /// manifest's own `prior_provenance_hash`, so signing the manifest signs this link.
    pub prior_provenance_hash: Option<Hash32>,
}

/// Why a provenance record's bytes could not be decoded.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum RecordDecodeError {
    /// The record declares a version newer than this build implements. It must be stored
    /// verbatim and quarantined, never applied, modified or re-encoded (versioning invariants
    /// 3 and 5); it becomes readable once the user updates.
    #[error("provenance record version {found} is newer than this build reads ({max_known})")]
    UnsupportedVersion {
        /// The version the record declares.
        found: u64,
        /// The newest version this build reads ([`PROVENANCE_RECORD_VERSION`]).
        max_known: u16,
    },
    /// The bytes are not a well-formed record of a version this build reads.
    #[error("malformed provenance record: {0}")]
    Malformed(String),
}

/// The `version` field's decoder: a version this build does not read is a decode error on
/// every path, so no caller can mistake a newer record for a current one.
fn known_record_version<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u16, D::Error> {
    let version = u16::deserialize(d)?;
    if version == 0 || version > PROVENANCE_RECORD_VERSION {
        return Err(serde::de::Error::custom(format!(
            "unsupported provenance record version {version}; this build reads 1..={PROVENANCE_RECORD_VERSION}"
        )));
    }
    Ok(version)
}

impl ProvenanceRecord {
    /// A current-version record linking `manifest` into `asset_id`'s chain behind `prior`.
    pub fn new(asset_id: Uuid, manifest: AssetManifest, prior: Option<Hash32>) -> Self {
        Self {
            version: PROVENANCE_RECORD_VERSION,
            asset_id,
            manifest,
            prior_provenance_hash: prior,
        }
    }

    /// Decode one record, reading its leading `version` before anything else.
    ///
    /// # Errors
    ///
    /// [`RecordDecodeError::UnsupportedVersion`] for a record a newer writer produced — the
    /// caller quarantines it — and [`RecordDecodeError::Malformed`] for anything else that does
    /// not decode, including a missing or non-integer version.
    pub fn decode(bytes: &[u8]) -> Result<Self, RecordDecodeError> {
        let value: ciborium::Value =
            cbor::from_slice(bytes).map_err(|e| RecordDecodeError::Malformed(e.to_string()))?;
        let version = value
            .as_map()
            .and_then(|map| map.iter().find(|(k, _)| k.as_text() == Some("version")))
            .and_then(|(_, v)| v.as_integer())
            .ok_or_else(|| {
                RecordDecodeError::Malformed("missing or non-integer record version".into())
            })?;
        let version = i128::from(version);
        if version > i128::from(PROVENANCE_RECORD_VERSION) {
            tracing::warn!(
                version = %version,
                max_known = PROVENANCE_RECORD_VERSION,
                "provenance record: version newer than this build reads"
            );
            return Err(RecordDecodeError::UnsupportedVersion {
                found: u64::try_from(version).unwrap_or(u64::MAX),
                max_known: PROVENANCE_RECORD_VERSION,
            });
        }
        cbor::from_slice(bytes).map_err(|e| RecordDecodeError::Malformed(e.to_string()))
    }

    /// The content hash of this record (SHA-256 over its canonical CBOR, signatures
    /// included), used as the next record's `prior_provenance_hash`.
    pub fn record_hash(&self) -> Hash32 {
        hash::hash_bytes(&cbor::to_canonical_vec(self).expect("provenance record serializes"))
    }

    /// Whether the manifest's `prior_provenance_hash` mirrors the record's, as required.
    ///
    /// Public because the chain walker is not the only checker: `capsule-library`'s
    /// `Workspace::apply_remote_entry` decodes a record straight off the wire and must run the
    /// same check before it trusts either copy. provenance.md's *Chained, Append-Only Structure*
    /// is explicit that the two copies are "a checked invariant, not trusted redundancy", and a
    /// checker that lived in one place would leave the wire path unchecked.
    pub fn mirrors_manifest(&self) -> bool {
        self.manifest.core.prior_provenance_hash == self.prior_provenance_hash
    }
}

/// Errors from building or walking a provenance chain.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ChainError {
    /// The first record was not a `create`, or a `create` appeared mid-chain.
    #[error("chain root must be a single create record")]
    BadRoot,
    /// A record's `prior_provenance_hash` does not match the current chain head.
    #[error("record does not chain to the current head (stale or forked)")]
    BrokenLink,
    /// A record's manifest prior hash does not mirror the record's prior hash.
    #[error("manifest prior hash does not mirror the record")]
    MirrorMismatch,
}

/// An in-memory append-only provenance chain for one asset.
#[derive(Debug, Clone, Default)]
pub struct ProvenanceChain {
    records: Vec<ProvenanceRecord>,
}

impl ProvenanceChain {
    /// An empty chain.
    pub fn new() -> Self {
        Self::default()
    }

    /// The current chain head hash (the last record's hash), or `None` if empty.
    pub fn head(&self) -> Option<Hash32> {
        self.records.last().map(ProvenanceRecord::record_hash)
    }

    /// All records, oldest first.
    pub fn records(&self) -> &[ProvenanceRecord] {
        &self.records
    }

    /// Append a record, enforcing the chain invariants:
    /// the first record must be a `create` with a null prior; every later record's prior
    /// must equal the current head; and each record's manifest prior must mirror it.
    pub fn append(&mut self, record: ProvenanceRecord) -> Result<(), ChainError> {
        if !record.mirrors_manifest() {
            return Err(ChainError::MirrorMismatch);
        }
        match self.head() {
            None => {
                if !record.manifest.core.action.is_create()
                    || record.prior_provenance_hash.is_some()
                {
                    return Err(ChainError::BadRoot);
                }
            }
            Some(head) => {
                if record.manifest.core.action.is_create() {
                    return Err(ChainError::BadRoot);
                }
                if record.prior_provenance_hash != Some(head) {
                    return Err(ChainError::BrokenLink);
                }
            }
        }
        self.records.push(record);
        Ok(())
    }

    /// Walk the chain forward from `create`, asserting every link and mirror holds. Detects
    /// a dropped or rewritten record as a non-matching prior hash. (Signature verification
    /// is `verify_asset`'s job; this is structural chain integrity.)
    pub fn verify_walk(records: &[ProvenanceRecord]) -> Result<(), ChainError> {
        let mut expected_prior: Option<Hash32> = None;
        for (i, rec) in records.iter().enumerate() {
            if !rec.mirrors_manifest() {
                return Err(ChainError::MirrorMismatch);
            }
            let is_create = rec.manifest.core.action.is_create();
            if i == 0 {
                if !is_create || rec.prior_provenance_hash.is_some() {
                    return Err(ChainError::BadRoot);
                }
            } else {
                if is_create {
                    return Err(ChainError::BadRoot);
                }
                if rec.prior_provenance_hash != expected_prior {
                    return Err(ChainError::BrokenLink);
                }
            }
            expected_prior = Some(rec.record_hash());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
