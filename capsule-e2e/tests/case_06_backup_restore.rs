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
//!
//! A recovered device restores the account's originals over the wire as well (#741): every
//! album's keys ride the escrow, so `capsule auth recover` then `capsule restore` brings back every
//! original byte for byte, resuming an interrupted run from its part file.
//!
//! The crash half of the restore leg — `capsule restore` or its server `SIGKILL`ed mid-fetch, then
//! rerun until the originals on disk are the imported files byte for byte — is
//! `tests/kill_anywhere.rs`.

use std::collections::BTreeMap;

use capsule_cli::remote::{self, AssetPushStatus, PushOptions, RemoteConfig, RemoteError};
use capsule_cli::restore::{
    AssetRestoreStatus, RESTORE_RANGE_WINDOW, RestoreOptions, RestoreScope, STAGING_DIR,
};
use capsule_cli::session::SessionStore;
use capsule_core::crypto::keys::MasterKey;
use capsule_core::crypto::primitives::DeviceTier;
use capsule_core::crypto::provenance::ProvenanceChain;
use capsule_core::crypto::verify_asset::VerifyOutcome;
use capsule_e2e::fixtures::synthetic_jpeg;
use capsule_e2e::{Device, FAST_KDF, PASSPHRASE, PASSWORD, PROTOCOL_VERSION, Server};
use capsule_library::lifecycle::{LifecycleError, Workspace};
use capsule_sdk::auth::{AuthClient, Session};
use capsule_sdk::client::AuthenticatedClient;
use capsule_sdk::directory::{DirectoryClient, DirectoryError};
use capsule_sdk::net::ConnectionClass;
use capsule_sdk::recovery::{MintedSecret, RecoveryClient, RecoveryError};
use uuid::Uuid;

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
/// `create_from_master`, then joining the account's device directory (#625) — over the CLI's
/// own session store, against the real server.
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

    // #625: the new device joined the account's directory rather than replacing it. A had
    // published its own; the command fetched it, appended the new device and published at
    // stored + 1, keeping every device A listed.
    let stored = a.directory();
    let republished = a
        .directory_client(&server)
        .fetch(a.user_id, &a.workspace.user_ik_public())
        .await
        .expect("the account's directory fetches and verifies under A's identity key");
    assert_eq!(
        republished.core.directory_version,
        stored.core.directory_version + 1,
        "the recovery published at stored + 1"
    );
    for device in &stored.core.devices {
        assert_eq!(
            republished.device(&device.device_id),
            Some(device),
            "every device the account already had survives the recovery's publish"
        );
    }
    assert!(
        republished.device(&recovered.device_id()).is_some(),
        "and the recovered device is listed"
    );
    assert_eq!(
        republished.core.devices.len(),
        stored.core.devices.len() + 1
    );
    assert_eq!(
        recovered.device_directory(),
        &republished,
        "the recovered library adopted the directory it published"
    );
    let device = recovered.device_id();
    drop(recovered);

    // It is a real library: it reopens under the passphrase chosen for it, holding the
    // account's directory rather than the version-1 one it was created with.
    let reopened = Workspace::open(&library, PASSPHRASE, FAST_KDF)
        .expect("the recovered library reopens under its new passphrase");
    assert_eq!(reopened.user_id(), a.user_id);
    assert_eq!(reopened.device_id(), device);
    assert_eq!(reopened.device_directory(), &republished);
}

