//! Idempotency keys for write surfaces (SSoT: [Threat Model — Idempotency Invariants]).
//! Every write surface has a single idempotency key: a duplicate (same key) is a no-op; a
//! conflict (same key, different content) is a corruption error. These constructors produce
//! a stable canonical key so a server can dedup deterministically.
//!
//! [Threat Model — Idempotency Invariants]: https://docs/design/threat-model/validation/#idempotency-invariants

use uuid::Uuid;

use crate::crypto::hash::Hash32;

/// A stable idempotency key (a canonical string a server can index).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IdempotencyKey(pub String);

/// `(owner_id, hash, album_id)` — session creation (`POST /upload`) dedup.
pub fn session_key(owner_id: &Uuid, hash: &Hash32, album_id: &Uuid) -> IdempotencyKey {
    IdempotencyKey(format!("session:{owner_id}:{}:{album_id}", hash.to_hex()))
}

/// `(asset_id, prior_provenance_hash, manifest_hash)` — lifecycle manifest write.
pub fn lifecycle_key(
    asset_id: &Uuid,
    prior: Option<Hash32>,
    manifest_hash: &Hash32,
) -> IdempotencyKey {
    let prior = prior.map_or_else(|| "null".into(), |h| h.to_hex());
    IdempotencyKey(format!(
        "lifecycle:{asset_id}:{prior}:{}",
        manifest_hash.to_hex()
    ))
}

/// `(upload_id, offset, chunk_hash)` — upload chunk (`PATCH /upload/{id}`).
pub fn chunk_key(upload_id: &Uuid, offset: u64, chunk_hash: &Hash32) -> IdempotencyKey {
    IdempotencyKey(format!(
        "chunk:{upload_id}:{offset}:{}",
        chunk_hash.to_hex()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_inputs_produce_the_same_key() {
        let owner = Uuid::from_u128(1);
        let album = Uuid::from_u128(2);
        let h = Hash32([7; 32]);
        assert_eq!(
            session_key(&owner, &h, &album),
            session_key(&owner, &h, &album)
        );
    }

    #[test]
    fn different_content_produces_a_different_key() {
        // Same (asset, prior) but a different manifest hash → a *conflict*, distinguishable
        // by the key differing (server treats same-key/different-content as corruption).
        let asset = Uuid::from_u128(1);
        let prior = Some(Hash32([1; 32]));
        let a = lifecycle_key(&asset, prior, &Hash32([2; 32]));
        let b = lifecycle_key(&asset, prior, &Hash32([3; 32]));
        assert_ne!(a, b);
    }

    #[test]
    fn null_prior_is_distinct_from_zero_hash() {
        let asset = Uuid::from_u128(1);
        let mh = Hash32([9; 32]);
        let create = lifecycle_key(&asset, None, &mh);
        let zero = lifecycle_key(&asset, Some(Hash32([0; 32])), &mh);
        assert_ne!(create, zero);
    }

    #[test]
    fn chunk_key_varies_by_offset() {
        let up = Uuid::from_u128(1);
        let h = Hash32([5; 32]);
        assert_ne!(chunk_key(&up, 0, &h), chunk_key(&up, 4096, &h));
    }
}
