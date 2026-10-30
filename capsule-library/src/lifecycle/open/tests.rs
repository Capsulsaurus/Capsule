use capsule_core::crypto::keys::{ClassicalAlgorithm, encapsulate_to_p256_public};
use capsule_core::crypto::primitives::Argon2Params;
use capsule_core::crypto::verify_asset::VerifyOutcome;
use tempfile::TempDir;

use super::super::{FAST_PARAMS, fast_workspace};
use super::*;
use crate::account::tests::MockP256Signer;

/// **The core claim.** An asset imported in one session is fully usable in the next:
/// its album key comes back from the sealed keystore, so the workspace can re-derive the file
/// key (proved by `verify_asset` accepting, which regenerates the ciphertext and re-checks its
/// content address) and can open the sealed metadata blob it persisted to disk.
#[test]
fn reopened_workspace_decrypts_an_asset_imported_before_close() {
    use capsule_core::crypto::encryption::{blob_nonce, open_blob};
    use capsule_core::crypto::keys::Amk;

    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("photo.jpg");
    let bytes = b"\xFF\xD8\xFF durable album key round trip".to_vec();
    fs::write(&img, &bytes).unwrap();

    let (album, id, blob_before) = {
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Trip").unwrap();
        let id = ws.import_asset(album, &img).unwrap();
        ws.tag_add(&id, "coast").unwrap();
        (album, id, ws.asset(&id).unwrap().metadata_blob.clone())
    };
    assert!(!blob_before.is_empty());

    let ws2 = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();

    // The album's key material is back...
    assert!(ws2.has_album(&album), "the album survived the close");
    assert_eq!(ws2.albums(), vec![(album, "Trip".to_string())]);
    // ...and so is the asset, with the exact sealed blob bytes the manifest committed to.
    let restored = ws2.asset(&id).expect("asset restored from disk");
    assert_eq!(restored.album_id, album);
    assert_eq!(restored.metadata_blob, blob_before);
    assert_eq!(ws2.read_plaintext(&id).unwrap(), bytes);
    assert_eq!(restored.sidecar.tags_user.value().len(), 1);

    // The real proof: `verify_asset` re-derives the file key from the restored AMK,
    // re-encrypts, and matches the manifest's committed ciphertext hash. A wrong (or fresh)
    // AMK cannot produce that.
    assert_eq!(ws2.verify(&id).unwrap(), VerifyOutcome::Accept);

    // And the sealed metadata blob opens under the restored AMK's derived blob key.
    let head = &restored.chain.records().last().unwrap().manifest;
    let amk = Amk::from_bytes(ws2.album(&album).unwrap().amks[&head.core.amk_version.0]);
    let nonce = blob_nonce(&restored.metadata_blob).unwrap();
    let blob_key = amk.derive_blob_key(&id, &nonce);
    let plaintext = open_blob(&blob_key, &restored.metadata_blob).unwrap();
    assert_eq!(
        plaintext,
        restored.sidecar.to_canonical_vec(),
        "the persisted blob decrypts to the signed sidecar under the restored AMK"
    );
}

/// A reopened library keeps *writing* into the album it already has — the write-tier
/// and admin keys and the attested authority all survive, so a second-session import self-
/// verifies under the same epoch instead of needing a brand-new album.
#[test]
fn reopened_workspace_writes_into_the_same_album() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let first = src.path().join("first.jpg");
    let second = src.path().join("second.jpg");
    fs::write(&first, b"\xFF\xD8\xFF session one").unwrap();
    fs::write(&second, b"\xFF\xD8\xFF session two").unwrap();

    let (album, id1) = {
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Imports").unwrap();
        let id1 = ws.import_asset(album, &first).unwrap();
        (album, id1)
    };

    let mut ws2 = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
    // Resolve-or-create returns the SAME album rather than minting a second one.
    assert_eq!(ws2.ensure_album(album, "Imports").unwrap(), album);
    assert_eq!(ws2.albums().len(), 1, "no duplicate album was minted");

    let id2 = ws2.import_asset(album, &second).unwrap();
    assert_eq!(ws2.verify(&id2).unwrap(), VerifyOutcome::Accept);
    // The pre-close asset still verifies in the same session as the new one.
    assert_eq!(ws2.verify(&id1).unwrap(), VerifyOutcome::Accept);
    // Both landed under the same epoch of the same album.
    let epoch_of = |ws: &Workspace, id: &Uuid| {
        ws.asset(id).unwrap().chain.records()[0]
            .manifest
            .core
            .amk_version
    };
    assert_eq!(epoch_of(&ws2, &id1), epoch_of(&ws2, &id2));
    // Metadata edits work too (they need the write-tier key and the counter).
    ws2.tag_add(&id1, "kept").unwrap();
    assert_eq!(ws2.verify(&id1).unwrap(), VerifyOutcome::Accept);

    // A rotation still works after a reopen: it needs the persisted admin key.
    assert_eq!(ws2.rotate_epoch(album).unwrap(), 2);
}

/// The authority — not just the content key — is durable: the admin-signed epoch
/// ledger comes back verifying, with the epoch ceiling and per-epoch write-tier keys intact,
/// and its local-only `amk_present` flags restored from the epochs actually held.
#[test]
fn reopened_workspace_restores_the_reference_authority_ledger() {
    use capsule_core::crypto::authority::AlbumAuthority;
    use capsule_core::crypto::keys::AmkVersion;

    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("photo.jpg");
    fs::write(&img, b"\xFF\xD8\xFF written at epoch one").unwrap();

    let (album, ceiling, write_pubs) = {
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Trip").unwrap();
        ws.import_asset(album, &img).unwrap();
        assert_eq!(ws.rotate_epoch(album).unwrap(), 2);
        let authority = ws.authority(&album).unwrap();
        let pubs: Vec<_> = (1..=2)
            .map(|e| authority.write_tier_pubkey(AmkVersion(e)))
            .collect();
        (album, authority.epoch_ceiling(), pubs)
    };

    let ws2 = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
    let authority = ws2.authority(&album).expect("authority restored");
    assert!(
        authority.admin_chain_verifies(),
        "the persisted ledger must re-verify its admin signature chain"
    );
    assert_eq!(authority.epoch_ceiling(), ceiling);
    assert_eq!(authority.album_id(), album);
    for (i, expected) in write_pubs.iter().enumerate() {
        assert_eq!(
            &authority.write_tier_pubkey(AmkVersion(i as u32 + 1)),
            expected,
            "epoch {} write-tier key survives the reopen",
            i + 1
        );
    }
    // `amk_present` is local-only state, restored from the epochs whose AMK is actually held.
    assert!(authority.has_amk(AmkVersion(1)));
    assert!(authority.has_amk(AmkVersion(2)));
}

