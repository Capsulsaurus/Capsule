use super::*;
use crate::crypto::primitives::CRYPTO_SUITE_ID;

fn minimal() -> SidecarV1 {
    SidecarV1 {
        sidecar_schema: SIDECAR_SCHEMA_V1,
        crypto_suite_id: CRYPTO_SUITE_ID,
        uuid: Uuid::from_u128(0x7777),
        hash: Hash32([0xAB; 32]),
        capture_timestamp: "2026-05-31T10:00:00Z".into(),
        capture_timestamp_user: Lww::new(),
        import_timestamp: "2026-05-31T11:00:00Z".into(),
        content_type: "image/jpeg".into(),
        dimensions: Some(Dimensions {
            width: 4032,
            height: 3024,
        }),
        lqip: None,
        tags_user: OrSet::new(),
        tags_ai: OrSet::new(),
        caption: Lww::new(),
        rating: Lww::new(),
        stack_membership: Lww::new(),
        regions: Regions::new(),
        cull: Lww::new(),
        hidden: Lww::new(),
        camera_id: Some(CameraId {
            model: "iPhone 15 Pro".into(),
            serial: "ABC123".into(),
        }),
        device_id: Uuid::from_u128(0xD1),
        session_id: Uuid::from_u128(0x5E),
        gps: Some(Gps {
            lat: 40.7128,
            lon: -74.0060,
            source: GpsSource::Exif,
            datum: GpsDatum::Wgs84,
        }),
        gps_user: Lww::new(),
        provenance_chain_hash: Some(Hash32([0xCC; 32])),
        unknown: BTreeMap::new(),
        signature: None,
    }
}

#[test]
fn schema_is_field_zero_canonically_first() {
    let s = minimal();
    let bytes = s.to_canonical_vec();
    // map head (0xAX for small maps) then the first key must be integer 0 (0x00),
    // then its value (schema = 1 → 0x01).
    assert!(
        (0xa0..=0xbb).contains(&bytes[0]),
        "expected a CBOR map head"
    );
    assert_eq!(bytes[1], 0x00, "first map key must be integer 0");
    assert_eq!(bytes[2], 0x01, "schema value 1");
}

#[test]
fn sign_verify_round_trip_through_canonical_cbor() {
    let ik = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
    let mut s = minimal();
    s.sign(&ik);
    assert!(s.verify(&ik.verifying_key()));

    let bytes = s.to_canonical_vec();
    let back = SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).unwrap();
    assert_eq!(back, s);
    assert!(back.verify(&ik.verifying_key()));
}

/// The generated LQIP (chromahash + version + dominant_color) lands in the sidecar,
/// is covered by the signature, and survives a canonical round-trip byte-identically.
#[test]
fn lqip_lands_in_signed_sidecar_and_round_trips() {
    let ik = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
    let mut s = minimal();
    s.lqip = Some(Lqip {
        chromahash: vec![1, 2, 3, 4, 5, 6, 7, 8],
        format_version: 1,
        dominant_color: [10, 20, 30],
    });
    s.sign(&ik);
    assert!(s.verify(&ik.verifying_key()));

    let bytes = s.to_canonical_vec();
    let back = SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).unwrap();
    assert_eq!(back.lqip, s.lqip);
    assert!(back.verify(&ik.verifying_key()));

    // Dropping the LQIP after signing invalidates the signature (it was covered).
    let mut stripped = back.clone();
    stripped.lqip = None;
    assert!(!stripped.verify(&ik.verifying_key()));
}

#[test]
fn tampering_after_signing_breaks_verification() {
    let ik = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
    let mut s = minimal();
    s.sign(&ik);
    s.content_type = "image/heic".into(); // change a field without re-signing
    assert!(!s.verify(&ik.verifying_key()));
}

#[test]
fn unknown_fields_are_preserved_and_signed() {
    let ik = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
    let mut s = minimal();
    s.unknown
        .insert("future_field".into(), Value::Text("future_value".into()));
    s.sign(&ik);

    let bytes = s.to_canonical_vec();
    let back = SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).unwrap();
    assert_eq!(
        back.unknown.get("future_field"),
        Some(&Value::Text("future_value".into()))
    );
    // Signature still verifies (it covered the unknown field)...
    assert!(back.verify(&ik.verifying_key()));
    // ...and stripping the unknown field invalidates the signature.
    let mut stripped = back.clone();
    stripped.unknown.clear();
    assert!(!stripped.verify(&ik.verifying_key()));
}

