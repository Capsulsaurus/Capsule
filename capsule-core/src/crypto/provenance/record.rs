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

/// One link in an asset's provenance chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceRecord {
    /// The asset this chain belongs to.
    pub asset_id: Uuid,
    /// The signed manifest for this transition.
    pub manifest: AssetManifest,
    /// SHA-256 of the previous record; null only for `action = create`. Mirrors the
    /// manifest's own `prior_provenance_hash`, so signing the manifest signs this link.
    pub prior_provenance_hash: Option<Hash32>,
}

impl ProvenanceRecord {
    /// The content hash of this record (SHA-256 over its canonical CBOR, signatures
    /// included), used as the next record's `prior_provenance_hash`.
    pub fn record_hash(&self) -> Hash32 {
        hash::hash_bytes(&cbor::to_canonical_vec(self).expect("provenance record serializes"))
    }

    /// Whether the manifest's `prior_provenance_hash` mirrors the record's, as required.
    fn mirrors_manifest(&self) -> bool {
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
mod tests {
    use super::*;
    use crate::crypto::keys::{AmkVersion, HybridSigningKey};
    use crate::crypto::primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION};
    use crate::crypto::provenance::action::Action;
    use crate::crypto::provenance::manifest::{ASSET_MANIFEST_VERSION, ManifestCore};

    const ASSET: u128 = 0xF11E;

    fn dev() -> HybridSigningKey {
        HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32])
    }
    fn wt() -> HybridSigningKey {
        HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32])
    }

    fn record(action: Action, prior: Option<Hash32>) -> ProvenanceRecord {
        let core = ManifestCore {
            version: ASSET_MANIFEST_VERSION.into(),
            crypto_suite_id: CRYPTO_SUITE_ID,
            protocol_version: PROTOCOL_VERSION.into(),
            file_id: Uuid::from_u128(ASSET),
            album_id: Uuid::from_u128(0xA1),
            amk_version: AmkVersion(1),
            ciphertext_hash: Hash32([0xCC; 32]),
            plaintext_size: 10,
            chunk_size: 65_520,
            nonce_prefix: [0; 7],
            created_by_user: Uuid::from_u128(0x05E2),
            created_by_device: Uuid::from_u128(0xD1),
            client_version: "t".into(),
            timestamp: "2026-05-31T00:00:00Z".into(),
            action,
            prior_provenance_hash: prior,
            retention_until: None,
        };
        ProvenanceRecord {
            asset_id: Uuid::from_u128(ASSET),
            manifest: core.sign(&dev(), &wt()),
            prior_provenance_hash: prior,
        }
    }

    fn build_chain() -> ProvenanceChain {
        let mut chain = ProvenanceChain::new();
        chain.append(record(Action::Create, None)).unwrap();
        let h1 = chain.head().unwrap();
        chain
            .append(record(Action::MetadataUpdate, Some(h1)))
            .unwrap();
        let h2 = chain.head().unwrap();
        chain.append(record(Action::Delete, Some(h2))).unwrap();
        chain
    }

    #[test]
    fn build_and_walk_a_valid_chain() {
        let chain = build_chain();
        assert_eq!(chain.records().len(), 3);
        ProvenanceChain::verify_walk(chain.records()).unwrap();
    }

    #[test]
    fn non_create_root_is_rejected() {
        let mut chain = ProvenanceChain::new();
        // MetadataUpdate with null prior: mirrors (both null) but is not a create.
        assert_eq!(
            chain.append(record(Action::MetadataUpdate, None)),
            Err(ChainError::BadRoot)
        );
    }

    #[test]
    fn second_create_is_rejected() {
        let mut chain = ProvenanceChain::new();
        chain.append(record(Action::Create, None)).unwrap();
        assert_eq!(
            chain.append(record(Action::Create, None)),
            Err(ChainError::BadRoot)
        );
    }

    #[test]
    fn stale_prior_hash_breaks_the_link() {
        let mut chain = ProvenanceChain::new();
        chain.append(record(Action::Create, None)).unwrap();
        // Append with a wrong (stale) prior hash → BrokenLink.
        assert_eq!(
            chain.append(record(Action::Delete, Some(Hash32([0xEE; 32])))),
            Err(ChainError::BrokenLink)
        );
    }

    #[test]
    fn rewriting_a_past_record_is_detected_by_the_walk() {
        let chain = build_chain();
        let mut records = chain.records().to_vec();
        // Tamper the middle record's timestamp (re-sign so its own sigs still verify, but the
        // chain hash it produced changes, breaking the downstream link).
        records[1].manifest.core.timestamp = "1999-01-01T00:00:00Z".into();
        assert_eq!(
            ProvenanceChain::verify_walk(&records),
            Err(ChainError::BrokenLink),
            "a rewritten middle record breaks the forward walk"
        );
    }

    #[test]
    fn dropping_a_record_is_detected() {
        let chain = build_chain();
        let mut records = chain.records().to_vec();
        records.remove(1); // drop the metadata-update
        assert_eq!(
            ProvenanceChain::verify_walk(&records),
            Err(ChainError::BrokenLink)
        );
    }

    #[test]
    fn manifest_prior_must_mirror_record_prior() {
        let mut chain = ProvenanceChain::new();
        chain.append(record(Action::Create, None)).unwrap();
        let head = chain.head().unwrap();
        // Build a record whose manifest prior disagrees with the record prior.
        let mut rec = record(Action::Delete, Some(head));
        rec.manifest.core.prior_provenance_hash = Some(Hash32([0x77; 32]));
        assert_eq!(chain.append(rec), Err(ChainError::MirrorMismatch));
    }
}