/// **Migration.** A library written before durable album keys has no `albums.cbor`. It
/// must still open — with zero albums and a warning — rather than failing. This is not a
/// regression: those assets' AMKs never existed anywhere but in the process that minted them.
#[test]
fn legacy_library_without_album_store_opens_empty() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("photo.jpg");
    fs::write(&img, b"\xFF\xD8\xFF pre-S-A10 asset").unwrap();

    let id = {
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Trip").unwrap();
        ws.import_asset(album, &img).unwrap()
    };

    // Simulate the on-disk shape before album keys were durable: signed artifacts present,
    // keystore absent.
    let store = capsule_core::crypto::keys::AlbumStore::path(lib.path());
    assert!(store.exists());
    fs::remove_file(&store).unwrap();

    let ws2 = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
    assert!(ws2.albums().is_empty(), "no album keys are recoverable");
    // The asset itself is still tracked (its plaintext is on disk) — only its key is gone.
    assert!(ws2.asset(&id).is_some());
    assert_eq!(
        ws2.read_plaintext(&id).unwrap(),
        b"\xFF\xD8\xFF pre-S-A10 asset"
    );
    // And every key-bearing operation refuses with a typed error rather than panicking.
    assert!(matches!(ws2.verify(&id), Err(LifecycleError::NotFound(_))));
    assert!(matches!(
        ws2.export_backup(&src.path().join("b.tar"), b"pw"),
        Err(LifecycleError::NotFound(_))
    ));
}

/// `export_backup` reads each asset's album AMK *and* its sealed metadata blob, so
/// it is the sharpest test that both survived the close — and the restored artifact must be
/// byte-identical to one exported before it.
#[test]
fn backup_export_round_trips_after_reopen() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("photo.jpg");
    let bytes = b"\xFF\xD8\xFF exported after a reopen".to_vec();
    fs::write(&img, &bytes).unwrap();

    let id = {
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Trip").unwrap();
        let id = ws.import_asset(album, &img).unwrap();
        ws.tag_add(&id, "coast").unwrap();
        id
    };

    // Export from the *reopened* workspace.
    let ws2 = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
    let archive = src.path().join("backup.tar");
    ws2.export_backup_with_params(&archive, b"recovery-pass", FAST_PARAMS)
        .unwrap();
    let exporter_pub = ws2.exporter_verifying_key();

    // Restore into a fresh library and confirm the bytes come back.
    let fresh = TempDir::new().unwrap();
    let mut ws3 = fast_workspace(fresh.path());
    assert_eq!(
        ws3.import_backup(&archive, b"recovery-pass", &exporter_pub)
            .unwrap(),
        1
    );
    assert_eq!(ws3.read_plaintext(&id).unwrap(), bytes);

    // The restore folded the escrowed AMK into ws3's durable keystore, so ws3 can itself
    // re-export the asset — the "keyless library made whole again" path.
    let album = ws3.asset(&id).unwrap().album_id;
    assert!(ws3.has_album(&album));
    let again = fresh.path().join("re-export.tar");
    ws3.export_backup_with_params(&again, b"pass-two", FAST_PARAMS)
        .unwrap();
    assert!(again.exists());

    // And those recovered keys are durable in ws3 too: reopening it keeps them.
    drop(ws3);
    let ws4 = Workspace::open(fresh.path(), b"passphrase", FAST_PARAMS).unwrap();
    assert!(
        ws4.has_album(&album),
        "recovered AMKs were persisted, not just held for the session"
    );
    assert_eq!(ws4.read_plaintext(&id).unwrap(), bytes);
}

/// An album recovered purely from a backup artifact holds content keys and no
/// signing capability — the artifact escrows AMKs, never the write-tier/admin keys or the
/// admin-signed ledger. Authoring a new write into it must be a typed refusal, not a fresh
/// admin key minted behind the user's back.
#[test]
fn a_backup_recovered_album_is_readable_but_not_writable() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("photo.jpg");
    fs::write(&img, b"\xFF\xD8\xFF escrowed key only").unwrap();

    let archive = src.path().join("backup.tar");
    let (album, exporter_pub) = {
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Trip").unwrap();
        ws.import_asset(album, &img).unwrap();
        ws.export_backup_with_params(&archive, b"pw", FAST_PARAMS)
            .unwrap();
        (album, ws.exporter_verifying_key())
    };

    let fresh = TempDir::new().unwrap();
    let mut ws2 = fast_workspace(fresh.path());
    ws2.import_backup(&archive, b"pw", &exporter_pub).unwrap();

    assert!(ws2.has_album(&album));
    let keys = ws2.album(&album).unwrap();
    assert!(keys.write_tier.is_none(), "no escrowed write capability");
    assert!(keys.admin.is_none(), "no escrowed admin capability");
    assert!(matches!(
        keys.write_tier_signer(),
        Err(LifecycleError::AlbumReadOnly(id)) if id == album
    ));
    // A new import into it refuses cleanly...
    let other = src.path().join("other.jpg");
    fs::write(&other, b"\xFF\xD8\xFF new write").unwrap();
    assert!(matches!(
        ws2.import_asset(album, &other),
        Err(LifecycleError::AlbumReadOnly(_))
    ));
    // ...as does a rotation, without half-mutating the AMK map.
    let epochs_before = ws2.album(&album).unwrap().amks.len();
    assert!(matches!(
        ws2.rotate_epoch(album),
        Err(LifecycleError::AlbumReadOnly(_))
    ));
    assert_eq!(ws2.album(&album).unwrap().amks.len(), epochs_before);
}

/// `create_album_with_id` refuses to mint over an album that already holds key
/// material: doing so would discard the AMKs every existing asset in it was encrypted under.
#[test]
fn creating_an_album_that_already_exists_is_refused() {
    let lib = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let id = ws.default_album_id();
    assert_eq!(ws.create_album_with_id(id, "Imports").unwrap(), id);
    assert!(matches!(
        ws.create_album_with_id(id, "Imports again"),
        Err(LifecycleError::AlbumExists(dup)) if dup == id
    ));
    // Resolve-or-create is the safe verb.
    assert_eq!(ws.ensure_album(id, "Imports").unwrap(), id);
    assert_eq!(ws.albums(), vec![(id, "Imports".to_string())]);
}

