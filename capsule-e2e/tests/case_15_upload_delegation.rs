//! **E2E case 15** — upload delegation (design/module-map.md; Peering § Delegation, issue #701).
//!
//! A (the origin) hands a sealed bundle to B, another device of the same account; B uploads it
//! under its own session; A releases the original only after verifying custody of the hashes it
//! computed itself. B's death mid-upload makes A re-queue it, and A's own upload then releases
//! it. Both legs are the CLI's orchestration (`capsule_cli::peer`), which is what
//! `capsule peer delegate`, `capsule peer accept` and `capsule peer status` run, over the
//! sessions `capsule auth login` persists, against the real server composition root.
//!
//! The server half is asserted too: it admits a manifest one device signed and another uploaded
//! on the signer's signature, and refuses one signed by a key that is not the named device's —
//! uploaded by a device of the account all the same — and one naming another account.
//!
//! The rows decided on one device (tampering, partial hand-off, an older delegate, a revoked
//! delegate, a delete after hand-off, double upload) are unit tests in
//! `capsule-sdk/src/peering/delegation/tests.rs`; the signer check's are in
//! `capsule-server/src/upload/signer/tests.rs` and `capsule-server/tests/upload.rs`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use capsule_cli::peer::{self, ACK_SUFFIX, AcceptOptions, BootClock, DelegateOptions, PeerState};
use capsule_cli::remote::{self, PushOptions, RemoteConfig};
use capsule_cli::session::SessionStore;
use capsule_core::crypto::keys::MasterKey;
use capsule_core::crypto::primitives::DeviceTier;
use capsule_e2e::fixtures::synthetic_jpeg;
use capsule_e2e::{Device, FAST_KDF, PASSPHRASE, PROTOCOL_VERSION, Server};
use capsule_library::import::UploadPolicy;
use capsule_library::lifecycle::Workspace;
use capsule_sdk::auth::Session;
use capsule_sdk::directory::DirectoryClient;
use capsule_sdk::net::ConnectionClass;
use capsule_sdk::peering::{DelegationState, RequeueReason};
use capsule_sdk::push::{bundle_blobs, create_request};
use capsule_sdk::recovery::RecoveryClient;
use capsule_sdk::upload::{UploadError, UploadOutcome};
use tempfile::TempDir;
use uuid::Uuid;

const RECOVERY_SECRET: &[u8] = b"case 15 recovery secret";

/// A boot clock the case advances by hand.
struct Clock(AtomicU64);

