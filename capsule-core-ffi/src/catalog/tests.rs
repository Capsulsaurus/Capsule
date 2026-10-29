use std::sync::atomic::{AtomicU32, Ordering};

use super::*;

/// A platform gate that always succeeds, counting the challenges it saw.
struct AllowGate {
    calls: AtomicU32,
}
impl AllowGate {
    fn new() -> Self {
        Self {
            calls: AtomicU32::new(0),
        }
    }
    fn calls(&self) -> u32 {
        self.calls.load(Ordering::Relaxed)
    }
}
impl LocalAuthGate for AllowGate {
    fn authenticate(&self, _view: GatedView) -> Result<(), LocalAuthError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

/// A platform gate that always refuses with a fixed error.
struct DenyGate(LocalAuthError);
impl LocalAuthGate for DenyGate {
    fn authenticate(&self, _view: GatedView) -> Result<(), LocalAuthError> {
        Err(self.0.clone())
    }
}

/// Unlock a view with a gate that always allows, for tests about something else.
fn unlock(cat: &Catalog, view: GatedView) {
    cat.unlock_view(view, Arc::new(AllowGate::new()))
        .expect("AllowGate never refuses");
}

fn asset(uuid: &str, hash: &str) -> AssetRecord {
    AssetRecord {
        uuid: uuid.to_string(),
        asset_type: "photo".to_string(),
        capture_timestamp: 1_720_000_000,
        capture_utc: Some(1_719_997_200),
        capture_tz_source: Some("offset_exif".to_string()),
        import_timestamp: 1_720_000_000,
        hash_sha256: hash.to_string(),
        width: Some(4032),
        height: Some(3024),
        duration_ms: None,
        stack_id: None,
        is_stack_hidden: false,
        chromahash: None,
        dominant_color: None,
        album_id: None,
        rating: 0,
        is_deleted: false,
        deleted_at: None,
        is_hidden: false,
        caption: None,
        latitude: None,
        longitude: None,
    }
}

/// A row as the index projects a trashed asset. The catalog has no trash write of its own,
/// so a test about reading the trash inserts the row a workspace `delete` would have left.
fn trashed(uuid: &str, hash: &str) -> AssetRecord {
    AssetRecord {
        is_deleted: true,
        deleted_at: Some(1_720_000_100),
        ..asset(uuid, hash)
    }
}

#[test]
fn test_open_in_memory_and_insert() {
    let cat = Catalog::open_in_memory().unwrap();
    assert!(cat.schema_version().unwrap() >= 2);

    cat.insert_asset(asset("u1", &"a".repeat(64))).unwrap();
    let found = cat.find_by_hash("a".repeat(64)).unwrap();
    assert_eq!(found.unwrap().uuid, "u1");

    let timeline = cat.query_timeline(0, 100).unwrap();
    assert_eq!(timeline.len(), 1);
}

#[test]
fn test_query_trash_lists_only_trashed_rows() {
    let cat = Catalog::open_in_memory().unwrap();
    unlock(&cat, GatedView::RecentlyDeleted);
    cat.insert_asset(asset("u2", &"b".repeat(64))).unwrap();
    assert!(cat.query_trash(0, 100).unwrap().is_empty());

    cat.insert_asset(trashed("u1", &"a".repeat(64))).unwrap();
    let trash = cat.query_trash(0, 100).unwrap();
    assert_eq!(trash.len(), 1);
    assert_eq!(trash[0].uuid, "u1");
    // Listed in the trash but absent from the timeline.
    let timeline = cat.query_timeline(0, 100).unwrap();
    assert_eq!(timeline.len(), 1);
    assert_eq!(timeline[0].uuid, "u2");
}

/// The Apple managed store's path: an asset sealed and deleted through the workspace verbs
/// is trashed in the index the `Catalog` reads, and stays trashed after that index is lost
/// and rebuilt, because the delete is a signed record rather than a flag on the row.
#[test]
fn a_workspace_delete_is_trashed_in_the_catalog_and_survives_an_index_rebuild() {
    use capsule_sdk::ffi::{FfiClientBuild, FfiDeviceTier, FfiWorkspace};

    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().join("library");
    let index = root.join("index/library.sqlite");
    let (asset_id, kept_id) = {
        let workspace = FfiWorkspace::create(
            root.to_string_lossy().into_owned(),
            b"test-passphrase".to_vec(),
            FfiDeviceTier::LowRam,
            FfiClientBuild {
                client_id: "capsule-core-ffi-test".into(),
                semver: "0.0.1".into(),
            },
        )
        .unwrap();
        let album = workspace.default_album_id().unwrap();
        let album = workspace.ensure_album(album, "Camera Roll".into()).unwrap();
        let asset_id = workspace
            .seal_asset(album.clone(), "IMG_0001.JPG".into(), b"trashed".to_vec())
            .unwrap();
        let kept_id = workspace
            .seal_asset(album, "IMG_0002.JPG".into(), b"kept".to_vec())
            .unwrap();
        workspace.delete_asset(asset_id.clone(), 30).unwrap();
        (asset_id, kept_id)
    };

    let assert_trashed = |when: &str| {
        let cat = Catalog::open(index.to_string_lossy().into_owned()).unwrap();
        let timeline = cat.query_timeline(0, 100).unwrap();
        assert_eq!(
            timeline.iter().map(|r| r.uuid.clone()).collect::<Vec<_>>(),
            vec![kept_id.clone()],
            "{when}: only the kept asset is on the timeline"
        );
        unlock(&cat, GatedView::RecentlyDeleted);
        let trash = cat.query_trash(0, 100).unwrap();
        assert_eq!(trash.len(), 1, "{when}: one asset in the trash");
        assert_eq!(trash[0].uuid, asset_id, "{when}");
        assert!(trash[0].is_deleted, "{when}");
    };
    assert_trashed("before the rebuild");

    for suffix in ["", "-wal", "-shm"] {
        let file = root.join(format!("index/library.sqlite{suffix}"));
        if file.exists() {
            std::fs::remove_file(file).unwrap();
        }
    }
    let library = capsule_library::library::open_library(&root).unwrap();
    capsule_library::library::rebuild_index(&library).unwrap();
    drop(library);
    assert_trashed("after the rebuild");
}

#[test]
fn test_album_membership() {
    let cat = Catalog::open_in_memory().unwrap();
    cat.insert_album(AlbumRecord {
        id: "alb-1".to_string(),
        name: "Trip".to_string(),
        created_at: 1_720_000_000,
        modified_at: 1_720_000_000,
        cover_asset_id: None,
    })
    .unwrap();
    cat.insert_asset(asset("u1", &"a".repeat(64))).unwrap();
    cat.set_asset_album("u1".to_string(), Some("alb-1".to_string()))
        .unwrap();

    assert_eq!(
        cat.query_album_assets("alb-1".to_string(), 0, 100)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(cat.list_albums().unwrap().len(), 1);

    cat.delete_album("alb-1".to_string()).unwrap();
    assert!(cat.find_album("alb-1".to_string()).unwrap().is_none());
    // The asset itself survives album deletion.
    assert!(cat.find_by_uuid("u1".to_string()).unwrap().is_some());
}

#[test]
fn test_query_timeline_filtered_by_type() {
    let cat = Catalog::open_in_memory().unwrap();
    cat.insert_asset(asset("p1", &"a".repeat(64))).unwrap();
    let mut video = asset("v1", &"b".repeat(64));
    video.asset_type = "video".to_string();
    cat.insert_asset(video).unwrap();

    let videos = cat
        .query_timeline_filtered(Some("video".to_string()), None, None, 0, 100)
        .unwrap();
    assert_eq!(videos.len(), 1);
    assert_eq!(videos[0].uuid, "v1");
}

// ── SR1 gated views ────────────────────────────────────────
//
// The FFI mirror of `capsule-core`'s
// `gated_hidden_query_refuses_without_grant_and_serves_with_one`: the surface the
// native apps actually call must enforce the same gate the Rust surface does.

/// The acceptance case: the FFI trash listing refuses without a grant, serves with
/// one, and a grant for the *other* gated view is not a substitute.
#[test]
fn trash_listing_refuses_without_grant_and_serves_with_one() {
    let cat = Catalog::open_in_memory().unwrap();
    // One soft-deleted asset, plus a live one that must never appear in the trash.
    cat.insert_asset(trashed("u1", &"a".repeat(64))).unwrap();
    cat.insert_asset(asset("u2", &"b".repeat(64))).unwrap();

    // Refuses before a grant.
    assert!(!cat.is_view_unlocked(GatedView::RecentlyDeleted));
    assert!(matches!(
        cat.query_trash(0, 100),
        Err(CatalogError::ViewLocked)
    ));

    // Serves the soft-deleted asset once a grant exists.
    unlock(&cat, GatedView::RecentlyDeleted);
    assert!(cat.is_view_unlocked(GatedView::RecentlyDeleted));
    let trash = cat.query_trash(0, 100).unwrap();
    assert_eq!(trash.len(), 1);
    assert_eq!(trash[0].uuid, "u1");
    assert!(trash[0].is_deleted);

    // A Hidden grant does NOT unlock the trash — grants are per-view.
    cat.relock_view(GatedView::RecentlyDeleted);
    unlock(&cat, GatedView::Hidden);
    assert!(cat.is_view_unlocked(GatedView::Hidden));
    assert!(matches!(
        cat.query_trash(0, 100),
        Err(CatalogError::ViewLocked)
    ));
}

/// A platform refusal is passed through unchanged, mints no grant, and leaves the
/// listing locked — the FFI mirror of
/// `open_propagates_platform_denial_and_stays_locked`.
#[test]
fn platform_denial_is_passed_through_and_leaves_the_view_locked() {
    let cat = Catalog::open_in_memory().unwrap();
    cat.insert_asset(trashed("u1", &"a".repeat(64))).unwrap();

    for refusal in [
        LocalAuthError::Cancelled,
        LocalAuthError::Unavailable,
        LocalAuthError::Failed,
    ] {
        let err = cat
            .unlock_view(
                GatedView::RecentlyDeleted,
                Arc::new(DenyGate(refusal.clone())),
            )
            .unwrap_err();
        assert_eq!(err, refusal);
        assert!(!cat.is_view_unlocked(GatedView::RecentlyDeleted));
        assert!(matches!(
            cat.query_trash(0, 100),
            Err(CatalogError::ViewLocked)
        ));
    }
}

/// Re-unlocking inside the grace window reuses the live grant instead of challenging
/// the platform again; `lock_views` drops every grant and re-locks the listing.
#[test]
fn grant_is_reused_within_grace_and_dropped_by_lock_views() {
    let cat = Catalog::open_in_memory().unwrap();
    cat.insert_asset(trashed("u1", &"a".repeat(64))).unwrap();

    let gate = Arc::new(AllowGate::new());
    cat.unlock_view(GatedView::RecentlyDeleted, gate.clone())
        .unwrap();
    cat.unlock_view(GatedView::RecentlyDeleted, gate.clone())
        .unwrap();
    assert_eq!(gate.calls(), 1, "re-unlock within grace does not re-auth");
    assert_eq!(cat.query_trash(0, 100).unwrap().len(), 1);

    // Backgrounding the app drops every grant.
    unlock(&cat, GatedView::Hidden);
    cat.lock_views();
    assert!(!cat.is_view_unlocked(GatedView::RecentlyDeleted));
    assert!(!cat.is_view_unlocked(GatedView::Hidden));
    assert!(matches!(
        cat.query_trash(0, 100),
        Err(CatalogError::ViewLocked)
    ));

    // A fresh unlock re-authenticates and serves again.
    cat.unlock_view(GatedView::RecentlyDeleted, gate.clone())
        .unwrap();
    assert_eq!(gate.calls(), 2, "unlock after lock_views re-auths");
    assert_eq!(cat.query_trash(0, 100).unwrap().len(), 1);
}

/// The audit's other half: the *default* projections stay ungated and must therefore
/// never leak what the gates protect. Deleted and hidden assets are excluded from the
/// timeline, the filtered timeline, and album listings with no grant in sight.
#[test]
fn default_projections_are_ungated_and_leak_neither_deleted_nor_hidden() {
    let cat = Catalog::open_in_memory().unwrap();
    cat.insert_album(AlbumRecord {
        id: "alb-1".to_string(),
        name: "Trip".to_string(),
        created_at: 1_720_000_000,
        modified_at: 1_720_000_000,
        cover_asset_id: None,
    })
    .unwrap();

    cat.insert_asset(asset("visible", &"a".repeat(64))).unwrap();
    cat.insert_asset(trashed("deleted", &"b".repeat(64)))
        .unwrap();
    let mut hidden = asset("hidden", &"c".repeat(64));
    hidden.is_hidden = true;
    cat.insert_asset(hidden).unwrap();
    for uuid in ["visible", "deleted", "hidden"] {
        cat.set_asset_album(uuid.to_string(), Some("alb-1".to_string()))
            .unwrap();
    }

    // No grant anywhere.
    assert!(!cat.is_view_unlocked(GatedView::RecentlyDeleted));
    assert!(!cat.is_view_unlocked(GatedView::Hidden));

    let only_visible = |rows: Vec<AssetRecord>| {
        assert_eq!(rows.len(), 1, "expected only the visible asset");
        assert_eq!(rows[0].uuid, "visible");
    };
    only_visible(cat.query_timeline(0, 100).unwrap());
    only_visible(
        cat.query_timeline_filtered(Some("photo".to_string()), None, None, 0, 100)
            .unwrap(),
    );
    only_visible(cat.query_album_assets("alb-1".to_string(), 0, 100).unwrap());

    // Point lookups are addressed by an identifier the caller already holds, not
    // listings, so they stay ungated by design — asserted so the choice is explicit.
    assert!(cat.find_by_uuid("deleted".to_string()).unwrap().is_some());
    assert!(cat.find_by_uuid("hidden".to_string()).unwrap().is_some());
}

// ── LQIP placeholder rendering ──────────────────────────────────────────

/// The native surface decodes the same bytes the import pipeline encoded, to the same
/// pixels `capsule_core::lqip` produces — the cross-surface criterion at the FFI boundary.
#[test]
fn render_lqip_matches_the_core_decoder() {
    use capsule_core::lqip::{Gamut, LQIP_FORMAT_V1, Lqip};

    let (w, h) = (120u32, 90u32);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            rgba.extend_from_slice(&[(x * 2) as u8, (y * 2) as u8, 128, 255]);
        }
    }
    let lqip = Lqip::encode(w, h, &rgba, Gamut::Srgb).expect("encode");
    assert_eq!(lqip.as_bytes().len(), 32, "the committed tier is 32 bytes");

