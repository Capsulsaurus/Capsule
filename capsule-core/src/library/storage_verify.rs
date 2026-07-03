//! The verify-before-destroy gate — client half of storage verification (slices `S-C3`
//! server endpoint / `S-D4` client wiring in the repo-root `SLICES.md`; SSoT:
//! [Import — Storage Verification](https://docs/design/import/storage-verification/)).
//!
//! Before any post-write local cleanup of irreplaceable bytes (releasing a device-owned
//! original, deleting a Move-import source, a streaming-mode release), a client requires
//! **both** halves to pass: `verify_asset` accepts the asset (crypto validity — the
//! offline core already implements it) and the server's `POST /storage/verify` verdict is
//! `durable` (stored ∧ indexed ∧ retrievable for every required blob). The predicate here
//! is the pure conjunction those call sites consume; fetching the verdict is `S-D4`.

use uuid::Uuid;

use crate::crypto::hash::Hash32;

/// A blob's role within an asset, as the storage-verification endpoint reports it
/// (closed enum; the value set is owned by the storage-verification doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobRole {
    /// The original ciphertext blob.
    Original,
    /// The encrypted metadata blob.
    Metadata,
    /// A derivative (thumbnail / preview / embedding) blob.
    Derivative,
    /// The provenance chain.
    Provenance,
}

/// The server's key-free per-blob verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobVerdict {
    /// The content address the client declared it relies on.
    pub hash: Hash32,
    /// The blob's role on the asset.
    pub role: BlobRole,
    /// Present in the blob store at its content address (`stat`), not merely in-flight.
    pub stored: bool,
    /// Referenced by a committed, `uploaded = true` index row.
    pub indexed: bool,
    /// Refcount > 0, not mid-GC (`collectable_since`), not quarantined.
    pub retrievable: bool,
}

impl BlobVerdict {
    /// One blob's contribution to durability: all three independent facts hold.
    pub fn safely_stored(&self) -> bool {
        self.stored && self.indexed && self.retrievable
    }
}

/// The per-asset verdict from `POST /storage/verify`. `durable` attests **this home
/// server's** storage only — never replicas or peers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageVerdict {
    /// The asset the verdict is for.
    pub asset_id: Uuid,
    /// Server-computed: every required blob is stored ∧ indexed ∧ retrievable.
    pub durable: bool,
    /// Per-blob detail, one entry per hash the client declared.
    pub blobs: Vec<BlobVerdict>,
    /// The server's trusted clock at verification (RFC 3339, like `received_at`).
    pub checked_at: String,
}

/// The verify-before-destroy predicate: destructive local cleanup of irreplaceable bytes
/// may proceed **only** when the server's verdict is `durable`, every declared blob
/// individually re-checks as safely stored (the client never trusts the server's
/// aggregate over the details it can recompute), and `verify_asset` accepted the asset.
///
/// A `false` result never triggers a destructive action — the caller retains the local
/// copy, retries with backoff, and surfaces "not yet confirmed on server".
pub fn release_is_safe(verdict: &StorageVerdict, verify_asset_accepted: bool) -> bool {
    verify_asset_accepted
        && verdict.durable
        && !verdict.blobs.is_empty()
        && verdict.blobs.iter().all(BlobVerdict::safely_stored)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(stored: bool, indexed: bool, retrievable: bool) -> BlobVerdict {
        BlobVerdict {
            hash: Hash32([0xAA; 32]),
            role: BlobRole::Original,
            stored,
            indexed,
            retrievable,
        }
    }

    fn verdict(durable: bool, blobs: Vec<BlobVerdict>) -> StorageVerdict {
        StorageVerdict {
            asset_id: Uuid::from_u128(1),
            durable,
            blobs,
            checked_at: "2026-07-02T00:00:00Z".into(),
        }
    }

    #[test]
    fn release_requires_both_halves_and_consistent_details() {
        let good = verdict(true, vec![blob(true, true, true)]);
        assert!(release_is_safe(&good, true));
        // Crypto half failed: never release.
        assert!(!release_is_safe(&good, false));
        // Server says durable but a detail row disagrees: the client's re-check wins.
        let inconsistent = verdict(true, vec![blob(true, true, false)]);
        assert!(!release_is_safe(&inconsistent, true));
        // Not durable: never release.
        assert!(!release_is_safe(
            &verdict(false, vec![blob(true, true, true)]),
            true
        ));
        // An empty verdict confirms nothing.
        assert!(!release_is_safe(&verdict(true, vec![]), true));
    }

    /// `S-D4` acceptance: the eviction sweep, Move-import source deletion, and
    /// streaming-mode release all gate on this predicate fed by a real
    /// `POST /storage/verify` response, and a non-`durable` verdict retains the local
    /// copy and surfaces the unconfirmed state.
    #[test]
    #[ignore = "S-D4 contract: verify-before-destroy wiring not yet implemented"]
    fn destructive_paths_gate_on_release_is_safe() {
        unimplemented!("implemented by slice S-D4");
    }
}
