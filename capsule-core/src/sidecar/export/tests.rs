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
    let kept = sidecar_for_export(
        &library,
        ExportPrivacy {
            retain_regions: true,
            ..ExportPrivacy::default()
        },
    );
    let labels: Vec<_> = kept
        .regions
        .values()
        .filter_map(|register| register.current.as_ref()?.value.as_ref())
        .flat_map(|region| region.labels.value())
        .collect();
    assert_eq!(labels, ["Alice"], "the opted-in region keeps its label");

    let stripped = sidecar_for_export(
        &library,
        ExportPrivacy {
            retain_regions: false,
            ..ExportPrivacy::RETAIN_ALL
        },
    );
    assert!(
        stripped.regions.is_empty(),
        "the region row alone is stripped"
    );
    assert_eq!(
        sidecar_for_export(&library, ExportPrivacy::RETAIN_ALL),
        library,
        "nothing stripped, signature kept"
    );
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
    assert_eq!(
        every_fix(&exported),
        every_fix(&library),
        "full precision kept"
    );
    assert_eq!(
        sidecar_for_export(&library, ExportPrivacy::RETAIN_ALL),
        library,
        "nothing stripped, signature kept"
    );
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
    let only_gps = ExportPrivacy {
        retain_full_gps: false,
        ..ExportPrivacy::RETAIN_ALL
    };
    assert_eq!(
        sidecar_for_export(&library, only_gps),
        library,
        "nothing to round, so nothing was stripped"
    );
}

const DEVICE: Uuid = Uuid::from_u128(0xD1);
const OTHER_DEVICE: Uuid = Uuid::from_u128(0xD2);
const SESSION: Uuid = Uuid::from_u128(0x5E);
const SERIAL: &str = "SN-0042-SECRET";

/// A sidecar carrying a value for every row of the privacy table, written from two devices: a
/// camera serial, the device and session ids, a full-precision location, a named face, user and
/// AI tags (one removed), stamped caption and rating edits, and a key this reader does not know.
fn sidecar_with_every_stripped_field() -> SidecarV1 {
    let mut s = sidecar_with_two_locations();
    let face = sidecar_with_a_named_face();
    s.regions = face.regions;
    s.camera_id = Some(crate::sidecar::CameraId {
        model: "Example X100".into(),
        serial: SERIAL.into(),
    });
    let first = AddId {
        device: DEVICE,
        counter: 0,
    };
    s.tags_user.add("beach".into(), first);
    s.tags_user.add(
        "holiday".into(),
        AddId {
            device: OTHER_DEVICE,
            counter: 0,
        },
    );
    s.tags_user.remove(first).expect("the add was observed");
    s.tags_ai.add(
        crate::sidecar::AiTag {
            tag: "sea".into(),
            model_id: "tagger".into(),
            model_version: "1".into(),
        },
        AddId {
            device: DEVICE,
            counter: 1,
        },
    );
    s.caption
        .set("first".into(), "2026-06-01T10:00:00Z", DEVICE);
    s.caption
        .set("second".into(), "2026-06-02T10:00:00Z", OTHER_DEVICE);
    s.rating.set(4, "2026-06-01T10:00:00Z", DEVICE);
    s.unknown.insert(
        "future_field".into(),
        ciborium::value::Value::Text("unknown-to-this-reader".into()),
    );
    s.sign(&HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]));
    s
}

/// Whether `bytes` carries `id` in either of the forms a UUID serializes to.
fn carries(bytes: &[u8], id: Uuid) -> bool {
    let text = id.hyphenated().to_string();
    let forms: [&[u8]; 2] = [id.as_bytes(), text.as_bytes()];
    forms
        .iter()
        .any(|form| bytes.windows(form.len()).any(|w| w == *form))
}

/// Whether `bytes` contains `needle`.
fn contains(bytes: &[u8], needle: &str) -> bool {
    bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
}