/// **`capsule auth recover` when the directory join fails** (#625): the account's published
/// directory is signed under an identity key other than the one the escrowed sealed identity
/// opens, so the fetched directory does not verify and the new device cannot join it. The
/// library the recovery created is removed, leaving the path as it was — absent when the
/// command made it, empty when the user did — so rerunning the command is the retry.
///
/// `capsule auth escrow` refuses to create that state (#656): it is shown refusing the foreign
/// library and writing nothing, and accepting the anchoring one. The state is then stored over
/// the SDK directly, as an earlier CLI or another client could have left it.
#[tokio::test]
async fn e2e_case_6_a_recovery_whose_directory_join_fails_leaves_nothing_behind() {
    let server = Server::boot().await;
    // Registers the account and publishes a directory under this library's identity key.
    let mut account = Device::register(&server, "cli-recover-join").await;

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

    // A second, unrelated library holds its own identity key for the same account.
    let root = tempfile::tempdir().expect("a library root");
    let mut other =
        Workspace::create_with_params(root.path(), PASSPHRASE, FAST_KDF).expect("a CLI library");
    assert_ne!(other.user_ik_public(), account.workspace.user_ik_public());
    let minted_id = other.user_id();
    let recovery =
        RecoveryClient::new(signed_in.clone(), server.base_url()).expect("the API root parses");

    // #656: `capsule auth escrow` refuses it before a secret is minted, and writes nothing.
    let Err(foreign) = remote::auth_escrow(
        &remote,
        &store,
        &mut other,
        false,
        DeviceTier::LowRam,
        |_| {
            panic!("a library whose key the account's directory is not signed under sees no secret")
        },
    )
    .await
    else {
        panic!("the account's directory does not verify under this library's identity key");
    };
    assert!(
        matches!(
            foreign,
            RemoteError::Directory(DirectoryError::UntrustedSignature)
        ),
        "got {foreign:?}"
    );
    assert_eq!(
        foreign.error_code(),
        Some("error.directory.untrusted_signature"),
        "the CLI localizes the refusal through its catalog code"
    );
    assert_eq!(other.user_id(), minted_id, "the library is not rebound");
    assert!(matches!(
        recovery.fetch_escrow().await,
        Err(RecoveryError::NotEnrolled)
    ));
    assert!(matches!(
        recovery.fetch_sealed_identity().await,
        Err(RecoveryError::IdentityNotStored)
    ));

    // The library whose key the directory is signed under passes the same check.
    remote::auth_escrow(
        &remote,
        &store,
        &mut account.workspace,
        false,
        DeviceTier::LowRam,
        |_| true,
    )
    .await
    .expect("the directory verifies under the anchoring library's key");

    // The state the refusal now prevents, stored over the SDK directly: the other library's
    // escrow and sealed identity, displacing the anchoring library's.
    other
        .bind_account_id(account.user_id)
        .expect("a library without assets binds");
    let secret = MintedSecret::generate();
    let blob = other
        .escrow_master_key(secret.as_bytes(), DeviceTier::LowRam)
        .expect("the master key wraps");
    recovery
        .store_escrow(&blob)
        .await
        .expect("the escrow is stored");
    recovery
        .store_sealed_identity(&other.sealed_identity())
        .await
        .expect("the sealed identity is stored");
    let shown = secret.to_hex();

    // The command makes the root: a failed join removes it.
    let created = home.path().join("recovered");
    let Err(refused) =
        remote::auth_recover(&remote, &store, &created, &shown, PASSPHRASE, FAST_KDF).await
    else {
        panic!("the account's directory does not verify under the escrowed identity");
    };
    assert!(
        matches!(
            refused,
            RemoteError::Directory(DirectoryError::UntrustedSignature)
        ),
        "got {refused:?}"
    );
    assert_eq!(
        refused.error_code(),
        Some("error.directory.untrusted_signature"),
        "the CLI localizes the failure through its catalog code"
    );
    assert!(
        !created.exists(),
        "the library the recovery created is gone"
    );

    // The user made the root: it stays, empty, as it was.
    let made = home.path().join("made-by-the-user");
    std::fs::create_dir(&made).expect("an empty directory");
    let Err(refused) =
        remote::auth_recover(&remote, &store, &made, &shown, PASSPHRASE, FAST_KDF).await
    else {
        panic!("the account's directory still does not verify");
    };
    assert!(
        matches!(
            refused,
            RemoteError::Directory(DirectoryError::UntrustedSignature)
        ),
        "got {refused:?}"
    );
    assert!(made.is_dir(), "the user's directory stays");
    assert_eq!(
        std::fs::read_dir(&made).expect("it reads").count(),
        0,
        "and holds nothing the recovery wrote"
    );

    // Nothing was published: the account's directory is still the one it had.
    assert_eq!(
        account
            .directory_client(&server)
            .fetch(account.user_id, &account.workspace.user_ik_public())
            .await
            .expect("the account's directory still verifies under its own key"),
        account.directory()
    );
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
    // An account no library has published a directory for: the CLI library below is its only
    // one, so its escrow publishes the account's first directory (#920).
    let (signed_in, account_id) = register_without_a_directory(&server).await;

    let home = tempfile::tempdir().expect("a temp CLI home");
    let store = SessionStore::new(home.path().join("session.json"));
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
    assert_ne!(used.user_id(), account_id);
    assert!(matches!(
        recovery.fetch_escrow().await,
        Err(RecoveryError::NotEnrolled)
    ));

    // A fresh CLI library: its own minted id, not the server's.
    let root = tempfile::tempdir().expect("a library root");
    let mut library =
        Workspace::create_with_params(root.path(), PASSPHRASE, FAST_KDF).expect("a CLI library");
    let minted_id = library.user_id();
    assert_ne!(minted_id, account_id);

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
    assert_eq!(setup.user_id, account_id, "the server's id for the account");
    assert!(!setup.replaced, "the account's first escrow");
    assert_eq!(library.user_id(), account_id, "the library is bound");
    // Sealing is randomized, so the stored bytes are not compared here; the recovery below
    // opening the account's identity key is what proves they are this library's.
    recovery
        .fetch_sealed_identity()
        .await
        .expect("the sealed identity is stored beside the escrow");
    // #920: the account had no directory, so the escrow published the bound library's own, and
    // the server accepts what this device uploads.
    let directory = DirectoryClient::new(signed_in.clone(), &server.auth_base());
    let escrowed = directory
        .fetch(account_id, &library.user_ik_public())
        .await
        .expect("the escrow published the account's first directory");
    assert_eq!(&escrowed, library.device_directory());
    assert!(escrowed.device(&library.device_id()).is_some());

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
    assert_eq!(recovered.user_id(), account_id);
    assert_eq!(recovered.user_ik_public(), library.user_ik_public());
    assert_eq!(recovered.default_album_id(), library.default_album_id());
    assert_ne!(recovered.device_id(), library.device_id());

    // #625: the recovery extended the directory the escrow published with the new device,
    // keeping the escrowed library's.
    let published = directory
        .fetch(account_id, &library.user_ik_public())
        .await
        .expect("the account's directory fetches and verifies");
    assert_eq!(
        published.core.directory_version,
        escrowed.core.directory_version + 1
    );
    assert_eq!(&published, recovered.device_directory());
    assert!(published.device(&recovered.device_id()).is_some());
    assert!(published.device(&library.device_id()).is_some());
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

/// **#741: the album keys ride the escrow**, so a device recovered with `capsule auth recover`
/// restores the account's originals with `capsule restore` and nothing else.
///
/// Device A holds two albums — the default one and `Trip`, rotated twice — with an asset under
/// every epoch of both. `capsule auth escrow` stores every album's keys beside the escrow; each
/// rotation after that is re-escrowed by `capsule push` before the new epoch's asset is sent.
/// `capsule auth recover` onto a fresh library B then holds A's albums whole, so resolving the
/// default album mints nothing and B's own escrows match A's. A restore on B whose part file an
/// earlier, interrupted run left half-written resumes from it, and every original comes back
/// byte for byte.
#[tokio::test]
async fn e2e_case_6_a_recovered_device_restores_every_album_byte_for_byte() {
    let server = Server::boot().await;
    let mut a = Device::register(&server, "album-keys").await;
    let default_album = a.workspace.default_album_id();
    // Every imported original's bytes, by asset id: what the restore must write back.
    let mut sources: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let resumed_asset = import_into(&mut a, &mut sources, default_album, "home.jpg", 1);
    let trip = a
        .workspace
        .create_album("Trip")
        .expect("a second album is created");
    import_into(&mut a, &mut sources, trip, "trip-epoch-1.jpg", 2);

    // A's CLI: its own session store, signed in as the account.
    let home_a = tempfile::tempdir().expect("A's CLI home");
    let store_a = SessionStore::new(home_a.path().join("session.json"));
    store_a
        .save(&a.session.export().await.expect("a live session exports"))
        .expect("the session persists");
    let remote = RemoteConfig {
        auth_endpoint: server.auth_base(),
        sync_endpoint: server.base_url().to_owned(),
        upload_endpoint: server.upload_base(),
        albums_endpoint: server.albums_base(),
        protocol_version: PROTOCOL_VERSION.to_owned(),
    };
    let mut shown = String::new();
    remote::auth_escrow(
        &remote,
        &store_a,
        &mut a.workspace,
        false,
        DeviceTier::LowRam,
        |secret| {
            shown = secret.to_hex();
            true
        },
    )
    .await
    .expect("`capsule auth escrow` stores the escrow and every album's keys");
    let recovery =
        RecoveryClient::new(a.session.clone(), server.base_url()).expect("the API root parses");
    let escrowed = recovery
        .list_album_keys()
        .await
        .expect("the album key escrows list");
    let mut both = vec![default_album, trip];
    both.sort();
    assert_eq!(
        escrowed.iter().map(|e| e.album_id).collect::<Vec<_>>(),
        both,
        "one escrow per album"
    );

    // Rotations after the setup: each push escrows the new epoch before its asset leaves. Each
    // epoch is pushed before the album rotates past it, because invariant 18 refuses a create
    // below the album's highest accepted epoch and no client path re-seals one yet (#822).
    let pushed = remote::push(&remote, &store_a, &mut a.workspace, PushOptions::default())
        .await
        .expect("the push runs");
    assert_eq!(
        pushed.album_keys_escrowed, 0,
        "the setup escrowed every album"
    );
    assert_eq!(pushed.verified_assets, sources.len(), "{pushed:#?}");
    for (epoch, seed) in [(2_u8, 3_u8), (3, 4)] {
        a.workspace.rotate_epoch(trip).expect("the album rotates");
        import_into(
            &mut a,
            &mut sources,
            trip,
            &format!("trip-epoch-{epoch}.jpg"),
            seed,
        );
        let pushed = remote::push(&remote, &store_a, &mut a.workspace, PushOptions::default())
            .await
            .expect("the push runs");
        assert_eq!(
            pushed.album_keys_escrowed, 1,
            "epoch {epoch}: only the rotated album was stale"
        );
        assert_eq!(pushed.verified_assets, sources.len(), "{pushed:#?}");
        assert!(
            pushed
                .outcomes
                .iter()
                .all(|o| o.status == AssetPushStatus::Verified),
            "{pushed:#?}"
        );
    }
    let again = remote::push(&remote, &store_a, &mut a.workspace, PushOptions::default())
        .await
        .expect("a second push runs");
    assert_eq!(again.album_keys_escrowed, 0, "every escrow is current");

    // Machine B: its own CLI home, signed in as the account, and `capsule auth recover`.
    let home_b = tempfile::tempdir().expect("B's CLI home");
    let store_b = SessionStore::new(home_b.path().join("session.json"));
    store_b
        .save(
            &a.login_again(&server)
                .await
                .export()
                .await
                .expect("a live session exports"),
        )
        .expect("the session persists");
    let recovered_root = home_b.path().join("library");
    let mut b = remote::auth_recover(
        &remote,
        &store_b,
        &recovered_root,
        &shown,
        PASSPHRASE,
        FAST_KDF,
    )
    .await
    .expect("`capsule auth recover` opens the account with its albums");
    assert_eq!(b.albums(), a.workspace.albums(), "every album, by name");

    // What every later command does first: resolve the default album. It is A's, not a new one.
    b.ensure_album(default_album, "Imports")
        .expect("the default album resolves");
    let held: BTreeMap<Uuid, Vec<u8>> = recovery
        .list_album_keys()
        .await
        .expect("the album key escrows list")
        .into_iter()
        .map(|e| (e.album_id, e.blob))
        .collect();
    let plan = b.plan_album_key_escrows(&held).expect("B's albums compare");
    assert!(
        plan.to_store.is_empty() && plan.uncovered.is_empty(),
        "B holds exactly the keys A escrowed, every epoch of every album"
    );

    // An earlier restore on B was cut off halfway through one original: its part file holds
    // the first half of that original's ciphertext.
    let out = tempfile::tempdir().expect("a restore directory");
    let bundle = a
        .workspace
        .upload_bundle(&resumed_asset)
        .expect("the asset's bundle");
    let ciphertext = bundle.original.to_vec().expect("the ciphertext reads");
    let half = ciphertext.len() / 2;
    let staging = out.path().join(STAGING_DIR);
    std::fs::create_dir_all(&staging).expect("the staging directory");
    std::fs::write(
        staging.join(format!("{}.part", bundle.ciphertext_hash.to_hex())),
        &ciphertext[..half],
    )
    .expect("the part file writes");

    let options = RestoreOptions {
        scope: RestoreScope::Library,
        out: out.path().to_path_buf(),
        connection: ConnectionClass::Unmetered,
        range_window: RESTORE_RANGE_WINDOW,
    };
    let restored = remote::restore(&remote, &store_b, &b, &options)
        .await
        .expect("`capsule restore` runs on the recovered device");
    assert_eq!(restored.assets, sources.len(), "{restored:#?}");
    assert_eq!(restored.restored, sources.len(), "{restored:#?}");
    assert_eq!(restored.failed, 0, "{restored:#?}");
    assert_eq!(
        restored.resumed_bytes, half as u64,
        "the interrupted original resumed from its part file"
    );
    for outcome in &restored.outcomes {
        assert_eq!(outcome.status, AssetRestoreStatus::Restored, "{outcome:#?}");
        let written = std::fs::read(outcome.path.as_ref().expect("a restored path"))
            .expect("the restored original reads");
        assert!(
            written == sources[&outcome.asset_id],
            "{} did not restore byte for byte",
            outcome.asset_id
        );
        if outcome.asset_id == resumed_asset.to_string() {
            assert_eq!(outcome.resumed_bytes, half as u64);
            assert_eq!(
                outcome.fetched_bytes,
                (ciphertext.len() - half) as u64,
                "only the bytes past the part file were fetched"
            );
        }
    }

    // B falls behind: A rotates `Trip` once more and pushes. B's next push must not replace
    // A's escrow with its shorter one — that would cost a later recovery the newest epoch.
    a.workspace.rotate_epoch(trip).expect("the album rotates");
    remote::push(&remote, &store_a, &mut a.workspace, PushOptions::default())
        .await
        .expect("A pushes the new epoch's escrow");
    let behind = remote::push(&remote, &store_b, &mut b, PushOptions::default())
        .await
        .expect("B's push runs");
    assert_eq!(
        behind.album_keys_escrowed, 0,
        "a device behind stores nothing"
    );

    // B then rotates on its own: its epoch 5 is not A's. The escrow is left as A wrote it, and
    // B's asset in that album is held back rather than sent under a key no recovery could find.
    b.rotate_epoch(trip).expect("B rotates on its own");
    let staged = home_b.path().join("diverged.jpg");
    std::fs::write(&staged, synthetic_jpeg()).expect("the fixture writes");
    let held_back = b.import_asset(trip, &staged).expect("B imports");
    let diverged = remote::push(&remote, &store_b, &mut b, PushOptions::default())
        .await
        .expect("B's push runs");
    assert_eq!(diverged.album_keys_escrowed, 0);
    let outcome = diverged
        .outcomes
        .iter()
        .find(|o| o.asset_id == held_back.to_string())
        .expect("B's asset is reported");
    assert_eq!(outcome.status, AssetPushStatus::Failed, "{outcome:#?}");
    assert_eq!(
        outcome.error.as_ref().map(|e| e.kind),
        Some("album_keys_not_escrowed")
    );

    // A recovery from what the server holds still has every one of A's epochs.
    let home_c = tempfile::tempdir().expect("C's CLI home");
    let c = remote::auth_recover(
        &remote,
        &store_b,
        &home_c.path().join("library"),
        &shown,
        PASSPHRASE,
        FAST_KDF,
    )
    .await
    .expect("a second recovery opens the account");
    let a_escrows: BTreeMap<Uuid, Vec<u8>> = a
        .workspace
        .albums()
        .into_iter()
        .map(|(id, _)| (id, a.workspace.album_key_escrow(&id).expect("A seals")))
        .collect();
    let plan = c
        .plan_album_key_escrows(&a_escrows)
        .expect("C's albums compare");
    assert!(
        plan.to_store.is_empty() && plan.uncovered.is_empty(),
        "C holds exactly A's albums, its newest epoch included"
    );
}

/// Import a distinct original seeded by `seed` into `album` on `device`, recording its bytes in
/// `sources` under the new asset's id. Past one STREAM chunk, so a half-written part file is a
/// real prefix to resume from.
fn import_into(
    device: &mut Device,
    sources: &mut BTreeMap<String, Vec<u8>>,
    album: Uuid,
    name: &str,
    seed: u8,
) -> Uuid {
    let mut bytes = synthetic_jpeg();
    bytes.extend(std::iter::repeat_n(seed, 64 * 1024 + usize::from(seed)));
    let path = device.staging.path().join(name);
    std::fs::write(&path, &bytes).expect("the fixture writes");
    let asset = device
        .workspace
        .import_asset(album, &path)
        .expect("the file imports");
    sources.insert(asset.to_string(), bytes);
    asset
}

/// Register a fresh account and read its server id, publishing no device directory for it —
/// what `capsule auth register` leaves behind, unlike [`Device::register`].
async fn register_without_a_directory(server: &Server) -> (Session, Uuid) {
    let email = format!("cli-escrow-{}@e2e.capsule.test", Uuid::now_v7().simple());
    let session = AuthClient::new(&server.auth_base())
        .expect("the auth base parses")
        .register(&email, PASSWORD)
        .await
        .expect("a fresh account registers");
    let profile = AuthenticatedClient::new(server.base_url(), session.clone())
        .expect("the API root parses")
        .get_profile(PROTOCOL_VERSION, None)
        .await
        .expect("the profile answers")
        .into_inner();
    let user_id = Uuid::parse_str(&profile.user_id).expect("the account id is a UUID");
    (session, user_id)
}
