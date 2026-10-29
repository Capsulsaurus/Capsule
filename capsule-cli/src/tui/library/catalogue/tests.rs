//! Catalogue tests: the snapshot a real workspace yields, and the views drawn from it.

use capsule_core::crypto::primitives::Argon2Params;
use capsule_core::domain::{GpsDatum, StackType};
use capsule_core::sidecar::{CullFlag, StackMembership};

use super::*;

const FAST_KDF: Argon2Params = Argon2Params {
    mem_kib: 64,
    t_cost: 1,
    p_cost: 1,
};

/// A fast-cost workspace with `count` imported assets, in a scratch directory removed on drop.
struct Fixture {
    dir: PathBuf,
    ws: Workspace,
    album: Uuid,
    ids: Vec<Uuid>,
}

impl Fixture {
    fn with_assets(count: usize) -> Self {
        let dir = std::env::temp_dir().join(format!("capsule-cli-catalogue-{}", nanoid::nanoid!()));
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
        Self {
            dir,
            ws,
            album,
            ids,
        }
    }

    fn stack(&mut self, stack_id: Uuid, asset: Uuid, role: StackRole) {
        self.ws
            .set_stack_membership(
                &asset,
                Some(StackMembership {
                    stack_id,
                    stack_type: StackType::Burst,
                    role,
                    member_index: None,
                }),
            )
            .expect("stack membership");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn every_asset_is_listed_with_its_rows_history_and_original() {
    let fx = Fixture::with_assets(3);
    let catalogue = collect(&fx.ws);
    assert_eq!(catalogue.assets().len(), 3);
    for id in &fx.ids {
        let asset = catalogue.asset(*id).expect("listed");
        assert_eq!(asset.album_id, fx.album);
        assert_eq!(asset.ext, "jpg");
        assert!(asset.original.as_ref().is_some_and(|p| p.is_file()));
        assert_eq!(asset.history.len(), 1);
        assert_eq!(asset.history[0].action, "create");
        assert!(asset.history[0].retention_until.is_none());
        let view = crate::show::collect(&fx.ws, id).expect("managed");
        assert_eq!(asset.rows, crate::show::rows(&view));
    }
    assert_eq!(catalogue.timeline().len(), 3);
}

#[test]
fn the_list_is_newest_capture_first_then_by_id() {
    let fx = Fixture::with_assets(4);
    let catalogue = collect(&fx.ws);
    for pair in catalogue.assets().windows(2) {
        assert!(
            pair[0].captured > pair[1].captured
                || (pair[0].captured == pair[1].captured && pair[0].asset_id < pair[1].asset_id),
            "{:?} before {:?}",
            pair[0].captured,
            pair[1].captured
        );
    }
}

#[test]
fn the_timeline_leaves_out_trash_hidden_and_non_primary_stack_members() {
    let mut fx = Fixture::with_assets(5);
    let (trashed, hidden, primary, member) = (fx.ids[0], fx.ids[1], fx.ids[2], fx.ids[3]);
    fx.ws.set_cull(&trashed, CullFlag::Reject).expect("cull");
    fx.ws.reject_sweep(30).expect("sweep");
    fx.ws.set_hidden(&hidden, true).expect("hide");
    let stack_id = Uuid::now_v7();
    fx.stack(stack_id, primary, StackRole::Primary);
    fx.stack(stack_id, member, StackRole::Member);

    let catalogue = collect(&fx.ws);
    let timeline = catalogue.timeline();
    assert_eq!(timeline.len(), 2, "{timeline:?}");
    assert!(timeline.contains(&primary));
    assert!(timeline.contains(&fx.ids[4]));

    // The album lists everything but trash; the stack lists both members, primary first.
    let album = catalogue.album_assets(fx.album);
    assert_eq!(album.len(), 4);
    assert!(!album.contains(&trashed));
    let stack = catalogue.stack(stack_id).expect("stack listed");
    assert_eq!(
        stack.members,
        vec![(primary, StackRole::Primary), (member, StackRole::Member)]
    );
    assert_eq!(catalogue.stacks().len(), 1);

    // The trashed asset's history carries the delete and its deadline.
    let trashed = catalogue.asset(trashed).expect("still managed");
    assert!(trashed.in_trash);
    let delete = trashed.history.last().expect("records");
    assert_eq!(delete.action, "delete");
    assert!(delete.retention_until.is_some());
}

#[test]
fn stacks_follow_their_newest_member_whichever_role_it_holds() {
    let mut fx = Fixture::with_assets(4);
    let order: Vec<Uuid> = collect(&fx.ws)
        .assets()
        .iter()
        .map(|a| a.asset_id)
        .collect();
    // `newer` holds the newest asset but the oldest primary, so ordering by the primary's
    // capture would list `older` first.
    let (newer, older) = (Uuid::now_v7(), Uuid::now_v7());
    fx.stack(newer, order[0], StackRole::Member);
    fx.stack(older, order[1], StackRole::Member);
    fx.stack(older, order[2], StackRole::Primary);
    fx.stack(newer, order[3], StackRole::Primary);

    let catalogue = collect(&fx.ws);
    let stacks: Vec<Uuid> = catalogue.stacks().iter().map(|s| s.stack_id).collect();
    assert_eq!(stacks, vec![newer, older]);
    assert_eq!(
        catalogue.stack(newer).expect("listed").members,
        vec![
            (order[3], StackRole::Primary),
            (order[0], StackRole::Member)
        ]
    );
}

#[test]
fn album_and_stack_labels_are_read() {
    let mut fx = Fixture::with_assets(2);
    let stack_id = Uuid::now_v7();
    fx.stack(stack_id, fx.ids[0], StackRole::Primary);
    fx.ws.add_album_label(fx.album, "trips").expect("label");
    fx.ws.add_album_label(fx.album, "family").expect("label");
    fx.ws.add_stack_label(stack_id, "best").expect("label");

    let catalogue = collect(&fx.ws);
    let album = catalogue.album(fx.album).expect("album listed");
    assert_eq!(album.name, "Imports");
    assert_eq!(
        album.labels,
        Ok(vec!["family".to_owned(), "trips".to_owned()])
    );
    let stack = catalogue.stack(stack_id).expect("stack listed");
    assert_eq!(stack.labels, Ok(vec!["best".to_owned()]));
}

#[test]
fn an_empty_library_is_an_empty_catalogue_with_its_album() {
    let fx = Fixture::with_assets(0);
    let catalogue = collect(&fx.ws);
    assert!(catalogue.assets().is_empty());
    assert!(catalogue.stacks().is_empty());
    assert_eq!(catalogue.albums().len(), 1);
    assert!(catalogue.timeline().is_empty());
}

#[test]
fn labels_regions_and_both_capture_times_and_locations_are_read() {
    let mut fx = Fixture::with_assets(1);
    let id = fx.ids[0];
    fx.ws.tag_add(&id, "beach").expect("tag");
    let shape = RegionShape::Rect {
        x: 0.1,
        y: 0.2,
        w: 0.3,
        h: 0.4,
    };
    let region = fx.ws.add_region(&id, shape).expect("region");
    fx.ws
        .add_region_label(&id, &region, "Ada")
        .expect("region label");
    let imported = collect(&fx.ws)
        .asset(id)
        .expect("asset")
        .capture_text
        .clone();
    fx.ws
        .set_capture_timestamp(&id, "2021-07-04T18:30:00Z".parse().expect("instant"))
        .expect("capture");
    fx.ws
        .set_location(&id, 48.5, 2.25, GpsDatum::Gcj02)
        .expect("location");

    let catalogue = collect(&fx.ws);
    let asset = catalogue.asset(id).expect("asset");
    assert_eq!(asset.labels, vec!["beach".to_owned()]);
    assert_eq!(
        asset.regions,
        vec![RegionEntry {
            region_id: region,
            outline: Outline(shape),
            labels: vec!["Ada".to_owned()],
        }]
    );
    assert_eq!(asset.capture_text, "2021-07-04T18:30:00Z");
    assert_eq!(asset.capture_imported, Some(imported));
    assert_eq!(
        asset.location,
        Some(Place {
            lat: 48.5,
            lon: 2.25,
            datum: GpsDatum::Gcj02,
            source: GpsSource::Manual,
        })
    );
    assert_eq!(asset.location_imported, None);
    assert!(asset.location_edited);
}

#[test]
fn outlines_and_places_compare_by_kind_and_bits() {
    let circle = Outline(RegionShape::Circle {
        cx: 0.5,
        cy: 0.5,
        r: 0.1,
    });
    assert_eq!(circle, circle);
    assert_ne!(
        circle,
        Outline(RegionShape::Circle {
            cx: 0.5,
            cy: 0.5,
            r: 0.2,
        })
    );
    assert_ne!(
        circle,
        Outline(RegionShape::Rect {
            x: 0.5,
            y: 0.5,
            w: 0.1,
            h: f32::from_bits(u32::MAX),
        })
    );
    let place = Place {
        lat: f64::NAN,
        lon: 0.0,
        datum: GpsDatum::Wgs84,
        source: GpsSource::Exif,
    };
    assert_eq!(place, place, "bitwise, so even a NaN equals itself");
    assert_ne!(place, Place { lon: -0.0, ..place });
}

#[test]
fn debug_prints_counts_not_rows() {
    let fx = Fixture::with_assets(2);
    let printed = format!("{:?}", collect(&fx.ws));
    assert_eq!(
        printed,
        "Catalogue { assets: 2, albums: 1, stacks: 0, delegations: 0, .. }"
    );
}

fn bare(asset_id: Uuid, album_id: Uuid, in_trash: bool) -> AssetEntry {
    AssetEntry {
        asset_id,
        album_id,
        captured: None,
        ext: "jpg".to_owned(),
        original: None,
        hidden: false,
        in_trash,
        stack: None,
        rows: Vec::new(),
        history: Vec::new(),
        labels: Vec::new(),
        regions: Vec::new(),
        capture_text: String::new(),
        capture_imported: None,
        location: None,
        location_imported: None,
        location_edited: false,
    }
}

#[test]
fn the_index_answers_by_id_in_list_order() {
    let (one, two) = (Uuid::from_u128(1), Uuid::from_u128(2));
    let (album, other) = (Uuid::from_u128(10), Uuid::from_u128(11));
    let mut dup = bare(one, other, false);
    dup.ext = "png".to_owned();
    let catalogue = Catalogue::new(
        vec![bare(one, album, false), bare(two, album, true), dup],
        vec![AlbumEntry {
            album_id: album,
            name: "A".to_owned(),
            labels: Ok(Vec::new()),
        }],
        Vec::new(),
    );
    // The first entry for an id is the one looked up.
    assert_eq!(catalogue.asset(one).map(|a| a.ext.as_str()), Some("jpg"));
    assert_eq!(catalogue.asset(two).map(|a| a.in_trash), Some(true));
    assert!(catalogue.asset(Uuid::from_u128(3)).is_none());
    assert_eq!(catalogue.album(album).map(|a| a.name.as_str()), Some("A"));
    assert!(catalogue.album(other).is_none());
    assert!(catalogue.stack(one).is_none());
    // Album lists leave out trash and keep list order; an unknown album lists nothing.
    assert_eq!(catalogue.album_assets(album), [one]);
    assert_eq!(catalogue.album_assets(other), [one]);
    assert!(catalogue.album_assets(Uuid::from_u128(12)).is_empty());
    assert_eq!(catalogue.timeline(), [one, one]);
    assert_eq!(catalogue, catalogue.clone());
}

/// The timeline listing is [`Catalogue::timeline`] in its order, each asset with its labels and
/// stack placement.
#[test]
fn the_timeline_listing_follows_the_timeline() {
    let mut fx = Fixture::with_assets(3);
    let stack_id = Uuid::now_v7();
    fx.stack(stack_id, fx.ids[0], StackRole::Primary);
    fx.stack(stack_id, fx.ids[1], StackRole::Member);
    fx.ws.tag_add(&fx.ids[2], "beach").expect("label");

    let catalogue = collect(&fx.ws);
    let Listing::Timeline { assets } = listing(&catalogue, ListView::Timeline) else {
        panic!("a timeline listing");
    };
    let ids: Vec<Uuid> = assets.iter().map(|a| a.asset_id).collect();
    assert_eq!(ids, catalogue.timeline());
    let primary = assets
        .iter()
        .find(|a| a.asset_id == fx.ids[0])
        .expect("listed");
    assert_eq!(
        primary.stack,
        Some(StackReport {
            stack_id,
            role: "primary"
        })
    );
    let labelled = assets
        .iter()
        .find(|a| a.asset_id == fx.ids[2])
        .expect("listed");
    assert_eq!(labelled.labels, ["beach"]);
    assert_eq!(labelled.album_id, fx.album);
    assert_eq!(labelled.ext, "jpg");
    let entry = catalogue.asset(fx.ids[2]).expect("listed");
    assert_eq!(labelled.capture_timestamp, entry.capture_text);
}

#[test]
fn the_albums_and_stacks_listings_carry_labels_and_contents() {
    let mut fx = Fixture::with_assets(2);
    let stack_id = Uuid::now_v7();
    fx.stack(stack_id, fx.ids[1], StackRole::Member);
    fx.stack(stack_id, fx.ids[0], StackRole::Primary);
    fx.ws.add_album_label(fx.album, "trips").expect("label");
    fx.ws.add_stack_label(stack_id, "best").expect("label");
    let catalogue = collect(&fx.ws);

    let Listing::Albums { albums } = listing(&catalogue, ListView::Albums) else {
        panic!("an albums listing");
    };
    assert_eq!(albums.len(), 1);
    assert_eq!(albums[0].album_id, fx.album);
    assert_eq!(albums[0].name, "Imports");
    assert_eq!(albums[0].labels, Some(vec!["trips".to_owned()]));
    assert_eq!(albums[0].labels_error, None);
    assert_eq!(albums[0].assets, catalogue.album_assets(fx.album));

    let Listing::Stacks { stacks } = listing(&catalogue, ListView::Stacks) else {
        panic!("a stacks listing");
    };
    assert_eq!(stacks.len(), 1);
    assert_eq!(stacks[0].stack_id, stack_id);
    assert_eq!(stacks[0].labels, Some(vec!["best".to_owned()]));
    assert_eq!(
        stacks[0].members,
        [
            ListedMember {
                asset_id: fx.ids[0],
                role: "primary"
            },
            ListedMember {
                asset_id: fx.ids[1],
                role: "member"
            },
        ],
        "the primary first"
    );
}

/// An unreadable label record lists as `null` labels beside the reason, never as no labels.
#[test]
fn an_unreadable_label_record_lists_its_reason() {
    let album = Uuid::from_u128(7);
    let stack = Uuid::from_u128(8);
    let catalogue = Catalogue::new(
        Vec::new(),
        vec![AlbumEntry {
            album_id: album,
            name: "A".to_owned(),
            labels: Err("unreadable".to_owned()),
        }],
        vec![StackEntry {
            stack_id: stack,
            members: Vec::new(),
            labels: Err("torn".to_owned()),
        }],
    );
    let Listing::Albums { albums } = listing(&catalogue, ListView::Albums) else {
        panic!("an albums listing");
    };
    assert_eq!(albums[0].labels, None);
    assert_eq!(albums[0].labels_error.as_deref(), Some("unreadable"));
    let Listing::Stacks { stacks } = listing(&catalogue, ListView::Stacks) else {
        panic!("a stacks listing");
    };
    assert_eq!(stacks[0].labels, None);
    assert_eq!(stacks[0].labels_error.as_deref(), Some("torn"));
    let json = serde_json::to_value(listing(&catalogue, ListView::Stacks)).expect("serialise");
    assert_eq!(json["view"], "stacks");
    assert_eq!(json["stacks"][0]["labels"], serde_json::Value::Null);
    assert_eq!(json["stacks"][0]["labels_error"], "torn");
}

#[test]
fn the_text_listing_prints_one_line_per_entry() {
    let stack = Uuid::from_u128(8);
    let member = Uuid::from_u128(1);
    let album = Uuid::from_u128(7);
    let timeline = Listing::Timeline {
        assets: vec![ListedAsset {
            asset_id: member,
            album_id: album,
            capture_timestamp: "2020-01-02T03:04:05Z".to_owned(),
            ext: "jpg".to_owned(),
            labels: vec!["a".to_owned(), "b".to_owned()],
            stack: None,
        }],
    };
    assert_eq!(
        render_listing(&timeline),
        format!("{member}  2020-01-02T03:04:05Z  jpg  a, b\n")
    );
    let albums = Listing::Albums {
        albums: vec![ListedAlbum {
            album_id: album,
            name: "Trips".to_owned(),
            labels: None,
            labels_error: Some("torn".to_owned()),
            assets: vec![member],
        }],
    };
    assert_eq!(
        render_listing(&albums),
        format!("{album}  Trips  1 asset(s)  labels unreadable: torn\n")
    );
    let stacks = Listing::Stacks {
        stacks: vec![ListedStack {
            stack_id: stack,
            labels: Some(Vec::new()),
            labels_error: None,
            members: vec![ListedMember {
                asset_id: member,
                role: "primary",
            }],
        }],
    };
    assert_eq!(
        render_listing(&stacks),
        format!("{stack}  1 member(s)  -\n  {member} (primary)\n")
    );
    for (empty, line) in [
        (
            Listing::Timeline { assets: Vec::new() },
            "The timeline is empty.\n",
        ),
        (Listing::Albums { albums: Vec::new() }, "No albums.\n"),
        (Listing::Stacks { stacks: Vec::new() }, "No stacks.\n"),
    ] {
        assert_eq!(render_listing(&empty), line);
    }
}