    let expected = lqip.decode_capped(48, 48);
    let got = render_lqip(
        LQIP_FORMAT_V1,
        lqip.as_bytes().to_vec(),
        lqip.dominant_color().to_vec(),
        48,
        48,
    );
    assert_eq!((got.width, got.height), (expected.width, expected.height));
    assert_eq!(got.rgba, expected.rgba, "byte-identical to the core");
    assert_eq!(got.rgba.len() as u32, got.width * got.height * 4);
}

/// Every malformed input paints the fallback rather than failing: an unknown version, a
/// corrupt payload, and a `dominant_color` that is not three bytes.
#[test]
fn render_lqip_never_fails_on_a_malformed_record() {
    use capsule_core::lqip::LQIP_FORMAT_V1;

    let fill = vec![9u8, 8, 7];
    let unknown = render_lqip(LQIP_FORMAT_V1 + 42, vec![1, 2, 3], fill.clone(), 32, 32);
    assert_eq!((unknown.width, unknown.height), (1, 1));
    assert_eq!(unknown.rgba, vec![9, 8, 7, 255]);

    let corrupt = render_lqip(LQIP_FORMAT_V1, vec![0xDE, 0xAD], fill, 32, 32);
    assert_eq!(corrupt.rgba, vec![9, 8, 7, 255]);

    // No usable fill: black, the conventional empty-cell colour.
    let no_fill = render_lqip(LQIP_FORMAT_V1, vec![0xDE, 0xAD], vec![1, 2], 32, 32);
    assert_eq!((no_fill.width, no_fill.height), (1, 1));
    assert_eq!(no_fill.rgba, vec![0, 0, 0, 255]);
}

