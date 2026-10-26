use std::collections::BTreeSet;

use capsule_core::crypto::authority::{OpenMlsAuthority, ReferenceAuthority};
use capsule_core::crypto::keys::albumstore::{AlbumStoreError, PersistedAlbum};
use capsule_core::crypto::keys::{AmkVersion, HybridSigningKey, MasterKey};
use capsule_core::crypto::primitives::Argon2Params;
use capsule_core::crypto::verify_asset::{RejectReason, VerifyOutcome};
use tempfile::TempDir;

use super::*;
use crate::lifecycle::{FAST_PARAMS, fast_workspace};

/// #468: a backup restored on a new device of the same account carries the album's public
/// authority, so the restored asset **verifies** — and still cannot be written, and the
/// authority survives a reopen (it is persisted and re-verified on load, not session state).
///
/// The new device's own directory lists only itself until it adopts the account's published
/// one (#550), so the test hands it A's directory to stand in for that step; what is under
/// test is the album authority, which `verify_asset` checks separately.
#[test]
fn a_restored_asset_verifies_under_the_authority_the_artifact_carried() {
    let (lib_a, lib_b, src) = (
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
    );
    let mut a = fast_workspace(lib_a.path());
    let album = a.create_album("Trip").unwrap();
    let photo = src.path().join("photo.jpg");
    fs::write(&photo, b"\xFF\xD8\xFF a photo worth verifying").unwrap();
    let asset = a.import_asset(album, &photo).unwrap();
    let archive = src.path().join("backup.tar");
    a.export_backup_with_params(&archive, b"pw", FAST_PARAMS)
        .unwrap();

    let mut b = Workspace::create_from_master(
        lib_b.path(),
        b"device b",
        FAST_PARAMS,
        MasterKey::from_bytes(*a.account.master.as_bytes()),
        a.user_id(),
        &a.sealed_identity(),
    )
    .unwrap();
    b.import_backup(&archive, b"pw", &a.exporter_verifying_key())
        .unwrap();

    // Without the account's directory the next refusal is the device, not the authority.
    assert_eq!(
        b.verify(&asset).unwrap(),
        VerifyOutcome::TerminalReject(RejectReason::UnknownDevice)
    );
    b.directory = a.directory.clone();
    assert_eq!(b.verify(&asset).unwrap(), VerifyOutcome::Accept);
    assert!(matches!(
        b.album(&album).unwrap().write_tier_signer(),
        Err(LifecycleError::AlbumReadOnly(id)) if id == album
    ));

    // Persisted: a reopened B still verifies (with the account directory stood in again).
    drop(b);
    let mut b = Workspace::open(lib_b.path(), b"device b", FAST_PARAMS).unwrap();
    b.directory = a.directory.clone();
    assert_eq!(b.verify(&asset).unwrap(), VerifyOutcome::Accept);
}

/// #667: a restored device recovers the album's name, labels and its stacks' labels from
/// the label records the artifact carries — not `Recovered album` — and keeps them across a
/// reopen.
#[test]
fn a_restored_device_recovers_album_names_and_labels() {
    use capsule_core::domain::StackType;
    use capsule_core::sidecar::sidecar_v1::{StackMembership, StackRole};

    let (lib_a, lib_b, src) = (
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
    );
    let mut a = fast_workspace(lib_a.path());
    let album = a.create_album("Trip").unwrap();
    a.rename_album(album, "Lisbon 2026").unwrap();
    a.add_album_label(album, "family").unwrap();
    let photo = src.path().join("photo.jpg");
    fs::write(&photo, b"\xFF\xD8\xFF a named album's photo").unwrap();
    let asset = a.import_asset(album, &photo).unwrap();
    let stack = Uuid::now_v7();
    a.set_stack_membership(
        &asset,
        Some(StackMembership {
            stack_id: stack,
            stack_type: StackType::Burst,
            role: StackRole::Primary,
            member_index: Some(0),
        }),
    )
    .unwrap();
    a.add_stack_label(stack, "keeper").unwrap();
    let archive = src.path().join("backup.tar");
    a.export_backup_with_params(&archive, b"pw", FAST_PARAMS)
        .unwrap();

    let mut b = Workspace::create_from_master(
        lib_b.path(),
        b"device b",
        FAST_PARAMS,
        MasterKey::from_bytes(*a.account.master.as_bytes()),
        a.user_id(),
        &a.sealed_identity(),
    )
    .unwrap();
    b.import_backup(&archive, b"pw", &a.exporter_verifying_key())
        .unwrap();

    assert_eq!(b.albums(), vec![(album, "Lisbon 2026".to_string())]);
    assert_eq!(
        b.album_labels(&album).unwrap(),
        BTreeSet::from(["family".to_string()])
    );
    assert_eq!(
        b.stack_labels(stack).unwrap(),
        BTreeSet::from(["keeper".to_string()])
    );

    drop(b);
    let b = Workspace::open(lib_b.path(), b"device b", FAST_PARAMS).unwrap();
    assert_eq!(b.albums(), vec![(album, "Lisbon 2026".to_string())]);
    assert_eq!(
        b.album_doc(&album).unwrap().current_name(),
        Some("Lisbon 2026")
    );
}

