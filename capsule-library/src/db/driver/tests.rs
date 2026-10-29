use rusqlite::OptionalExtension;

use super::*;
use crate::db::rows::{AlbumRow, AssetRow, AssetStackRow, CachedRepresentationRow, StackMemberRow};

fn make_asset(uuid: &str, hash: &str) -> AssetRow {
    AssetRow {
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

#[test]
fn test_init_schema_idempotent() {
    let db = DatabaseDriver::open_in_memory().unwrap();
    db.init_schema().unwrap(); // second call — should not fail
    assert_eq!(db.schema_version().unwrap(), 5);
}

/// The caption and location projections round-trip through insert, upsert and the row
/// mapper, and an empty caption is stored as no caption.
#[test]
fn caption_and_location_round_trip_and_an_empty_caption_is_none() {
    let db = DatabaseDriver::open_in_memory().unwrap();
    let mut row = make_asset("uuid-1", &"a".repeat(64));
    row.caption = Some("harbour at dusk".to_string());
    row.latitude = Some(37.8199);
    row.longitude = Some(-122.4783);
    db.insert_asset(&row).unwrap();
    assert_eq!(db.find_by_uuid("uuid-1").unwrap().unwrap(), row);

    row.caption = Some(String::new());
    row.latitude = None;
    row.longitude = None;
    db.upsert_asset(&row).unwrap();
    let stored = db.find_by_uuid("uuid-1").unwrap().unwrap();
    assert_eq!(stored.caption, None, "an empty caption is no caption");
    assert_eq!((stored.latitude, stored.longitude), (None, None));
}

/// `update_asset_caption` sets and clears the projection, and reports an unknown uuid as
/// `false` without writing anything.
#[test]
fn update_asset_caption_sets_clears_and_reports_a_missing_row() {
    let db = DatabaseDriver::open_in_memory().unwrap();
    db.insert_asset(&make_asset("uuid-1", &"a".repeat(64)))
        .unwrap();

    assert!(
        db.update_asset_caption("uuid-1", Some("on the pier"))
            .unwrap()
    );
    let caption = db.find_by_uuid("uuid-1").unwrap().unwrap().caption;
    assert_eq!(caption.as_deref(), Some("on the pier"));

    assert!(db.update_asset_caption("uuid-1", Some("")).unwrap());
    assert_eq!(db.find_by_uuid("uuid-1").unwrap().unwrap().caption, None);

    assert!(db.update_asset_caption("uuid-1", Some("again")).unwrap());
    assert!(db.update_asset_caption("uuid-1", None).unwrap());
    assert_eq!(db.find_by_uuid("uuid-1").unwrap().unwrap().caption, None);

    assert!(!db.update_asset_caption("uuid-missing", Some("x")).unwrap());
    assert!(db.find_by_uuid("uuid-missing").unwrap().is_none());
}

#[test]
fn test_insert_and_find_by_hash() {
    let db = DatabaseDriver::open_in_memory().unwrap();
    let asset = make_asset("uuid-1", &"a".repeat(64));
    db.insert_asset(&asset).unwrap();
    let found = db.find_by_hash(&"a".repeat(64)).unwrap();
    assert!(found.is_some());
    assert_eq!(found.unwrap().uuid, "uuid-1");
}

#[test]
fn test_find_by_hash_not_found() {
    let db = DatabaseDriver::open_in_memory().unwrap();
    let found = db.find_by_hash(&"b".repeat(64)).unwrap();
    assert!(found.is_none());
}

#[test]
fn test_query_timeline_excludes_deleted_and_hidden() {
    let db = DatabaseDriver::open_in_memory().unwrap();
    let a1 = make_asset("uuid-1", &"a".repeat(64));
    let mut a2 = make_asset("uuid-2", &"b".repeat(64));
    let mut a3 = make_asset("uuid-3", &"c".repeat(64));
    a2.is_deleted = true;
    a3.is_stack_hidden = true;
    db.insert_asset(&a1).unwrap();
    db.insert_asset(&a2).unwrap();
    db.insert_asset(&a3).unwrap();
    let timeline = db.query_timeline(0, 100).unwrap();
    assert_eq!(timeline.len(), 1);
    assert_eq!(timeline[0].uuid, "uuid-1");
}

/// A user-hidden asset is excluded from **every** default projection — timeline,
/// filtered timeline, and album — and surfaces only in `query_hidden`. Hiding is
/// view-layer only, so it keeps its album membership and stays directly reachable by
/// uuid and by hash (SSoT: design/organization § Hidden Assets).
#[test]
fn hidden_assets_are_excluded_from_default_projections_and_served_by_query_hidden() {
    let db = DatabaseDriver::open_in_memory().unwrap();
    let mut visible = make_asset("uuid-visible", &"a".repeat(64));
    visible.album_id = Some("album-1".to_string());
    let mut hidden = make_asset("uuid-hidden", &"b".repeat(64));
    hidden.album_id = Some("album-1".to_string());
    hidden.is_hidden = true;
    db.insert_asset(&visible).unwrap();
    db.insert_asset(&hidden).unwrap();

    // Default views: the hidden asset is gone.
    let timeline = db.query_timeline(0, 100).unwrap();
    assert_eq!(timeline.len(), 1);
    assert_eq!(timeline[0].uuid, "uuid-visible");

    let filtered = db
        .query_timeline_filtered(Some("photo"), None, None, 0, 100)
        .unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].uuid, "uuid-visible");

    let album = db.query_album_assets("album-1", 0, 100).unwrap();
    assert_eq!(album.len(), 1, "hidden assets leave the album projection");
    assert_eq!(album[0].uuid, "uuid-visible");

    // The Hidden view is where it lives.
    let hidden_view = db.query_hidden(0, 100).unwrap();
    assert_eq!(hidden_view.len(), 1);
    assert_eq!(hidden_view[0].uuid, "uuid-hidden");
    assert!(hidden_view[0].is_hidden);

    // Hiding is view-layer only: album membership and direct reachability survive.
    let direct = db.find_by_uuid("uuid-hidden").unwrap().unwrap();
    assert_eq!(direct.album_id.as_deref(), Some("album-1"));
    assert!(direct.is_hidden);
    assert!(db.find_by_hash(&"b".repeat(64)).unwrap().is_some());
}

