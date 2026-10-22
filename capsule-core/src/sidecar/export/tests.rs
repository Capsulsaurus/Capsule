use std::collections::BTreeMap;

use uuid::Uuid;

use super::*;
use crate::crypto::hash::Hash32;
use crate::crypto::keys::HybridSigningKey;
use crate::crypto::primitives::CRYPTO_SUITE_ID;
use crate::domain::GpsDatum;
use crate::metadata::crdt::{AddId, Lww, OrSet};
use crate::sidecar::region::{Region, RegionShape, Regions, write_region};
use crate::sidecar::{GpsSource, SIDECAR_SCHEMA_V1};

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
        capture_timestamp_user: Lww::new(),
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
        gps_user: Lww::new(),
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
            ..ExportPrivacy::default()
        },
    );
    assert_eq!(exported, library, "nothing stripped, signature kept");
}

fn fix(lat: f64, lon: f64, source: GpsSource) -> Gps {
    Gps {
        lat,
        lon,
        source,
        datum: GpsDatum::Wgs84,
    }
}

/// A sidecar carrying an as-imported fix and a user location register whose current value
/// displaced an earlier edit — three full-precision coordinates in all.
fn sidecar_with_two_locations() -> SidecarV1 {
    let mut s = sidecar_with_a_named_face();
    s.regions.clear();
    s.gps = Some(fix(40.712_776, -74.005_974, GpsSource::Exif));
    s.gps_user.set(
        Some(fix(48.858_370, 2.294_481, GpsSource::Manual)),
        "2026-06-01T10:00:00Z",
        Uuid::from_u128(0xD1),
    );
    s.gps_user.set(
        Some(fix(51.500_729, -0.124_625, GpsSource::Manual)),
        "2026-06-02T10:00:00Z",
        Uuid::from_u128(0xD2),
    );
    s.sign(&HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]));
    s
}

/// Every coordinate the copy carries, as-imported and user, live and superseded.
fn every_fix(s: &SidecarV1) -> Vec<Gps> {
    let user = s.gps_user.current.iter().chain(&s.gps_user.superseded);
    s.gps
        .iter()
        .chain(user.filter_map(|stamped| stamped.value.as_ref()))
        .cloned()
        .collect()
}

#[test]
fn both_the_imported_and_the_user_location_are_rounded_by_default() {
    let library = sidecar_with_two_locations();
    let exported = sidecar_for_export(&library, ExportPrivacy::default());

    let fixes = every_fix(&exported);
    assert_eq!(fixes.len(), 3, "the as-imported fix and both user edits");
    for (lat, lon) in [(40.71, -74.01), (51.5, -0.12), (48.86, 2.29)] {
        assert!(
            fixes.iter().any(|f| (f.lat, f.lon) == (lat, lon)),
            "({lat}, {lon}) is the rounded form of one fix: {fixes:?}"
        );
    }
    assert_eq!(exported.gps, Some(fix(40.71, -74.01, GpsSource::Exif)));
    assert_eq!(
        exported.effective_gps(),
        Some(&fix(51.5, -0.12, GpsSource::Manual))
    );
    assert!(
        exported.signature.is_none(),
        "the exporter re-signs the copy"
    );

    // The library's own sidecar keeps full precision and its signature.
    assert_eq!(
        library.gps.as_ref().map(|g| (g.lat, g.lon)),
        Some((40.712_776, -74.005_974))
    );
    assert_eq!(
        library.effective_gps().map(|g| (g.lat, g.lon)),
        Some((51.500_729, -0.124_625))
    );
    assert!(library.signature.is_some());
}

#[test]
fn full_gps_precision_is_kept_only_on_a_per_export_opt_in() {
    let library = sidecar_with_two_locations();
    let exported = sidecar_for_export(
        &library,
        ExportPrivacy {
            retain_full_gps: true,
            ..ExportPrivacy::default()
        },
    );
    assert_eq!(exported, library, "nothing stripped, signature kept");
}

#[test]
fn an_already_coarse_location_leaves_the_signature_in_place() {
    let mut library = sidecar_with_a_named_face();
    library.regions.clear();
    library.gps = Some(fix(40.5, -74.25, GpsSource::Exif));
    library
        .gps_user
        .set(None, "2026-06-01T10:00:00Z", Uuid::from_u128(0xD1));
    library.sign(&HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]));
    assert_eq!(
        sidecar_for_export(&library, ExportPrivacy::default()),
        library,
        "nothing to round, so nothing was stripped"
    );
}