/// #747: the evidence logs a library keeps beside an asset — custody receipts and signed
/// storage attestations — ride the backup and come back byte for byte on a restored device,
/// where a reopen still finds the asset's original rather than mistaking a log for it.
#[test]
fn evidence_logs_ride_the_backup_and_restore_byte_for_byte() {
    use capsule_core::crypto::keys::HybridSigningKey;
    use capsule_core::crypto::storage_attestation::{
        AttestedAsset, STORAGE_ATTESTATION_VERSION, StorageAttestation, StorageAttestationCore,
    };

    let (lib_a, lib_b, src) = (
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
    );
    let mut a = fast_workspace(lib_a.path());
    let album = a.create_album("Trip").unwrap();
    let photo = src.path().join("photo.jpg");
    let original = b"\xFF\xD8\xFF a photo with evidence".to_vec();
    fs::write(&photo, &original).unwrap();
    let asset = a.import_asset(album, &photo).unwrap();
    let capture = Some(a.asset(&asset).unwrap().capture_utc);

    let server = HybridSigningKey::from_seed64(&[3; 64]);
    let core = StorageAttestationCore {
        version: STORAGE_ATTESTATION_VERSION.to_owned(),
        server_id: "capsule.example".to_owned(),
        server_key_id: Hash32([0x11; 32]),
        account: a.user_id().to_string(),
        nonce: [4; 32],
        checked_at: "2026-10-21T00:00:00Z".to_owned(),
        verdicts: vec![AttestedAsset {
            asset_id: asset.to_string(),
            durable: true,
            blobs: Vec::new(),
        }],
    };
    let attestation = StorageAttestation {
        server_sig: server.sign(&core.signing_bytes()),
        core,
    }
    .to_canonical_cbor();
    crate::library::append_attestation(lib_a.path(), &asset, capture, &attestation).unwrap();
    // The receipt log is carried verbatim, so any bytes stand in for it here.
    fs::write(
        crate::library::receipts_path(lib_a.path(), &asset, capture),
        b"receipt log",
    )
    .unwrap();
    let attestation_log = fs::read(crate::library::attestations_path(
        lib_a.path(),
        &asset,
        capture,
    ))
    .unwrap();

    let archive = src.path().join("backup.tar");
    a.export_backup_with_params(&archive, b"pw", FAST_PARAMS)
        .unwrap();
    let mut b = same_account_device(&a, lib_b.path());
    b.import_backup(&archive, b"pw", &a.exporter_verifying_key())
        .unwrap();

    let restored_capture = Some(b.asset(&asset).unwrap().capture_utc);
    assert_eq!(
        fs::read(crate::library::attestations_path(
            lib_b.path(),
            &asset,
            restored_capture
        ))
        .unwrap(),
        attestation_log,
        "the attestation log restores byte for byte"
    );
    assert_eq!(
        fs::read(crate::library::receipts_path(
            lib_b.path(),
            &asset,
            restored_capture
        ))
        .unwrap(),
        b"receipt log",
        "the receipt log restores byte for byte"
    );
    let decoded =
        crate::library::load_attestations(lib_b.path(), &asset, restored_capture).unwrap();
    assert_eq!(decoded.len(), 1);
    assert!(decoded[0].verify_under(&server.verifying_key()));

    // A reopen walks the media directory: the logs are neither sidecars nor the original.
    drop(b);
    let b = Workspace::open(lib_b.path(), b"device b", FAST_PARAMS).unwrap();
    assert_eq!(b.read_plaintext(&asset).unwrap(), original);
    assert!(b.unanchored_sidecars().is_empty());
}

