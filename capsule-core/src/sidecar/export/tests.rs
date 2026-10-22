use std::collections::BTreeMap;

use uuid::Uuid;

use super::*;
use crate::crypto::hash::Hash32;
use crate::crypto::keys::HybridSigningKey;
use crate::crypto::primitives::CRYPTO_SUITE_ID;
use crate::metadata::crdt::{AddId, Lww, OrSet};
use crate::sidecar::SIDECAR_SCHEMA_V1;
use crate::sidecar::region::{Region, RegionShape, Regions, write_region};

fn sidecar_with_a_named_face() -> SidecarV1 {
    let mut region = Region::new(RegionShape::Rect {
        x: 0.1,
        y: 0.1,
        w: 0.2,
        h: 0.3,
    });
    region.labels.add(
        "Alice".into(),
        AddId {
            device: Uuid::from_u128(0xD1),
            counter: 0,
        },
    );
    let mut register = Lww::new();
    write_region(
        &mut register,
        Some(region),
        "2026-05-31T12:00:00Z".into(),
        Uuid::from_u128(0xD1),
    );
    let mut regions = Regions::new();
    regions.insert(Uuid::from_u128(0x7E61), register);
    let mut s = SidecarV1 {
        sidecar_schema: SIDECAR_SCHEMA_V1,
        crypto_suite_id: CRYPTO_SUITE_ID,
        uuid: Uuid::from_u128(0x7777),
        hash: Hash32([0xAB; 32]),
        capture_timestamp: "2026-05-31T10:00:00Z".into(),
        import_timestamp: "2026-05-31T11:00:00Z".into(),
        content_type: "image/jpeg".into(),
        dimensions: None,
        lqip: None,
        tags_user: OrSet::new(),
        tags_ai: OrSet::new(),
        caption: Lww::new(),
        rating: Lww::new(),
        stack_membership: Lww::new(),
        regions,
        cull: Lww::new(),
        hidden: Lww::new(),
        camera_id: None,
        device_id: Uuid::from_u128(0xD1),
        session_id: Uuid::from_u128(0x5E),
        gps: None,
        provenance_chain_hash: None,
        unknown: BTreeMap::new(),
        signature: None,
    };
    s.sign(&HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]));
    s
}

#[test]
fn regions_are_stripped_by_default() {
    let library = sidecar_with_a_named_face();
    let exported = sidecar_for_export(&library, ExportPrivacy::default());
    assert!(exported.regions.is_empty());
    let bytes = exported.to_canonical_vec();
    assert!(
        !bytes.windows(5).any(|w| w == b"Alice"),
        "no region label leaves"
    );
    assert!(
        exported.signature.is_none(),
        "a stripped copy is re-signed by the exporter"
    );
    // The library's own sidecar is untouched.
    assert_eq!(library.regions.len(), 1);
    assert!(library.signature.is_some());
}

#[test]
fn regions_are_kept_only_on_a_per_export_opt_in() {
    let library = sidecar_with_a_named_face();
    let exported = sidecar_for_export(
        &library,
        ExportPrivacy {
            retain_regions: true,
        },
    );
    assert_eq!(exported, library, "nothing stripped, signature kept");
}
