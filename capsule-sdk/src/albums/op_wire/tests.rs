//! The shared lifecycle op shape against a real library chain: the envelope projects the signed
//! core verbatim, the stretch rule picks what the server lacks, and the held rules decide
//! whether it may go now: held until the create is indexed, and never a too-late restore of a
//! held delete.

use capsule_core::crypto::hash::hash_bytes;
use capsule_core::crypto::provenance::Action;
use capsule_library::lifecycle::Workspace;
use uuid::Uuid;

use super::*;

/// A library holding one imported asset: its chain is a lone `create`.
fn library(root: &std::path::Path) -> (Workspace, Uuid) {
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
    (ws, asset)
}

/// A library holding one imported asset, trashed then purged early: its kept chain is the
/// create, the trash `delete`, and the shorter-retention re-issue.
fn purged_chain(root: &std::path::Path) -> Vec<ProvenanceRecord> {
    let (mut ws, asset) = library(root);
    ws.soft_delete(&asset, 30).expect("trashed");
    ws.purge(&asset).expect("purged");
    ws.purged_chain(&asset).expect("kept").to_vec()
}

fn records(ws: &Workspace, asset: &Uuid) -> Vec<ProvenanceRecord> {
    ws.asset(asset).expect("held").chain.records().to_vec()
}

fn actions(stretch: &[ProvenanceRecord]) -> Vec<Action> {
    stretch.iter().map(|r| r.manifest.core.action).collect()
}

fn delivery_actions(delivery: &HeldDelivery<'_>) -> Option<Vec<Action>> {
    match delivery {
        HeldDelivery::Send(stretch) => Some(actions(stretch)),
        HeldDelivery::Held { .. } => None,
    }
}

fn now() -> jiff::Timestamp {
    jiff::Timestamp::now()
}