/// A library with one photo in one album, exported to `archive`, with `pins` pinned first.
fn exported_with_pins(
    lib: &Path,
    src: &Path,
    pins: &[(&str, &[HybridVerifyingKey])],
) -> (Workspace, Uuid, std::path::PathBuf) {
    let mut a = fast_workspace(lib);
    let album = a.create_album("Trip").unwrap();
    let photo = src.join("photo.jpg");
    fs::write(&photo, b"\xFF\xD8\xFF a photo pushed somewhere").unwrap();
    let asset = a.import_asset(album, &photo).unwrap();
    for (origin, keys) in pins {
        crate::library::pin_attestation_keys(lib, origin, keys).unwrap();
    }
    let archive = src.join("backup.tar");
    a.export_backup_with_params(&archive, b"pw", FAST_PARAMS)
        .unwrap();
    (a, asset, archive)
}

fn server_key(seed: u8) -> HybridVerifyingKey {
    HybridSigningKey::from_seed64(&[seed; 64]).verifying_key()
}

const SERVER: &str = "https://capsule.example";

/// #805: the pin rides the backup byte for byte, and a run after the restore still refuses
/// a server that dropped a key pinned before the backup.
#[test]
fn the_attestation_key_pin_rides_the_backup_byte_for_byte() {
    let (lib_a, lib_b, src) = (
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
    );
    let (a, asset, archive) = exported_with_pins(
        lib_a.path(),
        src.path(),
        &[(SERVER, &[server_key(1), server_key(2)])],
    );
    let pin = fs::read(crate::library::attestation_key_pins_path(lib_a.path())).unwrap();

    let mut b = same_account_device(&a, lib_b.path());
    assert_eq!(
        b.import_backup(&archive, b"pw", &a.exporter_verifying_key())
            .unwrap(),
        1
    );

    assert_eq!(
        fs::read(crate::library::attestation_key_pins_path(lib_b.path())).unwrap(),
        pin,
        "the pin restores byte for byte"
    );
    assert!(b.asset(&asset).is_some());
    let error =
        crate::library::pin_attestation_keys(lib_b.path(), SERVER, &[server_key(2)]).unwrap_err();
    assert!(
        matches!(
            error,
            crate::library::KeyPinError::HistoryRewritten { missing: 1, .. }
        ),
        "{error:?}"
    );
}

/// #805: a restore into a library whose pin diverges from the artifact's is refused before
/// it writes anything — neither pin, nor any asset or key.
#[test]
fn a_diverging_pin_refuses_the_restore_and_writes_nothing() {
    let (lib_a, lib_b, src) = (
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
    );
    let (a, asset, archive) = exported_with_pins(
        lib_a.path(),
        src.path(),
        &[(SERVER, &[server_key(1), server_key(2)])],
    );
    let mut b = same_account_device(&a, lib_b.path());
    crate::library::pin_attestation_keys(lib_b.path(), SERVER, &[server_key(1), server_key(9)])
        .unwrap();
    let pin_before = fs::read(crate::library::attestation_key_pins_path(lib_b.path())).unwrap();

    let error = b
        .import_backup(&archive, b"pw", &a.exporter_verifying_key())
        .unwrap_err();

    assert!(
        matches!(
            &error,
            LifecycleError::Backup(backup::BackupError::AttestationKeyPinsDiverge {
                origin,
                only_carried: 1,
                only_local: 1,
            }) if origin == SERVER
        ),
        "{error:?}"
    );
    assert_eq!(
        fs::read(crate::library::attestation_key_pins_path(lib_b.path())).unwrap(),
        pin_before,
        "the library's pin is untouched"
    );
    assert!(b.asset(&asset).is_none(), "no asset was restored");
    let album = a.asset(&asset).unwrap().album_id;
    assert!(b.album(&album).is_err(), "no album key was absorbed");
}

/// An artifact whose pin has a schema newer than this build reads still restores its assets:
/// the pin is skipped with a warning, never a reason to refuse the backup.
#[test]
fn a_pin_of_a_newer_schema_does_not_block_the_restore() {
    let (lib_a, lib_b, src) = (
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
    );
    let (a, asset, _) = exported_with_pins(lib_a.path(), src.path(), &[]);
    #[derive(serde::Serialize)]
    struct FuturePinFile {
        version: u8,
        servers: String,
    }
    let mut input = a.backup_input().unwrap();
    // A pin file as a later build might write it: a newer version, a shape this one lacks.
    input.attestation_key_pins = capsule_core::cbor::to_canonical_vec(&FuturePinFile {
        version: u8::MAX,
        servers: "from the future".to_owned(),
    })
    .unwrap();
    let archive = src.path().join("newer-pin.tar");
    let bytes =
        backup::export_with_params(&input, b"pw", FAST_PARAMS, a.device_signer.as_ref()).unwrap();
    fs::write(&archive, bytes).unwrap();
    let mut b = same_account_device(&a, lib_b.path());

    assert_eq!(
        b.import_backup(&archive, b"pw", &a.exporter_verifying_key())
            .unwrap(),
        1
    );

    assert!(b.asset(&asset).is_some());
    assert!(!crate::library::attestation_key_pins_path(lib_b.path()).exists());
}