impl BootClock for Clock {
    fn epoch(&self) -> String {
        "case-15-boot".to_owned()
    }

    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// One device's CLI state: its session store and its peer state.
struct Cli {
    _home: TempDir,
    store: SessionStore,
    state: PeerState,
}

impl Cli {
    async fn signed_in(session: &Session) -> Self {
        let home = tempfile::tempdir().expect("a temp CLI home");
        let store = SessionStore::new(home.path().join("session.json"));
        store
            .save(&session.export().await.expect("a live session exports"))
            .expect("the session persists");
        let state = PeerState::new(home.path().join("peer"));
        Self {
            _home: home,
            store,
            state,
        }
    }
}

fn remote_for(server: &Server) -> RemoteConfig {
    RemoteConfig {
        auth_endpoint: server.auth_base(),
        sync_endpoint: server.base_url().to_owned(),
        upload_endpoint: server.upload_base(),
        albums_endpoint: server.albums_base(),
        protocol_version: PROTOCOL_VERSION.to_owned(),
    }
}

/// Device B: a new device of A's account, opened from A's escrowed master key and joined to the
/// account's published directory — the account's directory then lists A and B.
async fn second_device(server: &Server, a: &Device) -> (TempDir, Workspace, Session) {
    let escrow = a
        .workspace
        .escrow_master_key(RECOVERY_SECRET, DeviceTier::LowRam)
        .expect("the master key wraps");
    let recovery_a = RecoveryClient::new(a.session.clone(), server.base_url()).expect("a client");
    recovery_a.store_escrow(&escrow).await.expect("stores");
    recovery_a
        .store_sealed_identity(&a.workspace.sealed_identity())
        .await
        .expect("stores");

    let session_b = a.login_again(server).await;
    let recovery_b = RecoveryClient::new(session_b.clone(), server.base_url()).expect("a client");
    let wire = recovery_b
        .fetch_escrow()
        .await
        .expect("fetches")
        .blob()
        .clone();
    let sealed = recovery_b.fetch_sealed_identity().await.expect("fetches");
    let master = capsule_core::backup::recover_master_key(&wire, RECOVERY_SECRET).expect("opens");
    let root = tempfile::tempdir().expect("B's library root");
    let mut b = Workspace::create_from_master(
        root.path(),
        PASSPHRASE,
        FAST_KDF,
        MasterKey::from_bytes(master),
        a.user_id,
        &sealed,
    )
    .expect("B opens as A's account");
    let directory = DirectoryClient::new(session_b.clone(), &server.auth_base());
    let stored = directory
        .fetch(a.user_id, &b.user_ik_public())
        .await
        .expect("A's directory fetches");
    let extended = b
        .extend_account_directory(&stored)
        .expect("B joins the directory")
        .expect("B was not listed yet");
    directory
        .publish(&extended, &b.user_ik_public())
        .await
        .expect("the extended directory publishes");
    (root, b, session_b)
}

fn hand_off_file(out: &Path, asset: Uuid) -> PathBuf {
    out.join(format!("{}.{}", asset.simple(), peer::HAND_OFF_SUFFIX))
}

#[tokio::test]
async fn e2e_case_15_a_delegate_uploads_a_sealed_bundle_and_the_origin_releases_on_its_own_check() {
    let server = Server::boot().await;
    let remote = remote_for(&server);
    let mut a = Device::register(&server, "delegation-origin").await;
    let asset = a.import_file("handed.jpg", &synthetic_jpeg());
    assert!(
        !a.workspace
            .upload_bundle(&asset)
            .expect("the bundle")
            .derivatives
            .is_empty(),
        "the hand-off carries derivatives, so B verifies their signed manifests too"
    );
    let (_b_root, b, session_b) = second_device(&server, &a).await;
    let cli_a = Cli::signed_in(&a.login_again(&server).await).await;
    let cli_b = Cli::signed_in(&session_b).await;
    let clock = Clock(AtomicU64::new(1_000));
    let out = tempfile::tempdir().expect("the courier's directory");

    // ── A seals the asset for B: the original stays pinned ────────────────────────────────
    let options = DelegateOptions::new(b.device_id(), vec![asset], out.path().to_owned());
    let handed = peer::delegate(
        &remote,
        &cli_a.store,
        &a.workspace,
        &cli_a.state,
        &clock,
        &options,
    )
    .await
    .expect("the hand-off runs");
    assert!(handed.failed.is_empty(), "{handed:#?}");
    assert_eq!(handed.handed_off.len(), 1);
    let file = hand_off_file(out.path(), asset);
    assert!(file.exists());

    let before = peer::status(
        &remote,
        &cli_a.store,
        &a.workspace,
        &cli_a.state,
        &clock,
        &[],
    )
    .await
    .expect("status runs");
    let view = &before.delegations[0];
    assert_eq!(view.state, DelegationState::HandedOff);
    assert_eq!(
        view.status,
        format!("handed to device {}, awaiting server", b.device_id())
    );
    assert_eq!(
        view.kept_because,
        Some("not_durable"),
        "nothing is on the server yet"
    );

    // ── B takes it, persists it, acknowledges it, and uploads it under its own session ────
    let accepted = peer::accept(
        &remote,
        &cli_b.store,
        &b,
        &cli_b.state,
        &AcceptOptions {
            from: vec![file.clone()],
            policy: UploadPolicy::Full,
            connection: ConnectionClass::Unmetered,
        },
    )
    .await
    .expect("the accept runs");
    assert!(accepted.refused.is_empty(), "{accepted:#?}");
    assert_eq!(accepted.accepted.len(), 1);
    assert_eq!(accepted.uploads.len(), 1);
    assert_eq!(
        accepted.uploads[0].outcome, "removed",
        "B deletes its copy once the server holds every member: {accepted:#?}"
    );
    use capsule_sdk::peering::DelegateInbox as _;
    assert!(
        cli_b.state.inbox().addresses().expect("reads").is_empty(),
        "a delegate copy does not outlive custody"
    );
    assert!(
        b.asset_ids().is_empty(),
        "a delegate copy is never an asset of the delegate's library"
    );

    // The server published the asset A signed, uploaded by B's session.
    let feed = a.feed(&server).await;
    assert!(
        feed.iter().any(|entry| {
            entry.asset_id == asset.to_string().into_bytes() || entry.asset_id == asset.as_bytes()
        }),
        "the delegated asset is on the account's feed"
    );

    // ── A releases only on its own custody check of its own hashes ────────────────────────
    let ack = file.with_extension(ACK_SUFFIX);
    let after = peer::status(
        &remote,
        &cli_a.store,
        &a.workspace,
        &cli_a.state,
        &clock,
        &[ack],
    )
    .await
    .expect("status runs");
    assert!(after.refused_acks.is_empty(), "{after:#?}");
    assert_eq!(
        after.delegations[0].state,
        DelegationState::Released,
        "{after:#?}"
    );
}

#[tokio::test]
async fn e2e_case_15_a_delegate_that_dies_mid_upload_is_re_queued_and_the_origin_uploads_itself() {
    let server = Server::boot().await;
    let remote = remote_for(&server);
    let mut a = Device::register(&server, "delegation-requeue").await;
    let asset = a.import_file("stranded.jpg", &synthetic_jpeg());
    let (_b_root, b, _session_b) = second_device(&server, &a).await;
    let cli_a = Cli::signed_in(&a.login_again(&server).await).await;
    let clock = Clock(AtomicU64::new(0));
    let out = tempfile::tempdir().expect("the courier's directory");

    let mut options = DelegateOptions::new(b.device_id(), vec![asset], out.path().to_owned());
    options.deadline_ms = 60_000;
    peer::delegate(
        &remote,
        &cli_a.store,
        &a.workspace,
        &cli_a.state,
        &clock,
        &options,
    )
    .await
    .expect("the hand-off runs");

    // B took the bundle and died before uploading a byte. Nothing arrives; the deadline passes.
    clock.0.store(60_000, Ordering::SeqCst);
    let due = peer::status(
        &remote,
        &cli_a.store,
        &a.workspace,
        &cli_a.state,
        &clock,
        &[],
    )
    .await
    .expect("status runs");
    assert_eq!(due.delegations[0].state, DelegationState::Requeued);
    assert_eq!(
        due.delegations[0].requeued_because,
        Some(RequeueReason::DeadlineExpired)
    );
    assert_eq!(
        due.delegations[0].status,
        "re-queued to upload from this device"
    );

    // A uploads it itself; either upload's custody releases the original.
    let pushed = remote::push(
        &remote,
        &cli_a.store,
        &mut a.workspace,
        PushOptions::default(),
    )
    .await
    .expect("A pushes");
    assert_eq!(pushed.verified_assets, 1, "{pushed:#?}");
    let released = peer::status(
        &remote,
        &cli_a.store,
        &a.workspace,
        &cli_a.state,
        &clock,
        &[],
    )
    .await
    .expect("status runs");
    assert_eq!(released.delegations[0].state, DelegationState::Released);
}

#[tokio::test]
async fn e2e_case_15_the_server_admits_a_delegated_manifest_on_its_signers_key_alone() {
    let server = Server::boot().await;
    let mut a = Device::register(&server, "delegation-signer").await;
    let asset = a.import_file("signed.jpg", &synthetic_jpeg());
    let bundle = a.workspace.upload_bundle(&asset).expect("the bundle");
    let blobs = bundle_blobs(&bundle);
    let (provenance, address) = blobs.first().expect("the provenance member leads");
    let request = create_request(&bundle, provenance, address);
    let manifest = provenance.bytes.to_vec().expect("inline");

    // Uploaded by a device of the account that holds the proposer key A cross-signed: the
    // manifest names A's library device but is signed by the proposer's key. The uploader is
    // the account's, the named device is the account's, and the signature is not that device's:
    // refused at finalization.
    let mut record =
        capsule_core::crypto::provenance::ProvenanceRecord::decode(&manifest).expect("a record");
    record.manifest = record
        .manifest
        .core
        .clone()
        .sign(&a.proposer, &a.proposer)
        .expect("signs");
    let forged = capsule_core::cbor::to_canonical_vec(&record).expect("encodes");
    let forged_address = capsule_core::crypto::hash::hash_bytes(&forged).to_hex();
    let mut forged_request = request.clone();
    forged_request.size = forged.len() as u64;
    forged_request.hash.clone_from(&forged_address);
    forged_request.manifest_envelope.ciphertext_hash = forged_address;
    let delegate = a.login_again(&server).await;
    let uploads =
        capsule_sdk::upload::UploadClient::new(capsule_sdk::upload::UploadTransport::with_session(
            delegate.clone(),
            server.upload_base(),
            PROTOCOL_VERSION,
        ));
    let refused = uploads.upload(&forged_request, forged.as_slice()).await;
    assert!(
        matches!(
            &refused,
            Err(UploadError::Rejected { code: Some(code), .. })
                if code == "error.upload.envelope_rejected"
        ),
        "a manifest not signed by the device it names is refused: {refused:?}"
    );

    // The same account's other session uploading the genuine manifest: admitted on A's
    // signature, whoever uploads it.
    let admitted = uploads
        .upload(&request, manifest.as_slice())
        .await
        .expect("the origin's signed manifest is admitted from another session");
    assert!(matches!(admitted, UploadOutcome::Completed { .. }));

    // Another account's device: its manifest names its own account, never this uploader.
    let mut other = Device::register(&server, "delegation-stranger").await;
    let theirs = other.import_file("theirs.jpg", &synthetic_jpeg());
    let their_bundle = other.workspace.upload_bundle(&theirs).expect("a bundle");
    let their_blobs = bundle_blobs(&their_bundle);
    let (their_provenance, their_address) = their_blobs.first().expect("leads");
    let mut their_request = create_request(&their_bundle, their_provenance, their_address);
    // Filed into this account's album, as a courier for the wrong account would try.
    their_request.album_id = Some(bundle.album_id.to_string());
    their_request.manifest_envelope.album_id = Some(bundle.album_id.to_string());
    let stranger = uploads
        .upload(
            &their_request,
            their_provenance.bytes.to_vec().expect("inline").as_slice(),
        )
        .await;
    assert!(
        matches!(
            &stranger,
            Err(UploadError::Rejected {
                status: 400 | 403,
                ..
            })
        ),
        "another account's manifest is refused: {stranger:?}"
    );
}