/// **#467.** A library created from a recovered master key, the account id, and the sealed
/// identity *is* that account on a new device: the same id, default album and identity key,
/// a fresh device, a directory that verifies under the account's IK — and all of it survives
/// a reopen under the new device's passphrase.
#[test]
fn a_library_created_from_the_master_key_opens_as_the_account() {
    use capsule_core::crypto::keys::MasterKey;

    let lib_a = TempDir::new().unwrap();
    let lib_b = TempDir::new().unwrap();
    let a = fast_workspace(lib_a.path());
    let master = MasterKey::from_bytes(*a.account.master.as_bytes());

    let b = Workspace::create_from_master(
        lib_b.path(),
        b"device b passphrase",
        FAST_PARAMS,
        master,
        a.user_id(),
        &a.sealed_identity(),
    )
    .unwrap();
    assert_eq!(b.user_id(), a.user_id());
    assert_eq!(b.default_album_id(), a.default_album_id());
    assert_eq!(b.user_ik_public(), a.user_ik_public());
    assert_ne!(
        b.device_id(),
        a.device_id(),
        "a recovered device is a new device"
    );

    let directory = b.device_directory();
    assert!(directory.verify(&a.user_ik_public()));
    assert_eq!(directory.core.user_id, a.user_id());
    assert!(directory.device(&b.device_id()).is_some());

    let device_b = b.device_id();
    drop(b);
    let reopened = Workspace::open(lib_b.path(), b"device b passphrase", FAST_PARAMS).unwrap();
    assert_eq!(reopened.user_id(), a.user_id());
    assert_eq!(reopened.device_id(), device_b);
    assert_eq!(reopened.user_ik_public(), a.user_ik_public());
}

/// **#467.** A master key that did not seal the identity is refused before anything is
/// written: the sealed identity is also the proof that the master key is the account's.
#[test]
fn creating_from_a_foreign_master_key_is_refused_and_writes_nothing() {
    use capsule_core::crypto::keys::MasterKey;

    let lib_a = TempDir::new().unwrap();
    let lib_b = TempDir::new().unwrap();
    let a = fast_workspace(lib_a.path());
    let root_b = lib_b.path().join("library");

    let result = Workspace::create_from_master(
        &root_b,
        b"pw",
        FAST_PARAMS,
        MasterKey::generate(),
        a.user_id(),
        &a.sealed_identity(),
    );
    assert!(matches!(
        result,
        Err(LifecycleError::Crypto(CryptoError::Auth(_)))
    ));
    assert!(
        !root_b.exists(),
        "no library is initialised for a refused key"
    );
}

/// Account A with a second device cross-signed in (so its directory is past version 1 and
/// lists more than one device), plus a library recovered onto a new device of that account.
fn account_with_two_devices_and_a_recovered_one(
    lib_a: &Path,
    lib_b: &Path,
) -> (Workspace, Workspace, DeviceEntry) {
    use capsule_core::crypto::keys::{HybridSigningKey, MasterKey};

    let mut a = fast_workspace(lib_a);
    let other = DeviceEntry {
        device_id: Uuid::now_v7(),
        dsk_public: HybridSigningKey::generate().verifying_key(),
        dek_public: None,
        added_at: DEVICE_ADDED_AT.into(),
        revoked_at: None,
        unknown: Default::default(),
    };
    a.cross_sign_device(other.clone()).unwrap();
    let master = MasterKey::from_bytes(*a.account.master.as_bytes());
    let b = Workspace::create_from_master(
        lib_b,
        b"device b passphrase",
        FAST_PARAMS,
        master,
        a.user_id(),
        &a.sealed_identity(),
    )
    .unwrap();
    (a, b, other)
}

/// **#550.** A recovered device builds on the account's published directory: it keeps every
/// device already listed, appends its own entry at `stored + 1`, and signs under the account's
/// IK — so the result is both fresh (the server's stale-version check passes) and complete.
#[test]
fn a_recovered_device_extends_the_published_directory_at_the_next_version() {
    let lib_a = TempDir::new().unwrap();
    let lib_b = TempDir::new().unwrap();
    let (a, mut b, other) =
        account_with_two_devices_and_a_recovered_one(lib_a.path(), lib_b.path());
    let published = a.device_directory().clone();
    assert!(published.core.directory_version >= 1);
    assert_eq!(
        b.device_directory().core.directory_version,
        1,
        "before extending, B holds only the version-1 directory naming itself"
    );

    let extended = b
        .extend_account_directory(&published)
        .unwrap()
        .expect("B is not yet listed, so there is a new version to publish");
    assert_eq!(
        extended.core.directory_version,
        published.core.directory_version + 1
    );
    assert!(
        extended.verify(&a.user_ik_public()),
        "signed under the account's IK"
    );
    assert_eq!(extended.core.user_id, a.user_id());
    assert_eq!(extended.core.devices.len(), 3);
    assert_eq!(
        extended.device(&a.device_id()),
        published.device(&a.device_id())
    );
    assert_eq!(extended.device(&other.device_id), Some(&other));
    let own = extended.device(&b.device_id()).expect("B lists itself");
    assert!(own.dek_public.is_some(), "B's entry carries its DEK");
    assert_eq!(
        b.device_directory(),
        &extended,
        "B adopted what it returned"
    );
}

/// **#550.** Extending again from a directory that already lists this device under its own
/// key adopts it unchanged and has nothing to publish — the reopened-recovered-device case.
#[test]
fn extending_a_directory_that_already_lists_this_device_adopts_it_unchanged() {
    let lib_a = TempDir::new().unwrap();
    let lib_b = TempDir::new().unwrap();
    let (a, mut b, _) = account_with_two_devices_and_a_recovered_one(lib_a.path(), lib_b.path());
    let extended = b
        .extend_account_directory(a.device_directory())
        .unwrap()
        .unwrap();

    let device_b = b.device_id();
    drop(b);
    let mut reopened = Workspace::open(lib_b.path(), b"device b passphrase", FAST_PARAMS).unwrap();
    assert_eq!(reopened.device_id(), device_b);
    assert_eq!(
        reopened.device_directory(),
        &extended,
        "#562: the extended directory is restored, not rebuilt at version 1"
    );
    assert_eq!(reopened.extend_account_directory(&extended).unwrap(), None);
    assert_eq!(reopened.device_directory(), &extended);
}

/// **#550.** A directory that does not verify under the account's IK, or that names another
/// account, is refused and nothing is adopted: building on it would let whoever served it
/// choose the devices this account trusts.
#[test]
fn extending_an_untrusted_directory_is_refused_and_changes_nothing() {
    let lib_a = TempDir::new().unwrap();
    let lib_b = TempDir::new().unwrap();
    let lib_x = TempDir::new().unwrap();
    let (a, mut b, _) = account_with_two_devices_and_a_recovered_one(lib_a.path(), lib_b.path());
    let before = b.device_directory().clone();

    // Another account's directory, signed under its own IK.
    let foreign = fast_workspace(lib_x.path()).device_directory().clone();
    assert!(matches!(
        b.extend_account_directory(&foreign),
        Err(LifecycleError::UntrustedDirectory(_))
    ));

    // A's directory with its core tampered after signing.
    let mut tampered = a.device_directory().clone();
    tampered.core.devices.pop();
    assert!(matches!(
        b.extend_account_directory(&tampered),
        Err(LifecycleError::UntrustedDirectory(_))
    ));

    // Correctly signed under the account's IK, but naming another account id.
    let mut core = a.device_directory().core.clone();
    core.user_id = Uuid::now_v7();
    let misnamed = core.sign(&a.account.user_ik);
    assert!(matches!(
        b.extend_account_directory(&misnamed),
        Err(LifecycleError::UntrustedDirectory(_))
    ));

    assert_eq!(b.device_directory(), &before, "nothing was adopted");
}