/// An artifact from a library that pinned nothing — or from before the pin rode the
/// artifact — restores as before and leaves no pin behind.
#[test]
fn an_artifact_without_a_pin_restores_as_before() {
    let (lib_a, lib_b, src) = (
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
    );
    let (a, asset, archive) = exported_with_pins(lib_a.path(), src.path(), &[]);
    let mut b = same_account_device(&a, lib_b.path());

    assert_eq!(
        b.import_backup(&archive, b"pw", &a.exporter_verifying_key())
            .unwrap(),
        1
    );

    assert!(b.asset(&asset).is_some());
    assert!(!crate::library::attestation_key_pins_path(lib_b.path()).exists());
}

/// A restore never overwrites an evidence log already on disk, and writes none when the
/// artifact carried none.
#[test]
fn restoring_an_evidence_log_keeps_one_already_on_disk() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("media").join("x.attestations.cbor");

    restore_evidence_log(&path, &[]).unwrap();
    assert!(!path.exists(), "nothing carried, nothing written");

    restore_evidence_log(&path, b"from the artifact").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"from the artifact");
    assert!(
        !crate::library::tmp_path(&path).exists(),
        "the staged temporary was renamed into place"
    );

    restore_evidence_log(&path, b"a different copy").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"from the artifact");
}

/// An evidence log that is absent reads as empty; one that cannot be read fails the export
/// rather than leaving the artifact silently without it.
#[test]
fn an_unreadable_evidence_log_fails_rather_than_vanishing() {
    let dir = TempDir::new().unwrap();
    assert!(
        read_evidence_log(&dir.path().join("absent.cbor"))
            .unwrap()
            .is_empty()
    );
    // A directory where the log should be is a read error that is not "not found".
    assert!(matches!(
        read_evidence_log(dir.path()),
        Err(LifecycleError::Io(_))
    ));
}

/// A second device of `a`'s account in `dir`, holding none of its albums.
fn same_account_device(a: &Workspace, dir: &Path) -> Workspace {
    Workspace::create_from_master(
        dir,
        b"device b",
        FAST_PARAMS,
        MasterKey::from_bytes(*a.account.master.as_bytes()),
        a.user_id(),
        &a.sealed_identity(),
    )
    .unwrap()
}

/// A persisted label record that does not decode is left out of the export with a warning
/// rather than failing it: the assets, keys and every other record still travel, and the
/// album it named restores as `Recovered album`.
#[test]
fn an_unreadable_label_record_is_left_out_of_the_export() {
    use capsule_core::domain::StackType;
    use capsule_core::sidecar::sidecar_v1::{StackMembership, StackRole};

    let (lib_a, lib_b, src) = (
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
    );
    let mut a = fast_workspace(lib_a.path());
    let album = a.create_album("Trip").unwrap();
    let photo = src.path().join("photo.jpg");
    fs::write(&photo, b"\xFF\xD8\xFF a photo whose album record broke").unwrap();
    let asset = a.import_asset(album, &photo).unwrap();
    let stack = Uuid::now_v7();
    a.set_stack_membership(
        &asset,
        Some(StackMembership {
            stack_id: stack,
            stack_type: StackType::Burst,
            role: StackRole::Primary,
            member_index: Some(0),
        }),
    )
    .unwrap();
    a.add_stack_label(stack, "keeper").unwrap();
    fs::write(
        super::super::album::album_doc_path(lib_a.path(), &album),
        b"not a record",
    )
    .unwrap();
    assert!(a.album_doc(&album).is_err());

    let archive = src.path().join("backup.tar");
    a.export_backup_with_params(&archive, b"pw", FAST_PARAMS)
        .unwrap();

    let mut b = same_account_device(&a, lib_b.path());
    assert_eq!(
        b.import_backup(&archive, b"pw", &a.exporter_verifying_key())
            .unwrap(),
        1
    );
    assert_eq!(b.albums(), vec![(album, RECOVERED_ALBUM_NAME.to_string())]);
    assert_eq!(
        b.stack_labels(stack).unwrap(),
        BTreeSet::from(["keeper".to_string()])
    );
}

