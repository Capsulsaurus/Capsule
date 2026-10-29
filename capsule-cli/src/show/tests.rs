use capsule_core::crypto::primitives::Argon2Params;

use super::*;

const FAST_KDF: Argon2Params = Argon2Params {
    mem_kib: 64,
    t_cost: 1,
    p_cost: 1,
};

/// A fast-cost workspace with `count` imported assets of distinct bytes, in a scratch
/// directory that is removed on drop.
struct Fixture {
    dir: std::path::PathBuf,
    ws: Workspace,
    ids: Vec<Uuid>,
}

impl Fixture {
    fn with_assets(count: usize) -> Self {
        let dir = std::env::temp_dir().join(format!("capsule-cli-show-{}", nanoid::nanoid!()));
        let lib = dir.join("lib");
        std::fs::create_dir_all(&lib).expect("scratch library dir");
        let mut ws =
            Workspace::create_with_params(&lib, b"pw", FAST_KDF).expect("create workspace");
        let album = ws.default_album_id();
        ws.create_album_with_id(album, "Imports")
            .expect("create album");
        let ids = (0..count)
            .map(|n| {
                let src = dir.join(format!("photo-{n}.jpg"));
                let mut bytes = b"\xFF\xD8\xFF".to_vec();
                bytes.extend_from_slice(format!(" asset {n}").as_bytes());
                std::fs::write(&src, bytes).expect("fixture file");
                ws.import_asset(album, &src).expect("import")
            })
            .collect();
        Self { dir, ws, ids }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Three candidates whose hashes share a 10-character prefix, so ambiguity is a fact
/// about the listing rather than a lottery over real digests.
fn candidates() -> Vec<(Uuid, String)> {
    let ids = [Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()];
    vec![
        (ids[0], format!("0123456789{}", "a".repeat(54))),
        (ids[1], format!("0123456789{}", "b".repeat(54))),
        (ids[2], format!("fedcba9876{}", "c".repeat(54))),
    ]
}

#[test]
fn an_asset_id_resolves_to_itself_in_any_spelling() {
    let list = candidates();
    let id = list[1].0;
    assert_eq!(resolve_among(list.clone(), &id.to_string()), Ok(id));
    assert_eq!(
        resolve_among(list.clone(), &format!("  {}  ", id.simple())),
        Ok(id),
        "the simple (hyphen-less) spelling and surrounding whitespace are tolerated"
    );
    assert_eq!(
        resolve_among(list, &id.to_string().to_ascii_uppercase()),
        Ok(id)
    );
}

#[test]
fn a_hash_prefix_resolves_to_the_one_candidate_carrying_it() {
    let list = candidates();
    assert_eq!(resolve_among(list.clone(), "0123456789a"), Ok(list[0].0));
    assert_eq!(
        resolve_among(list.clone(), "0123456789B"),
        Ok(list[1].0),
        "case-folded"
    );
    assert_eq!(resolve_among(list.clone(), "fedcba98"), Ok(list[2].0));
    let (expected, full) = list[2].clone();
    assert_eq!(resolve_among(list, &full), Ok(expected), "the whole hash");
}

#[test]
fn an_ambiguous_prefix_is_refused_with_the_count() {
    let list = candidates();
    assert_eq!(
        resolve_among(list, "01234567"),
        Err(ShowError::Ambiguous {
            selector: "01234567".into(),
            count: 2
        })
    );
}

#[test]
fn a_short_or_non_hex_selector_is_invalid_and_a_missing_one_is_unknown() {
    let list = candidates();
    assert_eq!(
        resolve_among(list.clone(), "abc"),
        Err(ShowError::InvalidSelector("abc".into()))
    );
    assert_eq!(
        resolve_among(list.clone(), "not-a-hash-or-id"),
        Err(ShowError::InvalidSelector("not-a-hash-or-id".into()))
    );
    assert_eq!(
        resolve_among(list.clone(), "deadbeef00"),
        Err(ShowError::UnknownAsset("deadbeef00".into()))
    );
    let ghost = Uuid::now_v7();
    assert_eq!(
        resolve_among(list, &ghost.to_string()),
        Err(ShowError::UnknownAsset(ghost.to_string()))
    );
}

/// A 32-hex-digit hash prefix parses as a bare UUID; it must still reach the prefix arm.
#[test]
fn a_thirty_two_digit_hash_prefix_is_not_mistaken_for_an_unknown_id() {
    let list = candidates();
    let prefix = &list[2].1[..32];
    assert!(
        Uuid::parse_str(prefix).is_ok(),
        "the premise: it parses as a UUID"
    );
    assert_eq!(resolve_among(list.clone(), prefix), Ok(list[2].0));
}

#[test]
fn resolve_reads_the_workspaces_sidecar_hashes() {
    let fx = Fixture::with_assets(2);
    for id in &fx.ids {
        let hex = fx.ws.asset(id).expect("asset").sidecar.hash.to_hex();
        assert_eq!(resolve(&fx.ws, &hex[..MIN_HASH_PREFIX]), Ok(*id));
        assert_eq!(resolve(&fx.ws, &id.to_string()), Ok(*id));
    }
}

#[test]
fn collect_projects_the_signed_sidecar_and_the_chain() {
    let fx = Fixture::with_assets(1);
    let id = fx.ids[0];
    let asset = fx.ws.asset(&id).expect("asset");
    let view = collect(&fx.ws, &id).expect("managed");
    assert_eq!(view.asset_id, id);
    assert_eq!(view.album_id, fx.ws.default_album_id());
    assert_eq!(view.content_type, asset.sidecar.content_type);
    assert_eq!(view.hash, asset.sidecar.hash.to_hex());
    assert_eq!(view.capture_timestamp, asset.sidecar.capture_timestamp);
    assert_eq!(view.capture_timestamp_imported, None, "nothing corrected");
    assert!(!view.location_edited);
    assert_eq!(view.caption, None);
    assert_eq!(view.rating, None);
    assert!(view.tags_user.is_empty());
    assert_eq!(view.cull, CullFlag::Neutral);
    assert!(!view.hidden);
    assert!(!view.in_trash);
    assert_eq!(view.stack, None);
    assert_eq!(
        collect(&fx.ws, &Uuid::now_v7()),
        None,
        "an unknown id has no view"
    );
    assert!(!view.lqip);
    assert_eq!(view.provenance_records, asset.chain.records().len());
}

#[test]
fn collect_reflects_metadata_edits() {
    let mut fx = Fixture::with_assets(1);
    let id = fx.ids[0];
    let before = collect(&fx.ws, &id).expect("managed").provenance_records;
    fx.ws.set_caption(&id, "On the beach").expect("caption");
    fx.ws.tag_add(&id, "Vacation 2021").expect("tag");
    fx.ws.set_cull(&id, CullFlag::Pick).expect("cull");
    let view = collect(&fx.ws, &id).expect("managed");
    assert_eq!(view.caption.as_deref(), Some("On the beach"));
    assert_eq!(view.tags_user, vec!["Vacation 2021".to_string()]);
    assert_eq!(view.cull, CullFlag::Pick);
    assert_eq!(
        view.provenance_records,
        before + 3,
        "three signed metadata-updates on top of the create"
    );
}

/// A capture correction and a location edit show as the effective values, with the
/// as-imported ones carried beside them.
#[test]
fn collect_shows_the_correction_beside_the_imported_value() {
    let mut fx = Fixture::with_assets(1);
    let id = fx.ids[0];
    let imported = fx
        .ws
        .asset(&id)
        .expect("asset")
        .sidecar
        .capture_timestamp
        .clone();
    let corrected = jiff::Timestamp::from_second(1_000_000_000).expect("in range");
    fx.ws
        .set_capture_timestamp(&id, corrected)
        .expect("correct");
    fx.ws
        .set_location(&id, 51.5, -0.12, GpsDatum::Wgs84)
        .expect("locate");
    let view = collect(&fx.ws, &id).expect("managed");
    assert_eq!(view.capture_timestamp, corrected.to_string());
    assert_eq!(view.capture_timestamp_imported, Some(imported));
    assert!(view.location_edited);
    assert_eq!(view.gps_imported, None, "the bytes carried no fix");
    let fix = view.gps.expect("the edit is effective");
    assert_eq!(
        (fix.source, fix.datum),
        (GpsSource::Manual, GpsDatum::Wgs84)
    );

    fx.ws.clear_location(&id).expect("clear");
    let view = collect(&fx.ws, &id).expect("managed");
    assert!(view.location_edited);
    assert_eq!(view.gps, None);
}

fn sample_view() -> AssetView {
    AssetView {
        asset_id: Uuid::nil(),
        album_id: Uuid::max(),
        content_type: "image/jpeg".into(),
        hash: "ab".repeat(32),
        dimensions: Some((8, 8)),
        capture_timestamp: "2019-03-04T05:06:07Z".into(),
        capture_timestamp_imported: None,
        import_timestamp: "2026-09-02T00:00:00Z".into(),
        caption: Some("On the beach".into()),
        rating: Some(5),
        tags_user: vec!["Vacation 2021".into(), "beach".into()],
        tags_ai: vec![],
        gps: Some(Fix {
            lat: 10.0,
            lon: 20.0,
            source: GpsSource::Exif,
            datum: GpsDatum::Wgs84,
        }),
        location_edited: false,
        gps_imported: Some(Fix {
            lat: 10.0,
            lon: 20.0,
            source: GpsSource::Exif,
            datum: GpsDatum::Wgs84,
        }),
        cull: CullFlag::Neutral,
        hidden: false,
        in_trash: false,
        stack: None,
        lqip: false,
        provenance_records: 1,
    }
}

/// Every field the guide asks a user to verify is on the page, and every absent value is
/// spelled out rather than omitted.
#[test]
fn render_prints_every_field_and_spells_out_absent_values() {
    let page = render(&sample_view());
    for expected in [
        "00000000-0000-0000-0000-000000000000",
        "image/jpeg",
        &"ab".repeat(32),
        "8×8",
        "2019-03-04T05:06:07Z",
        "On the beach",
        "5/5",
        "Vacation 2021, beach",
        "10.000000, 20.000000 (WGS-84, EXIF)",
        "neutral",
    ] {
        assert!(page.contains(expected), "missing {expected:?} in:\n{page}");
    }
    // AI tags, stack and LQIP are absent in the sample: three unset rows.
    assert_eq!(page.matches(UNSET).count(), 3, "{page}");
    assert_eq!(
        page.lines().count(),
        18,
        "a header plus seventeen rows:\n{page}"
    );
    assert!(
        page.contains("\n  Content type:    image/jpeg\n"),
        "labels are padded to one column:\n{page}"
    );
    assert!(
        page.contains("\n  Provenance:      1 signed record(s)\n"),
        "{page}"
    );
}

#[test]
fn render_names_a_stack_placement_a_gcj02_manual_fix_and_the_flags() {
    let stack_id = Uuid::now_v7();
    let view = AssetView {
        gps: Some(Fix {
            lat: 39.9,
            lon: 116.4,
            source: GpsSource::Manual,
            datum: GpsDatum::Gcj02,
        }),
        stack: Some((stack_id, StackRole::Primary)),
        hidden: true,
        in_trash: true,
        lqip: true,
        dimensions: None,
        caption: None,
        ..sample_view()
    };
    let page = render(&view);
    assert!(
        page.contains("39.900000, 116.400000 (GCJ-02, manual)"),
        "a non-default datum is named, never silently read as WGS-84:\n{page}"
    );
    assert!(page.contains(&format!("{stack_id} (primary)")), "{page}");
    assert_eq!(
        page.matches("yes").count(),
        2,
        "hidden and in trash:\n{page}"
    );
    assert!(page.contains("  Placeholder:     present\n"), "{page}");
}

/// An edited asset prints the as-imported capture time and fix on their own rows, right
/// after the effective ones; an imported fix the user cleared reads as unset above it.
#[test]
fn render_prints_the_imported_values_beside_an_edit() {
    let view = AssetView {
        capture_timestamp: "2001-09-09T01:46:40Z".into(),
        capture_timestamp_imported: Some("2019-03-04T05:06:07Z".into()),
        gps: None,
        location_edited: true,
        ..sample_view()
    };
    let page = render(&view);
    assert!(
        page.contains(
            "  Captured:        2001-09-09T01:46:40Z\n  Orig. capture:   2019-03-04T05:06:07Z\n"
        ),
        "{page}"
    );
    assert!(
        page.contains(
            "  GPS:             (unset)\n  Orig. GPS:       10.000000, 20.000000 (WGS-84, EXIF)\n"
        ),
        "{page}"
    );
    assert_eq!(page.lines().count(), 20, "two more rows:\n{page}");
}

#[test]
fn describe_error_names_each_variant_with_its_detail() {
    let ambiguous = describe_error(&ShowError::Ambiguous {
        selector: "abcdef01".into(),
        count: 3,
    });
    assert!(
        ambiguous.contains("abcdef01") && ambiguous.contains('3'),
        "{ambiguous}"
    );
    let invalid = describe_error(&ShowError::InvalidSelector("zz".into()));
    assert!(
        invalid.contains("zz") && invalid.contains(&MIN_HASH_PREFIX.to_string()),
        "{invalid}"
    );
    let unknown = describe_error(&ShowError::UnknownAsset("deadbeef".into()));
    assert!(unknown.contains("deadbeef"), "{unknown}");
}

/// A swept asset prints the trash row: the trash fact comes from
/// `Workspace::is_trashed`, the one place the chain is replayed.
#[test]
fn a_swept_asset_shows_in_trash() {
    let mut fx = Fixture::with_assets(1);
    let id = fx.ids[0];
    fx.ws.set_cull(&id, CullFlag::Reject).expect("cull");
    let swept = fx.ws.reject_sweep(30).expect("sweep");
    assert_eq!(swept, vec![id]);
    let view = collect(&fx.ws, &id).expect("a trashed asset is still managed");
    assert!(view.in_trash);
    let page = render(&view);
    assert!(page.contains("  In trash:        yes\n"), "{page}");
}

#[test]
fn every_action_has_its_manifest_spelling() {
    let cases = [
        (ProvenanceAction::Create, "create"),
        (ProvenanceAction::Replace, "replace"),
        (ProvenanceAction::Delete, "delete"),
        (ProvenanceAction::MetadataUpdate, "metadata-update"),
        (ProvenanceAction::DerivativeAdd, "derivative-add"),
        (ProvenanceAction::DerivativeReplace, "derivative-replace"),
        (ProvenanceAction::TrashRestore, "trash-restore"),
    ];
    for (action, name) in cases {
        assert_eq!(action_name(action), name);
        // The same spelling the signed manifest serialises.
        let json = serde_json::to_string(&action).expect("serialise");
        assert_eq!(json, format!("\"{name}\""));
    }
}

/// The report lists the chain record by record, oldest first, and a sweep's `delete` record
/// carries its retention deadline.
#[test]
fn report_lists_the_provenance_history_record_by_record() {
    let mut fx = Fixture::with_assets(1);
    let id = fx.ids[0];
    fx.ws.set_cull(&id, CullFlag::Reject).expect("cull");
    fx.ws.reject_sweep(30).expect("sweep");
    let shown = report(&fx.ws, &id).expect("managed");
    let actions: Vec<&str> = shown.history.iter().map(|h| h.action).collect();
    assert_eq!(actions, ["create", "metadata-update", "delete"]);
    let chain = fx.ws.asset(&id).expect("asset").chain.records();
    for (entry, record) in shown.history.iter().zip(chain) {
        let core = &record.manifest.core;
        assert_eq!(entry.timestamp, core.timestamp);
        assert_eq!(entry.device, core.created_by_device);
        assert_eq!(entry.client_version, core.client_version);
        assert_eq!(entry.retention_until, core.retention_until);
    }
    assert!(shown.history[0].retention_until.is_none());
    assert!(shown.history[2].retention_until.is_some());
    assert!(shown.in_trash);
    assert_eq!(shown.cull, "reject");
    assert!(report(&fx.ws, &Uuid::now_v7()).is_none());
}

/// Every view field is in the document, an absent one as `null` rather than left out.
#[test]
fn the_report_spells_absent_values_as_null() {
    let mut view = sample_view();
    view.caption = None;
    view.dimensions = None;
    view.gps = None;
    view.stack = Some((Uuid::nil(), StackRole::Proxy));
    let json = serde_json::to_value(ShowReport::new(&view, Vec::new())).expect("serialise");
    for key in [
        "caption",
        "dimensions",
        "location",
        "capture_timestamp_imported",
    ] {
        assert_eq!(json.get(key), Some(&serde_json::Value::Null), "{key}");
    }
    assert_eq!(json["rating"], 5);
    assert_eq!(json["cull"], "neutral");
    assert_eq!(json["stack"]["role"], "proxy");
    assert_eq!(json["location_imported"]["datum"], "WGS-84");
    assert_eq!(json["location_imported"]["source"], "EXIF");
    assert_eq!(
        json["tags_user"],
        serde_json::json!(["Vacation 2021", "beach"])
    );
    assert_eq!(json["history"], serde_json::json!([]));

    let mut view = sample_view();
    view.dimensions = Some((640, 480));
    let json = serde_json::to_value(ShowReport::new(&view, Vec::new())).expect("serialise");
    assert_eq!(
        json["dimensions"],
        serde_json::json!({"width": 640, "height": 480})
    );
}