/// **#550.** A published directory that lists this device's id under a different signing key
/// is a conflict, not something to paper over: refused, nothing adopted.
#[test]
fn extending_a_directory_listing_this_device_under_another_key_is_refused() {
    use capsule_core::crypto::keys::{DirectoryError, HybridSigningKey};

    let lib_a = TempDir::new().unwrap();
    let lib_b = TempDir::new().unwrap();
    let (mut a, mut b, _) =
        account_with_two_devices_and_a_recovered_one(lib_a.path(), lib_b.path());
    let impostor = DeviceEntry {
        device_id: b.device_id(),
        dsk_public: HybridSigningKey::generate().verifying_key(),
        dek_public: None,
        added_at: DEVICE_ADDED_AT.into(),
        revoked_at: None,
        unknown: Default::default(),
    };
    let published = a.cross_sign_device(impostor).unwrap();
    let before = b.device_directory().clone();

    assert!(matches!(
        b.extend_account_directory(&published),
        Err(LifecycleError::Directory(DirectoryError::DeviceAlreadyListed(id))) if id == b.device_id()
    ));
    assert_eq!(b.device_directory(), &before);
}

/// **#550.** A published directory that lists this device's id under its own signing key but
/// **revoked** is refused, not adopted: a revoked device must not re-admit itself by
/// extending the directory that revoked it, nor treat that directory as already listing it.
#[test]
fn extending_a_directory_listing_this_device_revoked_is_refused() {
    use capsule_core::crypto::keys::DirectoryError;

    let lib_a = TempDir::new().unwrap();
    let lib_b = TempDir::new().unwrap();
    let (a, mut b, _) = account_with_two_devices_and_a_recovered_one(lib_a.path(), lib_b.path());
    let mut revoked = b
        .device_directory()
        .device(&b.device_id())
        .cloned()
        .expect("B lists itself");
    revoked.revoked_at = Some(DEVICE_ADDED_AT.into());
    // `cross_sign_device` refuses a revoked entry, so the account signs one in directly, as a
    // directory that later revoked B would read.
    let mut core = a.device_directory().core.clone();
    core.devices.push(revoked);
    core.directory_version += 1;
    let published = core.sign(&a.account.user_ik);
    assert!(published.verify(&b.user_ik_public()));
    let before = b.device_directory().clone();

    assert!(matches!(
        b.extend_account_directory(&published),
        Err(LifecycleError::Directory(DirectoryError::DeviceAlreadyListed(id))) if id == b.device_id()
    ));
    assert_eq!(b.device_directory(), &before, "nothing was adopted");
}

/// **#467.** A fresh library adopts the server's account id: the account file, the in-memory
/// account, and a re-signed directory at the next version all name it, and a reopen keeps it.
#[test]
fn binding_the_server_account_id_persists_and_re_signs_the_directory() {
    let lib = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    let before = ws.device_directory().core.directory_version;
    let server_id = Uuid::now_v7();

    ws.bind_account_id(server_id).unwrap();
    assert_eq!(ws.user_id(), server_id);
    let account_path = lib.path().join(".library").join("account.cbor");
    assert!(
        !crate::utils::paths::tmp_path(&account_path).exists(),
        "the staged account file was renamed into place, not left beside it"
    );
    let directory = ws.device_directory();
    assert_eq!(directory.core.user_id, server_id);
    assert_eq!(directory.core.directory_version, before + 1);
    assert!(directory.verify(&ws.user_ik_public()));
    assert!(directory.device(&ws.device_id()).is_some());

    // Binding the same id again changes nothing.
    ws.bind_account_id(server_id).unwrap();
    assert_eq!(ws.device_directory().core.directory_version, before + 1);

    drop(ws);
    let reopened = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
    assert_eq!(reopened.user_id(), server_id);
    assert_eq!(reopened.device_directory().core.user_id, server_id);
    assert_eq!(
        reopened.device_directory().core.directory_version,
        before + 1,
        "#562: the bumped version survives the reopen"
    );
}

/// A workspace at `lib` that has cross-signed one other device, so its directory is at
/// version 2 and lists two devices.
fn workspace_with_a_cross_signed_device(lib: &Path) -> Workspace {
    use capsule_core::crypto::keys::HybridSigningKey;

    let mut ws = fast_workspace(lib);
    ws.cross_sign_device(DeviceEntry {
        device_id: Uuid::now_v7(),
        dsk_public: HybridSigningKey::generate().verifying_key(),
        dek_public: None,
        added_at: DEVICE_ADDED_AT.into(),
        revoked_at: None,
        unknown: Default::default(),
    })
    .unwrap();
    assert_eq!(ws.device_directory().core.directory_version, 2);
    ws
}

/// **#562.** A bind whose directory write fails still binds (the account file names the new
/// id), and a retry with the same id is what writes the re-signed directory, so a reopen
/// restores it with every device it lists rather than rebuilding at version 1.
#[test]
fn a_bind_retry_persists_the_directory_an_earlier_bind_failed_to_write() {
    let lib = TempDir::new().unwrap();
    let mut ws = workspace_with_a_cross_signed_device(lib.path());
    let server_id = Uuid::now_v7();
    // A directory where the file belongs makes the rename fail.
    let directory_path = Workspace::directory_path(lib.path());
    fs::remove_file(&directory_path).unwrap();
    fs::create_dir(&directory_path).unwrap();

    let err = ws.bind_account_id(server_id).unwrap_err();
    assert!(matches!(err, LifecycleError::Io(_)), "{err:?}");
    assert_eq!(ws.user_id(), server_id, "the account file was rewritten");
    let bound = ws.device_directory().clone();
    assert_eq!(bound.core.user_id, server_id);
    assert_eq!(bound.core.directory_version, 3);

    fs::remove_dir(&directory_path).unwrap();
    ws.bind_account_id(server_id).unwrap();
    assert_eq!(ws.device_directory(), &bound, "the retry re-signs nothing");
    drop(ws);

    let reopened = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
    assert_eq!(reopened.device_directory(), &bound);
    assert_eq!(reopened.device_directory().core.devices.len(), 2);
}

/// **#562.** A bind whose directory write never landed, and that nobody retried, leaves the
/// pre-rebind directory on disk: signed by this account's IK but naming the old id. The
/// reopen re-signs it under the bound id at the next version and keeps every device it
/// lists, instead of a version-1 rebuild that is stale against what was published.
#[test]
fn a_directory_naming_the_pre_rebind_id_is_re_signed_on_open() {
    let lib = TempDir::new().unwrap();
    let mut ws = workspace_with_a_cross_signed_device(lib.path());
    let pre_rebind = ws.device_directory().clone();
    let server_id = Uuid::now_v7();
    ws.bind_account_id(server_id).unwrap();
    let user_ik = ws.user_ik_public();
    drop(ws);
    // As though the bind's directory write had failed after the account file was rewritten.
    plant_directory(lib.path(), &pre_rebind);

    let reopened = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
    let opened = reopened.device_directory();
    assert_eq!(opened.core.user_id, server_id);
    assert_eq!(opened.core.directory_version, 3);
    assert_eq!(opened.core.devices, pre_rebind.core.devices);
    assert!(opened.verify(&user_ik));
    let on_disk: DeviceDirectory =
        cbor::from_slice(&fs::read(Workspace::directory_path(lib.path())).unwrap()).unwrap();
    assert_eq!(on_disk, pre_rebind, "the open writes nothing back");
}