/// `is_hidden` is a projection, so it is settable, round-trips through the row mapper,
/// and moves the asset between the default and Hidden views both ways.
#[test]
fn update_asset_hidden_moves_the_asset_between_views() {
    let db = DatabaseDriver::open_in_memory().unwrap();
    db.insert_asset(&make_asset("uuid-1", &"a".repeat(64)))
        .unwrap();
    assert_eq!(db.query_timeline(0, 100).unwrap().len(), 1);
    assert!(db.query_hidden(0, 100).unwrap().is_empty());

    db.update_asset_hidden("uuid-1", true).unwrap();
    assert!(db.query_timeline(0, 100).unwrap().is_empty());
    assert_eq!(db.query_hidden(0, 100).unwrap().len(), 1);

    db.update_asset_hidden("uuid-1", false).unwrap();
    assert_eq!(db.query_timeline(0, 100).unwrap().len(), 1);
    assert!(db.query_hidden(0, 100).unwrap().is_empty());
}

/// A hidden asset that is also soft-deleted belongs to Recently Deleted, not Hidden —
/// the two gated views never show the same row.
#[test]
fn hidden_and_deleted_asset_appears_only_in_trash() {
    let db = DatabaseDriver::open_in_memory().unwrap();
    let mut row = make_asset("uuid-1", &"a".repeat(64));
    row.is_hidden = true;
    row.is_deleted = true;
    row.deleted_at = Some(1_720_000_100);
    db.insert_asset(&row).unwrap();

    assert!(db.query_hidden(0, 100).unwrap().is_empty());
    assert!(db.query_timeline(0, 100).unwrap().is_empty());
    let trash = db.query_trash(0, 100).unwrap();
    assert_eq!(trash.len(), 1);
    assert_eq!(trash[0].uuid, "uuid-1");
}

#[test]
fn test_soft_delete() {
    let db = DatabaseDriver::open_in_memory().unwrap();
    let asset = make_asset("uuid-1", &"a".repeat(64));
    db.insert_asset(&asset).unwrap();
    db.soft_delete("uuid-1", 1_720_000_100).unwrap();
    let timeline = db.query_timeline(0, 100).unwrap();
    assert!(timeline.is_empty());
}

#[test]
fn test_query_expired_trash() {
    let db = DatabaseDriver::open_in_memory().unwrap();
    let asset = make_asset("uuid-1", &"a".repeat(64));
    db.insert_asset(&asset).unwrap();
    // Delete with a timestamp far in the past
    db.soft_delete("uuid-1", 100).unwrap();
    let expired = db.query_expired_trash(30 * 86400).unwrap();
    assert_eq!(expired.len(), 1);
}