/// An import's caption and GPS fix cross the boundary on the record and read back intact.
#[test]
fn caption_and_location_round_trip_on_the_record() {
    let cat = Catalog::open_in_memory().unwrap();
    let mut rec = asset("u1", &"a".repeat(64));
    rec.caption = Some("harbour".to_string());
    rec.latitude = Some(-33.8568);
    rec.longitude = Some(151.2153);
    cat.insert_asset(rec.clone()).unwrap();
    assert_eq!(cat.find_by_uuid("u1".to_string()).unwrap(), Some(rec));
}

/// `set_caption` sets and clears the caption, and refuses an unknown asset with
/// `InvalidArgument` rather than reporting a caption saved that nothing holds.
#[test]
fn set_caption_sets_clears_and_refuses_an_unknown_asset() {
    let cat = Catalog::open_in_memory().unwrap();
    cat.insert_asset(asset("u1", &"a".repeat(64))).unwrap();
    let caption = |cat: &Catalog| cat.find_by_uuid("u1".to_string()).unwrap().unwrap().caption;

    cat.set_caption("u1".to_string(), Some("on the pier".to_string()))
        .unwrap();
    assert_eq!(caption(&cat).as_deref(), Some("on the pier"));

    cat.set_caption("u1".to_string(), None).unwrap();
    assert_eq!(caption(&cat), None);

    cat.set_caption("u1".to_string(), Some("x".to_string()))
        .unwrap();
    cat.set_caption("u1".to_string(), Some(String::new()))
        .unwrap();
    assert_eq!(caption(&cat), None, "an empty caption is no caption");

    let err = cat
        .set_caption("missing".to_string(), Some("x".to_string()))
        .unwrap_err();
    assert!(
        matches!(err, CatalogError::InvalidArgument { .. }),
        "{err:?}"
    );
}
