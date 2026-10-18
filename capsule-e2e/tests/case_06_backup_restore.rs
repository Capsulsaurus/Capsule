//! **E2E case 6** — backup → restore on a fresh device.
//!
//! Export a full backup → bootstrap a new device via passphrase and escrow → import the backup
//! → assert every asset present and verifiable.
//!
//! One test, the whole flow:
//!
//! - A escrows its master key at the low-RAM tier through the SDK's `RecoveryClient` over the
//!   real route; a second session fetches it byte for byte and `recover_master_key` yields A's
//!   key. Two Argon2id passes at `DeviceTier::LowRam` (the wrap and the recovery), which is the
//!   only memory-hard cost this crate pays: the tier is the thing under test here, so it is
//!   spelled out rather than made cheap.
//! - The new device opens *as A's account* with `Workspace::create_from_master`: the recovered
//!   master key, the server's account id, and A's sealed identity key. A stores the sealed
//!   identity beside the escrow (`PUT /v1/auth/escrow/identity`, #544) and the new session
//!   fetches it back, so every leg of the bootstrap crosses the wire: nothing but the recovery
//!   secret and the account's credentials is carried from A to the new device.
//! - The new device joins the account's device directory (#550): its own version-1 directory is
//!   refused as stale against the one A already published, so it fetches A's through the SDK's
//!   `DirectoryClient`, verifies it under the identity key it opened, appends itself with
//!   `Workspace::extend_account_directory` and publishes at the stored version + 1, keeping
//!   every device A listed.
//! - The new device imports A's backup under the exporter's verifying key, reads the asset byte
//!   for byte and walks its chain. The artifact carries the album's public authority (#468) and
//!   the adopted directory lists A's device, so `verify` accepts. The recovered album stays
//!   read-only. The export names [`FAST_KDF`] explicitly, and the import needs no such argument:
//!   the artifact records the cost it was wrapped under.

use capsule_core::crypto::keys::MasterKey;
use capsule_core::crypto::primitives::DeviceTier;
use capsule_core::crypto::provenance::ProvenanceChain;
use capsule_core::crypto::verify_asset::VerifyOutcome;
use capsule_core::lifecycle::{LifecycleError, Workspace};
use capsule_e2e::fixtures::synthetic_jpeg;
use capsule_e2e::{Device, FAST_KDF, PASSPHRASE, Server};
use capsule_sdk::directory::{DirectoryClient, DirectoryError};
use capsule_sdk::recovery::RecoveryClient;

const RECOVERY_SECRET: &[u8] = b"seven words the user wrote down somewhere safe";
const BACKUP_PASSPHRASE: &[u8] = b"backup passphrase";

