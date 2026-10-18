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

use capsule_cli::remote::{self, RemoteConfig, RemoteError};
use capsule_cli::session::SessionStore;
use capsule_core::crypto::keys::MasterKey;
use capsule_core::crypto::primitives::DeviceTier;
use capsule_core::crypto::provenance::ProvenanceChain;
use capsule_core::crypto::verify_asset::VerifyOutcome;
use capsule_core::lifecycle::{LifecycleError, Workspace};
use capsule_e2e::fixtures::synthetic_jpeg;
use capsule_e2e::{Device, FAST_KDF, PASSPHRASE, PROTOCOL_VERSION, Server};
use capsule_sdk::directory::{DirectoryClient, DirectoryError};
use capsule_sdk::recovery::{MintedSecret, RecoveryClient, RecoveryError};

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

/// **`capsule auth recover`**, the CLI's leg of case 6 (#544): the orchestration the command
/// runs — profile, escrow fetch, sealed-identity fetch, `recover_master_key`,
/// `create_from_master` — over the CLI's own session store, against the real server.
///
/// The secret is a minted one, typed back as the hex the user was shown: the escrow is wrapped
/// under its bytes, which is the reading the command must try. Every refusal is driven before
/// the success and leaves nothing behind.
#[tokio::test]
async fn e2e_case_6_the_cli_recover_command_opens_a_new_library_as_the_account() {
    let server = Server::boot().await;
    let a = Device::register(&server, "cli-recover").await;
    let secret = MintedSecret::generate();
    let escrow = a
        .workspace
        .escrow_master_key(secret.as_bytes(), DeviceTier::LowRam)
        .expect("the master key wraps under the minted secret");

    // The CLI's state: the session `capsule auth login` persists on the new machine.
    let home = tempfile::tempdir().expect("a temp CLI home");
    let store = SessionStore::new(home.path().join("session.json"));
    let remote = RemoteConfig {
        auth_endpoint: server.auth_base(),
        sync_endpoint: server.base_url().to_owned(),
        upload_endpoint: server.upload_base(),
        albums_endpoint: server.albums_base(),
        protocol_version: PROTOCOL_VERSION.to_owned(),
    };
    let library = home.path().join("recovered");

    // No session stored: nothing to recover as.
    assert!(matches!(
        remote::auth_recover(
            &remote,
            &store,
            &library,
            &secret.to_hex(),
            PASSPHRASE,
            FAST_KDF
        )
        .await,
        Err(RemoteError::NotAuthenticated)
    ));

    let signed_in = a.login_again(&server).await;
    store
        .save(&signed_in.export().await.expect("a live session exports"))
        .expect("the session persists");

    // An escrow without a sealed identity cannot be recovered as the account, and says so with
    // its own code before any key derivation.
    let recovery_a =
        RecoveryClient::new(a.session.clone(), server.base_url()).expect("the API root parses");
    recovery_a
        .store_escrow(&escrow)
        .await
        .expect("the escrow stores");
    let Err(missing) = remote::auth_recover(
        &remote,
        &store,
        &library,
        &secret.to_hex(),
        PASSPHRASE,
        FAST_KDF,
    )
    .await
    else {
        panic!("no sealed identity is stored yet, so recovery cannot succeed");
    };
    assert!(
        matches!(
            missing,
            RemoteError::Recovery(RecoveryError::IdentityNotStored)
        ),
        "got {missing:?}"
    );
    assert_eq!(
        missing.error_code(),
        Some("error.escrow.identity_not_stored")
    );
    assert!(!library.exists(), "a refused recovery writes nothing");

    recovery_a
        .store_sealed_identity(&a.workspace.sealed_identity())
        .await
        .expect("the sealed identity stores");

    // The wrong secret opens nothing, and writes nothing.
    let Err(wrong) = remote::auth_recover(
        &remote,
        &store,
        &library,
        &MintedSecret::generate().to_hex(),
        PASSPHRASE,
        FAST_KDF,
    )
    .await
    else {
        panic!("another secret does not open the escrow");
    };
    assert!(
        matches!(wrong, RemoteError::WrongRecoverySecret),
        "got {wrong:?}"
    );
    assert!(!library.exists(), "a refused recovery writes nothing");

    // The secret as the user was shown it: a new library, as A's account, on a new device.
    let recovered = remote::auth_recover(
        &remote,
        &store,
        &library,
        &secret.to_hex(),
        PASSPHRASE,
        FAST_KDF,
    )
    .await
    .expect("`capsule auth recover` completes");
    assert_eq!(recovered.user_id(), a.user_id, "the server's id for A");
    assert_eq!(
        recovered.user_ik_public(),
        a.workspace.user_ik_public(),
        "the identity the server pinned for A"
    );
    assert_eq!(
        recovered.default_album_id(),
        a.workspace.default_album_id(),
        "A's master key"
    );
    assert_ne!(recovered.device_id(), a.workspace.device_id());
    drop(recovered);

    // It is a real library: it reopens under the passphrase chosen for it.
    let reopened = Workspace::open(&library, PASSPHRASE, FAST_KDF)
        .expect("the recovered library reopens under its new passphrase");
    assert_eq!(reopened.user_id(), a.user_id);
}

