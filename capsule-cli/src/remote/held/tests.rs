//! Tests for the CLI's reading of the library and the feed a held delivery is judged on. The
//! held rules themselves are the SDK's and tested there (`capsule_sdk::albums::plan_delivery`);
//! these pin that the CLI judges them on the heads and the clock floor its feed drain names.

use std::path::PathBuf;

use capsule_core::crypto::provenance::Action;
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

/// The CLI judges the shared rule on what its drain read: an asset the feed does not list is
/// held, a listed one is sent the stretch after its head, and a server `changed_at` past a held
/// delete's window floors a lagging device clock, so the restore that undoes it is withheld.
#[test]
fn the_shared_rule_is_judged_on_the_feeds_heads_and_clock_floor() {
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

    let unlisted = ServerChains::default();
    assert_eq!(
        plan(&chain, unlisted.head(&asset), unlisted.judging_clock(now())),
        Delivery::Held {
            reason: HoldReason::NotIndexed,
            retention_until: None,
        }
    );

    let listed = |changed_at: &str| ServerChains {
        heads: HashMap::from([(asset, Some(create))]),
        latest_changed_at: Some(changed_at.parse().expect("a timestamp")),
    };
    let early = listed("2026-01-01T00:00:00Z");
    assert_eq!(
        delivery_actions(&plan(
            &chain,
            early.head(&asset),
            early.judging_clock(now())
        )),
        Some(vec![Action::Delete, Action::TrashRestore]),
        "inside the window by both clocks"
    );
    let late = listed("2999-01-01T00:00:00Z");
    assert_eq!(
        plan(&chain, late.head(&asset), late.judging_clock(now())),
        Delivery::Held {
            reason: HoldReason::RestoreTooLate,
            retention_until: Some(until),
        },
        "the server's clock says the window closed"
    );
}

/// The one stretch rule `plan` and the delegation ledger share: the records after the head, none
/// when the head is the last, the last alone when the head is unknown. Only `plan` turns
/// "none" into a replay of the head.
#[test]
fn after_head_is_the_records_the_server_lacks() {
    let scratch = Scratch::new();
    let (mut ws, asset) = library(&scratch);
    ws.soft_delete(&asset, 30).expect("deleted");
    let chain = records(&ws, &asset);
    let (create, head) = (chain[0].record_hash(), chain[1].record_hash());

    assert_eq!(
        actions(after_head(&chain, Some(create))),
        vec![Action::Delete]
    );
    assert!(after_head(&chain, Some(head)).is_empty(), "level: nothing");
    for unknown in [Some(hash_bytes(b"elsewhere")), None] {
        assert_eq!(
            actions(after_head(&chain, unknown)),
            vec![Action::Delete],
            "unknown: the head alone"
        );
    }
    assert!(after_head(&[], None).is_empty());

    let chains = ServerChains {
        heads: HashMap::from([(asset, Some(head))]),
        latest_changed_at: None,
    };
    assert!(chains.is_level(&asset, &chain));
    assert!(!chains.is_level(&asset, &chain[..1]));
    assert!(!ServerChains::default().is_level(&asset, &chain));
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