#[test]
fn test_update_stack_hidden() {
    let db = DatabaseDriver::open_in_memory().unwrap();
    let asset = make_asset("uuid-1", &"a".repeat(64));
    db.insert_asset(&asset).unwrap();
    db.update_stack_hidden("uuid-1", true).unwrap();
    let timeline = db.query_timeline(0, 100).unwrap();
    assert!(timeline.is_empty());
    db.update_stack_hidden("uuid-1", false).unwrap();
    let timeline = db.query_timeline(0, 100).unwrap();
    assert_eq!(timeline.len(), 1);
}

#[test]
fn test_insert_stack_and_members() {
    let db = DatabaseDriver::open_in_memory().unwrap();
    let a1 = make_asset("uuid-1", &"a".repeat(64));
    let a2 = make_asset("uuid-2", &"b".repeat(64));
    db.insert_asset(&a1).unwrap();
    db.insert_asset(&a2).unwrap();
    let stack = AssetStackRow {
        id: "stack-1".to_string(),
        stack_type: "raw_jpeg".to_string(),
        primary_asset_id: "uuid-1".to_string(),
        cover_asset_id: Some("uuid-1".to_string()),
        is_collapsed: true,
        is_auto_generated: true,
        created_at: 1_720_000_000,
        modified_at: 1_720_000_000,
    };
    db.insert_stack(&stack).unwrap();
    let m1 = StackMemberRow {
        id: "m-1".to_string(),
        stack_id: "stack-1".to_string(),
        asset_id: "uuid-1".to_string(),
        sequence_order: 0,
        member_role: "primary".to_string(),
        created_at: 1_720_000_000,
    };
    let m2 = StackMemberRow {
        id: "m-2".to_string(),
        stack_id: "stack-1".to_string(),
        asset_id: "uuid-2".to_string(),
        sequence_order: 1,
        member_role: "raw".to_string(),
        created_at: 1_720_000_000,
    };
    db.insert_stack_member(&m1).unwrap();
    db.insert_stack_member(&m2).unwrap();
    let members = db.list_stack_members("stack-1").unwrap();
    assert_eq!(members.len(), 2);
    assert_eq!(members[0].member_role, "primary");
    assert_eq!(members[1].member_role, "raw");
}

#[test]
fn test_upsert_asset() {
    let db = DatabaseDriver::open_in_memory().unwrap();
    let mut asset = make_asset("uuid-1", &"a".repeat(64));
    db.insert_asset(&asset).unwrap();
    asset.rating = 5;
    db.upsert_asset(&asset).unwrap();
    let found = db.find_by_hash(&"a".repeat(64)).unwrap().unwrap();
    assert_eq!(found.rating, 5);
}

#[test]
fn test_query_timeline_filtered_by_type() {
    let db = DatabaseDriver::open_in_memory().unwrap();
    let photo = make_asset("p-1", &"a".repeat(64));
    let mut video = make_asset("v-1", &"b".repeat(64));
    video.asset_type = "video".to_string();
    db.insert_asset(&photo).unwrap();
    db.insert_asset(&video).unwrap();

    let photos = db
        .query_timeline_filtered(Some("photo"), None, None, 0, 100)
        .unwrap();
    assert_eq!(photos.len(), 1);
    assert_eq!(photos[0].uuid, "p-1");

    let all = db
        .query_timeline_filtered(None, None, None, 0, 100)
        .unwrap();
    assert_eq!(all.len(), 2);
}

#[test]
fn test_query_timeline_filtered_by_time_window() {
    let db = DatabaseDriver::open_in_memory().unwrap();
    let mut early = make_asset("e-1", &"a".repeat(64));
    let mut late = make_asset("l-1", &"b".repeat(64));
    early.capture_utc = Some(1_000);
    late.capture_utc = Some(2_000_000_000);
    db.insert_asset(&early).unwrap();
    db.insert_asset(&late).unwrap();

    let recent = db
        .query_timeline_filtered(None, Some(1_000_000), None, 0, 100)
        .unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].uuid, "l-1");
}

#[test]
fn test_album_crud() {
    let db = DatabaseDriver::open_in_memory().unwrap();
    let album = AlbumRow {
        id: "album-1".to_string(),
        name: "Summer".to_string(),
        created_at: 1_720_000_000,
        modified_at: 1_720_000_000,
        cover_asset_id: None,
    };
    db.insert_album(&album).unwrap();
    assert_eq!(db.list_albums().unwrap().len(), 1);

    let mut updated = album.clone();
    updated.name = "Summer 2024".to_string();
    updated.modified_at = 1_720_000_100;
    db.update_album(&updated).unwrap();
    assert_eq!(
        db.find_album("album-1").unwrap().unwrap().name,
        "Summer 2024"
    );

    db.delete_album("album-1").unwrap();
    assert!(db.find_album("album-1").unwrap().is_none());
}