/// A record the artifact carries that does not open under its escrowed AMK is skipped on
/// restore: the assets, keys and every other record are restored around it.
#[test]
fn a_label_record_that_will_not_open_is_skipped_on_restore() {
    let (lib_a, lib_b, src) = (
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
    );
    let mut a = fast_workspace(lib_a.path());
    let album = a.create_album("Trip").unwrap();
    let photo = src.path().join("photo.jpg");
    fs::write(&photo, b"\xFF\xD8\xFF a photo beside a broken record").unwrap();
    a.import_asset(album, &photo).unwrap();

    let mut input = a.backup_input().unwrap();
    let broken_stack = Uuid::now_v7();
    input.docs.push(BackupDoc {
        kind: BackupDocKind::Stack,
        doc_id: broken_stack,
        album_id: album,
        amk_version: 1,
        sealed: vec![0; 64],
    });
    let bytes =
        backup::export_with_params(&input, b"pw", FAST_PARAMS, a.device_signer.as_ref()).unwrap();
    let archive = src.path().join("backup.tar");
    fs::write(&archive, bytes).unwrap();

    let mut b = same_account_device(&a, lib_b.path());
    assert_eq!(
        b.import_backup(&archive, b"pw", &a.exporter_verifying_key())
            .unwrap(),
        1
    );
    assert_eq!(b.albums(), vec![(album, "Trip".to_string())]);
    assert!(matches!(
        b.stack_labels(broken_stack),
        Err(LifecycleError::NotFound(_))
    ));
}

/// #551: an adopted web-upload drop (`key_mode = wrapped`) survives a backup round trip —
/// export ships the guest's ciphertext, and restore unwraps its key rather than deriving
/// one, so a fresh library of the same account reads the guest's plaintext back.
#[test]
fn an_adopted_drop_round_trips_through_backup() {
    use capsule_core::drop::{DropAdopter, LinkCaps, UploadLinkIssuer, seal_drop};

    let (lib_a, lib_b, src) = (
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
    );
    let mut a = fast_workspace(lib_a.path());
    let album = a.create_album("Guests").unwrap();
    let link = UploadLinkIssuer::create_link(&mut a, LinkCaps::default(), None).unwrap();
    let plaintext = b"\xFF\xD8\xFF a guest photo worth backing up";
    let sealed = seal_drop(plaintext, &link.drop_pubkey, "image/jpeg").unwrap();
    let drop_id = a.receive_drop(link.link_id, sealed).unwrap();
    let asset = DropAdopter::adopt(&mut a, drop_id, album)
        .unwrap()
        .asset_id();
    let archive = src.path().join("backup.tar");
    a.export_backup_with_params(&archive, b"pw", FAST_PARAMS)
        .unwrap();

    let mut b = Workspace::create_from_master(
        lib_b.path(),
        b"device b",
        FAST_PARAMS,
        MasterKey::from_bytes(*a.account.master.as_bytes()),
        a.user_id(),
        &a.sealed_identity(),
    )
    .unwrap();
    assert_eq!(
        b.import_backup(&archive, b"pw", &a.exporter_verifying_key())
            .unwrap(),
        1
    );
    assert_eq!(b.read_plaintext(&asset).unwrap(), plaintext);
    b.directory = a.directory.clone();
    assert_eq!(b.verify(&asset).unwrap(), VerifyOutcome::Accept);
}

