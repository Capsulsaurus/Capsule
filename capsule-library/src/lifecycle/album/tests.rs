use std::fs;

use capsule_core::crypto::verify_asset::VerifyOutcome;
use tempfile::TempDir;

use super::super::{FAST_PARAMS, fast_workspace};
use super::*;

/// A second device of the same account, holding `album`'s keys as the first does.
fn second_device(a: &Workspace, dir: &Path) -> Workspace {
    let mut b = Workspace::create_from_master(
        dir,
        b"device b",
        FAST_PARAMS,
        capsule_core::crypto::keys::MasterKey::from_bytes(*a.account.master.as_bytes()),
        a.user_id(),
        &a.sealed_identity(),
    )
    .unwrap();
    b.apply_album_store(&a.album_store_snapshot().unwrap());
    b.persist_albums().unwrap();
    b
}

/// #667: the album's name lives in its signed record from creation, survives a reopen, and
/// a rename is a stamped write.
#[test]
fn album_name_and_labels_live_in_the_signed_record() {
    let lib = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Trip").unwrap();
    assert_eq!(ws.album_doc(&album).unwrap().current_name(), Some("Trip"));

    ws.rename_album(album, "  Lisbon 2026 ").unwrap();
    ws.add_album_label(album, "family").unwrap();
    ws.add_album_label(album, "summer").unwrap();
    assert_eq!(ws.remove_album_label(album, "summer").unwrap(), 1);
    assert_eq!(ws.remove_album_label(album, "absent").unwrap(), 0);
    assert!(matches!(
        ws.add_album_label(album, "   "),
        Err(LifecycleError::InvalidLabel(_))
    ));

    drop(ws);
    let ws = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
    assert_eq!(ws.albums(), vec![(album, "Lisbon 2026".to_string())]);
    let doc = ws.album_doc(&album).unwrap();
    assert!(doc.verify(&ws.user_ik_public()));
    assert_eq!(doc.labels.value(), BTreeSet::from(["family".to_string()]));
}

/// Creation applies the same label rule to the name as a rename does: a name a rename
/// would refuse is refused before any key is minted, and an accepted one is stored
/// normalized in both the keystore and the record.
#[test]
fn creation_and_rename_apply_the_same_name_rule() {
    let lib = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let too_long = "x".repeat(257);
    for bad in ["", "   ", too_long.as_str()] {
        assert!(matches!(
            ws.create_album(bad),
            Err(LifecycleError::InvalidLabel(_))
        ));
    }
    assert!(ws.albums().is_empty(), "a refused name mints no album");

    let album = ws.create_album("  Spaced  ").unwrap();
    assert_eq!(ws.albums(), vec![(album, "Spaced".to_string())]);
    assert_eq!(ws.album_doc(&album).unwrap().current_name(), Some("Spaced"));
    for bad in ["", "   ", too_long.as_str()] {
        assert!(matches!(
            ws.rename_album(album, bad),
            Err(LifecycleError::InvalidLabel(_))
        ));
    }
}

/// An album that predates its record reads its keystore name under the legacy stamp, so
/// any real rename wins over it.
#[test]
fn an_album_without_a_record_reads_its_keystore_name() {
    let lib = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Old").unwrap();
    fs::remove_file(album_doc_path(lib.path(), &album)).unwrap();
    let doc = ws.album_doc(&album).unwrap();
    assert_eq!(doc.current_name(), Some("Old"));
    assert_eq!(
        doc.name.current.as_ref().unwrap().ts,
        LEGACY_NAME_TS,
        "a legacy name is stamped at the epoch"
    );
    ws.rename_album(album, "New").unwrap();
    assert_eq!(ws.albums(), vec![(album, "New".to_string())]);
}

/// Two devices rename and label the same album concurrently; exchanging the sealed records
/// in either order converges both on the later name and the union of the labels.
#[test]
fn concurrent_album_edits_converge_through_sealed_records() {
    let (lib_a, lib_b) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let mut a = fast_workspace(lib_a.path());
    let album = a.create_album("Trip").unwrap();
    let mut b = second_device(&a, lib_b.path());

    a.add_album_label(album, "beach").unwrap();
    b.add_album_label(album, "family").unwrap();
    a.rename_album(album, "From A").unwrap();
    b.rename_album(album, "From B").unwrap(); // stamped later

    let from_a = a.seal_album_doc(&album).unwrap();
    let from_b = b.seal_album_doc(&album).unwrap();
    a.apply_remote_album_doc(album, &from_b).unwrap();
    b.apply_remote_album_doc(album, &from_a).unwrap();

    for ws in [&a, &b] {
        assert_eq!(ws.albums(), vec![(album, "From B".to_string())]);
        assert_eq!(
            ws.album_labels(&album).unwrap(),
            BTreeSet::from(["beach".to_string(), "family".to_string()])
        );
    }
    // Replaying a stale record changes nothing.
    a.apply_remote_album_doc(album, &from_a).unwrap();
    assert_eq!(a.albums(), vec![(album, "From B".to_string())]);
}