#[test]
fn test_query_album_assets_and_detach_on_delete() {
    let db = DatabaseDriver::open_in_memory().unwrap();
    let album = AlbumRow {
        id: "album-1".to_string(),
        name: "Trip".to_string(),
        created_at: 1_720_000_000,
        modified_at: 1_720_000_000,
        cover_asset_id: None,
    };
    db.insert_album(&album).unwrap();

    let asset = make_asset("a-1", &"a".repeat(64));
    db.insert_asset(&asset).unwrap();
    db.set_asset_album("a-1", Some("album-1")).unwrap();

    let in_album = db.query_album_assets("album-1", 0, 100).unwrap();
    assert_eq!(in_album.len(), 1);

    // Deleting the album detaches its assets but does not delete them.
    db.delete_album("album-1").unwrap();
    assert!(db.query_album_assets("album-1", 0, 100).unwrap().is_empty());
    assert!(db.find_by_uuid("a-1").unwrap().is_some());
}

fn rep(uuid: &str, tier: &str, bytes: i64, last: i64) -> CachedRepresentationRow {
    CachedRepresentationRow {
        uuid: uuid.to_string(),
        tier: tier.to_string(),
        format: None,
        bytes,
        path: format!("/cache/{uuid}.{tier}"),
        last_accessed_at: last,
        pinned: false,
        is_owned_original: false,
    }
}

#[test]
fn purge_asset_drops_the_row_its_tags_and_its_cache_rows_only() {
    let db = DatabaseDriver::open_in_memory().unwrap();
    for (uuid, hash) in [("gone", "a"), ("kept", "b")] {
        db.insert_asset(&make_asset(uuid, &hash.repeat(64)))
            .unwrap();
        db.replace_asset_tags(uuid, &["beach".to_string()]).unwrap();
        db.upsert_representation(&rep(uuid, "thumbnail", 100, 10))
            .unwrap();
    }

    db.purge_asset("gone").unwrap();

    assert!(db.find_by_uuid("gone").unwrap().is_none());
    assert!(db.tags_for("gone").unwrap().is_empty());
    assert!(db.representations_for("gone").unwrap().is_empty());
    assert!(db.find_by_uuid("kept").unwrap().is_some());
    assert_eq!(db.tags_for("kept").unwrap(), vec!["beach".to_string()]);
    assert_eq!(db.representations_for("kept").unwrap().len(), 1);
}

fn stack(id: &str, primary: &str, cover: Option<&str>) -> AssetStackRow {
    AssetStackRow {
        id: id.to_string(),
        stack_type: "burst".to_string(),
        primary_asset_id: primary.to_string(),
        cover_asset_id: cover.map(str::to_string),
        is_collapsed: true,
        is_auto_generated: true,
        created_at: 1,
        modified_at: 1,
    }
}

fn member(stack_id: &str, asset_id: &str, order: i64) -> StackMemberRow {
    StackMemberRow {
        id: format!("{stack_id}#{asset_id}"),
        stack_id: stack_id.to_string(),
        asset_id: asset_id.to_string(),
        sequence_order: order,
        member_role: "member".to_string(),
        created_at: 1,
    }
}