/// Replace the persisted directory of the library at `lib` with `directory`.
fn plant_directory(lib: &Path, directory: &DeviceDirectory) {
    fs::write(
        Workspace::directory_path(lib),
        cbor::to_canonical_vec(directory).unwrap(),
    )
    .unwrap();
}

/// **#562.** A library written before the directory was persisted has no `directory.cbor`:
/// it opens with a fresh version-1 directory listing only this device, as before.
#[test]
fn a_library_without_a_persisted_directory_opens_with_a_fresh_one() {
    let lib = TempDir::new().unwrap();
    let ws = fast_workspace(lib.path());
    let device = ws.device_id();
    drop(ws);
    fs::remove_file(Workspace::directory_path(lib.path())).unwrap();

    let reopened = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
    let directory = reopened.device_directory();
    assert_eq!(directory.core.directory_version, 1);
    assert_eq!(directory.core.devices.len(), 1);
    assert!(directory.device(&device).is_some());
    assert!(directory.verify(&reopened.user_ik_public()));
}

/// **#562.** A persisted directory that is undecodable, tampered, foreign, or that does not
/// list this device live under its own key is never adopted: the reopen falls back to a
/// fresh version-1 directory, and the refused file is left where it was.
#[test]
fn an_untrustworthy_persisted_directory_is_refused_on_open() {
    use capsule_core::crypto::keys::HybridSigningKey;

    let lib = TempDir::new().unwrap();
    let lib_x = TempDir::new().unwrap();
    let mut ws = fast_workspace(lib.path());
    ws.cross_sign_device(DeviceEntry {
        device_id: Uuid::now_v7(),
        dsk_public: HybridSigningKey::generate().verifying_key(),
        dek_public: None,
        added_at: DEVICE_ADDED_AT.into(),
        revoked_at: None,
        unknown: Default::default(),
    })
    .unwrap();
    let genuine = ws.device_directory().clone();
    assert_eq!(genuine.core.directory_version, 2);
    let device = ws.device_id();
    let ik = ws.account.user_ik.clone();
    drop(ws);

    let mut tampered = genuine.clone();
    tampered.core.directory_version += 1;

    let foreign = fast_workspace(lib_x.path()).device_directory().clone();

    let mut without_me = genuine.core.clone();
    without_me.devices.retain(|d| d.device_id != device);
    let without_me = without_me.sign(&ik);

    let mut rekeyed = genuine.core.clone();
    for d in &mut rekeyed.devices {
        if d.device_id == device {
            d.dsk_public = HybridSigningKey::generate().verifying_key();
        }
    }
    let rekeyed = rekeyed.sign(&ik);

    let mut revoked = genuine.core.clone();
    for d in &mut revoked.devices {
        if d.device_id == device {
            d.revoked_at = Some(DEVICE_ADDED_AT.into());
        }
    }
    let revoked = revoked.sign(&ik);

    let mut older = genuine.core.clone();
    older.directory_version = 0;
    let older = older.sign(&ik);

    let refused = [
        ("older than a fresh one", older),
        ("tampered", tampered),
        ("foreign", foreign),
        ("without this device", without_me),
        ("this device rekeyed", rekeyed),
        ("this device revoked", revoked),
    ];
    for (case, directory) in &refused {
        plant_directory(lib.path(), directory);
        let reopened = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
        let opened = reopened.device_directory();
        assert_eq!(opened.core.directory_version, 1, "{case}");
        assert_eq!(opened.core.devices.len(), 1, "{case}");
        assert!(opened.device(&device).is_some(), "{case}");
        let on_disk: DeviceDirectory =
            cbor::from_slice(&fs::read(Workspace::directory_path(lib.path())).unwrap()).unwrap();
        assert_eq!(
            &on_disk, directory,
            "{case}: the refused file is left in place"
        );
    }

    fs::write(Workspace::directory_path(lib.path()), b"not cbor").unwrap();
    let reopened = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
    assert_eq!(reopened.device_directory().core.directory_version, 1);
    drop(reopened);

    // And the genuine one is restored once it is back.
    plant_directory(lib.path(), &genuine);
    let reopened = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
    assert_eq!(reopened.device_directory(), &genuine);
}

/// **#467.** Once an asset's manifest names the library's id, rebinding is refused — and the
/// refusal changes nothing, on disk or in memory.
#[test]
fn rebinding_after_an_import_is_refused() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("photo.jpg");
    fs::write(&img, b"\xFF\xD8\xFF authored before the rebind").unwrap();
    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Trip").unwrap();
    let asset = ws.import_asset(album, &img).unwrap();
    let original = ws.user_id();

    assert!(matches!(
        ws.bind_account_id(Uuid::now_v7()),
        Err(LifecycleError::AccountRebind(_))
    ));
    assert_eq!(ws.user_id(), original);
    assert_eq!(ws.device_directory().core.user_id, original);
    assert_eq!(ws.verify(&asset).unwrap(), VerifyOutcome::Accept);
    drop(ws);
    let reopened = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
    assert_eq!(reopened.user_id(), original);
}

/// A reopened library must never reissue an `add_id` counter it has already
/// written: `Workspace::open` reseeds from the maximum counter this device's own signed
/// sidecars record, so the next issued counter is strictly greater than every prior one.
#[test]
fn add_id_counter_reseeds_strictly_greater_after_reopen() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("photo.jpg");
    fs::write(&img, b"\xFF\xD8\xFF add-id durability bytes").unwrap();

    // Session 1: two tag adds burn counters 0 and 1 into the signed sidecar.
    let (device, issued) = {
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Trip").unwrap();
        let id = ws.import_asset(album, &img).unwrap();
        ws.tag_add(&id, "vacation").unwrap();
        ws.tag_add(&id, "coast").unwrap();
        let issued: Vec<u64> = ws
            .asset(&id)
            .unwrap()
            .sidecar
            .tags_user
            .entries()
            .iter()
            .map(|(add_id, _)| add_id.counter)
            .collect();
        assert_eq!(issued, vec![0, 1], "session 1 issues 0 and 1");
        (ws.account.device.device_id, issued)
    };

    // Session 2: a brand-new process over the same library.
    let ws2 = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
    assert_eq!(
        ws2.account.device.device_id, device,
        "reopening resumes the same device identity"
    );
    let mut counter = ws2.counter.clone();
    let next = counter.issue().counter;
    assert!(
        next > *issued.iter().max().unwrap(),
        "reseeded counter {next} must be strictly greater than every written counter {issued:?}"
    );
    assert_eq!(next, 2);
}