/// An album holding a held authority that no longer re-verifies (here a ledger whose admin
/// key was swapped) still gains the artifact's content keys: the held authority is neither
/// replaced nor re-checked, so it cannot fail the import. An adopted authority that does not
/// verify is still refused.
#[test]
fn a_held_tampered_authority_does_not_block_key_recovery() {
    let album = Uuid::from_u128(0xA1);
    let admin = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
    let write_tier = HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32]);
    let mut tampered = ReferenceAuthority::new(album, admin.verifying_key())
        .with_epoch(&admin, AmkVersion(1), &write_tier.verifying_key(), true)
        .to_ledger();
    tampered.admin_pub = HybridSigningKey::from_seed_bytes(&[5; 32], &[6; 32]).verifying_key();
    let tampered = PersistedAuthority::Reference(Box::new(tampered));
    let valid = PersistedAuthority::Reference(Box::new(
        ReferenceAuthority::new(album, admin.verifying_key())
            .with_epoch(&admin, AmkVersion(1), &write_tier.verifying_key(), true)
            .to_ledger(),
    ));
    assert!(tampered.restore(album, &BTreeSet::from([1])).is_err());

    // Held and tampered: the import proceeds and adds the missing epoch.
    let mut store = AlbumStore::new();
    store.upsert(PersistedAlbum::recovered(
        album,
        "Trip",
        vec![(album, 1, [1; 32])],
        Some(tampered.clone()),
    ));
    let (added, adopted) = merge_album(
        &mut store,
        album,
        "Trip",
        vec![(album, 1, [1; 32]), (album, 2, [2; 32])],
        Some(valid),
    )
    .unwrap();
    assert_eq!((added, adopted), (1, false));
    assert_eq!(
        store.get(&album).unwrap().held_epochs(),
        BTreeSet::from([1, 2])
    );
    assert!(matches!(
        store.get(&album).unwrap().authority,
        Some(PersistedAuthority::Reference(ref l)) if l.admin_pub != admin.verifying_key()
    ));

    // Adopted and tampered: refused.
    let mut store = AlbumStore::new();
    assert!(matches!(
        merge_album(&mut store, album, "Trip", vec![(album, 1, [1; 32])], Some(tampered)),
        Err(LifecycleError::AlbumStore(AlbumStoreError::LedgerRejected(id))) if id == album
    ));
}

/// Decision 2 of #468: an album whose authority is an MLS group exports its content keys but
/// no `authority/` entry, since the group's exported state holds private secrets.
#[test]
fn an_mls_authority_album_exports_without_an_authority_entry() {
    let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Group").unwrap();
    let photo = src.path().join("photo.jpg");
    fs::write(&photo, b"\xFF\xD8\xFF an mls album photo").unwrap();
    ws.import_asset(album, &photo).unwrap();
    ws.authorities.insert(
        album,
        Authority::OpenMls(Box::new(
            OpenMlsAuthority::create_self_group(crate::ambient::system_ids(), album).unwrap(),
        )),
    );

    let archive = src.path().join("backup.tar");
    ws.export_backup_with_params(&archive, b"pw", FAST_PARAMS)
        .unwrap();
    let artifact = BackupArtifact::open(
        &fs::read(&archive).unwrap(),
        b"pw",
        &ws.exporter_verifying_key(),
    )
    .unwrap();
    assert!(artifact.authority_ledgers().is_empty());
    assert!(artifact.amk_rows().iter().any(|row| row.0 == album));
}

/// The escrow verbs are a pair: what `escrow_master_key` mints is exactly what
/// `verify_escrow` opens back to *this* device's master key, and a wrong secret does not
/// verify. The master key itself never appears in either signature — that is the point of
/// putting these on `Workspace` rather than exposing the raw bytes.
#[test]
fn escrow_round_trips_under_its_recovery_secret() {
    let lib = TempDir::new().unwrap();
    let ws = fast_workspace(lib.path());
    // The FFI/CLI callers use a `DeviceTier`; a test uses the fast cost directly through
    // the same `pwkdf::wrap` the tier resolves to, so the suite does not pay 256 MiB.
    let blob = capsule_core::crypto::pwkdf::wrap_with(
        ws.account.master.as_bytes(),
        b"correct horse battery staple",
        Argon2Params {
            mem_kib: 64,
            t_cost: 1,
            p_cost: 1,
        },
    )
    .unwrap();

    assert_eq!(
        ws.verify_escrow(&blob, b"correct horse battery staple"),
        RecoveryVerdict::Verified
    );
    assert_eq!(
        ws.verify_escrow(&blob, b"the wrong secret"),
        RecoveryVerdict::NotVerified
    );
}

/// `count` distinct originals of `len` bytes each, imported into one album of `a`.
fn import_originals(a: &mut Workspace, src: &Path, count: u8, len: usize) -> Vec<Uuid> {
    let album = a.create_album("Large").unwrap();
    (0..count)
        .map(|n| {
            let mut bytes = b"\xFF\xD8\xFF a large original ".to_vec();
            bytes.extend((0..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(n)));
            let photo = src.join(format!("large-{n}.jpg"));
            fs::write(&photo, &bytes).unwrap();
            a.import_asset(album, &photo).unwrap()
        })
        .collect()
}