/// A record signed by another account is refused, and a record sealed for another album
/// does not open.
#[test]
fn a_foreign_album_record_is_refused() {
    let (lib_a, lib_c) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let mut a = fast_workspace(lib_a.path());
    let album = a.create_album("Trip").unwrap();
    let other = a.create_album("Other").unwrap();

    // Another account that somehow holds the same album keys.
    let mut c = fast_workspace(lib_c.path());
    c.apply_album_store(&a.album_store_snapshot().unwrap());
    c.persist_albums().unwrap();
    c.rename_album(album, "Hijacked").unwrap();
    let forged = c.seal_album_doc(&album).unwrap();
    assert!(matches!(
        a.apply_remote_album_doc(album, &forged),
        Err(LifecycleError::UnverifiedDocument(id)) if id == album
    ));
    assert!(a.apply_remote_album_doc(other, &forged).is_err());
    assert_eq!(a.album_doc(&album).unwrap().current_name(), Some("Trip"));
}

/// A persisted album record that was altered after signing, or filed under another album's
/// id, is refused when read rather than trusted; one that does not decode is a document
/// error.
#[test]
fn a_tampered_persisted_album_record_is_refused() {
    let lib = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Trip").unwrap();
    let other = ws.create_album("Other").unwrap();
    let path = album_doc_path(lib.path(), &album);

    let mut doc = ws.album_doc(&album).unwrap();
    doc.name.set(
        "Hijacked".into(),
        now_rfc3339(),
        ws.account.device.device_id,
    );
    fs::write(&path, doc.to_canonical_vec()).unwrap();
    assert!(matches!(
        ws.album_doc(&album),
        Err(LifecycleError::UnverifiedDocument(id)) if id == album
    ));

    fs::copy(album_doc_path(lib.path(), &other), &path).unwrap();
    assert!(matches!(
        ws.album_doc(&album),
        Err(LifecycleError::UnverifiedDocument(id)) if id == album
    ));

    fs::write(&path, b"not a record").unwrap();
    assert!(matches!(
        ws.album_doc(&album),
        Err(LifecycleError::Document(_))
    ));
}

#[test]
fn epoch_rotation_keeps_old_assets_verifiable_and_backs_up() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let a = src.path().join("a.jpg");
    let b = src.path().join("b.jpg");
    fs::write(&a, b"\xFF\xD8\xFF first photo, written at epoch 1").unwrap();
    fs::write(&b, b"\xFF\xD8\xFF second photo, written at epoch 2").unwrap();

    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Trip").unwrap();

    // Import at epoch 1, rotate the album, import at epoch 2.
    let id_a = ws.import_asset(album, &a).unwrap();
    assert_eq!(ws.rotate_epoch(album).unwrap(), 2);
    let id_b = ws.import_asset(album, &b).unwrap();

    // Each asset recorded the epoch it was written under...
    let epoch_of = |ws: &Workspace, id| {
        ws.asset(id).unwrap().chain.records()[0]
            .manifest
            .core
            .amk_version
    };
    assert_eq!(epoch_of(&ws, &id_a), AmkVersion(1));
    assert_eq!(epoch_of(&ws, &id_b), AmkVersion(2));
    // ...and BOTH still verify — the pre-rotation asset under its original epoch key (the
    // regression guard for the `current_epoch` file-key bug).
    assert_eq!(ws.verify(&id_a).unwrap(), VerifyOutcome::Accept);
    assert_eq!(ws.verify(&id_b).unwrap(), VerifyOutcome::Accept);

    // A cross-epoch backup escrows each asset's own-epoch AMK; restore into a fresh library
    // is byte-equal for both (guards the export file-key / blob-key / escrow-value epochs).
    let backup_path = src.path().join("backup.tar");
    ws.export_backup_with_params(&backup_path, b"recovery-pass", FAST_PARAMS)
        .unwrap();
    let exporter_pub = ws.exporter_verifying_key();

    let fresh = TempDir::new().unwrap();
    let mut ws2 = fast_workspace(fresh.path());
    let added = ws2
        .import_backup(&backup_path, b"recovery-pass", &exporter_pub)
        .unwrap();
    assert_eq!(added, 2);
    assert_eq!(
        ws2.read_plaintext(&id_a).unwrap(),
        ws.read_plaintext(&id_a).unwrap()
    );
    assert_eq!(
        ws2.read_plaintext(&id_b).unwrap(),
        ws.read_plaintext(&id_b).unwrap()
    );
}

