//! **E2E case 16** — `capsule verify --signed` keeps the server's signed durability verdict
//! beside the custody receipts (issue #747).
//!
//! The client leg is the CLI's own orchestration (`capsule_cli::remote::verify_storage`), which
//! is what `capsule verify` runs, over the session `capsule auth login` persists, against the
//! real server composition root. After a push, a signed verify must find the asset durable,
//! verify the attestation under the server's published attestation key, and append it to the
//! asset's `{uuid}.attestations.cbor` log; a second run appends a second attestation under a new
//! nonce rather than replacing the first.
//!
//! The refusal half — an attestation under a key the client never pinned, a replayed nonce, an
//! answer to another question — is driven against stand-ins in `capsule-cli/src/verify/tests.rs`
//! and `capsule-sdk/src/verify/tests.rs`; this case is the real-server half.

use capsule_cli::remote::{self, PushOptions, RemoteConfig};
use capsule_cli::session::SessionStore;
use capsule_cli::verify::{AssetVerifyStatus, VerifyOptions};
use capsule_e2e::fixtures::synthetic_jpeg;
use capsule_e2e::{Device, PROTOCOL_VERSION, Server};
use capsule_library::library::load_attestations;
use capsule_sdk::verify::{StorageVerifyClient, VerifyTransport};

#[tokio::test]
async fn e2e_case_16_signed_verify_keeps_a_verified_attestation_beside_the_receipts() {
    let server = Server::boot().await;
    let mut device = Device::register(&server, "verify-signed").await;
    let asset = device.import_file("kept.jpg", &synthetic_jpeg());

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

    let pushed = remote::push(
        &remote,
        &store,
        &mut device.workspace,
        PushOptions::default(),
    )
    .await
    .expect("the push completes");
    assert_eq!(pushed.verified_assets, 1, "{pushed:#?}");

    let options = VerifyOptions {
        signed: true,
        ..VerifyOptions::default()
    };
    let summary = remote::verify_storage(&remote, &store, &device.workspace, &options)
        .await
        .expect("the verify runs");
    assert_eq!(summary.assets, 1);
    assert_eq!(summary.durable_assets, 1, "{summary:#?}");
    assert_eq!(summary.attestations_stored, 1);
    let outcome = &summary.outcomes[0];
    assert_eq!(outcome.asset_id, asset.to_string());
    assert_eq!(outcome.status, AssetVerifyStatus::Durable);
    assert!(outcome.attestation_stored);

    // The kept attestation is evidence: it names the asset, and verifies under a key the server
    // publishes.
    let capture_utc = device.workspace.asset(&asset).map(|a| a.capture_utc);
    let log = load_attestations(device.root.path(), &asset, capture_utc).expect("the log decodes");
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].core.verdicts.len(), 1);
    assert_eq!(log[0].core.verdicts[0].asset_id, asset.to_string());
    assert!(log[0].core.verdicts[0].durable);
    let keys = StorageVerifyClient::new(VerifyTransport::with_session(
        device.login_again(&server).await,
        format!("{}/v1", server.base_url().trim_end_matches('/')),
    ))
    .fetch_attestation_keys()
    .await
    .expect("the attestation keys are published");
    assert!(
        keys.iter().any(|key| log[0].verify_under(key)),
        "the kept attestation verifies under the server's published key"
    );

    // A second run asks under a fresh nonce: the log grows, and keeps the first.
    let again = remote::verify_storage(&remote, &store, &device.workspace, &options)
        .await
        .expect("a second verify runs");
    assert_eq!(again.attestations_stored, 1);
    let relog =
        load_attestations(device.root.path(), &asset, capture_utc).expect("the log decodes");
    assert_eq!(relog.len(), 2);
    assert_eq!(relog[0], log[0], "the first attestation is kept as it was");
    assert_ne!(relog[0].core.nonce, relog[1].core.nonce);
}