#[test]
fn refuses_a_schema_newer_than_known() {
    let mut s = minimal();
    s.sidecar_schema = 99;
    let bytes = s.to_canonical_vec();
    // A client whose max known schema is 1 refuses to read schema 99.
    assert!(SidecarV1::from_canonical_slice(&bytes, 1).is_err());
    // A future client that knows schema 99 reads it.
    assert!(SidecarV1::from_canonical_slice(&bytes, 99).is_ok());
}

#[test]
fn canonical_encoding_is_deterministic() {
    let s = minimal();
    assert_eq!(s.to_canonical_vec(), s.to_canonical_vec());
}

#[test]
fn create_sidecar_provenance_chain_hash_is_wire_absent() {
    // A create sidecar references no prior head (sealing order step 2, `H = None`), so
    // `provenance_chain_hash` encodes as an absent map key and round-trips back to `None`.
    let ik = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
    let mut s = minimal();
    s.provenance_chain_hash = None;
    s.sign(&ik);
    let bytes = s.to_canonical_vec();
    let needle = b"provenance_chain_hash";
    assert!(
        !bytes.windows(needle.len()).any(|w| w == needle),
        "an absent prior head must not appear on the wire"
    );
    let back = SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).unwrap();
    assert_eq!(back.provenance_chain_hash, None);
    assert_eq!(back, s);
    assert!(back.verify(&ik.verifying_key()));
    // A post-create sidecar (minimal carries `Some`) keeps the key present.
    assert!(
        minimal()
            .to_canonical_vec()
            .windows(needle.len())
            .any(|w| w == needle),
        "a present prior head must appear as a byte string on the wire"
    );
}

#[test]
fn absent_stack_membership_is_wire_absent_and_byte_stable() {
    // A never-written register is omitted from the wire, so pre-existing
    // sidecars (and stackless assets) encode byte-identically to before the
    // Lww wrap.
    let s = minimal();
    let bytes = s.to_canonical_vec();
    let needle = b"stack_membership";
    assert!(!bytes.windows(needle.len()).any(|w| w == needle));
    let back = SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).unwrap();
    assert_eq!(back.stack_membership, Lww::new());
}

#[test]
fn stack_membership_round_trips_and_survives_signing() {
    let ik = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
    let mut s = minimal();
    s.stack_membership.set(
        Some(StackMembership {
            stack_id: Uuid::from_u128(0x51),
            stack_type: StackType::Burst,
            role: StackRole::Primary,
            member_index: Some(0),
        }),
        "2026-05-31T12:00:00Z",
        Uuid::from_u128(0xD1),
    );
    s.sign(&ik);
    let back = SidecarV1::from_canonical_slice(&s.to_canonical_vec(), SIDECAR_SCHEMA_V1).unwrap();
    assert_eq!(back, s);
    assert!(back.verify(&ik.verifying_key()));
}

#[test]
fn concurrent_stack_edits_converge_order_independently() {
    // Replica A joins a stack at 10:00; replica B removes membership at
    // 11:00. Merging in either order — or replaying either op again —
    // converges on the removal: grouping is idempotent and
    // order-independent by construction (metadata doc, Convergence table).
    let membership = Some(StackMembership {
        stack_id: Uuid::from_u128(0x51),
        stack_type: StackType::Burst,
        role: StackRole::Member,
        member_index: Some(3),
    });
    let mut a: Lww<Option<StackMembership>> = Lww::new();
    a.set(
        membership.clone(),
        "2026-05-31T10:00:00Z",
        Uuid::from_u128(0xA),
    );
    let mut b: Lww<Option<StackMembership>> = Lww::new();
    b.set(None, "2026-05-31T11:00:00Z", Uuid::from_u128(0xB));

    let mut ab = a.clone();
    ab.merge(&b);
    let mut ba = b.clone();
    ba.merge(&a);
    assert_eq!(ab.get(), ba.get());
    assert_eq!(ab.get(), Some(&None)); // the removal wins

    // Idempotent: replaying the stale join changes nothing.
    let mut replay = ab.clone();
    replay.merge(&a);
    assert_eq!(replay.get(), ab.get());
}

