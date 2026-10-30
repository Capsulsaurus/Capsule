use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use capsule_core::cbor;
use jiff::SignedDuration;
use tempfile::TempDir;

use super::*;
use crate::db::rows::CachedRepresentationRow;
use crate::library::{open_library, rebuild_index};
use crate::lifecycle::{FAST_PARAMS, fast_workspace};

/// A workspace with `n` plain (non-decodable) assets imported into one album.
fn library(lib: &TempDir, src: &TempDir, n: usize) -> (Workspace, Vec<Uuid>) {
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Trip").unwrap();
    let ids = (0..n)
        .map(|i| {
            let path = src.path().join(format!("photo-{i}.jpg"));
            let mut bytes = b"\xFF\xD8\xFF".to_vec();
            bytes.extend_from_slice(format!("trash fixture {i}").as_bytes());
            fs::write(&path, bytes).unwrap();
            ws.import_asset(album, &path).unwrap()
        })
        .collect();
    (ws, ids)
}

fn chain_len(ws: &Workspace, asset: &Uuid) -> usize {
    ws.asset(asset).unwrap().chain.records().len()
}

fn until_of(ws: &Workspace, asset: &Uuid) -> Timestamp {
    ws.trash_entry(asset)
        .and_then(|entry| entry.retention_until)
        .expect("a trashed asset with a signed window")
}

/// The provenance records on disk for `asset`, read the way `Workspace::open` reads them.
fn chain_on_disk(ws: &Workspace, capture_utc: i64, asset: &Uuid) -> Vec<ProvenanceRecord> {
    let path =
        media_dir(ws.root(), capture_utc).join(format!("{}.provenance.cbor", asset.simple()));
    cbor::from_slice(&fs::read(path).expect("the provenance chain is kept")).unwrap()
}

#[test]
fn a_delete_lists_the_asset_with_its_signed_retention_until() {
    let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (mut ws, ids) = library(&lib, &src, 2);
    assert!(ws.trash().is_empty());
    assert!(ws.trash_entry(&ids[0]).is_none());

    ws.soft_delete(&ids[0], 30).unwrap();

    let head = ws.asset(&ids[0]).unwrap().chain.records().last().unwrap();
    let signed: Timestamp = head
        .manifest
        .core
        .retention_until
        .as_deref()
        .unwrap()
        .parse()
        .unwrap();
    let entry = ws.trash_entry(&ids[0]).unwrap();
    assert_eq!(entry.retention_until, Some(signed));
    assert_eq!(ws.trash(), vec![entry]);
    assert!(ws.trash_entry(&ids[1]).is_none(), "the other asset is live");
}

#[test]
fn a_restore_before_retention_until_recovers_the_asset() {
    let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (mut ws, ids) = library(&lib, &src, 1);
    ws.soft_delete(&ids[0], 30).unwrap();
    let until = until_of(&ws, &ids[0]);

    ws.restore_at(&ids[0], until - SignedDuration::from_secs(1))
        .unwrap();

    assert!(ws.trash_entry(&ids[0]).is_none());
    let head = ws.asset(&ids[0]).unwrap().chain.records().last().unwrap();
    assert_eq!(head.manifest.core.action, Action::TrashRestore);
    let row = ws.db().find_by_uuid(&ids[0].to_string()).unwrap().unwrap();
    assert!(!row.is_deleted, "the index follows the chain");
}

#[test]
fn a_restore_at_or_after_retention_until_is_refused_and_writes_nothing() {
    let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (mut ws, ids) = library(&lib, &src, 1);
    ws.soft_delete(&ids[0], 30).unwrap();
    let until = until_of(&ws, &ids[0]);
    let before = chain_len(&ws, &ids[0]);

    for now in [until, until + SignedDuration::from_hours(1)] {
        match ws.restore_at(&ids[0], now) {
            Err(LifecycleError::RetentionElapsed {
                asset_id,
                retention_until,
            }) => {
                assert_eq!(asset_id, ids[0]);
                assert_eq!(retention_until, until.to_string());
            }
            other => panic!("expected RetentionElapsed, got {other:?}"),
        }
    }

    assert_eq!(chain_len(&ws, &ids[0]), before, "nothing was signed");
    assert!(
        ws.trash_entry(&ids[0]).is_some(),
        "the asset stays in the trash"
    );
}