/// The reset-to-zero case is the *only* one the durability rule licenses: a device
/// whose sidecars bear none of its `add_id`s has provably issued nothing.
#[test]
fn add_id_counter_resets_to_zero_for_a_device_that_issued_nothing() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("photo.jpg");
    fs::write(&img, b"\xFF\xD8\xFF no tags were ever added").unwrap();

    {
        // Import writes a sidecar, but an import issues no `add_id` — the OR-sets are empty.
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Trip").unwrap();
        ws.import_asset(album, &img).unwrap();
    }

    let ws2 = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
    assert_eq!(
        ws2.counter.peek(),
        0,
        "a device with no written add_id starts at zero"
    );
}

/// The high-water mark is per-device: another device's much larger counters in the
/// same sidecar must not inflate this device's next `add_id` (they are a different key in the
/// `(device, counter)` space, and consuming them would burn 500 of our own counters for
/// nothing).
#[test]
fn add_id_reseed_ignores_other_devices_counters() {
    use capsule_core::metadata::crdt::AddId;

    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("photo.jpg");
    fs::write(&img, b"\xFF\xD8\xFF two devices tagged this asset").unwrap();

    let asset_id = {
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Trip").unwrap();
        let id = ws.import_asset(album, &img).unwrap();
        ws.tag_add(&id, "ours").unwrap(); // our device: counter 0

        // Fold in a peer device's add at a far higher counter, exactly as a merge from that
        // peer's sidecar would, and re-persist the sidecar.
        let asset = ws.assets.get_mut(&id).unwrap();
        asset.sidecar.tags_user.add(
            "theirs".to_string(),
            AddId {
                device: Uuid::from_u128(0xBEEF),
                counter: 500,
            },
        );
        let path = ws.sidecar_path(ws.asset(&id).unwrap());
        fs::write(path, ws.asset(&id).unwrap().sidecar.to_canonical_vec()).unwrap();
        id
    };
    assert_ne!(asset_id, Uuid::nil());

    let ws2 = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
    assert_eq!(
        ws2.counter.peek(),
        1,
        "our next counter is one past OUR max (0), not one past the peer's 500"
    );
}

/// An injected `client_id` flows onto every write the workspace authors — the create
/// manifest and a later metadata-update record both report the app's identity (not the bare
/// `capsule-core` default), and each value parses as the normative grammar.
#[test]
fn injected_client_id_flows_to_every_write() {
    use capsule_core::client_build::ClientVersion;

    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("photo.jpg");
    std::fs::write(&img, b"\xFF\xD8\xFF client-id provenance bytes").unwrap();

    let mut ws = fast_workspace(lib.path()).with_client_id("capsule-ios", "9.9.9");
    let album = ws.create_album("Trip").unwrap();
    let id = ws.import_asset(album, &img).unwrap();

    // The create manifest reports the injected identity, grammar-conformant.
    let create_cv = ws
        .asset(&id)
        .unwrap()
        .chain
        .records()
        .last()
        .unwrap()
        .manifest
        .core
        .client_version
        .clone();
    assert!(create_cv.starts_with("capsule-ios/9.9.9+"), "{create_cv}");
    let parsed = ClientVersion::parse(&create_cv).expect("create client_version parses");
    assert_eq!(parsed.client_id, "capsule-ios");
    assert_eq!(parsed.semver, "9.9.9");

    // A metadata-update mints a fresh record that also carries the producing client.
    ws.tag_add(&id, "vacation").unwrap();
    let update_cv = &ws
        .asset(&id)
        .unwrap()
        .chain
        .records()
        .last()
        .unwrap()
        .manifest
        .core
        .client_version;
    assert!(update_cv.starts_with("capsule-ios/9.9.9+"), "{update_cv}");
    assert!(ClientVersion::parse(update_cv).is_some());
}

/// The default (un-injected) workspace reports the bare-core identity.
#[test]
fn default_workspace_reports_capsule_core() {
    use capsule_core::client_build::ClientVersion;

    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("photo.jpg");
    std::fs::write(&img, b"\xFF\xD8\xFF default identity bytes").unwrap();

    let mut ws = fast_workspace(lib.path());
    let album = ws.create_album("Trip").unwrap();
    let id = ws.import_asset(album, &img).unwrap();
    let cv = &ws
        .asset(&id)
        .unwrap()
        .chain
        .records()
        .last()
        .unwrap()
        .manifest
        .core
        .client_version;
    assert_eq!(ClientVersion::parse(cv).unwrap().client_id, "capsule-core");
}

#[test]
fn hardware_backed_device_imports_and_verifies() {
    use std::sync::Arc;

    use capsule_core::crypto::keys::{HardwareBackedSigner, MockHardwareSigner};

    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("photo.jpg");
    fs::write(&img, b"\xFF\xD8\xFF hardware-signed asset").unwrap();

    // The DSK's classical half lives in the (mock) secure element; the PQ half is the
    // software ξ seed. Create the workspace with the hardware-backed signer.
    let hw = Arc::new(MockHardwareSigner::new([5; 32], false));
    let signer = HardwareBackedSigner::enroll(hw, "device-dsk".into(), &[6; 32]).unwrap();
    let mut ws = Workspace::create_with_hardware_signer(
        lib.path(),
        b"passphrase",
        Argon2Params {
            mem_kib: 64,
            t_cost: 1,
            p_cost: 1,
        },
        Box::new(signer),
    )
    .unwrap();

    // The full offline lifecycle runs on hardware-composed signatures: the manifest's
    // device_sig (hardware Ed25519 ‖ software ML-DSA) verifies through `verify_asset`
    // against the directory key the workspace published from the same signer.
    let album = ws.create_album("Trip").unwrap();
    let asset = ws.import_asset(album, &img).unwrap();
    assert_eq!(ws.verify(&asset).unwrap(), VerifyOutcome::Accept);
    // A metadata edit re-signs with the hardware signer and still verifies.
    ws.tag_add(&asset, "vacation").unwrap();
    assert_eq!(ws.verify(&asset).unwrap(), VerifyOutcome::Accept);
    // The exporter key is the hardware-backed device key (not the account's software DSK).
    assert_eq!(
        ws.exporter_verifying_key(),
        ws.directory
            .device(&ws.account.device.device_id)
            .unwrap()
            .dsk_public
    );
}

// ── the device encryption key bound to a secure element, in a real workspace ──

/// The mock element + alias every hardware-bound workspace test binds to. A real Secure
/// Enclave / StrongBox / TPM adapter substitutes for the mock without a change above this
/// line — that is the point of the [`HardwareKeyAgreement`] seam.
fn mock_dek_binding(scalar: u8) -> HardwareDekBinding {
    use capsule_core::crypto::keys::MockP256KeyAgreement;

    HardwareDekBinding {
        element: Arc::new(MockP256KeyAgreement::new([scalar; 32], false)),
        key_alias: "device-dek".into(),
    }
}