/// `(primary_asset_id, cover_asset_id)` of stack `id`, or `None` when the row is gone.
fn stack_roles(db: &DatabaseDriver, id: &str) -> Option<(String, Option<String>)> {
    db.conn
        .query_row(
            "SELECT primary_asset_id, cover_asset_id FROM asset_stacks WHERE id = ?1",
            params![id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .unwrap()
}

#[test]
fn purge_asset_leaves_no_stack_or_cover_row_naming_it() {
    let db = DatabaseDriver::open_in_memory().unwrap();
    for (uuid, hash) in [("gone", "a"), ("next", "b"), ("last", "c"), ("solo", "d")] {
        db.insert_asset(&make_asset(uuid, &hash.repeat(64)))
            .unwrap();
    }
    // A stack the purged asset leads, a stack it is only the cover of, and one it is alone in.
    db.insert_stack(&stack("led", "gone", Some("gone")))
        .unwrap();
    db.insert_stack(&stack("covered", "next", Some("gone")))
        .unwrap();
    db.insert_stack(&stack("alone", "gone", None)).unwrap();
    db.insert_stack(&stack("other", "solo", Some("solo")))
        .unwrap();
    for (stack_id, asset, order) in [
        ("led", "gone", 0),
        ("led", "last", 2),
        ("led", "next", 1),
        ("covered", "next", 0),
        ("covered", "gone", 1),
        ("alone", "gone", 0),
        ("other", "solo", 0),
    ] {
        db.insert_stack_member(&member(stack_id, asset, order))
            .unwrap();
    }
    db.insert_album(&AlbumRow {
        id: "album".to_string(),
        name: "Trip".to_string(),
        created_at: 1,
        modified_at: 1,
        cover_asset_id: Some("gone".to_string()),
    })
    .unwrap();

    db.purge_asset("gone").unwrap();

    let members: Vec<String> = db
        .list_stack_members("led")
        .unwrap()
        .into_iter()
        .map(|m| m.asset_id)
        .collect();
    assert_eq!(members, ["next", "last"]);
    assert_eq!(
        stack_roles(&db, "led"),
        Some(("next".to_string(), Some("next".to_string()))),
        "the first remaining member by sequence takes the primary and the cover"
    );
    assert_eq!(
        stack_roles(&db, "covered"),
        Some(("next".to_string(), Some("next".to_string())))
    );
    assert_eq!(
        stack_roles(&db, "alone"),
        None,
        "an emptied stack is removed"
    );
    assert_eq!(
        stack_roles(&db, "other"),
        Some(("solo".to_string(), Some("solo".to_string()))),
        "another asset's stack is left alone"
    );
    let named: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM stack_members WHERE asset_id = 'gone'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(named, 0);
    assert_eq!(
        db.find_album("album").unwrap().unwrap().cover_asset_id,
        None
    );
}

fn evicted_ids(rows: &[CachedRepresentationRow]) -> Vec<&str> {
    rows.iter().map(|r| r.uuid.as_str()).collect()
}

#[test]
fn eviction_is_least_recently_accessed_first() {
    let db = DatabaseDriver::open_in_memory().unwrap();
    db.upsert_representation(&rep("a", "thumbnail", 100, 10))
        .unwrap();
    db.upsert_representation(&rep("b", "thumbnail", 100, 20))
        .unwrap();
    db.upsert_representation(&rep("c", "thumbnail", 100, 30))
        .unwrap();
    // 300 B reclaimable, budget 250 → evict only the least-recently-accessed.
    let evicted = db.eviction_candidates(250).unwrap();
    assert_eq!(evicted_ids(&evicted), ["a"]);
}

#[test]
fn eviction_tier_order_breaks_recency_ties() {
    let db = DatabaseDriver::open_in_memory().unwrap();
    // Equal recency, three tiers — original (biggest, most regenerable) is evicted first.
    db.upsert_representation(&rep("x", "original", 100, 50))
        .unwrap();
    db.upsert_representation(&rep("x", "preview", 100, 50))
        .unwrap();
    db.upsert_representation(&rep("x", "thumbnail", 100, 50))
        .unwrap();
    let evicted = db.eviction_candidates(250).unwrap();
    assert_eq!(
        evicted.iter().map(|r| r.tier.as_str()).collect::<Vec<_>>(),
        ["original"]
    );
}

#[test]
fn recency_promotion_protects_touched_representation() {
    let db = DatabaseDriver::open_in_memory().unwrap();
    db.upsert_representation(&rep("a", "thumbnail", 100, 10))
        .unwrap();
    db.upsert_representation(&rep("b", "thumbnail", 100, 20))
        .unwrap();
    db.upsert_representation(&rep("c", "thumbnail", 100, 30))
        .unwrap();
    // Viewing the oldest makes it newest, so the next-oldest is evicted instead.
    db.record_access("a", "thumbnail", 100).unwrap();
    let evicted = db.eviction_candidates(250).unwrap();
    assert_eq!(evicted_ids(&evicted), ["b"]);
}

#[test]
fn pinned_and_owned_originals_are_exempt() {
    let db = DatabaseDriver::open_in_memory().unwrap();
    let mut pinned = rep("p", "original", 1000, 10);
    pinned.pinned = true;
    let mut owned = rep("o", "original", 1000, 10);
    owned.is_owned_original = true;
    db.upsert_representation(&pinned).unwrap();
    db.upsert_representation(&owned).unwrap();
    db.upsert_representation(&rep("c", "thumbnail", 100, 30))
        .unwrap();
    // Budget 0 reclaims everything reclaimable, but the exempt rows survive the sweep.
    let evicted = db.eviction_candidates(0).unwrap();
    assert_eq!(evicted_ids(&evicted), ["c"]);
}

#[test]
fn eviction_empty_when_within_budget() {
    let db = DatabaseDriver::open_in_memory().unwrap();
    db.upsert_representation(&rep("a", "thumbnail", 100, 10))
        .unwrap();
    assert!(db.eviction_candidates(1000).unwrap().is_empty());
}
