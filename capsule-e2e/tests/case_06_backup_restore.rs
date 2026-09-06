//! **E2E case 6** — backup → restore on a fresh device.
//!
//! Export a full backup → bootstrap a new device via passphrase and escrow → import the backup
//! → assert every asset present and verifiable.
//!
//! Two tests, because until a `Workspace` can open *as* a recovered account (issue #467) the
//! escrow and the restore are independent halves: nothing the escrow recovers feeds the
//! restore, and nothing the restore needs comes from the escrow.
//!
//! - **`E2E case 6 (escrow)`**: A escrows its master key at the low-RAM tier through the SDK's
//!   `RecoveryClient` over the real route; a second session fetches it byte for byte and
//!   `recover_master_key` yields A's key, proved by re-deriving A's default album id. Two
//!   Argon2id passes at `DeviceTier::LowRam` (the wrap and the recovery) — the one memory-hard
//!   test in the crate.
//! - **`E2E case 6 (restore)`**: A exports a backup; a fresh library on a new root imports it
//!   under the exporter's verifying key, reads the asset byte for byte and walks its chain.
//!   `verify` is asserted to refuse: the artifact carries no album authority (issue #468).

use capsule_core::crypto::keys::MasterKey;
use capsule_core::crypto::primitives::DeviceTier;
use capsule_core::crypto::provenance::record::ProvenanceChain;
use capsule_core::lifecycle::{LifecycleError, Workspace};
use capsule_e2e::fixtures::synthetic_jpeg;
use capsule_e2e::{Device, FAST_KDF, PASSPHRASE, Server};
use capsule_sdk::recovery::RecoveryClient;

const RECOVERY_SECRET: &[u8] = b"seven words the user wrote down somewhere safe";
const BACKUP_PASSPHRASE: &[u8] = b"backup passphrase";

/// **E2E case 6 (escrow)**: the master key round-trips through the real escrow route and
/// recovers on a second device.
#[tokio::test]
async fn e2e_case_6_the_escrow_round_trips_and_recovers_the_master_key() {
    let server = Server::boot().await;
    let a = Device::register(&server, "device-a").await;

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
    assert_eq!(
        MasterKey::from_bytes(master).derive_default_album_id(),
        a.workspace.default_album_id(),
        "the recovered master key is A's: it derives A's default album id"
    );
}

/// **E2E case 6 (restore)**: a fresh library imports the backup, reads every asset and walks
/// its chain; `verify` refuses for want of the album authority the artifact does not carry.
#[tokio::test]
async fn e2e_case_6_a_fresh_library_restores_the_backup() {
    let server = Server::boot().await;
    let mut a = Device::register(&server, "device-a").await;
    let asset = a.import_jpeg("keepsake.jpg");
    let archive = a.staging.path().join("backup.tar");
    a.workspace
        .export_backup(&archive, BACKUP_PASSPHRASE)
        .expect("the backup exports");
    let exporter = a.workspace.exporter_verifying_key();

    let root_b = tempfile::tempdir().expect("a fresh library root");
    let mut b =
        Workspace::create_with_params(root_b.path(), PASSPHRASE, FAST_KDF).expect("a library");
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
        "the restore folded A's album keys into the fresh library"
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