/// #759: exporting holds one STREAM chunk of an original at a time, not the library. The
/// originals here sum to several times the ceiling and each alone exceeds it, so an export
/// that buffered even one of them whole would fail — and the artifact still restores every
/// original byte for byte.
#[test]
fn exporting_a_backup_holds_a_chunk_not_the_library() {
    const CEILING: usize = 2 << 20;
    const ORIGINAL: usize = 3 << 20;
    let (lib_a, lib_b, src) = (
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
    );
    let mut a = fast_workspace(lib_a.path());
    let assets = import_originals(&mut a, src.path(), 4, ORIGINAL);
    let archive = src.path().join("backup.tar");

    let (exported, peak) =
        crate::test_heap::peak_during(|| a.export_backup_with_params(&archive, b"pw", FAST_PARAMS));
    exported.unwrap();
    assert!(
        peak < CEILING,
        "peak heap {peak} B exceeds the {CEILING} B ceiling over {} B of originals",
        assets.len() * ORIGINAL
    );

    let mut b = same_account_device(&a, lib_b.path());
    assert_eq!(
        b.import_backup(&archive, b"pw", &a.exporter_verifying_key())
            .unwrap(),
        assets.len()
    );
    for id in &assets {
        assert_eq!(
            fs::read(b.media_path(&b.assets[id])).unwrap(),
            fs::read(a.media_path(&a.assets[id])).unwrap(),
            "asset {id} restores byte for byte"
        );
    }
}

/// A streamed export that fails part-way — here an original rewritten on disk after its
/// manifest was signed, so its bytes no longer hash to the address the MANIFEST already
/// committed to — leaves no partial artifact and does not clobber the backup already at
/// the destination.
#[test]
fn a_failed_export_leaves_the_previous_backup_untouched() {
    let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let mut a = fast_workspace(lib.path());
    let id = import_originals(&mut a, src.path(), 1, 4096)[0];
    let archive = src.path().join("backup.tar");
    fs::write(&archive, b"the previous backup").unwrap();

    let input = a.backup_input().unwrap();
    let media = a.media_path(&a.assets[&id]);
    let mut changed = fs::read(&media).unwrap();
    *changed.last_mut().unwrap() ^= 0xFF;
    fs::write(&media, &changed).unwrap();

    let err = write_artifact(&archive, |w| {
        backup::export_with_params_to(&input, b"pw", FAST_PARAMS, a.device_signer.as_ref(), w)
    })
    .unwrap_err();
    assert!(
        matches!(err, LifecycleError::Backup(backup::BackupError::Corrupt(_))),
        "{err:?}"
    );
    assert_eq!(fs::read(&archive).unwrap(), b"the previous backup");
    assert!(!crate::library::tmp_path(&archive).exists());
}

// ── Peer ingest ──────────────────────────────────────────────────────────────

/// Two devices of one account: `a` with `count` photos in one album, and `b` restored from
/// `a`'s backup, so it holds the album, its keys and those photos.
fn paired(lib_a: &Path, lib_b: &Path, src: &Path, count: u8) -> (Workspace, Workspace, Uuid) {
    let mut a = fast_workspace(lib_a);
    let album = a.create_album("Trip").unwrap();
    for n in 0..count {
        let photo = src.join(format!("held-{n}.jpg"));
        fs::write(
            &photo,
            [b"\xFF\xD8\xFF a held photo ".as_slice(), &[n]].concat(),
        )
        .unwrap();
        a.import_asset(album, &photo).unwrap();
    }
    let archive = src.join("pairing.tar");
    a.export_backup_with_params(&archive, b"pw", FAST_PARAMS)
        .unwrap();
    let mut b = Workspace::create_from_master(
        lib_b,
        b"device b",
        FAST_PARAMS,
        MasterKey::from_bytes(*a.account.master.as_bytes()),
        a.user_id(),
        &a.sealed_identity(),
    )
    .unwrap();
    b.import_backup(&archive, b"pw", &a.exporter_verifying_key())
        .unwrap();
    (a, b, album)
}

/// What a restore of `a`'s whole library returns: the assets a peer ingest would hand over.
fn restored_from(a: &Workspace, src: &Path) -> BTreeMap<Uuid, backup::RestoredAsset> {
    let archive = src.join("peer.tar");
    a.export_backup_with_params(&archive, b"pw", FAST_PARAMS)
        .unwrap();
    let bytes = fs::read(&archive).unwrap();
    BackupArtifact::open(&bytes, b"pw", &a.exporter_verifying_key())
        .unwrap()
        .restore(RestoreMode::Commit, &BTreeMap::new())
        .unwrap()
        .applied
        .into_iter()
        .map(|asset| (asset.asset_id, asset))
        .collect()
}

