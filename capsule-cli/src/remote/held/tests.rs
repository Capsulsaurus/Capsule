//! Tests for what of an asset's chain goes to the server, and when: held until the create is
//! indexed, then the stretch after the server's head, and never a too-late restore of a held
//! delete.

use std::path::PathBuf;

use capsule_library::lifecycle::Workspace;
use capsule_sdk::sync::{BlobManifest, ChangeKind};

use super::*;

/// A scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("capsule-cli-held-{}", nanoid::nanoid!()));
        std::fs::create_dir_all(&path).expect("a temp dir");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// A library holding one imported asset: its chain is a lone `create`.
fn library(scratch: &Scratch) -> (Workspace, Uuid) {
    let lib = scratch.0.join("library");
    std::fs::create_dir_all(&lib).expect("a library dir");
    let mut ws = Workspace::create_with_params(
        &lib,
        b"passphrase",
        capsule_core::crypto::primitives::Argon2Params {
            mem_kib: 64,
            t_cost: 1,
            p_cost: 1,
        },
    )
    .expect("a library");
    let album = ws.default_album_id();
    ws.ensure_album(album, "Default").expect("an album");
    let source = scratch.0.join("photo.jpg");
    std::fs::write(&source, b"a photo").expect("a source");
    let opts = capsule_library::lifecycle::SignedImportOptions {
        move_source: false,
        defer_source_release: false,
        stack: None,
        enrichment: None,
    };
    let asset = ws
        .import_asset_with(album, &source, &opts)
        .expect("imported")
        .asset_id;
    (ws, asset)
}

fn records(ws: &Workspace, asset: &Uuid) -> Vec<ProvenanceRecord> {
    ws.asset(asset).expect("held").chain.records().to_vec()
}

fn actions(stretch: &[ProvenanceRecord]) -> Vec<Action> {
    stretch.iter().map(|r| r.manifest.core.action).collect()
}

fn delivery_actions(delivery: &Delivery<'_>) -> Option<Vec<Action>> {
    match delivery {
        Delivery::Send(stretch) => Some(actions(stretch)),
        Delivery::Held { .. } => None,
    }
}

fn now() -> jiff::Timestamp {
    jiff::Timestamp::now()
}

/// Far past any window a test signs.
fn much_later() -> jiff::Timestamp {
    now() + jiff::SignedDuration::from_hours(24 * 365)
}

/// **The server never sees a record of an asset it has not indexed.** Whatever the chain
/// holds, an asset the feed does not list is held, and nothing of it is sent.
#[test]
fn an_asset_the_server_has_not_indexed_is_held() {
    let scratch = Scratch::new();
    let (mut ws, asset) = library(&scratch);
    ws.soft_delete(&asset, 30).expect("deleted");
    let chain = records(&ws, &asset);

    assert_eq!(
        plan(&chain, None, now()),
        Delivery::Held {
            reason: HoldReason::NotIndexed,
            retention_until: None,
        }
    );
}

/// Once the create is indexed, the records after the server's head go, in chain order: a
/// delete, and a restore made after it, each chaining onto the one before.
#[test]
fn an_indexed_asset_is_sent_the_stretch_after_the_servers_head() {
    let scratch = Scratch::new();
    let (mut ws, asset) = library(&scratch);
    let create = records(&ws, &asset)[0].record_hash();
    ws.soft_delete(&asset, 30).expect("deleted");

    let chain = records(&ws, &asset);
    assert_eq!(
        delivery_actions(&plan(&chain, Some(Some(create)), now())),
        Some(vec![Action::Delete])
    );

    ws.restore(&asset).expect("restored");
    let chain = records(&ws, &asset);
    assert_eq!(
        delivery_actions(&plan(&chain, Some(Some(create)), now())),
        Some(vec![Action::Delete, Action::TrashRestore]),
        "a restore inside the window goes after the delete it undoes"
    );
}

/// A server already level with the library is sent the head alone, which it answers as a
/// replay; a head the server holds that the chain does not know (or none at all) is the head
/// alone too, which the server judges and refuses as a stale chain if it must.
#[test]
fn a_level_or_unknown_server_head_is_sent_the_local_head_alone() {
    let scratch = Scratch::new();
    let (mut ws, asset) = library(&scratch);
    ws.soft_delete(&asset, 30).expect("deleted");
    let chain = records(&ws, &asset);
    let head = chain.last().expect("a head").record_hash();

    for server in [Some(head), Some(hash_bytes(b"elsewhere")), None] {
        assert_eq!(
            delivery_actions(&plan(&chain, Some(server), now())),
            Some(vec![Action::Delete]),
            "server head {server:?}"
        );
    }
}