fn with_region() -> (SidecarV1, Uuid) {
    use crate::metadata::crdt::AddId;
    use crate::sidecar::region::{Region, RegionShape, write_region};

    let mut s = minimal();
    let region_id = Uuid::from_u128(0x7E61);
    let mut region = Region::new(RegionShape::Circle {
        cx: 0.5,
        cy: 0.25,
        r: 0.125,
    });
    region.labels.add(
        "Alice".into(),
        AddId {
            device: Uuid::from_u128(0xD1),
            counter: 41,
        },
    );
    let mut register = Lww::new();
    write_region(
        &mut register,
        Some(region),
        "2026-05-31T12:00:00Z".into(),
        Uuid::from_u128(0xD1),
    );
    s.regions.insert(region_id, register);
    (s, region_id)
}

#[test]
fn absent_regions_are_wire_absent_and_byte_stable() {
    let bytes = minimal().to_canonical_vec();
    let needle = b"regions";
    assert!(!bytes.windows(needle.len()).any(|w| w == needle));
    let back = SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).unwrap();
    assert!(back.regions.is_empty());
}

#[test]
fn regions_round_trip_and_survive_signing() {
    let ik = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
    let (mut s, region_id) = with_region();
    s.sign(&ik);
    let back = SidecarV1::from_canonical_slice(&s.to_canonical_vec(), SIDECAR_SCHEMA_V1).unwrap();
    assert_eq!(back, s);
    assert!(back.verify(&ik.verifying_key()));
    assert!(back.regions.contains_key(&region_id));
    // Dropping the regions after signing invalidates the signature (they were covered).
    let mut stripped = back;
    stripped.regions.clear();
    assert!(!stripped.verify(&ik.verifying_key()));
}

/// A client that predates `regions` decodes it into `_unknown`, re-signs after an edit of
/// its own, and hands back a sidecar a current client reads the regions from intact.
#[test]
fn an_old_client_preserves_regions_as_an_unknown_key_through_a_re_sign() {
    let ik = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
    let (mut current, _) = with_region();
    current.sign(&ik);
    let bytes = current.to_canonical_vec();

    // What a pre-`regions` decoder holds: the same record with `regions` as an unknown key.
    let mut old = SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).unwrap();
    let raw = to_value(&old.regions);
    old.regions.clear();
    old.unknown.insert("regions".into(), raw);
    assert_eq!(
        old.to_canonical_vec(),
        bytes,
        "an unknown-key regions entry encodes byte-identically"
    );
    assert!(old.verify(&ik.verifying_key()));

    // The old client edits a field it knows and re-signs.
    old.caption.set(
        "seen by an old client".into(),
        "2026-06-01T00:00:00Z",
        Uuid::from_u128(0xD2),
    );
    old.sign(&ik);

    let back = SidecarV1::from_canonical_slice(&old.to_canonical_vec(), SIDECAR_SCHEMA_V1).unwrap();
    assert!(back.verify(&ik.verifying_key()));
    assert_eq!(back.regions, current.regions);
    assert!(back.unknown.is_empty());
    assert_eq!(
        back.caption.get().map(String::as_str),
        Some("seen by an old client")
    );
}

#[test]
fn max_add_counter_spans_region_labels() {
    let (s, _) = with_region();
    assert_eq!(s.max_add_counter_for(&Uuid::from_u128(0xD1)), Some(41));
    assert_eq!(s.max_add_counter_for(&Uuid::from_u128(0xD9)), None);
}

#[test]
fn absent_cull_and_hidden_are_wire_absent_and_byte_stable() {
    // Never-flagged assets omit both registers, so pre-existing sidecars
    // encode byte-identically to before the fields existed.
    let s = minimal();
    let bytes = s.to_canonical_vec();
    for needle in [b"cull".as_slice(), b"hidden".as_slice()] {
        assert!(
            !bytes.windows(needle.len()).any(|w| w == needle),
            "unexpected wire presence"
        );
    }
    let back = SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).unwrap();
    assert_eq!(back.cull, Lww::new());
    assert_eq!(back.hidden, Lww::new());
}