#[test]
fn a_restore_or_purge_of_an_asset_outside_the_trash_is_refused() {
    let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (mut ws, ids) = library(&lib, &src, 1);
    let before = chain_len(&ws, &ids[0]);

    assert!(matches!(ws.restore(&ids[0]), Err(LifecycleError::NotInTrash(id)) if id == ids[0]));
    assert!(matches!(ws.purge(&ids[0]), Err(LifecycleError::NotInTrash(id)) if id == ids[0]));
    assert!(matches!(
        ws.purge(&Uuid::now_v7()),
        Err(LifecycleError::NotFound(_))
    ));

    assert_eq!(chain_len(&ws, &ids[0]), before);
    assert!(ws.read_plaintext(&ids[0]).is_ok(), "nothing was removed");
}

#[test]
fn expiry_is_judged_by_retention_until_not_by_the_age_of_the_delete() {
    let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (mut ws, ids) = library(&lib, &src, 2);
    // Deleted first with a long window, then second with none: the older delete is the one
    // still protected.
    ws.soft_delete(&ids[0], 30).unwrap();
    ws.soft_delete(&ids[1], 0).unwrap();
    let short = until_of(&ws, &ids[1]);
    let long = until_of(&ws, &ids[0]);

    let expired: Vec<Uuid> = ws
        .expired_trash(short)
        .into_iter()
        .map(|entry| entry.asset_id)
        .collect();
    assert_eq!(expired, vec![ids[1]]);
    assert!(
        ws.expired_trash(short - SignedDuration::from_secs(1))
            .is_empty()
    );
    assert_eq!(ws.expired_trash(long).len(), 2);
}

#[test]
fn an_early_purge_appends_the_signed_shorter_retention_reissue_first() {
    let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (mut ws, ids) = library(&lib, &src, 1);
    ws.soft_delete(&ids[0], 30).unwrap();
    let until = until_of(&ws, &ids[0]);
    let capture_utc = ws.asset(&ids[0]).unwrap().capture_utc;
    let before = chain_len(&ws, &ids[0]);
    let now = until - SignedDuration::from_hours(24);

    let outcome = ws.purge_at(&ids[0], now).unwrap();

    let reissue = outcome
        .reissue
        .expect("an early purge re-issues the window");
    assert_eq!(reissue.manifest.core.action, Action::Delete);
    assert_eq!(
        reissue.manifest.core.retention_until.as_deref(),
        Some(now.to_string().as_str())
    );
    let on_disk = chain_on_disk(&ws, capture_utc, &ids[0]);
    assert_eq!(on_disk.len(), before + 1);
    assert_eq!(
        on_disk.last(),
        Some(&reissue),
        "the re-issue is the kept chain's head"
    );
}

#[test]
fn a_purge_after_retention_until_signs_nothing() {
    let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (mut ws, ids) = library(&lib, &src, 1);
    ws.soft_delete(&ids[0], 30).unwrap();
    let until = until_of(&ws, &ids[0]);
    let capture_utc = ws.asset(&ids[0]).unwrap().capture_utc;
    let before = chain_len(&ws, &ids[0]);

    let outcome = ws.purge_at(&ids[0], until).unwrap();

    assert!(outcome.reissue.is_none());
    assert_eq!(chain_on_disk(&ws, capture_utc, &ids[0]).len(), before);
}

