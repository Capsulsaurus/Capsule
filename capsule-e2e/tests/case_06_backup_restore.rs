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
//!   master key, the server's account id, and A's sealed identity key. The sealed identity has
//!   no server route yet, so the case hands it across in memory — the one leg of the bootstrap
//!   not exercised over the wire.
//! - The new device imports A's backup under the exporter's verifying key, reads the asset byte
//!   for byte and walks its chain. `verify` is asserted to refuse: the artifact carries no album
//!   authority (issue #468). The export names [`FAST_KDF`] explicitly, and the import needs no
//!   such argument: the artifact records the cost it was wrapped under.

use capsule_core::crypto::keys::MasterKey;
use capsule_core::crypto::primitives::DeviceTier;
use capsule_core::crypto::provenance::ProvenanceChain;
use capsule_core::lifecycle::{LifecycleError, Workspace};
use capsule_e2e::fixtures::synthetic_jpeg;
use capsule_e2e::{Device, FAST_KDF, PASSPHRASE, Server};
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
    RecoveryClient::new(a.session.clone(), server.base_url())
        .expect("the API root parses")
        .store_escrow(&escrow)
        .await
        .expect("the escrow stores");

    // The fresh device: a new session on the account, nothing else.
    let session_b = a.login_again(&server).await;
    let fetched = RecoveryClient::new(session_b, server.base_url())
        .expect("the API root parses")
        .fetch_escrow()
        .await
        .expect("the escrow fetches");
    let wire = fetched.blob().clone();
    assert_eq!(wire, escrow, "the escrow is ciphertext served verbatim");

    let master = capsule_core::backup::recover_master_key(&wire, RECOVERY_SECRET)
        .expect("the recovery secret opens the escrow");

    // The new device opens as A's account: the recovered master key, the server's id for the
    // account, and A's identity key sealed under that master key.
    let root_b = tempfile::tempdir().expect("a fresh library root");
    let mut b = Workspace::create_from_master(
        root_b.path(),
        PASSPHRASE,
        FAST_KDF,
        MasterKey::from_bytes(master),
        a.user_id,
        &a.workspace.sealed_identity(),
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

    // What the fresh device cannot yet do is run `verify_asset`: the backup artifact carries
    // the album's content keys and none of its authority (the admin-signed epoch ledger a
    // manifest's write signature is checked against), so the library has nothing to verify
    // the signature under. Asserted as the current truth; issue #468.
    match b.verify(&asset) {
        Err(LifecycleError::NotFound(what)) => assert!(
            what.contains("authority"),
            "the refusal names the missing authority: {what}"
        ),
        other => panic!("a restored album has no authority to verify under, got {other:?}"),
    }
}
