//! The pure half of `capsule sync reannounce`: what the library holds, and how the server's
//! answers become the report.

use capsule_core::crypto::primitives::Argon2Params;

use super::*;

/// A cheap KDF so a library opens in milliseconds.
const FAST_KDF: Argon2Params = Argon2Params {
    mem_kib: 64,
    t_cost: 1,
    p_cost: 1,
};

/// A scratch directory removed on drop.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "capsule-cli-reannounce-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7().simple()
        ));
        std::fs::create_dir_all(&path).expect("a scratch directory");
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn library(dir: &std::path::Path) -> (Workspace, uuid::Uuid) {
    let mut ws =
        Workspace::create_with_params(dir, b"passphrase", FAST_KDF).expect("a fresh library");
    let album = ws.create_album("Trip").expect("an album");
    (ws, album)
}

fn import(ws: &mut Workspace, album: uuid::Uuid, dir: &std::path::Path, name: &str) -> uuid::Uuid {
    let path = dir.join(name);
    std::fs::write(
        &path,
        [b"\xFF\xD8\xFF reannounce ".as_slice(), name.as_bytes()].concat(),
    )
    .expect("the source writes");
    ws.import_asset(album, &path).expect("the import")
}

#[test]
fn every_live_asset_is_held_as_its_encoded_chain_head_and_a_deletion_is_left_out() {
    let dir = Scratch::new("lib");
    let sources = Scratch::new("src");
    let (mut ws, album) = library(dir.path());
    let kept = import(&mut ws, album, sources.path(), "kept.jpg");
    let deleted = import(&mut ws, album, sources.path(), "deleted.jpg");
    ws.soft_delete(&deleted, 30).expect("the asset is deleted");

    let (held, skipped) = held_manifests(&ws).expect("the library reads");
    assert_eq!(skipped, 1);
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].asset_id, kept.to_string());
    let bundle = ws.upload_bundle(&kept).expect("a bundle builds");
    assert_eq!(
        held[0].manifest, bundle.provenance_blob,
        "the held record is the bytes an upload sends"
    );
}

fn result(outcome: ReannounceOutcome) -> ReannounceResult {
    ReannounceResult {
        asset_id: None,
        outcome,
    }
}

#[test]
fn each_answer_is_counted_and_reported_in_the_order_sent() {
    let summary = ReannounceSummary::answered(
        5,
        1,
        1,
        vec![
            (
                "a".to_owned(),
                result(ReannounceOutcome::Restored { sync_seq: Some(9) }),
            ),
            ("b".to_owned(), result(ReannounceOutcome::AlreadyIndexed)),
            (
                "c".to_owned(),
                result(ReannounceOutcome::MissingBlobs(vec!["ab".to_owned()])),
            ),
            (
                "d".to_owned(),
                result(ReannounceOutcome::Refused {
                    code: "error.sync.reannounce_denied".to_owned(),
                }),
            ),
        ],
    );
    assert!(!summary.dry_run);
    assert_eq!(
        (
            summary.reannounced,
            summary.restored,
            summary.already_indexed,
            summary.missing_blobs,
            summary.refused
        ),
        (4, 1, 1, 1, 1)
    );
    let json = serde_json::to_value(&summary).expect("the report serializes");
    assert_eq!(json["assets"][0]["status"], "restored");
    assert_eq!(json["assets"][0]["sync_seq"], 9);
    assert_eq!(json["assets"][1]["status"], "already_indexed");
    assert_eq!(json["assets"][2]["missing"][0], "ab");
    assert_eq!(json["assets"][3]["code"], "error.sync.reannounce_denied");
    assert!(
        json["assets"][1].get("code").is_none(),
        "absent facts are left out, not null"
    );
}

#[test]
fn a_dry_run_plans_every_unlisted_asset_and_sends_nothing() {
    let held = [
        HeldManifest {
            asset_id: "a".to_owned(),
            manifest: vec![1],
        },
        HeldManifest {
            asset_id: "b".to_owned(),
            manifest: vec![2],
        },
    ];
    let unlisted: Vec<&HeldManifest> = held.iter().collect();
    let summary = ReannounceSummary::planned(2, 0, 0, &unlisted);
    assert!(summary.dry_run);
    assert_eq!(summary.reannounced, 2);
    assert_eq!(summary.restored, 0);
    assert!(
        summary
            .assets
            .iter()
            .all(|line| line.status == AssetReannounceStatus::Planned)
    );
}