/// **A restore of a held delete is never sent too late.** Sent after the delete's window has
/// closed, the server would apply the delete, refuse the restore and purge an asset the user
/// restored in time; the pair is withheld and the closed window reported.
#[test]
fn a_restore_undoing_a_held_delete_whose_window_closed_is_withheld() {
    let scratch = Scratch::new();
    let (mut ws, asset) = library(&scratch);
    let create = records(&ws, &asset)[0].record_hash();
    ws.soft_delete(&asset, 1).expect("deleted");
    let until = records(&ws, &asset)
        .last()
        .and_then(|r| r.manifest.core.retention_until.clone())
        .expect("a window");
    ws.restore(&asset).expect("restored");
    let chain = records(&ws, &asset);

    assert_eq!(
        plan(&chain, Some(Some(create)), much_later()),
        Delivery::Held {
            reason: HoldReason::RestoreTooLate,
            retention_until: Some(until),
        }
    );
}

/// The withholding covers only a delete the server has not seen. One already applied leaves
/// the restore alone in the stretch, and the server's own clock judges it.
#[test]
fn a_restore_of_a_delete_the_server_holds_is_sent_whatever_the_clock() {
    let scratch = Scratch::new();
    let (mut ws, asset) = library(&scratch);
    ws.soft_delete(&asset, 1).expect("deleted");
    let delete = records(&ws, &asset).last().expect("a delete").record_hash();
    ws.restore(&asset).expect("restored");
    let chain = records(&ws, &asset);

    assert_eq!(
        delivery_actions(&plan(&chain, Some(Some(delete)), much_later())),
        Some(vec![Action::TrashRestore])
    );
}

fn entry(asset_id: &str, manifest_cbor: Vec<u8>, changed_at: &str) -> FeedEntry {
    FeedEntry {
        album_id: b"album".to_vec(),
        sync_seq: 1,
        protocol_version: "2026-05-31".to_owned(),
        kind: ChangeKind::Created,
        asset_id: asset_id.as_bytes().to_vec(),
        manifest_cbor,
        metadata_blob: Vec::new(),
        blobs: BlobManifest::default(),
        derivative_manifests: Vec::new(),
        original_held: true,
        purged: false,
        changed_at: changed_at.to_owned(),
    }
}

/// The feed fold: an asset's latest entry names its head (the digest of the record bytes it
/// carries), an entry with no record still marks the asset indexed, an id this library does
/// not use is skipped, and the latest `changed_at` floors the judging clock.
#[test]
fn the_feed_names_each_indexed_assets_head_and_floors_the_clock() {
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    let chains = ServerChains::from_feed(&[
        entry(&a.to_string(), b"first".to_vec(), "2026-10-01T00:00:00Z"),
        entry("not-a-uuid", b"x".to_vec(), "2026-10-02T00:00:00Z"),
        entry(&a.to_string(), b"second".to_vec(), "2026-10-03T00:00:00Z"),
        entry(&b.to_string(), Vec::new(), "2026-09-01T00:00:00Z"),
    ]);

    assert_eq!(chains.head(&a), Some(Some(hash_bytes(b"second"))));
    assert_eq!(chains.head(&b), Some(None), "indexed, head unknown");
    assert_eq!(chains.head(&Uuid::now_v7()), None, "not indexed");

    let seen: jiff::Timestamp = "2026-10-03T00:00:00Z".parse().expect("a timestamp");
    let behind: jiff::Timestamp = "2026-01-01T00:00:00Z".parse().expect("a timestamp");
    assert_eq!(
        chains.judging_clock(behind),
        seen,
        "a lagging clock is floored"
    );
    let ahead = much_later();
    assert_eq!(
        chains.judging_clock(ahead),
        ahead,
        "a clock past the floor is kept"
    );
    assert_eq!(
        ServerChains::default().judging_clock(behind),
        behind,
        "no drain, no floor"
    );
}