#[test]
fn cull_and_hidden_round_trip_and_survive_signing() {
    let ik = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
    let mut s = minimal();
    s.cull.set(
        CullFlag::Reject,
        "2026-05-31T12:00:00Z",
        Uuid::from_u128(0xD1),
    );
    s.hidden
        .set(true, "2026-05-31T12:00:01Z", Uuid::from_u128(0xD1));
    s.sign(&ik);
    let back = SidecarV1::from_canonical_slice(&s.to_canonical_vec(), SIDECAR_SCHEMA_V1).unwrap();
    assert_eq!(back, s);
    assert_eq!(back.cull.get(), Some(&CullFlag::Reject));
    assert_eq!(back.hidden.get(), Some(&true));
    assert!(back.verify(&ik.verifying_key()));
}

#[test]
fn concurrent_cull_flags_converge() {
    // Two devices flag the same asset concurrently: pick at 10:00, reject at
    // 11:00 — both merge orders converge on the later reject.
    let mut a: Lww<CullFlag> = Lww::new();
    a.set(CullFlag::Pick, "2026-05-31T10:00:00Z", Uuid::from_u128(0xA));
    let mut b: Lww<CullFlag> = Lww::new();
    b.set(
        CullFlag::Reject,
        "2026-05-31T11:00:00Z",
        Uuid::from_u128(0xB),
    );
    let mut ab = a.clone();
    ab.merge(&b);
    let mut ba = b.clone();
    ba.merge(&a);
    assert_eq!(ab.get(), ba.get());
    assert_eq!(ab.get(), Some(&CullFlag::Reject));
}

/// A sidecar with no user correction carries neither register on the wire, so every sidecar
/// written before the registers existed encodes byte-identically, and the effective values
/// are the as-imported ones.
#[test]
fn absent_user_capture_and_location_are_wire_absent_and_byte_stable() {
    let s = minimal();
    let bytes = s.to_canonical_vec();
    for needle in [b"capture_timestamp_user".as_slice(), b"gps_user".as_slice()] {
        assert!(
            !bytes.windows(needle.len()).any(|w| w == needle),
            "unexpected wire presence"
        );
    }
    let back = SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).unwrap();
    assert_eq!(back.capture_timestamp_user, Lww::new());
    assert_eq!(back.gps_user, Lww::new());
    assert_eq!(back.effective_capture_timestamp(), "2026-05-31T10:00:00Z");
    assert_eq!(back.effective_gps(), s.gps.as_ref());
}

/// Both registers round-trip under signature beside the as-imported values they override,
/// and a stamped `None` location hides the import's fix without erasing it.
#[test]
fn user_capture_and_location_round_trip_beside_the_imported_values() {
    let ik = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
    let mut s = minimal();
    s.capture_timestamp_user.set(
        "2019-03-04T05:06:07Z".to_string(),
        "2026-06-01T00:00:00Z",
        Uuid::from_u128(0xD1),
    );
    let manual = Gps {
        lat: 51.5,
        lon: -0.12,
        source: GpsSource::Manual,
        datum: GpsDatum::Wgs84,
    };
    s.gps_user.set(
        Some(manual.clone()),
        "2026-06-01T00:00:00Z",
        Uuid::from_u128(0xD1),
    );
    s.sign(&ik);

    let back = SidecarV1::from_canonical_slice(&s.to_canonical_vec(), SIDECAR_SCHEMA_V1).unwrap();
    assert_eq!(back, s);
    assert!(back.verify(&ik.verifying_key()));
    assert_eq!(back.capture_timestamp, "2026-05-31T10:00:00Z");
    assert_eq!(back.effective_capture_timestamp(), "2019-03-04T05:06:07Z");
    assert_eq!(back.gps.as_ref().map(|g| g.source), Some(GpsSource::Exif));
    assert_eq!(back.effective_gps(), Some(&manual));

    // Dropping a correction after signing invalidates the signature (it was covered).
    let mut stripped = back.clone();
    stripped.gps_user = Lww::new();
    assert!(!stripped.verify(&ik.verifying_key()));

    // A stamped `None` is "no location" — distinct from never having edited it.
    let mut cleared = back;
    cleared
        .gps_user
        .set(None, "2026-06-02T00:00:00Z", Uuid::from_u128(0xD1));
    assert_eq!(cleared.effective_gps(), None);
    assert!(cleared.gps.is_some(), "the import's fix is kept");
}

