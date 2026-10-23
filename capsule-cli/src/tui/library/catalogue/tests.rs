//! Catalogue tests: the snapshot a real workspace yields, and the views drawn from it.

use capsule_core::crypto::primitives::Argon2Params;
use capsule_core::domain::StackType;
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
fn debug_prints_counts_not_rows() {
    let fx = Fixture::with_assets(2);
    let printed = format!("{:?}", collect(&fx.ws));
    assert_eq!(printed, "Catalogue { assets: 2, albums: 1, stacks: 0 }");
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