#[test]
fn a_purge_leaves_no_original_derivative_or_cache_file_and_keeps_provenance() {
    let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let outside = TempDir::new().unwrap();
    let (mut ws, ids) = library(&lib, &src, 2);
    let (gone, kept) = (ids[0], ids[1]);
    let asset = ws.asset(&gone).unwrap();
    let capture_utc = asset.capture_utc;
    let original = ws.media_path(asset);
    let sidecar = ws.sidecar_path(asset);
    let blob = ws.metadata_blob_path(asset);
    let provenance = ws.provenance_path(asset);

    // Derivatives and cache entries for both assets, so the purge must tell them apart.
    let derivatives = media_dir(ws.root(), capture_utc).join("derivatives");
    fs::create_dir_all(&derivatives).unwrap();
    let mut doomed = vec![original.clone(), sidecar.clone(), blob.clone()];
    let mut survivors = Vec::new();
    for (id, list) in [(gone, &mut doomed), (kept, &mut survivors)] {
        for name in ["thumbnail.jxl", "preview.avif", "derivatives.cbor"] {
            let path = derivatives.join(format!("{}.{name}", id.simple()));
            fs::write(&path, b"derivative").unwrap();
            list.push(path);
        }
        let thumb = thumbnail_path(ws.root(), &id, ThumbnailSize::M).with_extension("webp");
        let cached = ws
            .root()
            .join("index")
            .join("cache")
            .join(id.simple().to_string());
        for path in [&thumb, &cached] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"cache").unwrap();
            list.push(path.clone());
        }
        ws.db()
            .upsert_representation(&CachedRepresentationRow {
                uuid: id.to_string(),
                tier: "preview".into(),
                format: Some("avif".into()),
                bytes: 5,
                path: cached.to_string_lossy().into_owned(),
                last_accessed_at: 0,
                pinned: false,
                is_owned_original: false,
            })
            .unwrap();
    }
    // A corrupt row naming a file outside the library is never followed.
    let foreign = outside.path().join("not-ours.jpg");
    fs::write(&foreign, b"someone else's").unwrap();
    ws.db()
        .upsert_representation(&CachedRepresentationRow {
            uuid: gone.to_string(),
            tier: "thumbnail".into(),
            format: None,
            bytes: 1,
            path: foreign.to_string_lossy().into_owned(),
            last_accessed_at: 0,
            pinned: false,
            is_owned_original: false,
        })
        .unwrap();

    let kept_rows = ws
        .db()
        .representations_for(&kept.to_string())
        .unwrap()
        .len();
    ws.soft_delete(&gone, 30).unwrap();
    let outcome = ws.purge(&gone).unwrap();

    for path in &doomed {
        assert!(!path.exists(), "{} survived the purge", path.display());
    }
    for path in &survivors {
        assert!(path.exists(), "{} belongs to another asset", path.display());
    }
    assert!(foreign.exists(), "a path outside the library is left alone");
    assert!(provenance.exists(), "the provenance chain is kept");
    assert_eq!(outcome.removed_files, doomed.len());
    assert!(ws.asset(&gone).is_none());
    assert!(ws.trash().is_empty());
    assert!(ws.db().find_by_uuid(&gone.to_string()).unwrap().is_none());
    assert!(
        ws.db()
            .representations_for(&gone.to_string())
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        ws.db()
            .representations_for(&kept.to_string())
            .unwrap()
            .len(),
        kept_rows
    );
}

#[test]
fn a_purged_asset_stays_gone_after_a_reopen_and_a_rebuild() {
    let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (gone, kept) = {
        let (mut ws, ids) = library(&lib, &src, 2);
        ws.soft_delete(&ids[0], 30).unwrap();
        ws.purge(&ids[0]).unwrap();
        (ids[0], ids[1])
    };

    {
        let ws = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
        assert!(
            ws.asset(&gone).is_none(),
            "the kept chain alone is no asset"
        );
        assert!(ws.asset(&kept).is_some());
    }

    fs::remove_file(lib.path().join("index/library.sqlite")).unwrap();
    let library = open_library(lib.path()).unwrap();
    rebuild_index(&library).unwrap();
    assert!(
        library
            .db
            .find_by_uuid(&gone.to_string())
            .unwrap()
            .is_none()
    );
    assert!(
        library
            .db
            .find_by_uuid(&kept.to_string())
            .unwrap()
            .is_some()
    );
}

#[test]
fn a_delete_survives_a_rebuild_still_trashed_with_its_window() {
    let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (asset, until) = {
        let (mut ws, ids) = library(&lib, &src, 1);
        ws.soft_delete(&ids[0], 30).unwrap();
        (ids[0], until_of(&ws, &ids[0]))
    };

    fs::remove_file(lib.path().join("index/library.sqlite")).unwrap();
    {
        let library = open_library(lib.path()).unwrap();
        rebuild_index(&library).unwrap();
        let row = library
            .db
            .find_by_uuid(&asset.to_string())
            .unwrap()
            .unwrap();
        assert!(row.is_deleted);
    }
    let ws = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
    assert_eq!(until_of(&ws, &asset), until);
}