/// Far past any window a test signs.
fn much_later() -> jiff::Timestamp {
    now() + jiff::SignedDuration::from_hours(24 * 365)
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

/// **The server never sees a record of an asset it has not indexed.** Whatever the chain
/// holds, an asset the feed does not list is held, and nothing of it is sent.
#[test]
fn an_asset_the_server_has_not_indexed_is_held() {
    let root = tempfile::TempDir::new().unwrap();
    let (mut ws, asset) = library(root.path());
    ws.soft_delete(&asset, 30).expect("deleted");
    let chain = records(&ws, &asset);

    assert_eq!(
        plan_delivery(&chain, None, now()),
        HeldDelivery::Held {
            reason: HoldReason::NotIndexed,
            retention_until: None,
        }
    );
}

/// Once the create is indexed, the records after the server's head go, in chain order: a
/// delete, and a restore made after it, each chaining onto the one before.
#[test]
fn an_indexed_asset_is_sent_the_stretch_after_the_servers_head() {
    let root = tempfile::TempDir::new().unwrap();
    let (mut ws, asset) = library(root.path());
    let create = records(&ws, &asset)[0].record_hash();
    ws.soft_delete(&asset, 30).expect("deleted");

    let chain = records(&ws, &asset);
    assert_eq!(
        delivery_actions(&plan_delivery(&chain, Some(Some(create)), now())),
        Some(vec![Action::Delete])
    );

    ws.restore(&asset).expect("restored");
    let chain = records(&ws, &asset);
    assert_eq!(
        delivery_actions(&plan_delivery(&chain, Some(Some(create)), now())),
        Some(vec![Action::Delete, Action::TrashRestore]),
        "a restore inside the window goes after the delete it undoes"
    );
}

/// A server already level with the library is sent the head alone, which it answers as a
/// replay; a head the server holds that the chain does not know (or none at all) is the head
/// alone too, which the server judges and refuses as a stale chain if it must.
#[test]
fn a_level_or_unknown_server_head_is_sent_the_local_head_alone() {
    let root = tempfile::TempDir::new().unwrap();
    let (mut ws, asset) = library(root.path());
    ws.soft_delete(&asset, 30).expect("deleted");
    let chain = records(&ws, &asset);
    let head = chain.last().expect("a head").record_hash();

    for server in [Some(head), Some(hash_bytes(b"elsewhere")), None] {
        assert_eq!(
            delivery_actions(&plan_delivery(&chain, Some(server), now())),
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
    let root = tempfile::TempDir::new().unwrap();
    let (mut ws, asset) = library(root.path());
    let create = records(&ws, &asset)[0].record_hash();
    ws.soft_delete(&asset, 1).expect("deleted");
    let until = records(&ws, &asset)
        .last()
        .and_then(|r| r.manifest.core.retention_until.clone())
        .expect("a window");
    ws.restore(&asset).expect("restored");
    let chain = records(&ws, &asset);

    assert_eq!(
        plan_delivery(&chain, Some(Some(create)), much_later()),
        HeldDelivery::Held {
            reason: HoldReason::RestoreTooLate,
            retention_until: Some(until.clone()),
        }
    );
    let at_close: jiff::Timestamp = until.parse().expect("a timestamp");
    assert!(
        matches!(
            plan_delivery(&chain, Some(Some(create)), at_close),
            HeldDelivery::Held {
                reason: HoldReason::RestoreTooLate,
                ..
            }
        ),
        "a window is closed at its own instant"
    );
    assert_eq!(
        delivery_actions(&plan_delivery(&chain, Some(Some(create)), now())),
        Some(vec![Action::Delete, Action::TrashRestore]),
        "inside the window the pair goes"
    );
}

/// A restore is judged against the delete right before it: a later delete whose window has
/// closed, with no restore after it, withholds nothing.
#[test]
fn a_restore_is_judged_against_the_delete_it_undoes() {
    let root = tempfile::TempDir::new().unwrap();
    let (mut ws, asset) = library(root.path());
    let create = records(&ws, &asset)[0].record_hash();
    ws.soft_delete(&asset, 30).expect("deleted");
    ws.restore(&asset).expect("restored");
    ws.soft_delete(&asset, 1).expect("deleted again");
    let chain = records(&ws, &asset);
    // Past the second delete's one-day window, inside the first's thirty days.
    let judge = now() + jiff::SignedDuration::from_hours(24 * 2);

    assert_eq!(
        delivery_actions(&plan_delivery(&chain, Some(Some(create)), judge)),
        Some(vec![Action::Delete, Action::TrashRestore, Action::Delete]),
        "no restore undoes the closed delete, so nothing is withheld"
    );
}

/// The withholding covers only a delete the server has not seen. One already applied leaves
/// the restore alone in the stretch, and the server's own clock judges it.
#[test]
fn a_restore_of_a_delete_the_server_holds_is_sent_whatever_the_clock() {
    let root = tempfile::TempDir::new().unwrap();
    let (mut ws, asset) = library(root.path());
    ws.soft_delete(&asset, 1).expect("deleted");
    let delete = records(&ws, &asset).last().expect("a delete").record_hash();
    ws.restore(&asset).expect("restored");
    let chain = records(&ws, &asset);

    assert_eq!(
        delivery_actions(&plan_delivery(&chain, Some(Some(delete)), much_later())),
        Some(vec![Action::TrashRestore])
    );
}

/// The judging clock is the device's, floored by the latest server `changed_at` a drain saw.
#[test]
fn the_judging_clock_is_floored_by_the_servers() {
    let seen: jiff::Timestamp = "2026-10-03T00:00:00Z".parse().expect("a timestamp");
    let behind: jiff::Timestamp = "2026-01-01T00:00:00Z".parse().expect("a timestamp");
    assert_eq!(
        judging_clock(behind, Some(seen)),
        seen,
        "a lagging clock is floored"
    );
    let ahead = much_later();
    assert_eq!(
        judging_clock(ahead, Some(seen)),
        ahead,
        "a clock past the floor is kept"
    );
    assert_eq!(judging_clock(behind, None), behind, "no drain, no floor");
}
