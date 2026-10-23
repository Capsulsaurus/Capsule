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