/// Two devices correct the same asset's capture time and location concurrently: merging the
/// registers in either order — or replaying either side — converges on the later write, and
/// the as-imported values are untouched by any of it.
#[test]
fn concurrent_capture_and_location_edits_converge_by_lww() {
    let base = minimal();
    let manual = |lat: f64| {
        Some(Gps {
            lat,
            lon: 0.0,
            source: GpsSource::Manual,
            datum: GpsDatum::Wgs84,
        })
    };
    let mut a = base.clone();
    a.capture_timestamp_user.set(
        "2019-01-01T00:00:00Z".to_string(),
        "2026-06-01T10:00:00Z",
        Uuid::from_u128(0xA),
    );
    a.gps_user
        .set(manual(1.0), "2026-06-01T12:00:00Z", Uuid::from_u128(0xA));
    let mut b = base.clone();
    b.capture_timestamp_user.set(
        "2020-01-01T00:00:00Z".to_string(),
        "2026-06-01T11:00:00Z",
        Uuid::from_u128(0xB),
    );
    // Same instant as A's location write: the larger device id breaks the tie.
    b.gps_user
        .set(manual(2.0), "2026-06-01T12:00:00Z", Uuid::from_u128(0xB));

    let merge = |into: &mut SidecarV1, from: &SidecarV1| {
        into.capture_timestamp_user
            .merge(&from.capture_timestamp_user);
        into.gps_user.merge(&from.gps_user);
    };
    let mut ab = a.clone();
    merge(&mut ab, &b);
    let mut ba = b.clone();
    merge(&mut ba, &a);
    for merged in [&ab, &ba] {
        assert_eq!(merged.effective_capture_timestamp(), "2020-01-01T00:00:00Z");
        assert_eq!(merged.effective_gps(), manual(2.0).as_ref());
        assert_eq!(merged.capture_timestamp, base.capture_timestamp);
        assert_eq!(merged.gps, base.gps);
    }
    assert_eq!(ab.to_canonical_vec(), ba.to_canonical_vec());

    // Replaying the losing side does not change the outcome.
    let mut replay = ab.clone();
    merge(&mut replay, &a);
    assert_eq!(
        replay.effective_capture_timestamp(),
        ab.effective_capture_timestamp()
    );
    assert_eq!(replay.effective_gps(), ab.effective_gps());
}

/// Datum-verbatim storage (WGS-84 arm): a `Wgs84` fix omits the `datum` key, so
/// the encoded `gps` value is byte-identical to the pre-`datum` known-answer shape
/// (`{lat, lon, source}`), and the whole sidecar carries no `datum` byte on the wire.
#[test]
fn wgs84_datum_is_wire_absent_and_byte_identical() {
    // The pre-`datum` known-answer wire shape of a fix: exactly `{lat, lon, source}`.
    #[derive(Serialize)]
    struct LegacyGps {
        lat: f64,
        lon: f64,
        source: GpsSource,
    }
    let legacy = LegacyGps {
        lat: 40.7128,
        lon: -74.0060,
        source: GpsSource::Exif,
    };
    let current = Gps {
        lat: 40.7128,
        lon: -74.0060,
        source: GpsSource::Exif,
        datum: GpsDatum::Wgs84,
    };
    let mut legacy_bytes = Vec::new();
    ciborium::ser::into_writer(&legacy, &mut legacy_bytes).unwrap();
    let mut current_bytes = Vec::new();
    ciborium::ser::into_writer(&current, &mut current_bytes).unwrap();
    assert_eq!(
        legacy_bytes, current_bytes,
        "a wgs84 datum must be wire-absent → byte-identical to the pre-datum vector"
    );

    // Whole-sidecar: the `datum` key never appears, and the fix decodes back to wgs84.
    let s = minimal();
    let bytes = s.to_canonical_vec();
    let needle = b"datum";
    assert!(
        !bytes.windows(needle.len()).any(|w| w == needle),
        "an absent (wgs84) datum must not appear on the wire"
    );
    let back = SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).unwrap();
    assert_eq!(back, s);
    assert_eq!(back.gps.as_ref().unwrap().datum, GpsDatum::Wgs84);
}

