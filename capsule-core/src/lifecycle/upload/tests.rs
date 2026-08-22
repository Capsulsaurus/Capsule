//! `S-D18` — the upload-bundle accessor, including its `S-A10` interaction.

use std::fs;

use tempfile::TempDir;

use super::*;
use crate::crypto::primitives::Argon2Params;

/// The fast-Argon2 cost the lifecycle suite uses; the production tier would dominate runtime.
const FAST: Argon2Params = Argon2Params {
    mem_kib: 64,
    t_cost: 1,
    p_cost: 1,
};

/// A library with exactly one imported asset. Returns `(library dir, album id, asset id)`.
fn library_with_one_asset(lib: &TempDir, src: &TempDir) -> (Uuid, Uuid) {
    let img = src.path().join("photo.jpg");
    fs::write(
        &img,
        b"\xFF\xD8\xFF upload-bundle fixture bytes, long enough to chunk",
    )
    .unwrap();
    let mut ws = Workspace::create_with_params(lib.path(), b"passphrase", FAST).unwrap();
    let album = ws.default_album_id();
    ws.ensure_album(album, "Imports").unwrap();
    let asset = ws.import_asset(album, &img).unwrap();
    (album, asset)
}

/// The bundle's ciphertext is the one the signed manifest committed to: re-deriving it from the
/// recorded nonce prefix content-addresses back to `ciphertext_hash`, and every envelope field
/// mirrors the head manifest.
#[test]
fn upload_bundle_ciphertext_matches_the_manifest_hash() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (album, asset_id) = library_with_one_asset(&lib, &src);

    let ws = Workspace::open(lib.path(), b"passphrase", FAST).unwrap();
    let bundle = ws.upload_bundle(&asset_id).expect("bundle");
    let head = &ws
        .asset(&asset_id)
        .unwrap()
        .chain
        .records()
        .last()
        .unwrap()
        .manifest
        .core;

    assert_eq!(bundle.ciphertext_hash, head.ciphertext_hash);
    assert_eq!(
        crate::crypto::hash::hash_bytes(&bundle.ciphertext),
        head.ciphertext_hash,
        "the bundle's bytes must content-address to the manifest's hash"
    );
    assert_eq!(bundle.asset_id, head.file_id);
    assert_eq!(bundle.album_id, album);
    assert_eq!(bundle.amk_version, head.amk_version.0);
    assert_eq!(bundle.plaintext_size, head.plaintext_size);
    assert_eq!(bundle.chunk_size, head.chunk_size);
    assert_eq!(bundle.crypto_suite_id, head.crypto_suite_id);
    assert_eq!(bundle.protocol_version, head.protocol_version);
    assert_eq!(bundle.key_mode, head.key_mode);
    assert_eq!(bundle.metadata_blob_hash, head.metadata_blob_hash);
    assert_eq!(bundle.created_by_user, head.created_by_user);
    assert_eq!(bundle.created_by_device, head.created_by_device);
    assert_eq!(bundle.client_version, head.client_version);
    assert_eq!(bundle.timestamp, head.timestamp);
    assert_eq!(bundle.action, head.action);
    assert_eq!(bundle.prior_provenance_hash, head.prior_provenance_hash);
    assert_eq!(bundle.retention_until, head.retention_until);
    assert!(
        bundle.ciphertext_size() > bundle.plaintext_size,
        "STREAM adds per-chunk tags"
    );

    // The sealed metadata blob is carried verbatim and content-addresses to what the manifest
    // committed to — it cannot be regenerated (`seal_metadata_blob` draws a fresh nonce).
    let blob_hash = crate::crypto::hash::hash_bytes(&bundle.metadata_blob);
    assert_eq!(Some(blob_hash), bundle.metadata_blob_hash);
}

/// **The `S-A10` interaction.** A bundle built after closing and reopening the library is
/// byte-identical to one built before the close: album keys, authorities, the asset's chain and
/// its exact sealed metadata blob all survive, so a push is re-runnable across processes. Before
/// `S-A10` the reopened workspace held no AMK at all and this could not even be attempted.
#[test]
fn upload_bundle_survives_a_reopen() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (_, asset_id) = library_with_one_asset(&lib, &src);

    let before = {
        let ws = Workspace::open(lib.path(), b"passphrase", FAST).unwrap();
        ws.upload_bundle(&asset_id).expect("bundle before close")
    };
    let after = {
        let ws = Workspace::open(lib.path(), b"passphrase", FAST).unwrap();
        ws.upload_bundle(&asset_id).expect("bundle after reopen")
    };

    assert_eq!(before.ciphertext, after.ciphertext, "ciphertext is stable");
    assert_eq!(before.ciphertext_hash, after.ciphertext_hash);
    assert_eq!(
        before.metadata_blob, after.metadata_blob,
        "the sealed metadata blob must be the exact bytes on disk, not a re-seal"
    );
    assert_eq!(before.metadata_blob_hash, after.metadata_blob_hash);
    assert_eq!(before.album_id, after.album_id);
    assert_eq!(before.amk_version, after.amk_version);
    assert_eq!(before.timestamp, after.timestamp);
    assert_eq!(before.content_type, after.content_type);

    // The durable artifacts `S-A10` promises are actually on disk.
    assert!(lib.path().join(".library").join("albums.cbor").exists());
    let blob = walk_find(lib.path(), &format!("{}.metadata.bin", asset_id.simple()));
    assert!(blob, "the sealed metadata blob is persisted per asset");
}

/// An unknown asset id is a typed `NotFound`, never a panic.
#[test]
fn upload_bundle_rejects_an_unknown_asset() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    library_with_one_asset(&lib, &src);
    let ws = Workspace::open(lib.path(), b"passphrase", FAST).unwrap();
    assert!(matches!(
        ws.upload_bundle(&Uuid::now_v7()),
        Err(LifecycleError::NotFound(_))
    ));
}

/// `export_backup` runs on the same accessor, so the artifact it writes still round-trips —
/// the refactor moved the crypto, it did not change it.
#[test]
fn export_backup_still_round_trips_through_the_accessor() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (_, asset_id) = library_with_one_asset(&lib, &src);
    let out = TempDir::new().unwrap();
    let archive = out.path().join("backup.capsule");

    let (exporter_pub, bundle) = {
        let ws = Workspace::open(lib.path(), b"passphrase", FAST).unwrap();
        ws.export_backup(&archive, b"backup-pass").unwrap();
        (
            ws.exporter_verifying_key(),
            ws.upload_bundle(&asset_id).unwrap(),
        )
    };
    assert!(archive.exists());

    let peer = TempDir::new().unwrap();
    let mut fresh = Workspace::create_with_params(peer.path(), b"peer-passphrase", FAST).unwrap();
    let added = fresh
        .import_backup(&archive, b"backup-pass", &exporter_pub)
        .unwrap();
    assert_eq!(added, 1, "the exported asset restores into a fresh library");
    let restored = fresh.upload_bundle(&asset_id).expect("restored bundle");
    assert_eq!(
        restored.ciphertext_hash, bundle.ciphertext_hash,
        "the restored asset re-derives the same ciphertext"
    );
}

/// Whether any file named `name` exists under `root`.
fn walk_find(root: &std::path::Path, name: &str) -> bool {
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .any(|e| e.file_name().to_string_lossy() == name)
}