#[test]
fn a_purge_records_a_tombstone_and_keeps_the_reissue_retrievable_across_a_reopen() {
    let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (gone, kept, reissue) = {
        let (mut ws, ids) = library(&lib, &src, 2);
        ws.soft_delete(&ids[0], 30).unwrap();
        let now = until_of(&ws, &ids[0]) - SignedDuration::from_hours(24);
        let reissue = ws.purge_at(&ids[0], now).unwrap().reissue.unwrap();
        assert_eq!(ws.tombstones(), BTreeSet::from([ids[0]]));
        assert_eq!(ws.purge_record(&ids[0]), Some(&reissue));
        assert_eq!(
            ws.local_heads().get(&ids[0]),
            Some(&reissue.record_hash()),
            "a purged asset is held at its kept chain's head"
        );
        (ids[0], ids[1], reissue)
    };

    let ws = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
    assert_eq!(ws.tombstones(), BTreeSet::from([gone]));
    assert_eq!(ws.purge_record(&gone), Some(&reissue));
    assert_eq!(ws.purge_record(&kept), None);
    assert_eq!(ws.local_heads().get(&gone), Some(&reissue.record_hash()));
}

#[test]
fn a_lost_sidecar_of_an_asset_that_was_not_purged_is_no_tombstone() {
    let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (live, trashed) = {
        let (mut ws, ids) = library(&lib, &src, 2);
        ws.soft_delete(&ids[1], 30).unwrap();
        for id in &ids {
            fs::remove_file(ws.sidecar_path(ws.asset(id).unwrap())).unwrap();
        }
        (ids[0], ids[1])
    };

    let ws = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
    assert!(ws.asset(&live).is_none(), "the damaged asset is skipped");
    assert!(ws.asset(&trashed).is_none());
    assert!(
        ws.tombstones().is_empty(),
        "a chain whose head is not a delete, or whose original is still there, is damage"
    );
    assert!(!ws.local_heads().contains_key(&live));
    assert!(!ws.local_heads().contains_key(&trashed));
}

#[test]
fn an_older_backup_or_peer_copy_never_resurrects_a_purged_asset() {
    let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (mut ws, ids) = library(&lib, &src, 1);
    let archive = src.path().join("before-purge.tar");
    ws.export_backup_with_params(&archive, b"pw", FAST_PARAMS)
        .unwrap();
    ws.soft_delete(&ids[0], 30).unwrap();
    ws.purge(&ids[0]).unwrap();

    let added = ws
        .import_backup(&archive, b"pw", &ws.exporter_verifying_key())
        .unwrap();
    assert_eq!(added, 0);
    assert!(ws.asset(&ids[0]).is_none());

    let bytes = fs::read(&archive).unwrap();
    let restored =
        capsule_core::backup::BackupArtifact::open(&bytes, b"pw", &ws.exporter_verifying_key())
            .unwrap()
            .restore(capsule_core::backup::RestoreMode::Commit, &BTreeMap::new())
            .unwrap()
            .applied;
    let refused = ws.apply_peer_restored(&restored[0]).unwrap_err();
    assert!(matches!(refused, LifecycleError::Purged(id) if id == ids[0]));
    assert!(ws.asset(&ids[0]).is_none());
}

/// A drain whose feed lists every asset in `heads` at the record at that index of its chain,
/// with the latest server `changed_at` it saw.
fn drain_at(ws: &Workspace, changed_at: Timestamp, heads: &[(Uuid, usize)]) -> FeedDrain {
    FeedDrain {
        latest_changed_at: Some(changed_at),
        server_heads: heads
            .iter()
            .map(|(asset, at)| {
                let head = ws.asset(asset).unwrap().chain.records()[*at].record_hash();
                (*asset, Some(head))
            })
            .collect(),
    }
}

fn ids_of(entries: &[TrashEntry]) -> Vec<Uuid> {
    entries.iter().map(|e| e.asset_id).collect()
}

/// **Expiry is judged by the server's clock as the drain saw it, never the device's.** The
/// sweep has no clock parameter at all: with no drain, or a drain that saw no entry, nothing
/// has expired; with one, the latest `changed_at` alone decides, however far the device's own
/// clock has run past the window.
#[test]
fn a_retention_sweep_judges_by_the_latest_changed_at_alone() {
    let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (mut ws, ids) = library(&lib, &src, 1);
    ws.soft_delete(&ids[0], 30).unwrap();
    let until = until_of(&ws, &ids[0]);
    let indexed = [(ids[0], chain_len(&ws, &ids[0]) - 1)];

    assert!(ws.expired_trash_after_drain(None).is_empty());
    let no_reading = FeedDrain {
        latest_changed_at: None,
        ..drain_at(&ws, until, &indexed)
    };
    assert_eq!(no_reading.judged_at(), None);
    assert!(
        ws.expired_trash_after_drain(Some(&no_reading)).is_empty(),
        "no server clock reading, so nothing is judged expired"
    );
    // The device's clock is past the window (the trash itself says so), the server's is not.
    assert_eq!(
        ids_of(&ws.expired_trash(until + SignedDuration::from_hours(1))),
        vec![ids[0]]
    );
    let behind = drain_at(&ws, until - SignedDuration::from_secs(1), &indexed);
    assert!(
        ws.expired_trash_after_drain(Some(&behind)).is_empty(),
        "a device clock running ahead never purges early"
    );
    let at_until = drain_at(&ws, until, &indexed);
    assert_eq!(at_until.judged_at(), Some(until));
    assert_eq!(
        ids_of(&ws.expired_trash_after_drain(Some(&at_until))),
        vec![ids[0]]
    );
}

