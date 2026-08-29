//! The visibility gate and the `original_held` derivation (the staged-uploads contract).
//!
//! The server gates an asset's visibility on **which blobs of the bundle are held**, and it can
//! do that without a key: every session declares its [`BlobRole`] at creation, so bundle
//! completeness is answerable from the roles alone.
//!
//! Both facts are **derived**, never stored: `original_held` in particular is a function of
//! whether the original-role blob has finalized, and a second column carrying it would be a
//! second source of truth that can disagree with the first.
//!
//! # Why the gate takes a bundle and not a role
//!
//! It used to take one role and answer "did finalizing *this* flip visibility", which read
//! naturally and was wrong. The upload protocol says an asset becomes visible "once its
//! **manifest and metadata blob** are finalized" — two blobs — and since `S-C30` the manifest is
//! a blob of its own rather than a JSON field on the session, which is precisely what the
//! protocol means when it says the amendment "makes the visibility gate literal". A per-role
//! predicate cannot express a conjunction over two roles: asked about the metadata blob it has
//! to answer `true` without knowing whether the manifest ever arrived, and an asset published
//! that way reaches the feed with no manifest for the receiving client to verify.
//!
//! The single-role form was not caught by its own test, which asserted that a provenance blob
//! does *not* flip visibility on its own — true, and true for the wrong reason. That is the
//! failure shape worth naming: the code, its test and its comment all agreed with each other,
//! and none of them agreed with the contract.

use std::collections::BTreeSet;

use crate::store::BlobRole;

/// The roles a bundle must hold before the server may publish it.
///
/// The **index tier** of the staged-upload ladder (`T0`), named here once so the gate and the
/// ladder cannot drift apart.
pub const INDEX_TIER_ROLES: [BlobRole; 2] = [BlobRole::Provenance, BlobRole::Metadata];

/// Whether a bundle holding `held` may be published to other devices.
///
/// Takes the roles the asset actually holds rather than the role that just finalized, because
/// the gate is a conjunction and the order blobs arrive in is not fixed — the protocol imposes
/// no wire ordering, so the manifest may land before or after the metadata blob.
pub fn bundle_is_publishable(held: impl IntoIterator<Item = BlobRole>) -> bool {
    let held: BTreeSet<BlobRole> = held.into_iter().collect();
    INDEX_TIER_ROLES.iter().all(|role| held.contains(role))
}

/// Whether finalizing a blob of this role *can* complete the index tier.
///
/// Not a visibility answer — [`bundle_is_publishable`] is — but the cheap pre-filter a caller
/// uses to decide whether a finalization is even worth re-evaluating the gate for. A derivative
/// or a backup can never be the blob that publishes an asset.
pub fn completes_index_tier(role: BlobRole) -> bool {
    INDEX_TIER_ROLES.contains(&role)
}

/// Derive `original_held` from the finalization state of the asset's original blob.
///
/// A one-line function on purpose: it is the *definition*, and the callers that need it — the
/// sync feed, the media-serving path — must share it rather than each re-deciding what "held"
/// means.
pub fn derive_original_held(original_finalized: bool) -> bool {
    original_finalized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publication_needs_the_manifest_and_the_metadata_blob() {
        assert!(
            !bundle_is_publishable([BlobRole::Metadata]),
            "a metadata blob alone publishes an asset whose manifest no client can verify"
        );
        assert!(
            !bundle_is_publishable([BlobRole::Provenance]),
            "a manifest alone describes an asset whose metadata has not landed"
        );
        assert!(bundle_is_publishable([
            BlobRole::Provenance,
            BlobRole::Metadata
        ]));
    }

    #[test]
    fn the_bundle_may_arrive_in_any_order() {
        // The protocol imposes no wire ordering, so the gate must be order-blind.
        assert!(bundle_is_publishable([
            BlobRole::Metadata,
            BlobRole::Provenance
        ]));
        assert!(bundle_is_publishable([
            BlobRole::Original,
            BlobRole::Derivative,
            BlobRole::Metadata,
            BlobRole::Backup,
            BlobRole::Provenance,
        ]));
    }

    #[test]
    fn the_bulk_tiers_never_publish_on_their_own() {
        for role in [BlobRole::Original, BlobRole::Derivative, BlobRole::Backup] {
            assert!(
                !completes_index_tier(role),
                "{role:?} is not part of the index tier"
            );
            assert!(
                !bundle_is_publishable([role]),
                "{role:?} must not make an asset visible on its own"
            );
        }
        for role in INDEX_TIER_ROLES {
            assert!(completes_index_tier(role));
        }
    }

    #[test]
    fn original_held_tracks_the_original_blob() {
        assert!(!derive_original_held(false));
        assert!(derive_original_held(true));
    }
}
