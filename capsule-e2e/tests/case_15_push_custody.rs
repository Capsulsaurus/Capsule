//! **E2E case 15** — `capsule push` confirms custody, keeps the receipts, and carries on past a
//! failed asset (issue #670).
//!
//! The client leg is the CLI's own orchestration (`capsule_cli::remote::push`), which is what
//! `capsule push` runs, over the session `capsule auth login` persists, against the real server
//! composition root. One asset pushes cleanly; the other cannot even build its upload bundle,
//! because its original was removed from the library. The run must not stop at the second: the
//! first reaches verified custody, its receipts land in the library's receipt log beside its
//! provenance chain, and the second is reported failed with its reason. The first run pins the
//! server's attestation-key history in the library (issue #714), and a run against a history
//! that no longer holds a pinned key is refused whole.
//!
//! The fault matrix of the custody check itself — a lost finalize, an index failure, a verdict
//! that is not durable, a withheld or mis-signed receipt — is driven against the mock server in
//! `capsule-sdk/src/push/tests.rs`; this case is the real-server half.

use std::path::{Path, PathBuf};

use capsule_cli::remote::{self, AssetPushStatus, PushOptions, RemoteConfig, RemoteError};
use capsule_cli::session::SessionStore;
use capsule_core::crypto::keys::{HybridSigningKey, HybridVerifyingKey};
use capsule_core::library::{
    KeyPinError, attestation_key_pins_path, load_pinned_attestation_keys, load_receipts,
    pin_attestation_keys,
};
use capsule_e2e::fixtures::{large_synthetic_jpeg, synthetic_jpeg};
use capsule_e2e::{Device, PROTOCOL_VERSION, Server};
use capsule_sdk::push::bundle_blobs;

/// Every file under `dir` whose name is `name`.
fn find_named(dir: &Path, name: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).expect("the library directory reads") {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            found.extend(find_named(&path, name));
        } else if path.file_name().is_some_and(|n| n == name) {
            found.push(path);
        }
    }
    found
}

#[tokio::test]
async fn e2e_case_15_push_verifies_custody_persists_receipts_and_carries_on_past_a_failure() {
    let server = Server::boot().await;
    let mut device = Device::register(&server, "push-custody").await;
    let good = device.import_file("good.jpg", &synthetic_jpeg());
    let broken = device.import_file("broken.jpg", &large_synthetic_jpeg());

    // The broken asset's original leaves the library, so its bundle cannot be built.
    let original = find_named(device.root.path(), &format!("{}.jpg", broken.simple()));
    assert_eq!(
        original.len(),
        1,
        "the broken asset's original is on disk: {original:?}"
    );
    std::fs::remove_file(&original[0]).expect("the original is removed");

    // The CLI's state: the session `capsule auth login` persists.
    let signed_in = device.login_again(&server).await;
    let home = tempfile::tempdir().expect("a temp CLI home");
    let store = SessionStore::new(home.path().join("session.json"));
    let persisted = signed_in.export().await.expect("a live session exports");
    store.save(&persisted).expect("the session persists");
    let remote = RemoteConfig {
        auth_endpoint: server.auth_base(),
        sync_endpoint: server.base_url().to_owned(),
        upload_endpoint: server.upload_base(),
        albums_endpoint: server.albums_base(),
        protocol_version: PROTOCOL_VERSION.to_owned(),
    };

    let summary = remote::push(&remote, &store, &device.workspace, PushOptions::default())
        .await
        .expect("the run completes, failures included");
    assert_eq!(summary.assets, 2);
    assert_eq!(summary.verified_assets, 1, "{summary:#?}");
    assert_eq!(summary.failed_assets, 1, "{summary:#?}");

    let outcome = |id: uuid::Uuid| {
        summary
            .outcomes
            .iter()
            .find(|o| o.asset_id == id.to_string())
            .expect("every asset has an outcome")
    };
    let failed = outcome(broken);
    assert_eq!(failed.status, AssetPushStatus::Failed);
    assert_eq!(failed.error.as_ref().expect("a reason").kind, "bundle");

    // The good asset: every blob confirmed, and one verified receipt per blob on disk.
    let bundle = device
        .workspace
        .upload_bundle(&good)
        .expect("the good asset still bundles");
    let hashes: Vec<String> = bundle_blobs(&bundle).into_iter().map(|(_, h)| h).collect();
    let verified = outcome(good);
    assert_eq!(verified.status, AssetPushStatus::Verified);
    assert!(verified.error.is_none());
    assert_eq!(verified.receipts, hashes.len());
    assert_eq!(summary.receipts, hashes.len());

    let capture_utc = device.workspace.asset(&good).map(|a| a.capture_utc);
    let log = load_receipts(device.root.path(), &good, capture_utc).expect("the log decodes");
    let mut logged: Vec<String> = log
        .iter()
        .map(|r| r.core.ciphertext_hash.to_hex())
        .collect();
    let mut expected = hashes.clone();
    logged.sort();
    expected.sort();
    assert_eq!(logged, expected, "one receipt per blob, persisted");
    assert!(log.iter().all(|r| r.core.asset_id == good.to_string()));

    // A second run re-confirms custody and stores nothing twice.
    let again = remote::push(&remote, &store, &device.workspace, PushOptions::default())
        .await
        .expect("a second run completes");
    assert_eq!(again.verified_assets, 1, "{again:#?}");
    assert_eq!(again.failed_assets, 1);
    assert_eq!(again.uploaded_blobs, 0, "nothing new crosses the wire");
    let relog = load_receipts(device.root.path(), &good, capture_utc).expect("the log decodes");
    assert_eq!(relog.len(), log.len(), "re-persisting is a no-op");

    // The first run pinned the server's attestation-key history under its origin (issue #714),
    // and every persisted receipt verifies under the pin.
    let origin = server.base_url().trim_end_matches('/');
    let pinned = load_pinned_attestation_keys(device.root.path(), origin)
        .expect("the pin reads")
        .expect("the first push pinned the server's keys");
    assert!(!pinned.is_empty());
    assert!(
        relog
            .iter()
            .all(|r| pinned.iter().any(|key| r.verify_under(key))),
        "every kept receipt verifies under the pinned history"
    );

    // A server whose published history no longer holds a pinned key is refused for the whole
    // run, and the pin is not silently replaced. Stand in for the rewrite by pinning a key the
    // server never published.
    std::fs::remove_file(attestation_key_pins_path(device.root.path())).expect("the pin exists");
    let foreign = HybridSigningKey::from_seed64(&[0x5A; 64]).verifying_key();
    pin_attestation_keys(device.root.path(), origin, std::slice::from_ref(&foreign))
        .expect("the foreign key pins");
    let refused = remote::push(&remote, &store, &device.workspace, PushOptions::default()).await;
    assert!(
        matches!(
            refused,
            Err(RemoteError::AttestationKeyPin(
                KeyPinError::HistoryRewritten {
                    missing: 1,
                    pinned: 1,
                    ..
                }
            ))
        ),
        "{refused:?}"
    );
    let still = load_pinned_attestation_keys(device.root.path(), origin)
        .expect("the pin reads")
        .expect("the pin is still there");
    assert_eq!(
        still
            .iter()
            .map(HybridVerifyingKey::to_bytes)
            .collect::<Vec<_>>(),
        [foreign.to_bytes()],
        "a refused run leaves the pin as it was"
    );
}