/// Datum-verbatim storage (GCJ-02 arm): a populated-`datum` vector. A `gcj02` fix
/// round-trips **unconverted** (lat/lon byte-for-byte unchanged), carries the `datum`
/// key on the wire, and survives signing.
#[test]
fn gcj02_datum_round_trips_unconverted_and_survives_signing() {
    let ik = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
    let mut s = minimal();
    // A user-entered coordinate that arrived already in China's GCJ-02 datum.
    s.gps = Some(Gps {
        lat: 39.90869,
        lon: 116.39745,
        source: GpsSource::Manual,
        datum: GpsDatum::Gcj02,
    });
    s.sign(&ik);
    assert!(s.verify(&ik.verifying_key()));

    let bytes = s.to_canonical_vec();
    // Populated datum is present on the wire.
    let needle = b"datum";
    assert!(
        bytes.windows(needle.len()).any(|w| w == needle),
        "a gcj02 datum must appear on the wire"
    );
    let back = SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).unwrap();
    assert_eq!(back, s);
    let gps = back.gps.as_ref().unwrap();
    // Stored verbatim — never converted at rest.
    assert_eq!(gps.datum, GpsDatum::Gcj02);
    assert_eq!(gps.lat.to_bits(), 39.90869_f64.to_bits());
    assert_eq!(gps.lon.to_bits(), 116.39745_f64.to_bits());
    assert!(back.verify(&ik.verifying_key()));
}

/// Datum-verbatim storage (BD-09 arm): BD-09 is **never a storable datum**. A
/// BD-09 input is folded at the edge and enters the sidecar as `datum = gcj02`, within
/// the documented sub-metre bound and deterministically — the same input yields a
/// byte-identical sidecar every time.
///
/// The known-answer pair is the GCJ-02 anchor from the arm above pushed through the
/// closed-form *forward* GCJ-02 → BD-09 transform; folding it must land back on the
/// anchor.
#[test]
fn bd09_input_is_folded_to_gcj02_within_bound_and_deterministically() {
    use crate::domain::{BD09_FOLD_BOUND_METRES, Bd09Coord, fold_bd09_to_gcj02};

    /// The GCJ-02 point the BD-09 input below is the forward image of.
    const GCJ02_TRUTH: (f64, f64) = (39.90869, 116.39745);
    const BD09_INPUT: Bd09Coord = Bd09Coord {
        lat: 39.915_033_233_576_175,
        lon: 116.403_823_024_761_45,
    };
    /// Metres per degree of latitude, for expressing the residual as a distance.
    const METRES_PER_DEGREE: f64 = 111_320.0;

    let (lat, lon) = fold_bd09_to_gcj02(BD09_INPUT).unwrap();

    // Within the documented sub-metre bound of the true GCJ-02 point.
    let d_lat = (lat - GCJ02_TRUTH.0) * METRES_PER_DEGREE;
    let d_lon = (lon - GCJ02_TRUTH.1) * METRES_PER_DEGREE * lat.to_radians().cos();
    let error_metres = d_lat.hypot(d_lon);
    assert!(
        error_metres < BD09_FOLD_BOUND_METRES,
        "BD-09 fold must land within {BD09_FOLD_BOUND_METRES} m, got {error_metres} m"
    );

    // BD-09 never reaches the wire: what is stored is the folded GCJ-02 pair.
    let stored = |lat: f64, lon: f64| {
        let mut s = minimal();
        s.gps = Some(Gps {
            lat,
            lon,
            source: GpsSource::Manual,
            datum: GpsDatum::Gcj02,
        });
        s
    };
    let s = stored(lat, lon);
    let bytes = s.to_canonical_vec();
    let back = SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).unwrap();
    let gps = back.gps.as_ref().unwrap();
    assert_eq!(
        gps.datum,
        GpsDatum::Gcj02,
        "a folded BD-09 input stores as gcj02"
    );
    assert_ne!(
        gps.lat.to_bits(),
        BD09_INPUT.lat.to_bits(),
        "BD-09 must never be stored verbatim"
    );
    assert_ne!(
        gps.lon.to_bits(),
        BD09_INPUT.lon.to_bits(),
        "BD-09 must never be stored verbatim"
    );

    // Deterministic: an independent fold of the same input encodes byte-identically.
    let (again_lat, again_lon) = fold_bd09_to_gcj02(BD09_INPUT).unwrap();
    assert_eq!(
        bytes,
        stored(again_lat, again_lon).to_canonical_vec(),
        "same BD-09 input must yield a byte-identical sidecar"
    );
}