/// **The acceptance case.** A workspace created with a `HardwareKeyAgreement` round-trips
/// lock/unlock: close it, reopen it with the same element, and the DEK is still the same
/// hardware-bound key — a secret encapsulated to it *before* the close is recovered *after*
/// the reopen, with the classical ECDH done inside the element both times.
///
/// The asset import either side of the close is what makes this a workspace test rather than
/// a keystore one: the hardware DEK must coexist with the whole durable-state
/// restore, not merely survive in isolation.
#[test]
fn hardware_dek_workspace_round_trips_lock_and_unlock() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("photo.jpg");
    fs::write(&img, b"\xFF\xD8\xFF hardware-DEK asset").unwrap();
    let binding = mock_dek_binding(0x7A);

    let (published, ciphertext, sent_secret, asset) = {
        let mut ws = Workspace::create_with_hardware_dek(
            lib.path(),
            b"passphrase",
            FAST_PARAMS,
            binding.clone(),
        )
        .unwrap();
        assert!(
            ws.device_dek_is_hardware_bound(),
            "the workspace's DEK must report itself hardware-bound"
        );

        let published = ws.device_dek_public();
        let (ct, sent) = encapsulate_to_p256_public(&published).unwrap();
        assert_eq!(
            ws.device_dek_decapsulate(&ct).unwrap(),
            sent,
            "the live workspace decapsulates through the element"
        );

        let album = ws.create_album("Trip").unwrap();
        let asset = ws.import_asset(album, &img).unwrap();
        assert_eq!(ws.verify(&asset).unwrap(), VerifyOutcome::Accept);
        (published, ct, sent, asset)
    };

    // Reopen with the element re-attached: the same DEK comes back and still opens the
    // ciphertext sealed to it in the previous session.
    let reopened =
        Workspace::open_with_hardware_dek(lib.path(), b"passphrase", FAST_PARAMS, binding).unwrap();
    assert!(reopened.device_dek_is_hardware_bound());
    assert_eq!(reopened.device_dek_public(), published);
    assert_eq!(
        reopened.device_dek_decapsulate(&ciphertext).unwrap(),
        sent_secret,
        "a secret sealed before the close must open after the reopen"
    );
    // The durable state is unaffected by the hardware binding.
    assert_eq!(reopened.verify(&asset).unwrap(), VerifyOutcome::Accept);
}

/// Reopening a hardware-bound library **without** the element fails loudly instead of handing
/// back a workspace whose DEK silently decapsulates nothing.
#[test]
fn reopening_a_hardware_dek_library_without_the_element_is_refused() {
    let lib = TempDir::new().unwrap();
    drop(
        Workspace::create_with_hardware_dek(
            lib.path(),
            b"passphrase",
            FAST_PARAMS,
            mock_dek_binding(0x1D),
        )
        .unwrap(),
    );

    assert!(
        matches!(
            Workspace::open(lib.path(), b"passphrase", FAST_PARAMS),
            Err(LifecycleError::Crypto(CryptoError::Key(_)))
        ),
        "a hardware-bound library must not open without its secure element"
    );
}

/// **Existing software-DEK workspaces are unaffected.** A workspace created the ordinary way
/// still reports a software DEK, still reopens with plain `open`, and its DEK still
/// round-trips under the software X-Wing form — the fallback the design keeps for hosts with
/// no secure element.
#[test]
fn software_dek_workspaces_are_unaffected() {
    let lib = TempDir::new().unwrap();

    let published = {
        let ws = fast_workspace(lib.path());
        assert!(!ws.device_dek_is_hardware_bound());
        ws.device_dek_public()
    };

    let reopened = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
    assert!(!reopened.device_dek_is_hardware_bound());
    assert_eq!(reopened.device_dek_public(), published);

    let (ct, sent) = capsule_core::crypto::keys::encapsulate_to_public(&published).unwrap();
    assert_eq!(reopened.device_dek_decapsulate(&ct).unwrap(), sent);

    // A P-256-hybrid ciphertext is length-rejected by the software DEK, so the two
    // compositions can never be confused for one another.
    let hw_lib = TempDir::new().unwrap();
    let hw_ws = Workspace::create_with_hardware_dek(
        hw_lib.path(),
        b"passphrase",
        FAST_PARAMS,
        mock_dek_binding(0x2E),
    )
    .unwrap();
    let (hw_ct, _) = encapsulate_to_p256_public(&hw_ws.device_dek_public()).unwrap();
    assert!(reopened.device_dek_decapsulate(&hw_ct).is_err());
}

// ── the device signing key bound to a secure element, across a reopen ──

const DSK_ALIAS: &str = "capsule-device-dsk";

fn mock_signer_binding(element: &Arc<MockP256Signer>) -> HardwareSignerBinding {
    HardwareSignerBinding {
        element: element.clone(),
        key_alias: DSK_ALIAS.into(),
    }
}

fn hardware_signed_workspace(lib: &Path, element: &Arc<MockP256Signer>) -> Workspace {
    Workspace::create_with_p256_hardware_signer(
        lib,
        b"passphrase",
        FAST_PARAMS,
        mock_signer_binding(element),
        &[0x42; 32],
    )
    .unwrap()
}

/// **The acceptance case.** A library created with a hardware-held P-256 signing key reopens
/// on the same key: the device directory it persisted is restored rather than replaced by a
/// fresh version-1 one under the software key, an asset signed before the close still
/// verifies, and a write after the reopen is signed by the element.
#[test]
fn a_hardware_signed_library_reopens_on_the_key_it_published() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let img = src.path().join("photo.jpg");
    fs::write(&img, b"\xFF\xD8\xFF hardware-signed asset").unwrap();
    let element = Arc::new(MockP256Signer::new(0x31));

    let (published, directory, asset) = {
        let mut ws = hardware_signed_workspace(lib.path(), &element);
        let published = ws.exporter_verifying_key();
        assert_eq!(
            published.classical_algorithm(),
            ClassicalAlgorithm::EcdsaP256
        );
        assert_ne!(published, ws.account.device.dsk.verifying_key());
        let album = ws.create_album("Trip").unwrap();
        let asset = ws.import_asset(album, &img).unwrap();
        (published, ws.directory.clone(), asset)
    };

    let mut reopened = Workspace::open_with_hardware_signer(
        lib.path(),
        b"passphrase",
        FAST_PARAMS,
        mock_signer_binding(&element),
    )
    .unwrap();
    assert_eq!(reopened.exporter_verifying_key(), published);
    assert_eq!(
        reopened.directory, directory,
        "the persisted directory listing the hardware key is restored unchanged"
    );
    assert_eq!(reopened.verify(&asset).unwrap(), VerifyOutcome::Accept);
    reopened.tag_add(&asset, "after-reopen").unwrap();
    assert_eq!(reopened.verify(&asset).unwrap(), VerifyOutcome::Accept);
    assert_eq!(element.key_count(), 1, "the reopen minted no second key");
}

