//! **E2E case 15** — upload delegation (design/module-map.md; Peering § Delegation, issue #701).
//!
//! A (the origin) hands a sealed bundle to B, another device of the same account; B uploads it
//! under its own session; A releases the original only after verifying custody of the hashes it
//! computed itself. B's death mid-upload makes A re-queue it, and A's own upload then releases
//! it. Both legs are the CLI's orchestration (`capsule_cli::peer`), which is what
//! `capsule peer delegate`, `capsule peer accept` and `capsule peer status` run, over the
//! sessions `capsule auth login` persists, against the real server composition root. The
//! hand-off travels by file and over the LAN peering channel (`peer delegate --peer` against
//! `peer accept --listen` on loopback, a real mutual TLS 1.3 channel).
//!
//! The server half is asserted too: it admits a manifest one device signed and another uploaded
//! on the signer's signature, and refuses one signed by a key that is not the named device's —
//! uploaded by a device of the account all the same — and one naming another account.
//!
//! A delete A makes after the hand-off reaches the server only once A's own custody check finds
//! the create B uploaded, and `capsule push` leaves an open hand-off to B but pushes a re-queued
//! one.
//!
//! The rows decided on one device (tampering, partial hand-off, an older delegate, a revoked
//! delegate, the ledger's hold of a delete after hand-off, double upload) are unit tests in
//! `capsule-sdk/src/peering/delegation/tests.rs`; the signer check's are in
//! `capsule-server/src/upload/signer/tests.rs` and `capsule-server/tests/upload.rs`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use capsule_cli::peer::{
    self, ACK_SUFFIX, AcceptOptions, BootClock, DelegateOptions, PeerListener, PeerState,
};
use capsule_cli::peering_state::DbDirectoryVersions;
use capsule_cli::remote::{self, AssetPushStatus, PushOptions, RemoteConfig};
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
use capsule_sdk::sync::ChangeKind;
use capsule_sdk::upload::{UploadError, UploadOutcome};
use migration::{Migrator, MigratorTrait as _};
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

/// One device's CLI state: its session store, its peer state and the directory floor its CLI
/// database persists.
struct Cli {
    _home: TempDir,
    store: SessionStore,
    state: PeerState,
    versions: DbDirectoryVersions,
}

