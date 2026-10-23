use uuid::Uuid;

use super::*;
use crate::crypto::keys::{AmkVersion, HybridSigningKey};
use crate::crypto::primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION};
use crate::crypto::provenance::{ASSET_MANIFEST_VERSION, Action, KeyMode, ManifestCore};

#[test]
fn hash_length_and_size_and_content_type() {
    assert!(hash_length_ok(CRYPTO_SUITE_ID, 32));
    assert!(!hash_length_ok(CRYPTO_SUITE_ID, 31));
    assert!(!hash_length_ok(0x9999, 32));

    assert!(size_in_bounds(1, 100));
    assert!(size_in_bounds(100, 100));
    assert!(!size_in_bounds(0, 100));
    assert!(!size_in_bounds(101, 100));

    let allowed = ["image/jpeg", "image/heic"];
    assert!(content_type_allowed("image/jpeg", &allowed));
    assert!(!content_type_allowed("application/x-evil", &allowed));
}

#[test]
fn timestamp_and_added_at_rules() {
    assert_eq!(
        device_added_before("2026-05-30T00:00:00Z", "2026-05-31T00:00:00Z"),
        Some(true)
    );
    assert_eq!(
        device_added_before("2026-06-01T00:00:00Z", "2026-05-31T00:00:00Z"),
        Some(false)
    );
    assert_eq!(device_added_before("bogus", "2026-05-31T00:00:00Z"), None);

    assert_eq!(
        timestamp_within_drift("2026-05-31T00:00:00Z", "2026-05-31T00:00:00Z", 30),
        Some(true)
    );
    assert_eq!(
        timestamp_within_drift("2026-01-01T00:00:00Z", "2026-05-31T00:00:00Z", 30),
        Some(false)
    );
}

#[test]
fn prior_hash_and_amk_monotonic() {
    let h = Hash32([1; 32]);
    // create: prior None, no stored head.
    assert!(prior_hash_matches(None, None, true));
    assert!(!prior_hash_matches(Some(h), None, true));
    // non-create: prior must equal stored head.
    assert!(prior_hash_matches(Some(h), Some(h), false));
    assert!(!prior_hash_matches(Some(h), Some(Hash32([2; 32])), false));
    assert!(!prior_hash_matches(None, Some(h), false));

    assert!(amk_version_monotonic(1, None));
    assert!(amk_version_monotonic(3, Some(2)));
    assert!(amk_version_monotonic(2, Some(2)));
    assert!(!amk_version_monotonic(1, Some(2)));
}

fn core(action: Action, prior: Option<Hash32>, amk: u32) -> ManifestCore {
    let dev = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
    let wt = HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32]);
    let c = ManifestCore {
        version: ASSET_MANIFEST_VERSION.into(),
        crypto_suite_id: CRYPTO_SUITE_ID,
        protocol_version: PROTOCOL_VERSION.into(),
        file_id: Uuid::from_u128(1),
        album_id: Uuid::from_u128(2),
        amk_version: AmkVersion(amk),
        ciphertext_hash: Hash32([0; 32]),
        plaintext_size: 10,
        chunk_size: 65_520,
        nonce_prefix: [0; 7],
        key_mode: KeyMode::Derived,
        wrapped_file_key: None,
        metadata_blob_hash: None,
        created_by_user: Uuid::from_u128(3),
        created_by_device: Uuid::from_u128(4),
        client_version: "t".into(),
        timestamp: "2026-05-31T00:00:00Z".into(),
        action,
        prior_provenance_hash: prior,
        upgraded_from: None,
        unknown: Default::default(),
        retention_until: None,
    };
    let _ = c.clone().sign(&dev, &wt); // ensure it's a well-formed signable core
    c
}

fn ctx<'a>(head: Option<Hash32>, amk: Option<u32>) -> EnvelopeContext<'a> {
    EnvelopeContext {
        album_pin: PROTOCOL_VERSION,
        device_added_at: "2026-05-30T00:00:00Z",
        server_clock: "2026-05-31T01:00:00Z",
        drift_days: 30,
        stored_chain_head: head,
        stored_amk_version: amk,
    }
}

#[test]
fn envelope_accepts_valid_create_and_update() {
    let c = core(Action::Create, None, 1);
    assert_eq!(check_manifest_envelope(&c, &ctx(None, None)), Ok(()));

    let head = Hash32([9; 32]);
    let u = core(Action::MetadataUpdate, Some(head), 1);
    assert_eq!(
        check_manifest_envelope(&u, &ctx(Some(head), Some(1))),
        Ok(())
    );
}

#[test]
fn envelope_rejects_each_invariant() {
    // Album pin mismatch.
    let mut c = core(Action::Create, None, 1);
    c.protocol_version = "1999-01-01".into();
    assert_eq!(
        check_manifest_envelope(&c, &ctx(None, None)),
        Err(EnvelopeReject::AlbumPinMismatch)
    );

    // Unknown suite.
    let mut c = core(Action::Create, None, 1);
    c.crypto_suite_id = 0x9999;
    assert_eq!(
        check_manifest_envelope(&c, &ctx(None, None)),
        Err(EnvelopeReject::UnknownSuite)
    );

    // Stale chain (update whose prior != stored head).
    let u = core(Action::MetadataUpdate, Some(Hash32([1; 32])), 1);
    assert_eq!(
        check_manifest_envelope(&u, &ctx(Some(Hash32([2; 32])), Some(1))),
        Err(EnvelopeReject::StaleChain)
    );

    // AMK regression.
    let u = core(Action::MetadataUpdate, Some(Hash32([2; 32])), 1);
    assert_eq!(
        check_manifest_envelope(&u, &ctx(Some(Hash32([2; 32])), Some(5))),
        Err(EnvelopeReject::AmkRegressed)
    );

    // Device added after the manifest timestamp.
    let c = core(Action::Create, None, 1);
    let mut bad = ctx(None, None);
    bad.device_added_at = "2027-01-01T00:00:00Z";
    assert_eq!(
        check_manifest_envelope(&c, &bad),
        Err(EnvelopeReject::DeviceAddedAfter)
    );
}

#[test]
fn invariant_25_metadata_blob_hash_matches_key_free() {
    use crate::crypto::encryption::seal_blob;

    let blob = seal_blob(&[0x33; 32], b"the canonical sidecar bytes");
    let committed = crate::crypto::encryption::blob_ciphertext_hash(&blob);

    // A create manifest committing to the blob's content address accepts, no key needed.
    let mut c = core(Action::Create, None, 1);
    c.metadata_blob_hash = Some(committed);
    assert!(metadata_blob_hash_matches(c.metadata_blob_hash, &blob));
    assert_eq!(check_metadata_blob_envelope(&c, &blob), Ok(()));

    // A one-byte mutation of the blob (what a malicious client might substitute) is caught.
    let mut tampered = blob.clone();
    tampered[0] ^= 0x01;
    assert_eq!(
        check_metadata_blob_envelope(&c, &tampered),
        Err(EnvelopeReject::MetadataBlobHashMismatch)
    );

    // A manifest committing to no hash cannot admit a bundle that carries a blob.
    let none = core(Action::Delete, Some(Hash32([9; 32])), 1);
    assert!(!metadata_blob_hash_matches(none.metadata_blob_hash, &blob));
    assert_eq!(
        check_metadata_blob_envelope(&none, &blob),
        Err(EnvelopeReject::MetadataBlobHashMismatch)
    );
}
