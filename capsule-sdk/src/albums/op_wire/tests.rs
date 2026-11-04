//! The shared lifecycle op shape against a real library chain: the envelope projects the signed
//! core verbatim, and the stretch rule picks what the server lacks.

use capsule_core::crypto::hash::hash_bytes;
use capsule_core::crypto::provenance::Action;
use capsule_library::lifecycle::Workspace;
use uuid::Uuid;

use super::*;

/// A library holding one imported asset, trashed then purged early: its kept chain is the
/// create, the trash `delete`, and the shorter-retention re-issue.
fn purged_chain(root: &std::path::Path) -> Vec<ProvenanceRecord> {
    let lib = root.join("library");
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
    let source = root.join("photo.jpg");
    std::fs::write(&source, b"a photo").expect("a source");
    let opts = capsule_library::lifecycle::SignedImportOptions {
        move_source: false,
        defer_source_release: false,
        stack: None,
        enrichment: None,
    };
    let asset: Uuid = ws
        .import_asset_with(album, &source, &opts)
        .expect("imported")
        .asset_id;
    ws.soft_delete(&asset, 30).expect("trashed");
    ws.purge(&asset).expect("purged");
    ws.purged_chain(&asset).expect("kept").to_vec()
}

fn actions(stretch: &[ProvenanceRecord]) -> Vec<Action> {
    stretch.iter().map(|r| r.manifest.core.action).collect()
}

#[test]
fn chain_after_is_the_records_the_server_lacks() {
    let root = tempfile::TempDir::new().unwrap();
    let chain = purged_chain(root.path());
    assert_eq!(
        actions(&chain),
        vec![Action::Create, Action::Delete, Action::Delete]
    );
    let hash = |at: usize| Some(chain[at].record_hash());

    assert_eq!(chain_after(&chain, hash(0)), &chain[1..]);
    assert_eq!(chain_after(&chain, hash(1)), &chain[2..]);
    assert!(chain_after(&chain, hash(2)).is_empty(), "level: nothing");
    for unknown in [Some(hash_bytes(b"elsewhere")), None] {
        assert_eq!(
            chain_after(&chain, unknown),
            &chain[2..],
            "unknown: the head alone"
        );
    }
    assert!(chain_after(&[], None).is_empty());
}

#[test]
fn stretch_after_replays_the_head_when_level() {
    let root = tempfile::TempDir::new().unwrap();
    let chain = purged_chain(root.path());
    let hash = |at: usize| Some(chain[at].record_hash());

    assert_eq!(stretch_after(&chain, hash(0)), &chain[1..]);
    assert_eq!(stretch_after(&chain, hash(1)), &chain[2..]);
    assert_eq!(
        stretch_after(&chain, hash(2)),
        &chain[2..],
        "level: a replay"
    );
    assert_eq!(stretch_after(&chain, None), &chain[2..]);
    assert!(stretch_after(&[], None).is_empty());
}

#[test]
fn the_envelope_projects_the_signed_core_verbatim() {
    let root = tempfile::TempDir::new().unwrap();
    let chain = purged_chain(root.path());
    let reissue = &chain[2].manifest.core;
    let envelope = op_envelope(reissue);

    assert_eq!(envelope.action, "delete");
    assert_eq!(envelope.file_id, reissue.file_id.to_string());
    assert_eq!(envelope.album_id, Some(reissue.album_id.to_string()));
    assert_eq!(envelope.ciphertext_hash, reissue.ciphertext_hash.to_hex());
    assert_eq!(
        envelope.prior_provenance_hash,
        Some(chain[1].record_hash().to_hex()),
        "the re-issue names the delete it chains onto"
    );
    assert_eq!(envelope.retention_until, reissue.retention_until);
    assert_eq!(envelope.timestamp, reissue.timestamp);
    assert!(
        !envelope.key_mode.is_empty(),
        "the key mode is its wire name"
    );
    assert_eq!(envelope.metadata_blob_hash, None, "a delete binds no blob");
    assert_eq!(
        (
            envelope.original_blob_hash,
            envelope.derivative_role,
            envelope.derivative_format
        ),
        (None, None, None),
        "a lifecycle op names no derivative"
    );
}