/// **`capsule auth escrow`** then **`capsule auth recover`** (#624): a library the CLI created
/// — carrying its own minted account id — is set up for recovery by the CLI, and the CLI
/// recovers it on a new machine from the secret the escrow command showed and nothing else.
///
/// Every refusal is driven first and shown to store nothing: a library that already holds
/// assets cannot be rebound, and a failed type-back gate writes nothing. Then the setup, the
/// refusal of a second setup without `--replace`, the recovery, and a `--replace` that retires
/// the first secret.
#[tokio::test]
async fn e2e_case_6_the_cli_escrow_command_sets_up_what_recover_restores() {
    let server = Server::boot().await;
    let account = Device::register(&server, "cli-escrow").await;

    let home = tempfile::tempdir().expect("a temp CLI home");
    let store = SessionStore::new(home.path().join("session.json"));
    let signed_in = account.login_again(&server).await;
    store
        .save(&signed_in.export().await.expect("a live session exports"))
        .expect("the session persists");
    let remote = RemoteConfig {
        auth_endpoint: server.auth_base(),
        sync_endpoint: server.base_url().to_owned(),
        upload_endpoint: server.upload_base(),
        albums_endpoint: server.albums_base(),
        protocol_version: PROTOCOL_VERSION.to_owned(),
    };
    let recovery =
        RecoveryClient::new(signed_in.clone(), server.base_url()).expect("the API root parses");

    // A library that already signed an asset under its own id cannot adopt the server's.
    let used_root = tempfile::tempdir().expect("a library root");
    let mut used = Workspace::create_with_params(used_root.path(), PASSPHRASE, FAST_KDF)
        .expect("a CLI library");
    let album = used.default_album_id();
    used.ensure_album(album, "Imports")
        .expect("the default album's keys exist");
    let jpeg = used_root.path().join("keepsake.jpg");
    std::fs::write(&jpeg, synthetic_jpeg()).expect("the fixture writes");
    used.import_asset(album, &jpeg).expect("the file imports");
    // Refused before a secret is minted: the user is never shown one the bind would discard.
    let Err(rebind) = remote::auth_escrow(
        &remote,
        &store,
        &mut used,
        false,
        DeviceTier::LowRam,
        |_| panic!("a library with assets is refused before a recovery secret is shown"),
    )
    .await
    else {
        panic!("a library with assets cannot be bound to the server's id");
    };
    assert!(
        matches!(
            rebind,
            RemoteError::Lifecycle(LifecycleError::AccountRebind(_))
        ),
        "got {rebind:?}"
    );
    assert_ne!(used.user_id(), account.user_id);
    assert!(matches!(
        recovery.fetch_escrow().await,
        Err(RecoveryError::NotEnrolled)
    ));

    // A fresh CLI library: its own minted id, not the server's.
    let root = tempfile::tempdir().expect("a library root");
    let mut library =
        Workspace::create_with_params(root.path(), PASSPHRASE, FAST_KDF).expect("a CLI library");
    let minted_id = library.user_id();
    assert_ne!(minted_id, account.user_id);

    // The type-back gate failing stores nothing and leaves the library unbound.
    let Err(unconfirmed) = remote::auth_escrow(
        &remote,
        &store,
        &mut library,
        false,
        DeviceTier::LowRam,
        |_| false,
    )
    .await
    else {
        panic!("an unconfirmed secret is not stored");
    };
    assert!(
        matches!(unconfirmed, RemoteError::RecoverySecretNotConfirmed),
        "got {unconfirmed:?}"
    );
    assert_eq!(library.user_id(), minted_id, "the library is not rebound");
    assert!(matches!(
        recovery.fetch_escrow().await,
        Err(RecoveryError::NotEnrolled)
    ));

    // The setup: the secret is captured where the user would read it off the terminal.
    let mut shown = String::new();
    let setup = remote::auth_escrow(
        &remote,
        &store,
        &mut library,
        false,
        DeviceTier::LowRam,
        |secret| {
            shown = secret.to_hex();
            true
        },
    )
    .await
    .expect("`capsule auth escrow` completes");
    assert_eq!(
        setup.user_id, account.user_id,
        "the server's id for the account"
    );
    assert!(!setup.replaced, "the account's first escrow");
    assert_eq!(library.user_id(), account.user_id, "the library is bound");
    // Sealing is randomized, so the stored bytes are not compared here; the recovery below
    // opening the account's identity key is what proves they are this library's.
    recovery
        .fetch_sealed_identity()
        .await
        .expect("the sealed identity is stored beside the escrow");

    // A second setup without --replace would retire the secret just shown: refused, and the
    // secret is never minted.
    let Err(again) = remote::auth_escrow(
        &remote,
        &store,
        &mut library,
        false,
        DeviceTier::LowRam,
        |_| panic!("no secret is minted when an escrow is already stored"),
    )
    .await
    else {
        panic!("a stored escrow is not replaced without --replace");
    };
    assert!(
        matches!(again, RemoteError::EscrowAlreadyStored),
        "got {again:?}"
    );

    // The new machine: `capsule auth recover` with the secret the escrow command showed.
    let new_home = tempfile::tempdir().expect("a new machine");
    let recovered_root = new_home.path().join("recovered");
    let recovered = remote::auth_recover(
        &remote,
        &store,
        &recovered_root,
        &shown,
        PASSPHRASE,
        FAST_KDF,
    )
    .await
    .expect("`capsule auth recover` opens what `capsule auth escrow` stored");
    assert_eq!(recovered.user_id(), account.user_id);
    assert_eq!(recovered.user_ik_public(), library.user_ik_public());
    assert_eq!(recovered.default_album_id(), library.default_album_id());
    assert_ne!(recovered.device_id(), library.device_id());
    drop(recovered);

    // --replace stores a new escrow, and the first secret stops opening it.
    let replaced = remote::auth_escrow(
        &remote,
        &store,
        &mut library,
        true,
        DeviceTier::LowRam,
        |_| true,
    )
    .await
    .expect("`capsule auth escrow --replace` completes");
    assert!(replaced.replaced, "the earlier escrow was displaced");
    let Err(stale) = remote::auth_recover(
        &remote,
        &store,
        &new_home.path().join("stale"),
        &shown,
        PASSPHRASE,
        FAST_KDF,
    )
    .await
    else {
        panic!("the retired secret opens nothing");
    };
    assert!(
        matches!(stale, RemoteError::WrongRecoverySecret),
        "got {stale:?}"
    );
}