/// Every album's escrow, keyed as the server lists them.
fn escrows_of(ws: &Workspace) -> BTreeMap<Uuid, Vec<u8>> {
    ws.albums()
        .into_iter()
        .map(|(id, _)| (id, ws.album_key_escrow(&id).unwrap()))
        .collect()
}

/// #741: an album is stored while the server lacks its escrow, current once it holds it, and
/// stored again once an epoch is rotated in — the escrow only ever grows. Bytes that do not
/// open as the album are left alone and the album reported uncovered, never overwritten.
#[test]
fn an_album_key_escrow_is_stored_until_it_holds_the_current_state() {
    let lib = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let default = ws.default_album_id();
    ws.ensure_album(default, "Imports").unwrap();
    let trip = ws.create_album("Trip").unwrap();
    let mut both = vec![default, trip];
    both.sort();

    let plan = ws.plan_album_key_escrows(&BTreeMap::new()).unwrap();
    assert_eq!(plan.to_store, both);
    let mut escrowed = escrows_of(&ws);
    let plan = ws.plan_album_key_escrows(&escrowed).unwrap();
    assert!(plan.to_store.is_empty());
    assert_eq!(plan.current, 2);

    ws.rotate_epoch(trip).unwrap();
    assert_eq!(
        ws.plan_album_key_escrows(&escrowed).unwrap().to_store,
        vec![trip],
        "a rotated-in epoch strictly extends the escrow"
    );

    escrowed.insert(trip, ws.album_key_escrow(&trip).unwrap());
    escrowed.insert(default, b"not an escrow".to_vec());
    let plan = ws.plan_album_key_escrows(&escrowed).unwrap();
    assert!(
        plan.to_store.is_empty(),
        "an unreadable escrow is never overwritten"
    );
    assert_eq!(plan.uncovered, vec![default]);
    assert!(matches!(
        ws.album_key_escrow(&Uuid::now_v7()),
        Err(LifecycleError::NotFound(_))
    ));
}

/// #741 D1: a device **behind** the escrow — it holds fewer epochs of an album than another
/// device escrowed — never replaces it, so a later recovery keeps every epoch; and two
/// devices that each rotated independently leave the escrow alone and report the album
/// uncovered rather than one dropping the other's epoch.
#[test]
fn a_device_behind_or_diverged_from_the_escrow_never_shrinks_it() {
    let src = TempDir::new().unwrap();
    let mut a = fast_workspace(src.path());
    let trip = a.create_album("Trip").unwrap();
    let at_epoch_1: Vec<(Uuid, Vec<u8>)> = escrows_of(&a).into_iter().collect();

    // B is recovered while the album is at epoch 1; A then rotates and escrows epoch 2.
    let dst = TempDir::new().unwrap();
    let mut b = recovered_from(&a, dst.path());
    b.adopt_album_key_escrows(&at_epoch_1).unwrap();
    a.rotate_epoch(trip).unwrap();
    let ahead = escrows_of(&a);

    let plan = b.plan_album_key_escrows(&ahead).unwrap();
    assert!(
        plan.to_store.is_empty(),
        "B's epoch-1 state would shrink A's"
    );
    assert!(
        plan.uncovered.is_empty(),
        "every epoch B holds is in A's escrow"
    );
    assert_eq!(plan.current, 1);

    // B rotates on its own: epoch 2 under another key. Neither may replace the other.
    b.rotate_epoch(trip).unwrap();
    let plan = b.plan_album_key_escrows(&ahead).unwrap();
    assert!(plan.to_store.is_empty());
    assert_eq!(plan.uncovered, vec![trip]);

    // A recovery from what the server holds still opens both of A's epochs.
    let again = TempDir::new().unwrap();
    let mut c = recovered_from(&a, again.path());
    c.adopt_album_key_escrows(&ahead.into_iter().collect::<Vec<_>>())
        .unwrap();
    assert_eq!(c.albums[&trip].amks, a.albums[&trip].amks);
}