/// The peer export is every managed asset, each with the album key it is sealed under.
#[test]
fn the_peer_export_carries_every_asset_and_its_album_key() {
    let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let mut a = fast_workspace(lib.path());
    let ids = import_originals(&mut a, src.path(), 2, 64);
    let export = a.peer_export().unwrap();
    let exported: BTreeSet<Uuid> = export.assets.iter().map(|asset| asset.asset_id).collect();
    assert_eq!(exported, ids.iter().copied().collect());
    for asset in &export.assets {
        let epoch = asset.provenance.last().unwrap().manifest.core.amk_version.0;
        assert_eq!(
            export.amks.get(&(asset.album_id, epoch)),
            a.album(&asset.album_id).unwrap().amks.get(&epoch),
            "asset {} is exported with its own album key",
            asset.asset_id
        );
    }
}

/// An asset a peer delivers that this device lacks is written whole: held at the peer's head,
/// its original on disk byte for byte, and still there after a reopen.
#[test]
fn a_new_peer_asset_is_written_and_survives_a_reopen() {
    let (lib_a, lib_b, src) = (
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
    );
    let (mut a, mut b, album) = paired(lib_a.path(), lib_b.path(), src.path(), 1);
    let photo = src.path().join("new.jpg");
    fs::write(&photo, b"\xFF\xD8\xFF taken after the pairing").unwrap();
    let new = a.import_asset(album, &photo).unwrap();
    assert!(b.asset(&new).is_none());

    let restored = restored_from(&a, src.path());
    b.apply_peer_restored(&restored[&new]).unwrap();

    assert_eq!(b.local_heads().get(&new), a.local_heads().get(&new));
    assert_eq!(
        fs::read(b.media_path(&b.assets[&new])).unwrap(),
        b"\xFF\xD8\xFF taken after the pairing"
    );
    drop(b);
    let b = Workspace::open(lib_b.path(), b"device b", FAST_PARAMS).unwrap();
    assert_eq!(b.local_heads().get(&new), a.local_heads().get(&new));
}

/// A forward update of an asset this device holds adopts the peer's head and rewrites the
/// original where it already is: the held media path and date bucket are kept.
#[test]
fn a_forward_peer_update_keeps_the_held_media_path() {
    let (lib_a, lib_b, src) = (
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
    );
    let (mut a, mut b, _) = paired(lib_a.path(), lib_b.path(), src.path(), 1);
    let held = *b.assets.keys().next().unwrap();
    let path_before = b.media_path(&b.assets[&held]);
    let capture_before = b.assets[&held].capture_utc;
    a.tag_add(&held, "keeper").unwrap();
    assert_ne!(b.local_heads().get(&held), a.local_heads().get(&held));

    let restored = restored_from(&a, src.path());
    b.apply_peer_restored(&restored[&held]).unwrap();

    assert_eq!(b.local_heads().get(&held), a.local_heads().get(&held));
    assert_eq!(b.media_path(&b.assets[&held]), path_before);
    assert_eq!(b.assets[&held].capture_utc, capture_before);
    assert_eq!(b.assets.len(), 1);
    drop(b);
    let b = Workspace::open(lib_b.path(), b"device b", FAST_PARAMS).unwrap();
    assert_eq!(b.local_heads().get(&held), a.local_heads().get(&held));
    assert_eq!(b.media_path(&b.assets[&held]), path_before);
}

/// An asset of an album this device does not hold is refused, and nothing of it is written.
#[test]
fn a_peer_asset_of_an_album_not_held_is_refused_unwritten() {
    let (lib_a, lib_c, src) = (
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
    );
    let mut a = fast_workspace(lib_a.path());
    let id = import_originals(&mut a, src.path(), 1, 64)[0];
    let restored = restored_from(&a, src.path());
    let mut c = fast_workspace(lib_c.path());

    let refused = c.apply_peer_restored(&restored[&id]).unwrap_err();
    assert!(
        matches!(refused, LifecycleError::NotFound(_)),
        "{refused:?}"
    );
    assert!(c.asset(&id).is_none());
    assert!(c.local_heads().is_empty());
}