/// **A held delete never expires.** The server has no window running for a `delete` it has not
/// indexed, so the sweep keeps the asset however late the server's clock is: not listed, listed
/// at its create, or listed by an entry that carries no record. Only once the feed's head is
/// the delete (or a record after it) does the window run; the user's own purge still removes a
/// held one.
#[test]
fn a_retention_sweep_never_expires_a_held_delete() {
    let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (mut ws, ids) = library(&lib, &src, 2);
    for id in &ids {
        ws.soft_delete(id, 1).unwrap();
    }
    let late = until_of(&ws, &ids[0]).max(until_of(&ws, &ids[1])) + SignedDuration::from_hours(1);
    let delete_at = chain_len(&ws, &ids[0]) - 1;

    let unlisted = drain_at(&ws, late, &[(ids[1], delete_at)]);
    assert_eq!(
        ids_of(&ws.expired_trash_after_drain(Some(&unlisted))),
        vec![ids[1]],
        "the unlisted asset's delete is held"
    );
    let at_create = drain_at(&ws, late, &[(ids[0], 0), (ids[1], 0)]);
    assert!(
        ws.expired_trash_after_drain(Some(&at_create)).is_empty(),
        "the server's head precedes both deletes"
    );
    let mut no_record = drain_at(&ws, late, &[(ids[1], delete_at)]);
    no_record.server_heads.insert(ids[0], None);
    assert_eq!(
        ids_of(&ws.expired_trash_after_drain(Some(&no_record))),
        vec![ids[1]],
        "an entry with no record is no evidence the server indexed the delete"
    );
    let mut foreign = drain_at(&ws, late, &[]);
    foreign
        .server_heads
        .insert(ids[0], Some(capsule_core::crypto::hash::hash_bytes(b"x")));
    assert!(
        ws.expired_trash_after_drain(Some(&foreign)).is_empty(),
        "a head this chain does not hold"
    );

    ws.purge(&ids[0]).unwrap();
    assert_eq!(ws.tombstones(), BTreeSet::from([ids[0]]));
}

/// **A purged asset's held records are delivered from its kept chain.** The whole chain stays
/// readable after a purge and after a reopen: the create, the delete the server never saw, and
/// the shorter-retention re-issue at its head, so a delivery can send the stretch after the
/// server's head from the tombstone.
#[test]
fn a_purged_asset_keeps_its_whole_chain_across_a_reopen() {
    let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (gone, chain) = {
        let (mut ws, ids) = library(&lib, &src, 2);
        ws.soft_delete(&ids[0], 30).unwrap();
        let now = until_of(&ws, &ids[0]) - SignedDuration::from_hours(24);
        let reissue = ws.purge_at(&ids[0], now).unwrap().reissue.unwrap();
        let chain = ws.purged_chain(&ids[0]).expect("a kept chain").to_vec();
        assert_eq!(chain.last(), Some(&reissue));
        assert_eq!(
            chain.first().map(|r| r.manifest.core.action),
            Some(Action::Create)
        );
        let actions: Vec<_> = chain
            .iter()
            .rev()
            .take(2)
            .map(|r| r.manifest.core.action)
            .collect();
        assert_eq!(
            actions,
            vec![Action::Delete, Action::Delete],
            "the re-issue over the held delete"
        );
        assert_eq!(ws.purged_chain(&ids[1]), None);
        (ids[0], chain)
    };

    let ws = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
    assert_eq!(ws.purged_chain(&gone), Some(chain.as_slice()));
    assert_eq!(ws.purge_record(&gone), chain.last());
}