impl Cli {
    async fn signed_in(session: &Session, device: Uuid) -> Self {
        let home = tempfile::tempdir().expect("a temp CLI home");
        let store = SessionStore::new(home.path().join("session.json"));
        store
            .save(&session.export().await.expect("a live session exports"))
            .expect("the session persists");
        let state = PeerState::new(home.path().join("peer"));
        let url = format!(
            "sqlite://{}?mode=rwc",
            home.path().join("cli.sqlite").display()
        );
        let db = sea_orm::Database::connect(&url)
            .await
            .expect("the CLI database opens");
        Migrator::up(&db, None)
            .await
            .expect("the CLI database migrates");
        let versions = DbDirectoryVersions::open(db, device)
            .await
            .expect("the directory floor opens");
        Self {
            _home: home,
            store,
            state,
            versions,
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
    let cli_a = Cli::signed_in(&a.login_again(&server).await, a.workspace.device_id()).await;
    let cli_b = Cli::signed_in(&session_b, b.device_id()).await;
    let clock = Clock(AtomicU64::new(1_000));
    let out = tempfile::tempdir().expect("the courier's directory");

    // ── A seals the asset for B: the original stays pinned ────────────────────────────────
    let options = DelegateOptions::new(b.device_id(), vec![asset], out.path().to_owned());
    let handed = peer::delegate(
        &remote,
        &cli_a.store,
        &a.workspace,
        &cli_a.state,
        &cli_a.versions,
        &clock,
        &options,
    )
    .await
    .expect("the hand-off runs");
    assert!(handed.failed.is_empty(), "{handed:#?}");
    assert_eq!(handed.handed_off.len(), 1);
    let file = hand_off_file(out.path(), asset);
    assert!(file.exists());
    use capsule_sdk::peering::DirectoryVersions as _;
    assert!(
        cli_a.versions.highest().expect("the floor reads") >= 1,
        "the run admitted the account's directory into the persisted floor"
    );

    let before = peer::status(
        &remote,
        &cli_a.store,
        &a.workspace,
        &cli_a.state,
        &cli_a.versions,
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
        &cli_b.versions,
        &AcceptOptions {
            from: vec![file.clone()],
            policy: UploadPolicy::Full,
            connection: ConnectionClass::Unmetered,
            listen: None,
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
        &cli_a.versions,
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

/// The asset's feed entries, as the change kinds the server published for it.
fn kinds_of(feed: &[capsule_sdk::sync::FeedEntry], asset: Uuid) -> Vec<ChangeKind> {
    let wanted = asset.to_string().into_bytes();
    feed.iter()
        .filter(|entry| entry.asset_id == wanted || entry.asset_id == asset.as_bytes())
        .map(|entry| entry.kind)
        .collect()
}

/// A deletes the asset after handing it to B. The delete stays on A, held in the origin ledger,
/// while the server has not indexed the create — B has not uploaded it yet — and reaches the
/// server only once A's own custody check finds the create's receipts; it is sent once.
#[tokio::test]
async fn e2e_case_15_a_delete_after_hand_off_reaches_the_server_only_once_custody_exists() {
    let server = Server::boot().await;
    let remote = remote_for(&server);
    let mut a = Device::register(&server, "delegation-delete").await;
    let asset = a.import_file("deleted-after.jpg", &synthetic_jpeg());
    let (_b_root, b, session_b) = second_device(&server, &a).await;
    let cli_a = Cli::signed_in(&a.login_again(&server).await, a.workspace.device_id()).await;
    let cli_b = Cli::signed_in(&session_b, b.device_id()).await;
    let clock = Clock(AtomicU64::new(1_000));
    let out = tempfile::tempdir().expect("the courier's directory");

    let options = DelegateOptions::new(b.device_id(), vec![asset], out.path().to_owned());
    let handed = peer::delegate(
        &remote,
        &cli_a.store,
        &a.workspace,
        &cli_a.state,
        &cli_a.versions,
        &clock,
        &options,
    )
    .await
    .expect("the hand-off runs");
    assert!(handed.failed.is_empty(), "{handed:#?}");

    // ── A deletes the asset; B has not uploaded it, so the delete is held ─────────────────
    a.workspace.soft_delete(&asset, 30).expect("A deletes it");
    let held = peer::status(
        &remote,
        &cli_a.store,
        &a.workspace,
        &cli_a.state,
        &cli_a.versions,
        &clock,
        &[],
    )
    .await
    .expect("status runs");
    let outcomes: Vec<&str> = held.deletes.iter().map(|d| d.outcome).collect();
    assert_eq!(outcomes, vec!["held"], "{held:#?}");
    assert_eq!(held.delegations[0].state, DelegationState::HandedOff);
    assert!(
        kinds_of(&a.feed(&server).await, asset).is_empty(),
        "the server has seen neither the create nor the delete"
    );
    // A second run does not hold it twice, and still sends nothing.
    let again = peer::status(
        &remote,
        &cli_a.store,
        &a.workspace,
        &cli_a.state,
        &cli_a.versions,
        &clock,
        &[],
    )
    .await
    .expect("status runs");
    let outcomes: Vec<&str> = again.deletes.iter().map(|d| d.outcome).collect();
    assert_eq!(outcomes, vec!["held"], "{again:#?}");

    // ── B uploads the create under its own session ────────────────────────────────────────
    let file = hand_off_file(out.path(), asset);
    let accepted = peer::accept(
        &remote,
        &cli_b.store,
        &b,
        &cli_b.state,
        &cli_b.versions,
        &AcceptOptions {
            from: vec![file.clone()],
            policy: UploadPolicy::Full,
            connection: ConnectionClass::Unmetered,
            listen: None,
        },
    )
    .await
    .expect("the accept runs");
    assert_eq!(accepted.uploads.len(), 1, "{accepted:#?}");
    assert_eq!(accepted.uploads[0].outcome, "removed", "{accepted:#?}");
    let kinds = kinds_of(&a.feed(&server).await, asset);
    assert!(
        !kinds.is_empty() && !kinds.contains(&ChangeKind::Deleted),
        "the create is indexed and nothing deleted it yet: {kinds:?}"
    );

    // ── A's own custody check releases it and sends the held delete ───────────────────────
    let released = peer::status(
        &remote,
        &cli_a.store,
        &a.workspace,
        &cli_a.state,
        &cli_a.versions,
        &clock,
        &[file.with_extension(ACK_SUFFIX)],
    )
    .await
    .expect("status runs");
    assert_eq!(
        released.delegations[0].state,
        DelegationState::Released,
        "{released:#?}"
    );
    let outcomes: Vec<&str> = released.deletes.iter().map(|d| d.outcome).collect();
    assert_eq!(outcomes, vec!["sent"], "{released:#?}");
    assert!(
        kinds_of(&a.feed(&server).await, asset).contains(&ChangeKind::Deleted),
        "the delete reached the server after the create's custody"
    );

    // Sent once: a later run has nothing left to send.
    let after = peer::status(
        &remote,
        &cli_a.store,
        &a.workspace,
        &cli_a.state,
        &cli_a.versions,
        &clock,
        &[],
    )
    .await
    .expect("status runs");
    assert!(after.deletes.is_empty(), "{after:#?}");
}

/// `capsule peer delegate --peer` against `capsule peer accept --listen` on loopback: A dials B's
/// listener, the pair completes the mutual TLS handshake and the hybrid identity check, B pulls
/// the hand-off over the verified channel, persists it and acknowledges it over the same channel,
/// then uploads it; A records the acknowledgement as it arrives and releases the original only on
/// its own custody check.
#[tokio::test]
async fn e2e_case_15_peer_delegate_hands_off_over_the_lan_channel_to_peer_accept_listening() {
    let server = Server::boot().await;
    let remote = remote_for(&server);
    let mut a = Device::register(&server, "delegation-channel").await;
    let asset = a.import_file("carried.jpg", &synthetic_jpeg());
    let (_b_root, b, session_b) = second_device(&server, &a).await;
    let cli_a = Cli::signed_in(&a.login_again(&server).await, a.workspace.device_id()).await;
    let cli_b = Cli::signed_in(&session_b, b.device_id()).await;
    let clock = Clock(AtomicU64::new(1_000));

    let listener = PeerListener::bind(
        "127.0.0.1:0".parse().expect("an address"),
        Duration::from_secs(60),
    )
    .await
    .expect("B listens on loopback");
    let address = listener.local_addr().expect("bound").to_string();
    let accept_options = AcceptOptions {
        from: Vec::new(),
        policy: UploadPolicy::Full,
        connection: ConnectionClass::Unmetered,
        listen: Some(listener),
    };
    let accepting = peer::accept(
        &remote,
        &cli_b.store,
        &b,
        &cli_b.state,
        &cli_b.versions,
        &accept_options,
    );
    let delegate_options = DelegateOptions::over_peer(address, Some(b.device_id()), vec![asset]);
    let delegating = peer::delegate(
        &remote,
        &cli_a.store,
        &a.workspace,
        &cli_a.state,
        &cli_a.versions,
        &clock,
        &delegate_options,
    );
    let (accepted, handed) = tokio::join!(accepting, delegating);
    let accepted = accepted.expect("the accept runs");
    let handed = handed.expect("the hand-off runs");

    // A: handed to the device the channel verified, and acknowledged over it.
    assert!(handed.failed.is_empty(), "{handed:#?}");
    assert_eq!(handed.delegate, Some(b.device_id()));
    assert_eq!(handed.handed_off.len(), 1);
    assert!(handed.handed_off[0].acknowledged, "{handed:#?}");
    assert!(
        handed.handed_off[0].file.is_none(),
        "nothing was written to a file"
    );

    // B: pulled from the verified origin, persisted, acknowledged, uploaded and cleaned up.
    assert!(accepted.refused.is_empty(), "{accepted:#?}");
    assert_eq!(accepted.accepted.len(), 1);
    let taken = &accepted.accepted[0];
    assert_eq!(taken.peer, Some(a.workspace.device_id()));
    assert_eq!(taken.acknowledged, Some(true));
    assert_eq!(taken.ack.hand_off, handed.handed_off[0].hand_off);
    assert_eq!(
        accepted.uploads.len(),
        1,
        "B uploads what it pulled: {accepted:#?}"
    );
    assert_eq!(accepted.uploads[0].outcome, "removed", "{accepted:#?}");
    use capsule_sdk::peering::DelegateInbox as _;
    assert!(cli_b.state.inbox().addresses().expect("reads").is_empty());
    assert!(b.asset_ids().is_empty(), "never an asset of B's library");

    // A took the acknowledgement over the channel (no ack file) and releases on its own check.
    let after = peer::status(
        &remote,
        &cli_a.store,
        &a.workspace,
        &cli_a.state,
        &cli_a.versions,
        &clock,
        &[],
    )
    .await
    .expect("status runs");
    assert_eq!(
        after.delegations[0].state,
        DelegationState::Released,
        "{after:#?}"
    );
}

/// A `--peer` that is not on the local network is refused before a byte is sent, and the run
/// reports every asset under the stable `not_lan` kind.
#[tokio::test]
async fn e2e_case_15_peer_delegate_refuses_a_peer_off_the_local_network() {
    let server = Server::boot().await;
    let remote = remote_for(&server);
    let mut a = Device::register(&server, "delegation-not-lan").await;
    let asset = a.import_file("kept.jpg", &synthetic_jpeg());
    let cli_a = Cli::signed_in(&a.login_again(&server).await, a.workspace.device_id()).await;
    let handed = peer::delegate(
        &remote,
        &cli_a.store,
        &a.workspace,
        &cli_a.state,
        &cli_a.versions,
        &Clock(AtomicU64::new(0)),
        &DelegateOptions::over_peer("192.0.2.1:7420", None, vec![asset]),
    )
    .await
    .expect("the run reports per asset");
    assert!(handed.handed_off.is_empty());
    assert_eq!(handed.delegate, None);
    assert_eq!(handed.failed.len(), 1);
    assert_eq!(handed.failed[0].kind, "not_lan");
    assert!(
        cli_a
            .state
            .load_ledger()
            .expect("reads")
            .get(&asset)
            .is_none(),
        "nothing was handed off, so nothing is waited on"
    );
}

/// The device at `--peer` verifies as a device of the account, but not the `--to` device the
/// user named: nothing is sealed or sent, every asset is reported under `peer_mismatch`, and
/// the listener, whose session ends with no listing, keeps waiting until its timeout.
#[tokio::test]
async fn e2e_case_15_peer_delegate_refuses_a_peer_that_is_not_the_named_device() {
    let server = Server::boot().await;
    let remote = remote_for(&server);
    let mut a = Device::register(&server, "delegation-mismatch").await;
    let asset = a.import_file("named.jpg", &synthetic_jpeg());
    let (_b_root, b, session_b) = second_device(&server, &a).await;
    let cli_a = Cli::signed_in(&a.login_again(&server).await, a.workspace.device_id()).await;
    let cli_b = Cli::signed_in(&session_b, b.device_id()).await;

    let listener = PeerListener::bind(
        "127.0.0.1:0".parse().expect("an address"),
        Duration::from_secs(3),
    )
    .await
    .expect("B listens on loopback");
    let address = listener.local_addr().expect("bound").to_string();
    let accept_options = AcceptOptions {
        from: Vec::new(),
        policy: UploadPolicy::Full,
        connection: ConnectionClass::Unmetered,
        listen: Some(listener),
    };
    let someone_else = Uuid::now_v7();
    let delegate_options = DelegateOptions::over_peer(address, Some(someone_else), vec![asset]);
    let clock = Clock(AtomicU64::new(0));
    let (accepted, handed) = tokio::join!(
        peer::accept(
            &remote,
            &cli_b.store,
            &b,
            &cli_b.state,
            &cli_b.versions,
            &accept_options,
        ),
        peer::delegate(
            &remote,
            &cli_a.store,
            &a.workspace,
            &cli_a.state,
            &cli_a.versions,
            &clock,
            &delegate_options,
        )
    );
    let handed = handed.expect("the run reports per asset");
    assert_eq!(
        handed.delegate,
        Some(b.device_id()),
        "the channel verified B"
    );
    assert!(handed.handed_off.is_empty());
    assert_eq!(handed.failed.len(), 1);
    assert_eq!(handed.failed[0].kind, "peer_mismatch");
    assert!(
        cli_a
            .state
            .load_ledger()
            .expect("reads")
            .get(&asset)
            .is_none()
    );

    let accepted = accepted.expect("the accept runs");
    assert!(accepted.accepted.is_empty());
    let kinds: Vec<&str> = accepted.refused.iter().map(|f| f.kind).collect();
    assert!(
        kinds.contains(&"peering_timeout"),
        "no session completed before the listen timeout: {kinds:?}"
    );
    use capsule_sdk::peering::DelegateInbox as _;
    assert!(cli_b.state.inbox().addresses().expect("reads").is_empty());
}

#[tokio::test]
async fn e2e_case_15_a_delegate_that_dies_mid_upload_is_re_queued_and_the_origin_uploads_itself() {
    let server = Server::boot().await;
    let remote = remote_for(&server);
    let mut a = Device::register(&server, "delegation-requeue").await;
    let asset = a.import_file("stranded.jpg", &synthetic_jpeg());
    let (_b_root, b, _session_b) = second_device(&server, &a).await;
    let cli_a = Cli::signed_in(&a.login_again(&server).await, a.workspace.device_id()).await;
    let clock = Clock(AtomicU64::new(0));
    let out = tempfile::tempdir().expect("the courier's directory");

    let mut options = DelegateOptions::new(b.device_id(), vec![asset], out.path().to_owned());
    options.deadline_ms = 60_000;
    peer::delegate(
        &remote,
        &cli_a.store,
        &a.workspace,
        &cli_a.state,
        &cli_a.versions,
        &clock,
        &options,
    )
    .await
    .expect("the hand-off runs");

    // While the hand-off is open and inside its deadline, `capsule push` leaves it to B.
    let push_now = |delegated| remote::PushRun {
        delegated,
        ..remote::PushRun::default()
    };
    let skipped = remote::push_run(
        &remote,
        &cli_a.store,
        &mut a.workspace,
        PushOptions::default(),
        push_now(peer::delegated_assets(&cli_a.state, &clock).expect("the ledger reads")),
    )
    .await
    .expect("A pushes");
    assert_eq!(skipped.delegated_assets, 1, "{skipped:#?}");
    assert_eq!(skipped.outcomes[0].status, AssetPushStatus::Delegated);
    assert_eq!(skipped.uploaded_blobs, 0, "nothing of it was sent");
    assert!(
        kinds_of(&a.feed(&server).await, asset).is_empty(),
        "the server has not seen the asset"
    );

    // B took the bundle and died before uploading a byte. Nothing arrives; the deadline passes.
    clock.0.store(60_000, Ordering::SeqCst);
    let due = peer::status(
        &remote,
        &cli_a.store,
        &a.workspace,
        &cli_a.state,
        &cli_a.versions,
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

    // A re-queued asset is pushed from here: A uploads it itself, and either upload's custody
    // releases the original.
    let delegated = peer::delegated_assets(&cli_a.state, &clock).expect("the ledger reads");
    assert!(
        delegated.is_empty(),
        "a re-queued hand-off is not left to B"
    );
    let pushed = remote::push_run(
        &remote,
        &cli_a.store,
        &mut a.workspace,
        PushOptions::default(),
        push_now(delegated),
    )
    .await
    .expect("A pushes");
    assert_eq!(pushed.verified_assets, 1, "{pushed:#?}");
    assert_eq!(pushed.delegated_assets, 0);
    assert_eq!(pushed.outcomes[0].status, AssetPushStatus::Verified);
    let released = peer::status(
        &remote,
        &cli_a.store,
        &a.workspace,
        &cli_a.state,
        &cli_a.versions,
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