#[test]
fn the_strict_copy_carries_no_row_of_the_privacy_table() {
    let library = sidecar_with_every_stripped_field();
    let exported = sidecar_for_export(&library, ExportPrivacy::default());
    let bytes = exported.to_canonical_vec();

    for id in [DEVICE, OTHER_DEVICE, SESSION] {
        assert!(!carries(&bytes, id), "{id} does not leave");
    }
    assert!(
        !contains(&bytes, SERIAL),
        "the camera serial does not leave"
    );
    assert!(!contains(&bytes, "Alice"), "no personal contact tag leaves");
    assert!(
        !contains(&bytes, "unknown-to-this-reader"),
        "a key this reader cannot judge does not leave"
    );
    assert!(exported.unknown.is_empty());
    assert_eq!(exported.device_id, Uuid::nil());
    assert_eq!(exported.session_id, Uuid::nil());
    assert!(exported.regions.is_empty());
    let fixes = every_fix(&exported);
    assert_eq!(fixes.len(), 3);
    assert!(
        fixes
            .iter()
            .all(|f| (round_coordinate(f.lat), round_coordinate(f.lon)) == (f.lat, f.lon)),
        "every coordinate is rounded: {fixes:?}"
    );
    assert!(
        exported.signature.is_none(),
        "the stripped copy is unsigned"
    );

    // What the recipient may see survives the strip.
    let camera = exported.camera_id.as_ref().expect("the camera model stays");
    assert_eq!(camera.model, "Example X100");
    assert!(camera.serial.is_empty());
    assert_eq!(
        exported.tags_user.value(),
        ["holiday".to_owned()].into(),
        "the live tag stays, the removed one leaves with its tombstone"
    );
    assert_eq!(exported.tags_ai.value().len(), 1);
    assert_eq!(
        exported.caption.current.as_ref().map(|c| c.value.as_str()),
        Some("second")
    );
    assert_eq!(exported.rating.current.as_ref().map(|r| r.value), Some(4));

    // The re-encoded copy reads back as a sidecar.
    assert_eq!(
        SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).as_ref(),
        Ok(&exported)
    );
}

#[test]
fn the_library_sidecar_is_untouched_by_the_strip() {
    let library = sidecar_with_every_stripped_field();
    let before = library.to_canonical_vec();
    let _ = sidecar_for_export(&library, ExportPrivacy::default());
    assert_eq!(library.to_canonical_vec(), before);
    assert!(
        library.verify(&HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]).verifying_key()),
        "the library copy keeps a signature that verifies"
    );
}

#[test]
fn each_identifier_is_kept_only_on_its_own_opt_in() {
    let library = sidecar_with_every_stripped_field();

    let serial = sidecar_for_export(
        &library,
        ExportPrivacy {
            retain_camera_serial: true,
            ..ExportPrivacy::default()
        },
    );
    assert_eq!(serial.camera_id, library.camera_id);
    assert_eq!(serial.device_id, Uuid::nil());
    assert_eq!(serial.session_id, Uuid::nil());

    let device = sidecar_for_export(
        &library,
        ExportPrivacy {
            retain_device_ids: true,
            ..ExportPrivacy::default()
        },
    );
    assert_eq!(device.device_id, DEVICE);
    assert_eq!(device.caption, library.caption, "register stamps are kept");
    assert_eq!(device.tags_user, library.tags_user, "add-ids are kept");
    assert_eq!(device.session_id, Uuid::nil());
    assert!(
        device
            .camera_id
            .as_ref()
            .is_some_and(|c| c.serial.is_empty())
    );

    let session = sidecar_for_export(
        &library,
        ExportPrivacy {
            retain_session_id: true,
            ..ExportPrivacy::default()
        },
    );
    assert_eq!(session.session_id, SESSION);
    assert_eq!(session.device_id, Uuid::nil());
}

#[test]
fn unknown_keys_leave_under_no_opt_in() {
    let library = sidecar_with_every_stripped_field();
    let exported = sidecar_for_export(&library, ExportPrivacy::RETAIN_ALL);
    assert!(exported.unknown.is_empty());
    assert!(
        exported.signature.is_none(),
        "dropping a key voids the signature"
    );
    let mut expected = library.clone();
    expected.unknown.clear();
    expected.signature = None;
    assert_eq!(exported, expected, "every opted-in row is kept as it was");
}