/// A hardware-signed library is refused by every open that supplies no signing element,
/// rather than reopened on the software key, which would change the key the directory
/// publishes.
#[test]
fn a_hardware_signed_library_is_refused_without_its_element() {
    let lib = TempDir::new().unwrap();
    let element = Arc::new(MockP256Signer::new(0x32));
    drop(hardware_signed_workspace(lib.path(), &element));

    assert!(matches!(
        Workspace::open(lib.path(), b"passphrase", FAST_PARAMS),
        Err(LifecycleError::Crypto(CryptoError::Key(_)))
    ));
    assert!(matches!(
        Workspace::open_with_hardware_dek(
            lib.path(),
            b"passphrase",
            FAST_PARAMS,
            mock_dek_binding(0x33)
        ),
        Err(LifecycleError::Crypto(CryptoError::Key(_)))
    ));
}

/// An element that lost the key (a cleared keystore) or is switched off refuses the open; the
/// lost key is never replaced by a fresh one under the same alias.
#[test]
fn a_hardware_signed_library_is_refused_when_the_element_cannot_produce_its_key() {
    let lib = TempDir::new().unwrap();
    let element = Arc::new(MockP256Signer::new(0x34));
    drop(hardware_signed_workspace(lib.path(), &element));
    let open = || {
        Workspace::open_with_hardware_signer(
            lib.path(),
            b"passphrase",
            FAST_PARAMS,
            mock_signer_binding(&element),
        )
    };

    element.set_unavailable(true);
    assert!(matches!(
        open(),
        Err(LifecycleError::Crypto(CryptoError::Key(_)))
    ));
    element.set_unavailable(false);

    element.forget(DSK_ALIAS);
    assert!(matches!(
        open(),
        Err(LifecycleError::Crypto(CryptoError::Key(_)))
    ));
    assert_eq!(element.key_count(), 0, "no replacement key was minted");
}

/// **Older libraries.** A library created on the software key (every library from before the
/// binding was recorded) opens through `open_with_hardware_signer` on that software key: the
/// recorded binding decides, the offered element is never asked for a key, and nothing about
/// the library changes.
#[test]
fn a_software_signed_library_opens_on_its_software_key_when_an_element_is_offered() {
    let lib = TempDir::new().unwrap();
    let (software_key, directory) = {
        let ws = fast_workspace(lib.path());
        (ws.exporter_verifying_key(), ws.directory.clone())
    };
    let element = Arc::new(MockP256Signer::new(0x35));

    let reopened = Workspace::open_with_hardware_signer(
        lib.path(),
        b"passphrase",
        FAST_PARAMS,
        mock_signer_binding(&element),
    )
    .unwrap();
    assert_eq!(reopened.exporter_verifying_key(), software_key);
    assert_eq!(reopened.directory, directory);
    assert_eq!(element.key_count(), 0);
    assert!(!DeviceSignerRecord::path(lib.path()).exists());
}

/// A first open of a laid-out library with no account enrolls the fresh account's signing key
/// in the element, and records it so the next open rebuilds the same key.
#[test]
fn a_first_open_with_an_element_records_a_hardware_signing_key() {
    let lib = TempDir::new().unwrap();
    crate::library::init::init_library(lib.path(), "Capsule").unwrap();
    let element = Arc::new(MockP256Signer::new(0x36));
    let open = || {
        Workspace::open_with_hardware_signer(
            lib.path(),
            b"passphrase",
            FAST_PARAMS,
            mock_signer_binding(&element),
        )
        .unwrap()
    };

    let published = open().exporter_verifying_key();
    assert_eq!(
        published.classical_algorithm(),
        ClassicalAlgorithm::EcdsaP256
    );
    assert_eq!(open().exporter_verifying_key(), published);
    assert_eq!(element.key_count(), 1);
}

/// A record left by a creation that stopped before its account file was written is sealed under
/// a master key no account holds: the first open removes it rather than refusing the library.
#[test]
fn a_record_left_by_an_interrupted_creation_is_removed_on_first_open() {
    let lib = TempDir::new().unwrap();
    crate::library::init::init_library(lib.path(), "Capsule").unwrap();
    let element = Arc::new(MockP256Signer::new(0x37));
    let (_, stale) = DeviceSignerRecord::enroll(
        mock_signer_binding(&element),
        &[1; 32],
        &capsule_core::crypto::keys::MasterKey::generate(),
    )
    .unwrap();
    stale.persist(lib.path()).unwrap();

    drop(Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap());
    assert!(!DeviceSignerRecord::path(lib.path()).exists());
    drop(Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap());
}

/// An element that refuses enrollment fails the creation before anything is written under the
/// root, and never falls back to the software key.
#[test]
fn a_refused_hardware_signer_creation_writes_nothing() {
    let lib = TempDir::new().unwrap();
    let root = lib.path().join("library");
    let element = Arc::new(MockP256Signer::new(0x38));
    element.set_unavailable(true);

    assert!(matches!(
        Workspace::create_with_p256_hardware_signer(
            &root,
            b"passphrase",
            FAST_PARAMS,
            mock_signer_binding(&element),
            &[0x42; 32],
        ),
        Err(LifecycleError::Crypto(CryptoError::Key(_)))
    ));
    assert!(!root.exists());
}

/// **Newer-catalog refusal at the workspace boundary.** `Workspace::open` no longer
/// stringifies a library-open failure into `Io`: a catalog newer than this build surfaces
/// as `LifecycleError::Library(LibraryError::CatalogTooNew { .. })`, so a client can name
/// both versions and tell the user to update rather than to check their disk.
#[test]
fn open_surfaces_a_too_new_catalog_as_a_typed_library_error() {
    use crate::db::schema::SCHEMA_VERSION;
    use crate::library::LibraryError;

    let lib = TempDir::new().unwrap();
    drop(fast_workspace(lib.path()));

    let db_path = lib.path().join("index/library.sqlite");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(&format!("PRAGMA user_version = {};", SCHEMA_VERSION + 1))
            .unwrap();
    }
    let before = fs::read(&db_path).unwrap();

    match Workspace::open(lib.path(), b"passphrase", FAST_PARAMS) {
        Err(LifecycleError::Library(LibraryError::CatalogTooNew { found, supported })) => {
            assert_eq!(found, SCHEMA_VERSION + 1);
            assert_eq!(supported, SCHEMA_VERSION);
        }
        Ok(_) => panic!("a too-new catalog must not open"),
        Err(other) => panic!("expected Library(CatalogTooNew), got {other:?}"),
    }
    assert_eq!(fs::read(&db_path).unwrap(), before, "nothing was written");
    assert!(!lib.path().join(".library/lock").exists());
}