/// #741: a library created from the master key alone adopts every escrowed album whole —
/// every epoch, its signing keys and its authority — so `ensure_album` resolves the default
/// album rather than minting over it, the recovered device can still author into it, and its
/// own escrows match the ones it adopted.
#[test]
fn a_recovered_library_adopts_every_escrowed_album_before_ensure_album() {
    let src = TempDir::new().unwrap();
    let mut a = fast_workspace(src.path());
    let default = a.default_album_id();
    a.ensure_album(default, "Imports").unwrap();
    let trip = a.create_album("Trip").unwrap();
    a.rotate_epoch(trip).unwrap();
    let escrowed: Vec<(Uuid, Vec<u8>)> = escrows_of(&a).into_iter().collect();

    let dst = TempDir::new().unwrap();
    let mut b = Workspace::create_from_master(
        dst.path(),
        b"device b",
        FAST_PARAMS,
        capsule_core::crypto::keys::MasterKey::from_bytes(*a.account.master.as_bytes()),
        a.user_id(),
        &a.sealed_identity(),
    )
    .unwrap();
    assert!(b.albums().is_empty());
    let adopted = b.adopt_album_key_escrows(&escrowed).unwrap();
    assert_eq!(
        adopted,
        AdoptedAlbumKeys {
            adopted: 2,
            epochs: 3,
            already_held: 0,
            unopenable: 0,
        }
    );
    assert_eq!(b.albums(), a.albums());
    assert_eq!(b.albums[&trip].amks, a.albums[&trip].amks);
    assert_eq!(b.albums[&trip].current_epoch, 2);

    b.ensure_album(default, "Imports").unwrap();
    assert_eq!(
        b.albums[&default].amks, a.albums[&default].amks,
        "ensure_album resolved the escrowed default album and minted nothing"
    );
    let plan = b.plan_album_key_escrows(&escrows_of(&a)).unwrap();
    assert!(
        plan.to_store.is_empty() && plan.uncovered.is_empty(),
        "the recovered library escrows exactly what it adopted"
    );

    // It writes into the adopted album under the escrowed epoch, and the asset verifies.
    let file = dst.path().join("photo.bin");
    fs::write(&file, b"recovered device bytes").unwrap();
    let asset = b.import_asset(trip, &file).unwrap();
    assert_eq!(b.verify(&asset).unwrap(), VerifyOutcome::Accept);

    // Adopting again holds what it has, and the adoption survives a reopen.
    let again = b.adopt_album_key_escrows(&escrowed).unwrap();
    assert_eq!(again.adopted, 0);
    assert_eq!(again.already_held, 2);
    drop(b);
    let b = Workspace::open(dst.path(), b"device b", FAST_PARAMS).unwrap();
    assert_eq!(b.albums(), a.albums());
}

/// A library created from `a`'s master key and identity, holding no album yet.
fn recovered_from(a: &Workspace, dir: &Path) -> Workspace {
    Workspace::create_from_master(
        dir,
        b"device b",
        FAST_PARAMS,
        capsule_core::crypto::keys::MasterKey::from_bytes(*a.account.master.as_bytes()),
        a.user_id(),
        &a.sealed_identity(),
    )
    .unwrap()
}

/// #741: an escrow that opens but is inconsistent — served under another album's id —
/// refuses the whole adoption, and nothing is persisted: a library missing one album would
/// mint over it at the next `ensure_album`.
#[test]
fn an_inconsistent_escrow_refuses_the_whole_adoption() {
    let src = TempDir::new().unwrap();
    let mut a = fast_workspace(src.path());
    let default = a.default_album_id();
    a.ensure_album(default, "Imports").unwrap();
    let trip = a.create_album("Trip").unwrap();

    let dst = TempDir::new().unwrap();
    let mut b = recovered_from(&a, dst.path());
    let escrowed = vec![
        (default, a.album_key_escrow(&default).unwrap()),
        (Uuid::now_v7(), a.album_key_escrow(&trip).unwrap()),
    ];
    assert!(matches!(
        b.adopt_album_key_escrows(&escrowed),
        Err(LifecycleError::AlbumStore(
            AlbumStoreError::EscrowAlbumMismatch { .. }
        ))
    ));
    assert!(b.albums().is_empty());
    drop(b);
    let b = Workspace::open(dst.path(), b"device b", FAST_PARAMS).unwrap();
    assert!(b.albums().is_empty(), "nothing was persisted");
}

/// #741: an escrow sealed under another master key — left by another library of the account
/// whose escrow was replaced — is skipped and counted, and the rest are adopted: refusing it
/// would lock the account out of recovery, since nothing removes it.
#[test]
fn an_escrow_under_another_master_key_is_skipped_not_fatal() {
    let src = TempDir::new().unwrap();
    let mut a = fast_workspace(src.path());
    let trip = a.create_album("Trip").unwrap();
    let other = TempDir::new().unwrap();
    let mut stranger = fast_workspace(other.path());
    let theirs = stranger.create_album("Theirs").unwrap();

    let dst = TempDir::new().unwrap();
    let mut b = recovered_from(&a, dst.path());
    let escrowed = vec![
        (trip, a.album_key_escrow(&trip).unwrap()),
        (theirs, stranger.album_key_escrow(&theirs).unwrap()),
    ];
    let report = b.adopt_album_key_escrows(&escrowed).unwrap();
    assert_eq!(report.adopted, 1);
    assert_eq!(report.unopenable, 1);
    assert_eq!(b.albums(), a.albums());
    assert!(!b.has_album(&theirs));
}