/// **E2E case 6**: the master key round-trips through the real escrow route, a new device opens
/// as A's account from it, and that device restores A's backup.
#[tokio::test]
async fn e2e_case_6_a_new_device_bootstraps_from_the_escrow_and_restores_the_backup() {
    let server = Server::boot().await;
    let mut a = Device::register(&server, "device-a").await;
    let asset = a.import_jpeg("keepsake.jpg");
    let archive = a.staging.path().join("backup.tar");
    a.workspace
        .export_backup_with_params(&archive, BACKUP_PASSPHRASE, FAST_KDF)
        .expect("the backup exports");
    let exporter = a.workspace.exporter_verifying_key();

    let escrow = a
        .workspace
        .escrow_master_key(RECOVERY_SECRET, DeviceTier::LowRam)
        .expect("the master key wraps under the recovery secret");
    let recovery_a =
        RecoveryClient::new(a.session.clone(), server.base_url()).expect("the API root parses");
    recovery_a
        .store_escrow(&escrow)
        .await
        .expect("the escrow stores");
    let sealed = a.workspace.sealed_identity();
    let stored = recovery_a
        .store_sealed_identity(&sealed)
        .await
        .expect("the sealed identity stores beside the escrow");
    assert!(!stored.replaced, "A's first sealed identity");

    // The fresh device: a new session on the account, nothing else.
    let session_b = a.login_again(&server).await;
    let recovery_b =
        RecoveryClient::new(session_b.clone(), server.base_url()).expect("the API root parses");
    let fetched = recovery_b.fetch_escrow().await.expect("the escrow fetches");
    let wire = fetched.blob().clone();
    assert_eq!(wire, escrow, "the escrow is ciphertext served verbatim");
    let sealed_wire = recovery_b
        .fetch_sealed_identity()
        .await
        .expect("the sealed identity fetches");
    assert_eq!(
        sealed_wire, sealed,
        "the sealed identity is ciphertext served verbatim"
    );

    let master = capsule_core::backup::recover_master_key(&wire, RECOVERY_SECRET)
        .expect("the recovery secret opens the escrow");

    // The new device opens as A's account: the recovered master key, the server's id for the
    // account, and A's identity key sealed under that master key — every input but the recovery
    // secret came over the wire.
    let root_b = tempfile::tempdir().expect("a fresh library root");
    let mut b = Workspace::create_from_master(
        root_b.path(),
        PASSPHRASE,
        FAST_KDF,
        MasterKey::from_bytes(master),
        a.user_id,
        &sealed_wire,
    )
    .expect("the recovered master key opens A's sealed identity");
    assert_eq!(b.user_id(), a.user_id, "B is A's account");
    assert_eq!(
        b.default_album_id(),
        a.workspace.default_album_id(),
        "the recovered master key is A's: it derives A's default album id"
    );
    assert_eq!(
        b.user_ik_public(),
        a.workspace.user_ik_public(),
        "B signs under the identity the server pinned for A"
    );
    assert_ne!(b.device_id(), a.workspace.device_id(), "B is a new device");
    assert!(b.asset_ids().is_empty(), "no prior state");

    // #550: B joins the account's published directory rather than replacing it. The directory
    // B was created with names only B at version 1, and the server already holds A's at a
    // higher version, so publishing it as-is is refused as stale.
    let directory_b = DirectoryClient::new(session_b, &server.auth_base());
    let stored = directory_b
        .fetch(a.user_id, &b.user_ik_public())
        .await
        .expect(
            "A's directory fetches and verifies under the IK B opened from the sealed identity",
        );
    assert!(stored.core.directory_version >= 1);
    assert_eq!(
        stored,
        a.directory(),
        "the server serves A's document verbatim"
    );
    assert_eq!(b.device_directory().core.directory_version, 1);
    assert!(matches!(
        directory_b
            .publish(b.device_directory(), &b.user_ik_public())
            .await,
        Err(DirectoryError::VersionConflict)
    ));

    let extended = b
        .extend_account_directory(&stored)
        .expect("B appends itself to the account's directory")
        .expect("the stored directory does not list B yet");
    assert_eq!(
        directory_b
            .publish(&extended, &b.user_ik_public())
            .await
            .expect("the extended directory publishes"),
        stored.core.directory_version + 1,
        "B publishes at stored + 1"
    );
    let republished = directory_b
        .fetch(a.user_id, &b.user_ik_public())
        .await
        .expect("the account's directory fetches again");
    assert_eq!(republished, extended);
    for device in &stored.core.devices {
        assert_eq!(
            republished.device(&device.device_id),
            Some(device),
            "every device the account already had survives B's publish"
        );
    }
    assert!(
        republished.device(&b.device_id()).is_some(),
        "and B is listed"
    );
    assert_eq!(
        republished.core.devices.len(),
        stored.core.devices.len() + 1
    );

    let restored = b
        .import_backup(&archive, BACKUP_PASSPHRASE, &exporter)
        .expect("the backup imports under the exporter's key");
    assert_eq!(restored, 1);
    assert_eq!(b.asset_ids(), vec![asset]);
    assert_eq!(
        b.read_plaintext(&asset).expect("the asset decrypts"),
        synthetic_jpeg()
    );
    assert!(
        b.has_album(&a.workspace.default_album_id()),
        "the restore folded A's album keys into the new device"
    );

    // The restored chain is structurally intact, and the plaintext above is the manifest's:
    // the ciphertext decrypted under the recovered album key to the bytes A imported.
    let chain = &b.asset(&asset).expect("the restored asset").chain;
    assert_eq!(chain.records().len(), 1);
    ProvenanceChain::verify_walk(chain.records()).expect("the restored chain walks");

    // The artifact carries the album's public authority (the admin-signed epoch ledger a
    // manifest's write signature is checked against, #468), and B's directory is now the
    // account's, listing A's device (#550), so the restored asset verifies end to end.
    assert_eq!(
        b.verify(&asset)
            .expect("the restored album has an authority to verify under"),
        VerifyOutcome::Accept,
        "the restored asset verifies under the account directory B adopted"
    );

    // The restore escrowed no signing capability: the recovered album stays read-only.
    let other = a.staging.path().join("other.jpg");
    std::fs::write(&other, synthetic_jpeg()).expect("a second photo stages");
    assert!(matches!(
        b.import_asset(a.workspace.default_album_id(), &other),
        Err(LifecycleError::AlbumReadOnly(_))
    ));
}
