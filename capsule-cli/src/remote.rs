//! The CLI's networked commands — `auth login/logout`, `sync`, `list` — expressed
//! entirely over `capsule-sdk`.
//!
//! The CLI is a client: it **never hand-rolls a network flow**. Every request here
//! goes through an SDK primitive — [`AuthClient`]/[`Session`] for auth, and
//! [`SyncConsumer`]/[`SyncState`] for the feed — so token refresh, the protocol
//! handshake, the `401`/`Unauthenticated` retry, and the anti-rewind contract are
//! the SDK's, not re-implemented here. These functions are the testable core the
//! binary's command arms call and the E2E round-trip drives directly.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;

use capsule_core::crypto::keys::{HybridVerifyingKey, MasterKey};
use capsule_core::crypto::primitives::{Argon2Params, DeviceTier};
use capsule_library::import::UploadPolicy;
use capsule_library::library::{
    KeyPinError, ReleaseDecision, RetainReason, StorageVerdict, StorageVerifier,
    pin_attestation_keys,
};
use capsule_library::lifecycle::{LifecycleError, ResealRefusal, Workspace};
use capsule_sdk::albums::{AlbumClient, AlbumTransport};
use capsule_sdk::auth::{AuthClient, AuthError, LoginOutcome, Session};
use capsule_sdk::client::AuthenticatedClient;
use capsule_sdk::directory::{DirectoryClient, DirectoryError};
use capsule_sdk::events::TransferEvents;
use capsule_sdk::fetch::HttpBlobSource;
use capsule_sdk::net::ConnectionClass;
use capsule_sdk::push::{self, PushError};
use capsule_sdk::recovery::{MintedSecret, RecoveryClient, RecoveryError};
use capsule_sdk::staged::{StagedScheduler, TierSessionOutcome, held_from_feed};
use capsule_sdk::sync::{FeedEntry, SyncConsumer, SyncCursor, SyncError, SyncState};
use capsule_sdk::upload::{UploadClient, UploadError, UploadTransport};
use capsule_sdk::verify::{
    AssetQuery, HeldRelease, ReleaseCoordinator, StorageVerifyClient, VerifiedAttestation,
    VerifyError, VerifyTransport,
};
use sea_orm::{ConnectionTrait, TransactionTrait};
use secrecy::SecretString;
use serde::Serialize;
use thiserror::Error;
use tracing::instrument;
use uuid::Uuid;

use crate::restore::{self, RestoreOptions, RestoreSummary};
use crate::session::{SessionError, SessionStore};
use crate::syncstore::{self, SyncStoreError, SyncedAssetView};
use crate::verify::{self, VerifyOptions, VerifySummary};

/// The default sync page size (matches the server's `DEFAULT_PAGE_SIZE`).
pub const DEFAULT_SYNC_PAGE_SIZE: u32 = 256;

/// The remote endpoints and the protocol pin the CLI speaks.
#[derive(Debug, Clone)]
pub struct RemoteConfig {
    /// Base URL of the auth service (`/login`, `/refresh`, `/logout` hang off it).
    pub auth_endpoint: String,
    /// Base URL of the gRPC sync feed (`capsule.sync.v1.SyncService`).
    pub sync_endpoint: String,
    /// Base URL of the upload surface — `POST` creates a session there, and
    /// `PATCH`/`HEAD`/`DELETE` hang under `{upload_endpoint}/{id}`.
    pub upload_endpoint: String,
    /// Base URL of the album surface — `POST` provisions the caller's derived album id
    /// there, and the lifecycle-write surface hangs under
    /// `{albums_endpoint}/{album_id}/ops`.
    pub albums_endpoint: String,
    /// The client's max-known protocol version (`YYYY-MM-DD`), sent on the
    /// handshake and used as the forward-version ceiling.
    ///
    /// [`RemoteConfig::from_env`] sets it to `capsule-core`'s
    /// [`PROTOCOL_VERSION`](capsule_core::crypto::primitives::PROTOCOL_VERSION) and nothing
    /// else: a binary sends one protocol version on every request (system invariant 8), and
    /// the SDK's transport already sends that one as a default header. Only a test pins another
    /// value, to play a client outside the server's window.
    pub protocol_version: String,
}

/// The default server origin — one host, one port, matching `mise run serve-memory`.
pub const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:3000";

impl RemoteConfig {
    /// Build from `CAPSULE_ENDPOINT` (one origin for the whole server), with
    /// `CAPSULE_AUTH_ENDPOINT` / `CAPSULE_SYNC_ENDPOINT` / `CAPSULE_UPLOAD_ENDPOINT` /
    /// `CAPSULE_ALBUMS_ENDPOINT` as per-surface overrides for split deployments.
    ///
    /// The protocol version is not configurable: it is the build's (system invariant 8), so
    /// the upload path can never send a different date from every other request this binary
    /// makes.
    ///
    /// The previous defaults pointed at `127.0.0.1:8080` and `:8081` — two ports that only
    /// ever existed in the integration test, which spins the auth and sync routers on
    /// separate listeners. A real server serves both from one port, so the defaults could
    /// never reach it and every CLI invocation needed two environment variables set by hand.
    pub fn from_env() -> Self {
        let base = std::env::var("CAPSULE_ENDPOINT")
            .unwrap_or_else(|_| DEFAULT_ENDPOINT.to_string())
            .trim_end_matches('/')
            .to_string();
        Self {
            auth_endpoint: std::env::var("CAPSULE_AUTH_ENDPOINT")
                .unwrap_or_else(|_| Self::auth_endpoint_for(&base)),
            // The gRPC service mounts at the server root: tonic discards any path on the
            // endpoint URI (`AddOrigin` keeps scheme + authority only), so a prefixed sync
            // URL is silently unreachable. Pass the bare origin.
            upload_endpoint: std::env::var("CAPSULE_UPLOAD_ENDPOINT")
                .unwrap_or_else(|_| Self::upload_endpoint_for(&base)),
            albums_endpoint: std::env::var("CAPSULE_ALBUMS_ENDPOINT")
                .unwrap_or_else(|_| Self::albums_endpoint_for(&base)),
            sync_endpoint: std::env::var("CAPSULE_SYNC_ENDPOINT").unwrap_or(base),
            protocol_version: capsule_core::crypto::primitives::PROTOCOL_VERSION.to_owned(),
        }
    }

    /// The auth surface hangs off `/v1/auth` on the shared origin.
    fn auth_endpoint_for(base: &str) -> String {
        format!("{base}/v1/auth")
    }

    /// The upload surface hangs off `/v1/upload` on the shared origin.
    fn upload_endpoint_for(base: &str) -> String {
        format!("{base}/v1/upload")
    }

    /// The album surface hangs off `/v1/albums` on the shared origin — the API root, not
    /// under `/upload`, per the authorization contract.
    fn albums_endpoint_for(base: &str) -> String {
        format!("{base}/v1/albums")
    }
}

/// Everything the networked commands can fail with. The binary maps these to
/// localized output; where an `error.*` catalog code applies it is surfaced via
/// [`RemoteError::error_code`].
#[derive(Debug, Error)]
pub enum RemoteError {
    /// An auth flow (login/refresh/logout) failed.
    #[error(transparent)]
    Auth(#[from] AuthError),
    /// A sync flow (feed pull / reconciliation) failed.
    #[error(transparent)]
    Sync(#[from] SyncError),
    /// The persistent session store failed.
    #[error(transparent)]
    Session(#[from] SessionError),
    /// The local sync store failed.
    #[error(transparent)]
    SyncStore(#[from] SyncStoreError),
    /// Reading an asset's upload bundle out of the local library failed.
    #[error(transparent)]
    Lifecycle(#[from] LifecycleError),
    /// An asset's upload failed.
    #[error(transparent)]
    Push(#[from] PushError),
    /// The server's published attestation keys could not be fetched, so no custody receipt
    /// could be verified and a push could confirm nothing.
    #[error("the server's attestation keys could not be fetched: {0}")]
    AttestationKeys(#[source] VerifyError),
    /// The server's published attestation keys contradict the history this library pinned for
    /// it (a pinned key was dropped or replaced), or the pin could not be read or written. The
    /// run trusts nothing the server signs and the pin is left as it was.
    #[error(transparent)]
    AttestationKeyPin(#[from] KeyPinError),
    /// The server endpoint has no origin to key its attestation-key pin by.
    #[error(
        "the server endpoint {endpoint:?} has no origin to pin attestation keys under: {reason}"
    )]
    ServerOrigin {
        /// The endpoint as configured.
        endpoint: String,
        /// Why no origin could be derived from it.
        reason: String,
    },
    /// A command needing an authenticated session found none stored.
    #[error("not authenticated")]
    NotAuthenticated,
    /// The freshly-logged-in session held no exportable tokens (should not happen).
    #[error("login produced no session tokens")]
    EmptySession,
    /// An escrow or sealed-identity call failed (`capsule auth recover`).
    #[error(transparent)]
    Recovery(#[from] RecoveryError),
    /// Fetching or publishing the account's signed device directory failed
    /// (`capsule auth recover` joining the new device to it), or `capsule auth escrow` found it
    /// signed under an identity key other than the library's (`UntrustedSignature`).
    #[error(transparent)]
    Directory(#[from] DirectoryError),
    /// The account profile could not be read, so the account's server id is unknown.
    #[error("the account profile could not be read: {0}")]
    Profile(String),
    /// The recovery secret did not open this account's escrow.
    #[error("the recovery secret does not open this account's escrow")]
    WrongRecoverySecret,
    /// `capsule auth escrow` without `--replace` found an escrow already stored for the account:
    /// storing another would retire the recovery secret that opens it.
    #[error("this account already has a recovery escrow stored")]
    EscrowAlreadyStored,
    /// The user did not type the minted recovery secret back, so nothing was stored.
    #[error("the recovery secret was not confirmed")]
    RecoverySecretNotConfirmed,
    /// `capsule restore` could not prepare its output directory.
    #[error("the restore output directory could not be prepared: {0}")]
    RestoreOutput(#[source] std::io::Error),
}

impl RemoteError {
    /// The stable `error.*` catalog code a client localizes, when one applies.
    pub fn error_code(&self) -> Option<&str> {
        match self {
            Self::Auth(error) => error.error_code(),
            Self::Sync(error) => error.error_code(),
            Self::Push(error) => error.error_code(),
            Self::AttestationKeys(VerifyError::Status { code, .. }) => code.as_deref(),
            Self::Recovery(error) => error.error_code(),
            Self::Directory(error) => error.error_code(),
            _ => None,
        }
    }
}

/// Authenticate with email + password and persist the session.
///
/// The whole flow is [`AuthClient::login`]; the CLI only exports the resulting
/// [`capsule_sdk::auth::PersistedSession`] to its store. Tokens are never logged.
#[instrument(skip(store, password))]
pub async fn auth_login(
    remote: &RemoteConfig,
    store: &SessionStore,
    email: &str,
    password: &str,
) -> Result<LoginStep, RemoteError> {
    let client = AuthClient::new(&remote.auth_endpoint)?;
    let outcome = client.login(email, password).await?;
    settle_login(store, outcome).await
}

/// Act on what a sign-in answered: persist a session, or hand back the second-factor challenge.
///
/// One place for the password path and the identity-provider path (`capsule auth login
/// --oidc`), so the two ways a sign-in can answer cannot be settled differently.
pub(crate) async fn settle_login(
    store: &SessionStore,
    outcome: LoginOutcome,
) -> Result<LoginStep, RemoteError> {
    match outcome {
        LoginOutcome::Session(session) => {
            persist(store, session).await?;
            tracing::info!("login persisted to the session store");
            Ok(LoginStep::Complete)
        }
        // The sign-in is not finished. Nothing is persisted: there is no session yet,
        // and writing a half-authentication to the store would leave a later command believing
        // it was signed in.
        LoginOutcome::SecondFactorRequired { mfa_token, .. } => {
            tracing::info!("login needs a second factor");
            Ok(LoginStep::SecondFactorRequired { mfa_token })
        }
    }
}

/// How far [`auth_login`] got.
///
/// The CLI is interactive, so it can finish the ceremony by asking for a code — which is why
/// this is a value the caller acts on rather than an error it reports.
#[derive(Debug)]
pub enum LoginStep {
    /// Signed in, and the session is in the store.
    Complete,
    /// The password verified; a code is needed.
    SecondFactorRequired {
        /// The challenge to hand to [`auth_verify_totp`]. Good once, and for five minutes.
        mfa_token: SecretString,
    },
}

/// Complete a sign-in with the code an authenticator app is showing, and persist the session.
#[instrument(skip(store, mfa_token, totp_code))]
pub async fn auth_verify_totp(
    remote: &RemoteConfig,
    store: &SessionStore,
    mfa_token: &SecretString,
    totp_code: &str,
) -> Result<(), RemoteError> {
    let client = AuthClient::new(&remote.auth_endpoint)?;
    let session = client.verify_second_factor(mfa_token, totp_code).await?;
    persist(store, session).await?;
    tracing::info!("second factor accepted; session persisted");
    Ok(())
}

/// Export a freshly opened session and write it to the store.
///
/// One place, so the two ways a sign-in can finish cannot persist differently.
async fn persist(store: &SessionStore, session: Session) -> Result<(), RemoteError> {
    let persisted = session.export().await.ok_or(RemoteError::EmptySession)?;
    store.save(&persisted)?;
    Ok(())
}

/// Create an account and persist the resulting session, exactly as [`auth_login`] does.
///
/// Account creation previously had no CLI surface at all: the only documented route was a
/// hand-written `curl` against `/v1/auth/register`, which put a raw HTTP call in the
/// getting-started path of a client whose whole contract is that it never hand-rolls a
/// network flow. This goes through the SDK's `register` like every other command.
#[instrument(skip(store, password), fields(email = %email))]
pub async fn auth_register(
    remote: &RemoteConfig,
    store: &SessionStore,
    email: &str,
    password: &str,
) -> Result<(), RemoteError> {
    let client = AuthClient::new(&remote.auth_endpoint)?;
    let session = client.register(email, password).await?;
    let persisted = session.export().await.ok_or(RemoteError::EmptySession)?;
    store.save(&persisted)?;
    tracing::info!("registration persisted to the session store");
    Ok(())
}

/// Revoke the session server-side and clear the local store.
///
/// Returns `false` when there was no session to log out of. The local store is
/// cleared even if the server-side revoke fails on the wire — a local logout always
/// succeeds — and the revoke error is then propagated so the caller can warn.
#[instrument(skip(store))]
pub async fn auth_logout(remote: &RemoteConfig, store: &SessionStore) -> Result<bool, RemoteError> {
    let Some(persisted) = store.load()? else {
        return Ok(false);
    };
    let client = AuthClient::new(&remote.auth_endpoint)?;
    let session = client.resume(persisted)?;
    let revoke = session.logout().await;
    store.clear()?;
    revoke?;
    tracing::info!("logout revoked server-side and cleared the session store");
    Ok(true)
}

/// Resume the stored session, and hand back a guard that persists whatever it rotated to.
///
/// # The bug this exists to close
///
/// The server rotates on refresh: `POST /v1/auth/refresh` mints a new pair and **closes the old
/// session**, so the refresh token in the store is dead the moment the SDK uses it. Only
/// `auth_login` and `auth_register` ever wrote the store, so every later command — `sync`,
/// `push`, `list` — refreshed, worked, and threw the rotated pair away on exit. Roughly fifteen
/// minutes after signing in, every command demanded an interactive login while the refresh token
/// it had been handed still had seven days left.
///
/// It is fixed here rather than inside the SDK because the store is the CLI's: the SDK's
/// `Session` is deliberately in-memory and says so, and a library that wrote to a caller's disk
/// would be the surprising thing.
pub(crate) async fn resume_session(
    remote: &RemoteConfig,
    store: &SessionStore,
) -> Result<Session, RemoteError> {
    let persisted = store.load()?.ok_or(RemoteError::NotAuthenticated)?;
    let client = AuthClient::new(&remote.auth_endpoint)?;
    Ok(client.resume(persisted)?)
}

/// Write back whatever the session now holds.
///
/// Called on the way out of **every** command that used one, on the success path and the error
/// path alike: a refresh that landed before a later failure still rotated the token, and
/// discarding it there would leave the store holding a pair the server has already closed —
/// which is the same bug, reached by a different route.
///
/// A session that has been logged out exports nothing; that is not an error, it is the store
/// having nothing left to hold.
pub(crate) async fn checkpoint(store: &SessionStore, session: &Session) {
    let Some(persisted) = session.export().await else {
        return;
    };
    match store.save(&persisted) {
        Ok(()) => tracing::debug!("the rotated session was written back to the store"),
        // Deliberately not fatal to the command that just succeeded. The work is done; the
        // cost of a failed write-back is one interactive login, and turning it into a command
        // failure would throw away work that landed.
        Err(error) => tracing::warn!(%error, "the rotated session could not be persisted"),
    }
}

// ─── Recovery (#544) ─────────────────────────────────────────────────────────

/// `capsule auth recover`: bootstrap a library on this machine **as the signed-in account**,
/// from what the server holds and the recovery secret alone.
///
/// The flow, every leg over the SDK:
///
/// 1. resume the stored session and read the account's server id (`GET /v1/auth/profile`) —
///    the id the new library must carry, because the server refuses a directory or a manifest
///    that names any other;
/// 2. fetch the escrow, the sealed identity and the album key escrows (`GET /v1/auth/escrow`,
///    `GET /v1/auth/escrow/identity`, `GET /v1/auth/escrow/albums`) — all before any key
///    derivation, so an account that never stored its identity fails at once rather than after
///    an Argon2id pass;
/// 3. unwrap the master key with the recovery secret ([`recovery_secret_candidates`] says how
///    the typed secret is read);
/// 4. create the library at `library` with `Workspace::create_from_master`, which opens the
///    sealed identity under the recovered master key and refuses before writing anything if it
///    does not open;
/// 5. adopt every escrowed album with `Workspace::adopt_album_key_escrows` (#741), so the
///    library holds the account's albums — every epoch, as the device that escrowed them held
///    them — before any later command resolves its default album with `ensure_album`, which
///    would otherwise mint a fresh key under an album id the account already wrote under;
/// 6. join the account's device directory (#625): fetch the
///    account's signed directory (`GET /v1/auth/devices/directory/{user_id}`), verify it under
///    the identity key the sealed identity opened, append the new device with
///    `Workspace::extend_account_directory` and publish it at the stored version + 1. An
///    account that never published one gets the new library's own, at version 1.
///
/// Steps 5 and 6 are part of the recovery, not a follow-up: a library missing an album would
/// mint over it, and a device the directory does not list signs nothing the account's other
/// devices accept. So a failure in either **removes the library step 4 created** — it holds
/// nothing but the keys minted for it moments earlier — and returns the error, which leaves the
/// path as it was and makes rerunning the command the retry. Every refusal before step 4 writes
/// nothing.
///
/// # Errors
///
/// [`RemoteError::NotAuthenticated`] with no stored session; [`RemoteError::Recovery`] for an
/// escrow, sealed-identity or album-key refusal (`IdentityNotStored` carries its own code);
/// [`RemoteError::WrongRecoverySecret`] when no reading of the secret opens the escrow;
/// [`RemoteError::Lifecycle`] when the library cannot be created, an album key escrow opens
/// under the recovered master key but is inconsistent (one sealed under another master key is
/// skipped, not refused), or the account's directory cannot be extended with the new device; [`RemoteError::Directory`] when the account's directory cannot be fetched, does
/// not verify, or the publish is refused.
#[instrument(skip(store, recovery_secret, passphrase, params), fields(library = %library.display()))]
pub async fn auth_recover(
    remote: &RemoteConfig,
    store: &SessionStore,
    library: &std::path::Path,
    recovery_secret: &str,
    passphrase: &[u8],
    params: Argon2Params,
) -> Result<Workspace, RemoteError> {
    let session = resume_session(remote, store).await?;
    let outcome = recover_with(
        remote,
        session.clone(),
        library,
        recovery_secret,
        passphrase,
        params,
    )
    .await;
    checkpoint(store, &session).await;
    outcome
}

/// The recovery itself, over a session somebody else is responsible for persisting.
async fn recover_with(
    remote: &RemoteConfig,
    session: Session,
    library: &std::path::Path,
    recovery_secret: &str,
    passphrase: &[u8],
    params: Argon2Params,
) -> Result<Workspace, RemoteError> {
    // `sync_endpoint` is the bare server origin, the base every generated operation hangs off.
    let api_root = &remote.sync_endpoint;
    let user_id = account_id(api_root, session.clone()).await?;
    let recovery = RecoveryClient::new(session.clone(), api_root)?;
    let escrow = recovery.fetch_escrow().await?;
    let sealed = recovery.fetch_sealed_identity().await?;
    let album_keys: Vec<(Uuid, Vec<u8>)> = recovery
        .list_album_keys()
        .await?
        .into_iter()
        .map(|escrowed| (escrowed.album_id, escrowed.blob))
        .collect();
    tracing::info!(
        %user_id,
        albums = album_keys.len(),
        "recover: fetched the escrow, the sealed identity and the album key escrows"
    );

    let master = recovery_secret_candidates(recovery_secret)
        .into_iter()
        .find_map(|candidate| {
            capsule_core::backup::recover_master_key(escrow.blob(), &candidate).ok()
        })
        .ok_or_else(|| {
            tracing::warn!("recover: the recovery secret did not open the escrow");
            RemoteError::WrongRecoverySecret
        })?;

    // `create_from_master` refuses a non-empty root, so whatever is under `library` after it
    // returns is this run's alone; `existed` says whether the root itself is.
    let existed = library.exists();
    let mut workspace = Workspace::create_from_master(
        library,
        passphrase,
        params,
        MasterKey::from_bytes(master),
        user_id,
        &sealed,
    )?;
    tracing::info!(
        %user_id,
        device = %workspace.device_id(),
        "recover: the library opens as the recovered account on a new device"
    );

    let directory = DirectoryClient::new(session, &remote.auth_endpoint);
    let joined = match workspace.adopt_album_key_escrows(&album_keys) {
        Ok(adopted) => {
            tracing::info!(
                %user_id,
                albums = adopted.adopted,
                epochs = adopted.epochs,
                unopenable = adopted.unopenable,
                "recover: the account's albums are held before anything resolves one"
            );
            join_account_directory(&directory, &mut workspace).await
        }
        Err(error) => Err(error.into()),
    };
    match joined {
        Ok(version) => {
            tracing::info!(
                %user_id,
                device = %workspace.device_id(),
                directory_version = version,
                "recover: the new device is listed in the account's directory"
            );
            Ok(workspace)
        }
        Err(error) => {
            tracing::warn!(
                %user_id,
                device = %workspace.device_id(),
                %error,
                "recover: the new library could not adopt the account's albums or join its \
                 directory; removing the library"
            );
            // Close the library's files before removing them.
            drop(workspace);
            discard_new_library(library, existed);
            Err(error)
        }
    }
}

/// Make `workspace`'s device a member of its account's published device directory, returning
/// the `directory_version` the server stores afterwards.
///
/// The account's directory is fetched and verified under the identity key `workspace` holds —
/// a document that does not verify is refused by the SDK and never reaches the extend. With one
/// published, the device is appended with `Workspace::extend_account_directory` and the result
/// is published at the stored version + 1, keeping every device already listed. A directory
/// that already lists the device under its key is adopted unchanged and nothing is published.
/// With none published ([`DirectoryError::NotPublished`]), the workspace's own directory —
/// this device alone, at version 1 — is the account's first.
async fn join_account_directory(
    directory: &DirectoryClient,
    workspace: &mut Workspace,
) -> Result<u64, RemoteError> {
    let identity = workspace.user_ik_public();
    let user_id = workspace.user_id();
    let to_publish = match directory.fetch(user_id, &identity).await {
        Ok(published) => {
            let stored = published.core.directory_version;
            tracing::debug!(
                %user_id,
                directory_version = stored,
                devices = published.core.devices.len(),
                "recover: the account's directory fetched and verified"
            );
            let Some(extended) = workspace.extend_account_directory(&published)? else {
                tracing::info!(
                    %user_id,
                    directory_version = stored,
                    "recover: the account's directory already lists this device"
                );
                return Ok(stored);
            };
            extended
        }
        Err(DirectoryError::NotPublished) => {
            tracing::info!(
                %user_id,
                "recover: the account has no directory yet; publishing this device's own"
            );
            workspace.device_directory().clone()
        }
        Err(error) => return Err(error.into()),
    };
    Ok(directory.publish(&to_publish, &identity).await?)
}

/// Remove the library a failed recovery created at `root`, restoring the path to how it was:
/// absent when `existed` is false, an empty directory when it is true (the only kind
/// `create_from_master` accepts).
///
/// Best effort: the recovery has already failed, and its error is the one the caller reports.
/// A removal that fails is logged with the path, so whatever it left can be found and removed.
fn discard_new_library(root: &std::path::Path, existed: bool) {
    let outcome = if existed {
        std::fs::read_dir(root).and_then(|entries| {
            entries.into_iter().try_for_each(|entry| {
                let path = entry?.path();
                if path.is_dir() {
                    std::fs::remove_dir_all(&path)
                } else {
                    std::fs::remove_file(&path)
                }
            })
        })
    } else {
        std::fs::remove_dir_all(root)
    };
    match outcome {
        Ok(()) => tracing::info!(root = %root.display(), "recover: the new library was removed"),
        Err(error) => tracing::warn!(
            root = %root.display(),
            %error,
            "recover: the new library could not be fully removed"
        ),
    }
}

/// What [`escrow_album_keys`] left on the server.
#[derive(Debug, Default)]
struct AlbumKeyEscrows {
    /// Albums whose escrow was stored this run.
    stored: usize,
    /// Albums whose epochs the server's escrow does not cover: it disagrees with this device,
    /// does not open under its master key, or the account's escrows are full. Their assets must
    /// not be sent.
    uncovered: HashSet<Uuid>,
}

/// Bring the server's album key escrows (#741) level with `workspace`: list what the account
/// holds (`GET /v1/auth/escrow/albums`), ask the workspace for its plan
/// (`Workspace::plan_album_key_escrows`), and store each album the plan names
/// (`PUT /v1/auth/escrow/albums/{album_id}`). An escrow is never shrunk: one that holds epochs
/// this device lacks is left alone, and one that disagrees with it is left alone and the album
/// reported uncovered.
///
/// Run before any asset leaves the device, so the server never holds an asset whose album key
/// it does not also hold sealed under the master key — the property that lets
/// `capsule auth recover` open every album the account has pushed.
///
/// # Errors
///
/// [`RemoteError::Recovery`] when the listing or a store is refused (except a full account,
/// which leaves that album uncovered), and [`RemoteError::Lifecycle`] when an album cannot be
/// sealed.
async fn escrow_album_keys(
    recovery: &RecoveryClient,
    workspace: &Workspace,
) -> Result<AlbumKeyEscrows, RemoteError> {
    let held: BTreeMap<Uuid, Vec<u8>> = recovery
        .list_album_keys()
        .await?
        .into_iter()
        .map(|escrowed| (escrowed.album_id, escrowed.blob))
        .collect();
    let plan = workspace.plan_album_key_escrows(&held)?;
    let mut outcome = AlbumKeyEscrows {
        stored: 0,
        uncovered: plan.uncovered.iter().copied().collect(),
    };
    for album_id in &plan.to_store {
        let sealed = workspace.album_key_escrow(album_id)?;
        match recovery.store_album_keys(*album_id, &sealed).await {
            Ok(stored) => {
                outcome.stored += 1;
                tracing::info!(
                    %album_id,
                    replaced = stored.replaced,
                    "album keys: the album's key escrow is current on the server"
                );
            }
            Err(error @ RecoveryError::AlbumKeysFull { .. }) => {
                tracing::warn!(%album_id, %error, "album keys: the account's escrows are full");
                outcome.uncovered.insert(*album_id);
            }
            Err(error) => return Err(error.into()),
        }
    }
    tracing::info!(
        escrowed = held.len(),
        stored = outcome.stored,
        current = plan.current,
        uncovered = outcome.uncovered.len(),
        "album keys: the server's album key escrows are brought level"
    );
    Ok(outcome)
}

/// The signed-in account's id, as the server assigned it.
async fn account_id(api_root: &str, session: Session) -> Result<Uuid, RemoteError> {
    let client = AuthenticatedClient::new(api_root, session)
        .map_err(|error| RemoteError::Profile(error.to_string()))?;
    let profile = client
        .get_profile(capsule_core::crypto::primitives::PROTOCOL_VERSION, None)
        .await
        .map_err(|error| RemoteError::Profile(error.to_string()))?
        .into_inner();
    Uuid::parse_str(&profile.user_id).map_err(|error| {
        RemoteError::Profile(format!(
            "the account id {:?} is not a UUID: {error}",
            profile.user_id
        ))
    })
}

/// The byte strings a typed recovery secret may stand for, in the order they are tried.
///
/// A secret the SDK minted (`MintedSecret`) is 32 random bytes shown to the user as 64
/// lowercase hex characters, and the escrow is wrapped under the **bytes**, not the hex. A
/// secret the user chose is wrapped under its UTF-8 text. So a 64-character hex string is tried
/// decoded first and then as typed, and anything else only as typed. Trying both costs one
/// extra Argon2id pass in the rare case a chosen passphrase is itself 64 hex characters, and
/// guessing one reading would lock that user out.
///
/// Surrounding whitespace is dropped: it is never part of a secret a person copied.
#[must_use]
pub fn recovery_secret_candidates(typed: &str) -> Vec<Vec<u8>> {
    let typed = typed.trim();
    let mut candidates = Vec::with_capacity(2);
    if typed.len() == 64
        && let Ok(bytes) = hex::decode(typed)
    {
        candidates.push(bytes);
    }
    candidates.push(typed.as_bytes().to_vec());
    candidates
}

/// What `capsule auth escrow` stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EscrowSetup {
    /// The server's id for the account, which the library is now bound to.
    pub user_id: Uuid,
    /// Whether the escrow displaced an earlier one, so the previous recovery secret has
    /// stopped working and backups made under it need re-exporting or destroying.
    pub replaced: bool,
}

/// `capsule auth escrow`: set up recovery for `workspace` **as the signed-in account**, so
/// [`auth_recover`] can later restore it on a new machine from the recovery secret alone.
///
/// The flow, every network leg over the SDK:
///
/// 1. resume the stored session and read the account's server id (`GET /v1/auth/profile`),
///    then refuse with [`LifecycleError::AccountRebind`] when the library's id differs from it
///    and the library already holds assets, so no secret is minted that the bind would discard;
///    and refuse with [`DirectoryError::UntrustedSignature`] when the account's published
///    device directory does not verify under the library's identity key — another library of
///    the account anchored its own, so a recovery from this escrow could never join it;
/// 2. unless `replace`, refuse with [`RemoteError::EscrowAlreadyStored`] when the account
///    already has an escrow (`GET /v1/auth/escrow`): storing another would retire the secret
///    that opens it, and that may belong to another library of the same account;
/// 3. mint a recovery secret and hand it to `confirm`, which shows it once and runs the
///    type-back gate; `false` is [`RemoteError::RecoverySecretNotConfirmed`];
/// 4. bind the library to the server's id (`Workspace::bind_account_id`), which keeps its own
///    [`LifecycleError::AccountRebind`] refusal as the authority behind step 1's early one;
/// 5. wrap the master key under the secret at `tier` and store it (`PUT /v1/auth/escrow`),
///    then store the sealed identity beside it (`PUT /v1/auth/escrow/identity`) and each
///    album's key escrow the server does not hold current (`PUT /v1/auth/escrow/albums/{id}`,
///    #741), so a recovery from this escrow opens the library's albums.
///
/// Every refusal before step 4 writes nothing, locally or on the server. A failure after it
/// leaves the library bound, which a rerun treats as already done.
///
/// # Errors
///
/// [`RemoteError::NotAuthenticated`] with no stored session; [`RemoteError::Profile`] when the
/// account id cannot be read; the refusals above; [`RemoteError::Directory`] when the account's
/// directory does not verify under the library's identity key (code
/// `error.directory.untrusted_signature`) or cannot be fetched; [`RemoteError::Lifecycle`] for
/// the bind or the wrap; [`RemoteError::Recovery`] for an escrow or sealed-identity call.
#[instrument(skip_all, fields(library_user = %workspace.user_id(), replace = replace))]
pub async fn auth_escrow(
    remote: &RemoteConfig,
    store: &SessionStore,
    workspace: &mut Workspace,
    replace: bool,
    tier: DeviceTier,
    confirm: impl FnOnce(&MintedSecret) -> bool,
) -> Result<EscrowSetup, RemoteError> {
    let session = resume_session(remote, store).await?;
    let outcome = escrow_with(remote, session.clone(), workspace, replace, tier, confirm).await;
    checkpoint(store, &session).await;
    outcome
}

/// The escrow setup itself, over a session somebody else is responsible for persisting.
async fn escrow_with(
    remote: &RemoteConfig,
    session: Session,
    workspace: &mut Workspace,
    replace: bool,
    tier: DeviceTier,
    confirm: impl FnOnce(&MintedSecret) -> bool,
) -> Result<EscrowSetup, RemoteError> {
    let api_root = &remote.sync_endpoint;
    let user_id = account_id(api_root, session.clone()).await?;
    // The bind in step 4 refuses a library that already authored assets under its own id. Ask
    // the same question here, before a secret is minted, so the user is never shown and made to
    // record a secret that is then discarded.
    let assets = workspace.asset_ids().len();
    if workspace.user_id() != user_id && assets > 0 {
        tracing::warn!(
            %user_id,
            library_user = %workspace.user_id(),
            assets,
            "escrow: refused before minting, the library already authored assets under its own id"
        );
        return Err(LifecycleError::AccountRebind(format!(
            "{assets} asset(s) already name {} as created_by_user",
            workspace.user_id()
        ))
        .into());
    }
    let directory = DirectoryClient::new(session.clone(), &remote.auth_endpoint);
    ensure_identity_anchors_account(&directory, user_id, workspace).await?;
    let recovery = RecoveryClient::new(session, api_root)?;
    if !replace {
        match recovery.fetch_escrow().await {
            Ok(_) => {
                tracing::warn!(%user_id, "escrow: refused, an escrow is already stored");
                return Err(RemoteError::EscrowAlreadyStored);
            }
            Err(RecoveryError::NotEnrolled) => {
                tracing::debug!(%user_id, "escrow: no escrow stored yet");
            }
            Err(error) => return Err(error.into()),
        }
    }

    let secret = MintedSecret::generate();
    if !confirm(&secret) {
        tracing::warn!(%user_id, "escrow: the recovery secret was not typed back; nothing stored");
        return Err(RemoteError::RecoverySecretNotConfirmed);
    }

    workspace.bind_account_id(user_id)?;
    tracing::info!(%user_id, "escrow: the library is bound to the server's account id");

    let blob = workspace.escrow_master_key(secret.as_bytes(), tier)?;
    let stored = recovery.store_escrow(&blob).await?;
    let identity = recovery
        .store_sealed_identity(&workspace.sealed_identity())
        .await?;
    let albums = escrow_album_keys(&recovery, workspace).await?;
    tracing::info!(
        %user_id,
        escrow_replaced = stored.replaced,
        identity_replaced = identity.replaced,
        albums = albums.stored,
        uncovered = albums.uncovered.len(),
        "escrow: the escrow, the sealed identity and the album keys are stored"
    );
    Ok(EscrowSetup {
        user_id,
        replaced: stored.replaced,
    })
}

/// Refuse to escrow a library whose identity key is not the one account `user_id`'s published
/// device directory is signed under.
///
/// The directory is fetched under `workspace`'s identity key: a document that verifies, or none
/// published at all ([`DirectoryError::NotPublished`], so the first recovery publishes the
/// account's first directory), passes. A document that does not verify
/// ([`DirectoryError::UntrustedSignature`]) means another library of the account anchored its
/// own key: a recovery from this library's escrow could never join that directory (#625), so
/// the escrow is refused before a secret is minted. Any other failure is returned as is, since
/// the question cannot be answered without the directory.
async fn ensure_identity_anchors_account(
    directory: &DirectoryClient,
    user_id: Uuid,
    workspace: &Workspace,
) -> Result<(), RemoteError> {
    match directory.fetch(user_id, &workspace.user_ik_public()).await {
        Ok(published) => {
            tracing::debug!(
                %user_id,
                directory_version = published.core.directory_version,
                "escrow: the account's directory verifies under this library's identity key"
            );
            Ok(())
        }
        Err(DirectoryError::NotPublished) => {
            tracing::debug!(%user_id, "escrow: the account has no directory published yet");
            Ok(())
        }
        Err(DirectoryError::UntrustedSignature) => {
            tracing::warn!(
                %user_id,
                library_user = %workspace.user_id(),
                "escrow: refused before minting, the account's directory is signed under another \
                 library's identity key"
            );
            Err(DirectoryError::UntrustedSignature.into())
        }
        Err(error) => {
            tracing::warn!(
                %user_id,
                %error,
                "escrow: refused, the account's directory could not be fetched"
            );
            Err(error.into())
        }
    }
}

/// The outcome of a `capsule sync` run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SyncSummary {
    /// Total feed entries applied across all pages.
    pub applied: usize,
    /// Number of distinct albums touched this run.
    pub albums: usize,
    /// Number of feed pages drained.
    pub pages: usize,
    /// Whether this was a dry run (nothing persisted).
    pub dry_run: bool,
    /// Whether the server had moved the library to a new sync epoch, so the drain started over
    /// from zero (#697): what it no longer lists is for `capsule sync reannounce` to give back.
    pub resynced: bool,
}

/// Drain the sync feed into the local store.
///
/// Auth rides the resumed [`Session`] (pre-flight
/// refresh + one refresh-and-retry on `Unauthenticated`); the feed and the
/// anti-rewind contract ride [`SyncConsumer::pull_into`] over a [`SyncState`]
/// rehydrated from the local store. Each validated page is persisted in one
/// transaction (unless `dry_run`), and the cursor + high-water marks survive to the
/// next run.
///
/// `from_start` discards the persisted cursor and re-drains the feed from the beginning —
/// the recovery move when the local store has drifted or been partially lost. It does **not**
/// weaken the anti-rewind contract: the per-album high-water marks are still rehydrated from
/// the store, so a replayed entry older than what was already applied is still refused.
#[instrument(skip(store, db))]
pub async fn sync<C: ConnectionTrait + TransactionTrait>(
    remote: &RemoteConfig,
    store: &SessionStore,
    db: &C,
    page_size: u32,
    dry_run: bool,
    from_start: bool,
) -> Result<SyncSummary, RemoteError> {
    let session = resume_session(remote, store).await?;
    let outcome = sync_with(remote, db, session.clone(), page_size, dry_run, from_start).await;
    // Before returning, and on the failing path too: the refresh that rotated the pair happened
    // whether or not the work after it succeeded.
    checkpoint(store, &session).await;
    outcome
}

/// The drain itself, over a session somebody else is responsible for persisting.
async fn sync_with<C: ConnectionTrait + TransactionTrait>(
    remote: &RemoteConfig,
    db: &C,
    session: Session,
    page_size: u32,
    dry_run: bool,
    from_start: bool,
) -> Result<SyncSummary, RemoteError> {
    // One endpoint now: the feed is `GET /v1/sync` on the same REST base URL every other call
    // uses, so there is no second connection to dial and no second transport to configure.
    let consumer = SyncConsumer::with_session(&remote.sync_endpoint, session)?;

    let mut state = syncstore::load_sync_state(db, &remote.protocol_version).await?;
    if from_start {
        tracing::info!("sync --force: re-draining the feed from the start of the cursor");
        // Rebuild with a start cursor but the SAME high-water marks: the cursor is only a
        // resume point, whereas the marks are the anti-rewind floor. Dropping them too would
        // let a replayed stale entry overwrite a newer one.
        let high_water: Vec<(Vec<u8>, u64)> = state
            .high_water_marks()
            .map(|(album, seq)| (album.to_vec(), seq))
            .collect();
        state = SyncState::restore(&remote.protocol_version, SyncCursor::start(), high_water);
    }

    let mut summary = SyncSummary {
        dry_run,
        ..Default::default()
    };
    let mut albums = std::collections::HashSet::new();
    let mut resynced = false;
    // The latest server clock reading among the landed entries, which floors the device clock
    // the retention sweep judges deadlines at.
    let mut server_clock = None;

    loop {
        // `pull_into` validates the page against the in-memory `SyncState`
        // (forward-version + anti-rewind) and advances its cursor before returning.
        let page = match consumer.pull_into(&mut state, page_size).await {
            Ok(page) => page,
            // The server rebuilt this library's index and moved it to a new sync epoch (#697):
            // the cursor and the anti-rewind floor go, nothing held goes, and the drain starts
            // over. Once: a second stale answer to an empty cursor is a server fault, not a
            // reason to loop.
            Err(error) if error.is_stale_cursor() && !resynced => {
                tracing::info!(
                    dry_run,
                    "the server moved this library to a new sync epoch; resyncing from the start"
                );
                if !dry_run {
                    syncstore::reset_for_resync(db).await?;
                }
                state.reset_for_resync();
                resynced = true;
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        if page.entries.is_empty() {
            break;
        }
        for entry in &page.entries {
            albums.insert(entry.album_id.clone());
        }
        if dry_run {
            summary.applied += page.entries.len();
        } else {
            summary.applied += syncstore::persist_page(db, &page).await?;
            server_clock = server_clock.max(syncstore::latest_changed_at(&page));
        }
        summary.pages += 1;
    }
    if !dry_run {
        // Only here, at the feed's head: a delete landed on one page may be restored on a later
        // one, and sweeping before that page lands would tombstone a restored asset for good. A
        // run that landed no page still owes the sweep, since a window closes on the clock.
        let at = syncstore::judged_at(jiff::Timestamp::now(), server_clock);
        syncstore::record_elapsed_purges(db, at).await?;
    }

    summary.albums = albums.len();
    summary.resynced = resynced;
    tracing::info!(
        applied = summary.applied,
        albums = summary.albums,
        pages = summary.pages,
        dry_run,
        "sync drained the feed"
    );
    Ok(summary)
}

// ─── Push ────────────────────────────────────────────────────────────

/// How a `capsule push` run is configured.
#[derive(Debug, Clone, Copy)]
pub struct PushOptions {
    /// Plan the run and report it without opening a single upload session.
    pub dry_run: bool,
    /// Ignore server truth and re-drive every blob. The server still answers
    /// `duplicate_blob` for what it holds, which resolves as a merge — so this costs
    /// requests, never correctness.
    pub force: bool,
    /// `Full` opens every tier eagerly; `Staged` gates the above-index tiers on the
    /// connection class.
    pub policy: UploadPolicy,
    /// The detected connection class the staged tier gate consults.
    pub connection: ConnectionClass,
}

impl Default for PushOptions {
    fn default() -> Self {
        Self {
            dry_run: false,
            force: false,
            policy: UploadPolicy::Full,
            connection: ConnectionClass::Unmetered,
        }
    }
}

/// Where one asset ended up after a `capsule push` run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetPushStatus {
    /// Dry run: the work was planned and nothing left the device.
    Planned,
    /// The server reported every blob durable and a verified custody receipt for each is
    /// persisted in the library.
    Verified,
    /// The asset did not reach verified custody; `error` says where it stopped.
    Failed,
    /// Skipped: the asset is handed to another device of the account and its delegation is
    /// still inside its deadline ([`PushRun::delegated`]), so that device uploads it. Not a
    /// failure; a re-queued delegation is pushed like any other asset.
    Delegated,
}

/// Why an asset failed, in the shape `--json` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AssetPushFailure {
    /// A stable name for where the asset stopped: `bundle`, `album`, `upload`, `create_refused`,
    /// `reseal`, `unknown_blob`, `verify_unavailable`, `not_durable`, `receipt_missing`,
    /// `receipt_rejected`, `receipt_store` or `album_keys_not_escrowed`.
    pub kind: &'static str,
    /// The server's stable `error.*` code, when it supplied one.
    pub code: Option<String>,
    /// The English detail.
    pub message: String,
}

/// What a `capsule push` run did for one asset — one line of the `--json` report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AssetPushOutcome {
    /// The asset id.
    pub asset_id: String,
    /// Where the asset ended up.
    pub status: AssetPushStatus,
    /// Blobs transferred to completion this run (planned, on a dry run).
    pub uploaded: usize,
    /// Blobs the server already held and merged onto (`duplicate_blob`).
    pub merged: usize,
    /// Blobs skipped before any request because the feed says the server holds them.
    pub already_held: usize,
    /// Blobs a staged policy deferred to a later window.
    pub deferred: usize,
    /// Bytes handed to the upload client this run (planned, on a dry run).
    pub bytes: u64,
    /// Verified custody receipts persisted for the asset this run.
    pub receipts: usize,
    /// Whether the server refused the asset's create because the album's epoch had moved past
    /// it, and the run re-sealed it at the album's current epoch before pushing it again
    /// (issue #822).
    pub resealed: bool,
    /// Why the asset failed, for a [`AssetPushStatus::Failed`] asset only.
    pub error: Option<AssetPushFailure>,
}

impl AssetPushOutcome {
    fn new(asset_id: Uuid, status: AssetPushStatus) -> Self {
        Self {
            asset_id: asset_id.to_string(),
            status,
            uploaded: 0,
            merged: 0,
            already_held: 0,
            deferred: 0,
            bytes: 0,
            receipts: 0,
            resealed: false,
            error: None,
        }
    }
}

/// Why one asset failed inside a run that carries on past it.
#[derive(Debug, Error)]
enum AssetError {
    /// The library could not build the asset's upload bundle.
    #[error(transparent)]
    Bundle(#[from] LifecycleError),
    /// The transfer or the custody check failed.
    #[error(transparent)]
    Push(#[from] PushError),
    /// A verified receipt could not be written to the library.
    #[error("the custody receipt could not be stored in the library: {0}")]
    ReceiptStore(#[from] capsule_library::library::ReceiptStoreError),
    /// The server refused the asset's create and the library could not re-seal it at the
    /// album's current epoch (issue #822). When this device holds no newer epoch than the one
    /// the asset is sealed under, nothing says the epoch was why the server refused it (a
    /// revoked device or withdrawn write access is refused the same way), so that case keeps
    /// the `upload` kind any other refusal has.
    #[error("the server refused this create ({refused}); {error}")]
    Reseal {
        /// The server's refusal.
        refused: UploadError,
        /// Why the re-seal did not happen.
        error: LifecycleError,
    },
    /// The asset's album keys are not covered by the server's escrow, so the asset is held back:
    /// sent, it would be one a recovered device could never open.
    #[error(
        "album {0}'s keys are not escrowed on the server (its escrow disagrees with this \
         library's, or the account's escrows are full); the asset was not sent"
    )]
    KeysNotEscrowed(Uuid),
}

impl AssetError {
    fn failure(&self) -> AssetPushFailure {
        let (kind, code) = match self {
            Self::Bundle(_) => ("bundle", None),
            Self::Push(error) => (error.kind(), error.error_code().map(str::to_owned)),
            Self::ReceiptStore(_) => ("receipt_store", None),
            Self::Reseal { refused, error } => {
                let kind = match error {
                    LifecycleError::ResealRefused {
                        reason: ResealRefusal::NoNewerEpoch { .. },
                        ..
                    } => "upload",
                    _ => "reseal",
                };
                let code = match refused {
                    UploadError::Rejected { code, .. } => code.clone(),
                    _ => None,
                };
                (kind, code)
            }
            Self::KeysNotEscrowed(_) => ("album_keys_not_escrowed", None),
        };
        AssetPushFailure {
            kind,
            code,
            message: self.to_string(),
        }
    }
}

/// The outcome of a `capsule push` run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PushSummary {
    /// Managed assets considered.
    pub assets: usize,
    /// Assets for which at least one blob session was opened.
    pub pushed_assets: usize,
    /// Assets that reached verified custody this run.
    pub verified_assets: usize,
    /// Assets that did not; each is in `outcomes` with its error.
    pub failed_assets: usize,
    /// Assets skipped because another device of the account holds an open hand-off for them
    /// ([`AssetPushStatus::Delegated`]).
    pub delegated_assets: usize,
    /// Verified custody receipts persisted to the library this run.
    pub receipts: usize,
    /// Blobs transferred to completion.
    pub uploaded_blobs: usize,
    /// Blobs the server already held and merged onto (`duplicate_blob`).
    pub merged_blobs: usize,
    /// Blobs skipped before any request because the feed says the server holds them.
    pub already_held_blobs: usize,
    /// Blobs a staged policy deferred to a later window.
    pub deferred_blobs: usize,
    /// Bytes handed to the upload client this run.
    pub bytes: u64,
    /// Distinct albums registered with the server this run. Provisioning is
    /// idempotent, so this counts requests made, not rows created.
    pub provisioned_albums: usize,
    /// How many of those registrations actually created a server-side album row. Zero on a
    /// re-run — the album was already bound.
    pub created_albums: usize,
    /// Albums whose key escrow (#741) was stored this run because the server's was missing or
    /// stale — a new album, or one rotated since. Zero on a re-run.
    pub album_keys_escrowed: usize,
    /// Whether this was a dry run (nothing left the device).
    pub dry_run: bool,
    /// One entry per asset considered, in library order.
    pub outcomes: Vec<AssetPushOutcome>,
}

impl PushSummary {
    /// Whether the run moved nothing — the re-run-against-an-unchanged-library case.
    #[must_use]
    pub fn is_no_op(&self) -> bool {
        self.uploaded_blobs == 0 && self.merged_blobs == 0
    }

    /// The assets that did not reach verified custody.
    pub fn failures(&self) -> impl Iterator<Item = &AssetPushOutcome> {
        self.outcomes
            .iter()
            .filter(|outcome| outcome.status == AssetPushStatus::Failed)
    }
}

/// The set of blob content addresses the server durably holds, folded off a **fresh** feed
/// pull — the server truth a push resumes from (there is no push-side state file).
///
/// Deliberately drained through a scratch [`SyncState`] rather than the persisted one: reading
/// what the server holds must not advance (or depend on) the cursor `capsule sync` owns.
#[instrument(skip(session))]
pub async fn held_blobs(
    remote: &RemoteConfig,
    session: Session,
    page_size: u32,
) -> Result<HashSet<String>, RemoteError> {
    let entries = drain_feed(remote, session, page_size).await?;
    let mut held = HashSet::new();
    for entry in &entries {
        held.extend(held_from_feed(entry));
    }
    tracing::info!(
        entries = entries.len(),
        blobs = held.len(),
        "push: server truth read off the sync feed"
    );
    Ok(held)
}

/// Every entry of a **fresh** feed drain, in feed order, through a scratch [`SyncState`]: start
/// cursor, no high-water marks, nothing persisted. Reading the server's truth must neither
/// advance nor depend on the cursor `capsule sync` owns.
async fn drain_feed(
    remote: &RemoteConfig,
    session: Session,
    page_size: u32,
) -> Result<Vec<FeedEntry>, RemoteError> {
    let consumer = SyncConsumer::with_session(&remote.sync_endpoint, session)?;
    let mut state = SyncState::restore(&remote.protocol_version, SyncCursor::start(), Vec::new());
    let mut entries = Vec::new();
    loop {
        let page = consumer.pull_into(&mut state, page_size).await?;
        if page.entries.is_empty() {
            break;
        }
        entries.extend(page.entries);
    }
    Ok(entries)
}

// ─── Re-announce (#781) ──────────────────────────────────────────────────────

/// `capsule sync reannounce`: give the server back the signed manifests `workspace` holds and
/// the server's feed no longer lists.
///
/// The feed is drained fresh, as a push reads it, and every live asset it does not list is sent
/// through the SDK's [`reannounce_unlisted`](capsule_sdk::sync::reannounce::reannounce_unlisted):
/// manifests only, never bytes. The server restores what it still holds the blobs for and names
/// what it does not, which `capsule push` uploads again. A dry run drains the feed and sends
/// nothing.
///
/// # Errors
///
/// [`RemoteError::NotAuthenticated`] with no stored session; [`RemoteError::Sync`] when the feed
/// cannot be read or a batch is refused whole; [`RemoteError::Lifecycle`] when a held record
/// does not encode.
#[instrument(skip(store, workspace))]
pub async fn reannounce(
    remote: &RemoteConfig,
    store: &SessionStore,
    workspace: &Workspace,
    dry_run: bool,
) -> Result<crate::reannounce::ReannounceSummary, RemoteError> {
    let session = resume_session(remote, store).await?;
    let outcome = reannounce_with(remote, workspace, session.clone(), dry_run).await;
    checkpoint(store, &session).await;
    outcome
}

/// The re-announce itself, over a session somebody else is responsible for persisting.
async fn reannounce_with(
    remote: &RemoteConfig,
    workspace: &Workspace,
    session: Session,
    dry_run: bool,
) -> Result<crate::reannounce::ReannounceSummary, RemoteError> {
    use capsule_sdk::sync::reannounce::{reannounce_unlisted, unlisted};

    use crate::reannounce::{ReannounceSummary, held_manifests};

    let (held, skipped_deleted) = held_manifests(workspace)?;
    let listed: HashSet<String> = drain_feed(remote, session.clone(), DEFAULT_SYNC_PAGE_SIZE)
        .await?
        .iter()
        .map(|entry| String::from_utf8_lossy(&entry.asset_id).into_owned())
        .collect();
    tracing::info!(
        held = held.len(),
        skipped_deleted,
        listed = listed.len(),
        dry_run,
        "re-announce: the library is compared with the feed"
    );
    let summary = if dry_run {
        ReannounceSummary::planned(
            held.len(),
            skipped_deleted,
            listed.len(),
            &unlisted(&held, &listed),
        )
    } else {
        let consumer = SyncConsumer::with_session(&remote.sync_endpoint, session)?;
        let answers = reannounce_unlisted(&consumer, &held, &listed).await?;
        ReannounceSummary::answered(held.len(), skipped_deleted, listed.len(), answers)
    };
    summary.log();
    Ok(summary)
}

// ─── Restore (#673) ──────────────────────────────────────────────────────────

/// `capsule restore`: download the originals in `options.scope` into `options.out` and verify
/// each byte for byte.
///
/// The feed is drained fresh, as a push reads it, and each live asset's head is restored by
/// [`restore::restore_entries`] over the SDK's
/// [`HttpBlobSource`] at `GET /v1/blob/{hash}`: a resumable ranged fetch, the library's
/// `verify_asset` chokepoint, decryption under the library's album keys, a check of the
/// plaintext against the signed sidecar's digest, and an atomic write.
///
/// What fails the whole run is what every asset needs: the session, the feed read, and the
/// output directory. One asset's failure is recorded in its outcome and the run moves on.
///
/// # Errors
///
/// [`RemoteError::NotAuthenticated`] with no stored session; [`RemoteError::Sync`] when the feed
/// cannot be read; [`RemoteError::RestoreOutput`] when the output directory cannot be prepared.
#[instrument(skip(store, workspace), fields(out = %options.out.display()))]
pub async fn restore(
    remote: &RemoteConfig,
    store: &SessionStore,
    workspace: &Workspace,
    options: &RestoreOptions,
) -> Result<RestoreSummary, RemoteError> {
    let session = resume_session(remote, store).await?;
    let outcome = restore_with(remote, workspace, session.clone(), options).await;
    checkpoint(store, &session).await;
    outcome
}

/// The restore itself, over a session somebody else is responsible for persisting.
async fn restore_with(
    remote: &RemoteConfig,
    workspace: &Workspace,
    session: Session,
    options: &RestoreOptions,
) -> Result<RestoreSummary, RemoteError> {
    let entries = drain_feed(remote, session.clone(), DEFAULT_SYNC_PAGE_SIZE).await?;
    tracing::info!(entries = entries.len(), "restore: the feed is drained");
    // `sync_endpoint` is the bare server origin; the blob route hangs off `/v1`.
    let source = HttpBlobSource::new(
        session,
        format!("{}/v1", remote.sync_endpoint.trim_end_matches('/')),
    );
    restore::restore_entries(workspace, &source, entries, options)
        .await
        .map_err(RemoteError::RestoreOutput)
}

/// Push every managed asset of `workspace` to the server.
///
/// The CLI never hand-rolls a network flow: the album is provisioned through
/// `capsule_sdk::push::ensure_album`, the bytes ride `capsule_sdk::upload`'s resumable client,
/// the ordering rides `capsule_sdk::staged`'s scheduler, and the bundle comes from
/// `capsule_core`'s `Workspace::upload_bundle` — this function only stitches them together.
///
/// **Provisioning comes first**. An asset's album id is derived from the
/// account master key, so the server has never heard of it; invariant 6 refuses an upload
/// into an album that does not exist or is not writable by the caller. Each *distinct* album
/// the run touches is registered once, lazily, before its first blob session opens — and
/// because provisioning is idempotent, a second `capsule push` re-registers the same id, gets
/// the same success, and still moves nothing.
///
/// **A transfer is not custody** (issue #670). After an asset's ladder, the run asks the server
/// whether it durably holds every blob the asset relies on and requires a custody receipt per
/// blob that verifies under the server's attestation-key history as this library pinned it
/// (`capsule_sdk::push::verify_custody`); the verified receipts are appended to the asset's
/// `{uuid}.receipts.cbor` log in the library. Only then is the asset `Verified`.
///
/// **The key history is pinned, not trusted per run** (issue #714). The first run against a
/// server pins the history it publishes, keyed by the server's canonical origin; a later run
/// accepts only an extension of it, and a history that drops or replaces a pinned key fails
/// the run with [`RemoteError::AttestationKeyPin`] before anything is sent.
///
/// **One asset's failure is that asset's.** An asset whose bundle, album, transfer, custody
/// check or receipt write fails is recorded as `Failed` with its error, and the run moves on to
/// the next one; the caller reports every failure and exits non-zero. What fails the whole run
/// is what every asset needs: the session, the feed read, and the pinned attestation keys.
///
/// Re-runnable by construction. The outstanding transfer work comes from server truth, so a
/// second run against an unchanged library uploads nothing; it re-confirms custody of every
/// asset, and re-persisting a receipt already in the log is a no-op.
#[instrument(skip(store, workspace), fields(endpoint = %remote.upload_endpoint))]
pub async fn push(
    remote: &RemoteConfig,
    store: &SessionStore,
    workspace: &mut Workspace,
    options: PushOptions,
) -> Result<PushSummary, RemoteError> {
    push_run(remote, store, workspace, options, PushRun::default()).await
}

/// [`push()`] over the assets `run` names, publishing to `run`'s transfer events and telling its
/// observer which asset the run is on. `capsule push --asset` and the TUI's push screen run
/// through here: a retry of failed assets is this over just those assets, so nothing is
/// re-imported and the assets that already reached custody are not asked about again.
///
/// # Errors
///
/// As [`push()`].
#[instrument(skip(store, workspace, run), fields(endpoint = %remote.upload_endpoint))]
pub async fn push_run(
    remote: &RemoteConfig,
    store: &SessionStore,
    workspace: &mut Workspace,
    options: PushOptions,
    run: PushRun<'_>,
) -> Result<PushSummary, RemoteError> {
    let session = resume_session(remote, store).await?;
    let outcome = push_with(remote, workspace, session.clone(), options, run).await;
    checkpoint(store, &session).await;
    outcome
}

/// Where a push stands for one asset, told to a [`PushRun`]'s observer as the run moves. The
/// transfer and custody steps themselves (finalize, merge, verify, receipt) are on the run's
/// [`TransferEvents`]; this only frames them by asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetProgress {
    /// The run built the asset's bundle and is about to send it. `blobs` are the content
    /// addresses of every blob the bundle holds, so an observer can tell which asset a
    /// blob-keyed transfer event belongs to. A create re-sealed at the album's current epoch is
    /// started again, with its new blobs.
    Started {
        /// The asset.
        asset_id: Uuid,
        /// Its blobs' content addresses (lowercase hex), in ladder order.
        blobs: Vec<String>,
    },
    /// The asset's outcome, exactly as the run's summary records it.
    Finished(AssetPushOutcome),
}

/// The optional parts of a push: which assets, and who watches it.
#[derive(Default)]
pub struct PushRun<'a> {
    /// The assets to push, in this order; `None` pushes every asset of the library. An id the
    /// library does not hold fails as that asset's `bundle` error, so a mistyped id is reported
    /// rather than skipped.
    pub assets: Option<Vec<Uuid>>,
    /// Where the run's uploads and custody checks publish their transfer events.
    pub events: TransferEvents,
    /// Told when each asset starts and finishes.
    pub observer: Option<&'a mut dyn FnMut(AssetProgress)>,
    /// Assets handed to another device whose delegation is still open and inside its deadline
    /// ([`crate::peer::open_delegations`]): each is reported [`AssetPushStatus::Delegated`] and
    /// nothing of it is sent, so the push does not spend the bandwidth the hand-off saves.
    /// Deduplication would make sending it safe; skipping it is only the cheaper of two correct
    /// runs.
    pub delegated: BTreeSet<Uuid>,
}

impl std::fmt::Debug for PushRun<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PushRun")
            .field("assets", &self.assets)
            .field("delegated", &self.delegated)
            .field("events", &self.events)
            .field("observed", &self.observer.is_some())
            .finish()
    }
}

impl PushRun<'_> {
    /// Tell the observer, building what to tell it only when there is one.
    fn tell(&mut self, progress: impl FnOnce() -> AssetProgress) {
        if let Some(observer) = self.observer.as_mut() {
            observer(progress());
        }
    }
}

/// `assets` with repeats dropped, the first occurrence kept.
pub(crate) fn distinct(assets: Vec<Uuid>) -> Vec<Uuid> {
    let mut seen = HashSet::new();
    assets.into_iter().filter(|id| seen.insert(*id)).collect()
}

/// The push itself, over a session somebody else is responsible for persisting.
async fn push_with(
    remote: &RemoteConfig,
    workspace: &mut Workspace,
    session: Session,
    options: PushOptions,
    mut run: PushRun<'_>,
) -> Result<PushSummary, RemoteError> {
    let asset_ids = match run.assets.take() {
        Some(assets) => distinct(assets),
        None => workspace.asset_ids(),
    };
    let mut summary = PushSummary {
        assets: asset_ids.len(),
        dry_run: options.dry_run,
        ..Default::default()
    };
    // The album keys go first (#741): an asset on the server whose album key the server does not
    // also hold, sealed under the master key, is one a recovered device could never open. So a
    // refused escrow fails the run before any byte of any asset is sent — and it runs even with
    // no asset to push, so an album created here is escrowed before another device of the
    // account can be recovered without it.
    // An album whose escrow could not be made to cover this device's epochs (it disagrees with
    // the server's, or the account's escrows are full) holds its assets back: each fails with
    // `album_keys_not_escrowed` rather than landing where no recovery could open it.
    let mut uncovered = HashSet::new();
    if !options.dry_run {
        let recovery = RecoveryClient::new(session.clone(), &remote.sync_endpoint)?;
        let escrows = escrow_album_keys(&recovery, workspace).await?;
        summary.album_keys_escrowed = escrows.stored;
        uncovered = escrows.uncovered;
    }
    if asset_ids.is_empty() {
        tracing::info!("push: the library holds no assets");
        return Ok(summary);
    }

    // Server truth first — the whole resume story. `--force` deliberately reads nothing.
    let held = if options.force {
        tracing::info!("push: --force, re-driving every blob regardless of server truth");
        HashSet::new()
    } else {
        held_blobs(remote, session.clone(), DEFAULT_SYNC_PAGE_SIZE).await?
    };

    let upload = UploadClient::new(UploadTransport::with_session(
        session.clone(),
        &remote.upload_endpoint,
        &remote.protocol_version,
    ))
    .with_connection_class(options.connection)
    .with_events(run.events.clone());
    let albums = AlbumClient::new(AlbumTransport::with_session(
        session.clone(),
        &remote.albums_endpoint,
    ));
    // `sync_endpoint` is the bare server origin; the storage and receipt routes hang off `/v1`.
    let verify = StorageVerifyClient::new(VerifyTransport::with_session(
        session,
        format!("{}/v1", remote.sync_endpoint.trim_end_matches('/')),
    ));
    let scheduler = StagedScheduler::new(options.policy, options.connection);

    if options.dry_run {
        for asset_id in asset_ids {
            let outcome = if run.delegated.contains(&asset_id) {
                delegated(asset_id)
            } else {
                plan_asset(workspace, &scheduler, &held, options.force, asset_id)
            };
            run.tell(|| AssetProgress::Finished(outcome.clone()));
            summary.record(outcome);
        }
        summary.log();
        return Ok(summary);
    }

    // Every receipt is checked under the server's pinned key history, so without it no asset
    // could be confirmed: that is a failure of the run, not of an asset.
    let attestation_keys = pinned_attestation_keys(remote, workspace, &verify).await?;

    let mut pusher = AssetPusher {
        workspace,
        upload,
        albums,
        verify,
        events: run.events.clone(),
        attestation_keys,
        scheduler,
        held,
        force: options.force,
        provisioned: HashSet::new(),
        uncovered,
    };
    for asset_id in asset_ids {
        if run.delegated.contains(&asset_id) {
            let outcome = delegated(asset_id);
            run.tell(|| AssetProgress::Finished(outcome.clone()));
            summary.record(outcome);
            continue;
        }
        let mut outcome = AssetPushOutcome::new(asset_id, AssetPushStatus::Verified);
        if let Err(error) = pusher
            .push(asset_id, &mut outcome, &mut summary, &mut run)
            .await
        {
            tracing::warn!(%asset_id, %error, "push: asset failed; continuing with the next");
            outcome.status = AssetPushStatus::Failed;
            outcome.error = Some(error.failure());
        }
        run.tell(|| AssetProgress::Finished(outcome.clone()));
        summary.record(outcome);
    }
    summary.log();
    Ok(summary)
}

/// The canonical origin (`scheme://host[:port]`, default port elided, host lowercased) of the
/// server endpoint: the key its attestation-key history is pinned under, so the same server
/// reached through a differently written endpoint shares one pin.
fn server_origin(endpoint: &str) -> Result<String, RemoteError> {
    let refuse = |reason: String| RemoteError::ServerOrigin {
        endpoint: endpoint.to_owned(),
        reason,
    };
    let url = url::Url::parse(endpoint).map_err(|e| refuse(e.to_string()))?;
    let origin = url.origin();
    if !origin.is_tuple() {
        return Err(refuse("the URL has no network origin".to_owned()));
    }
    Ok(origin.ascii_serialization())
}

/// Fetch the server's published attestation-key history and check it against the history this
/// library pinned for the server's origin: pinned on first contact, re-pinned when only
/// extended, refused when a pinned key was dropped or replaced
/// ([`capsule_library::library::pin_attestation_keys`]). The returned pinned history is what every
/// receipt and signed attestation of the run is verified under.
pub(crate) async fn pinned_attestation_keys(
    remote: &RemoteConfig,
    workspace: &Workspace,
    verify: &StorageVerifyClient,
) -> Result<Vec<HybridVerifyingKey>, RemoteError> {
    let origin = server_origin(&remote.sync_endpoint)?;
    let published = verify
        .fetch_attestation_keys()
        .await
        .map_err(RemoteError::AttestationKeys)?;
    let pinned = pin_attestation_keys(workspace.root(), &origin, &published)?;
    tracing::info!(
        %origin,
        published = published.len(),
        pinned = pinned.keys.len(),
        outcome = ?pinned.outcome,
        "attestation keys checked against the pin"
    );
    Ok(pinned.keys)
}

/// A dry run's plan for one asset: what would be sent, from server truth, with nothing sent.
fn plan_asset<H: std::hash::BuildHasher>(
    workspace: &Workspace,
    scheduler: &StagedScheduler,
    held: &HashSet<String, H>,
    force: bool,
    asset_id: Uuid,
) -> AssetPushOutcome {
    let mut outcome = AssetPushOutcome::new(asset_id, AssetPushStatus::Planned);
    match workspace.upload_bundle(&asset_id) {
        Ok(bundle) => {
            let planned = push::plan(scheduler, &bundle, held, force);
            outcome.uploaded = planned.blobs.len();
            outcome.already_held = planned.already_held;
            outcome.deferred = planned.deferred;
            outcome.bytes = planned.bytes;
            tracing::info!(
                %asset_id,
                planned = planned.blobs.len(),
                "push: dry run — would open these tier sessions"
            );
        }
        Err(error) => {
            tracing::warn!(%asset_id, %error, "push: dry run could not build the bundle");
            outcome.status = AssetPushStatus::Failed;
            outcome.error = Some(AssetError::from(error).failure());
        }
    }
    outcome
}

/// The outcome of an asset skipped because another device holds its open hand-off: nothing of
/// it was read or sent.
fn delegated(asset_id: Uuid) -> AssetPushOutcome {
    tracing::info!(
        %asset_id,
        "push: skipped; handed to another device and still inside its deadline"
    );
    AssetPushOutcome::new(asset_id, AssetPushStatus::Delegated)
}

/// Everything a run shares across its assets.
struct AssetPusher<'a> {
    workspace: &'a mut Workspace,
    upload: UploadClient,
    albums: AlbumClient,
    verify: StorageVerifyClient,
    /// Where the custody check publishes its verdict and receipts.
    events: TransferEvents,
    attestation_keys: Vec<HybridVerifyingKey>,
    scheduler: StagedScheduler,
    held: HashSet<String>,
    force: bool,
    /// Albums registered this run. Provisioning is idempotent server-side, so this only saves
    /// making the same request once per asset; an album whose registration failed is not in
    /// it, so the next asset in that album tries again.
    provisioned: HashSet<Uuid>,
    /// Albums whose keys the server's escrow does not cover (#741); their assets are not sent.
    uncovered: HashSet<Uuid>,
}

impl AssetPusher<'_> {
    /// Push one asset, confirm custody, and persist its receipts, filling `outcome` once each
    /// step completes. A transfer that fails partway leaves `uploaded`, `merged` and `bytes` at
    /// zero for the asset even when some of its blobs landed, because [`push::push_bundle`]
    /// returns only its error; a re-run derives the remaining work from server truth.
    #[instrument(skip(self, outcome, summary, run), fields(%asset_id))]
    async fn push(
        &mut self,
        asset_id: Uuid,
        outcome: &mut AssetPushOutcome,
        summary: &mut PushSummary,
        run: &mut PushRun<'_>,
    ) -> Result<(), AssetError> {
        let bundle = self.workspace.upload_bundle(&asset_id)?;
        run.tell(|| started(&bundle));
        if self.uncovered.contains(&bundle.album_id) {
            return Err(AssetError::KeysNotEscrowed(bundle.album_id));
        }

        // Invariant 6 needs the album to exist and be writable before the first session
        // opens; a dry run deliberately reaches nothing, so it never provisions either.
        if !self.provisioned.contains(&bundle.album_id) {
            let album = push::ensure_album(&self.albums, bundle.album_id)
                .await
                .map_err(PushError::Album)?;
            self.provisioned.insert(bundle.album_id);
            summary.provisioned_albums += 1;
            if album.created {
                summary.created_albums += 1;
            }
        }

        let (bundle, report) = match self.push_bundle(&bundle).await {
            Ok(report) => (bundle, report),
            // Issue #822: the album's epoch moved past the one this create is sealed under, so
            // the server refused it for good. Re-seal it at the album's current epoch and push
            // it once more; a second refusal fails the asset like any other.
            Err(PushError::CreateRefused { source, .. }) => {
                tracing::warn!(
                    amk_version = bundle.amk_version,
                    "push: the server refused this create; re-sealing it at the album's \
                     current epoch"
                );
                let resealed = self.workspace.reseal_create(&asset_id).map_err(|error| {
                    AssetError::Reseal {
                        refused: *source,
                        error,
                    }
                })?;
                outcome.resealed = true;
                tracing::info!(
                    from_epoch = resealed.from_epoch,
                    to_epoch = resealed.to_epoch,
                    derivatives = resealed.derivatives,
                    "push: create re-sealed; pushing it again"
                );
                let bundle = self.workspace.upload_bundle(&asset_id)?;
                run.tell(|| started(&bundle));
                let report = self.push_bundle(&bundle).await?;
                (bundle, report)
            }
            Err(error) => return Err(error.into()),
        };
        outcome.already_held = report.already_held;
        outcome.deferred = report.deferred;
        outcome.bytes = report.bytes;
        for blob in &report.pushed {
            match blob.outcome {
                TierSessionOutcome::Uploaded { .. } => outcome.uploaded += 1,
                TierSessionOutcome::AlreadyStored { .. } => outcome.merged += 1,
            }
        }

        let custody = push::verify_custody_with_events(
            &self.verify,
            &self.attestation_keys,
            &bundle,
            &report,
            jiff::Timestamp::now().as_second(),
            &self.events,
        )
        .await?;

        // The receipt log lives beside the provenance chain, in the same capture-month shard.
        let capture_utc = self
            .workspace
            .asset(&asset_id)
            .map(|asset| asset.capture_utc);
        for receipt in &custody.receipts {
            capsule_library::library::append_receipt(self.workspace.root(), capture_utc, receipt)?;
            outcome.receipts += 1;
        }
        tracing::info!(
            receipts = outcome.receipts,
            checked_at = %custody.checked_at,
            "push: custody verified and receipts persisted"
        );
        Ok(())
    }

    /// Drive one bundle's ladder against server truth.
    async fn push_bundle(
        &self,
        bundle: &capsule_library::lifecycle::UploadBundle,
    ) -> Result<push::AssetPushReport, PushError> {
        push::push_bundle(
            &self.upload,
            &self.scheduler,
            bundle,
            &self.held,
            self.force,
        )
        .await
    }
}

/// The [`AssetProgress::Started`] for `bundle`.
fn started(bundle: &capsule_library::lifecycle::UploadBundle) -> AssetProgress {
    AssetProgress::Started {
        asset_id: bundle.asset_id,
        blobs: push::bundle_blobs(bundle)
            .into_iter()
            .map(|(_, hash)| hash)
            .collect(),
    }
}

impl PushSummary {
    /// Fold one asset's outcome into the run's totals.
    fn record(&mut self, outcome: AssetPushOutcome) {
        self.uploaded_blobs += outcome.uploaded;
        self.merged_blobs += outcome.merged;
        self.already_held_blobs += outcome.already_held;
        self.deferred_blobs += outcome.deferred;
        self.bytes += outcome.bytes;
        self.receipts += outcome.receipts;
        if outcome.uploaded + outcome.merged > 0 {
            self.pushed_assets += 1;
        }
        match outcome.status {
            AssetPushStatus::Verified => self.verified_assets += 1,
            AssetPushStatus::Failed => self.failed_assets += 1,
            AssetPushStatus::Delegated => self.delegated_assets += 1,
            AssetPushStatus::Planned => {}
        }
        self.outcomes.push(outcome);
    }

    /// Log the run's totals, one structured event.
    fn log(&self) {
        tracing::info!(
            assets = self.assets,
            provisioned_albums = self.provisioned_albums,
            created_albums = self.created_albums,
            album_keys_escrowed = self.album_keys_escrowed,
            pushed_assets = self.pushed_assets,
            verified_assets = self.verified_assets,
            failed_assets = self.failed_assets,
            delegated_assets = self.delegated_assets,
            receipts = self.receipts,
            uploaded = self.uploaded_blobs,
            merged = self.merged_blobs,
            already_held = self.already_held_blobs,
            deferred = self.deferred_blobs,
            bytes = self.bytes,
            dry_run = self.dry_run,
            "push complete"
        );
    }
}

/// The custody line after a push: how many assets reached verified custody, and how many
/// receipts the run stored. English inline, as every CLI line is going to be (issue #680).
#[must_use]
pub fn custody_line(summary: &PushSummary) -> String {
    let mut line = format!(
        "Custody verified for {} of {} assets; {} receipts stored in the library.",
        summary.verified_assets, summary.assets, summary.receipts
    );
    if summary.delegated_assets > 0 {
        line.push_str(&format!(
            " Skipped {} handed to another device; `capsule peer status` follows them.",
            summary.delegated_assets
        ));
    }
    line
}

/// One failed asset, as the line the human output prints for it.
#[must_use]
pub fn failure_line(outcome: &AssetPushOutcome) -> String {
    let detail = outcome
        .error
        .as_ref()
        .map_or("no detail", |error| error.message.as_str());
    format!("asset {} failed: {detail}", outcome.asset_id)
}

/// Why a finished run exits non-zero, or `None` when every asset reached its goal.
#[must_use]
pub fn failed_run_reason(summary: &PushSummary) -> Option<String> {
    (summary.failed_assets > 0).then(|| {
        format!(
            "{} of {} assets did not reach verified custody",
            summary.failed_assets, summary.assets
        )
    })
}

// ─── Lifecycle ops ───────────────────────────────────────────────────────────

/// The `POST /v1/albums/{album}/ops` body for `asset_id`'s chain head when that head is a
/// `delete`, encoded as the JSON the request carries: the signed delete a delegated asset's
/// origin ledger holds until the create's custody exists
/// ([`OriginLedger::hold_delete`](capsule_sdk::peering::OriginLedger::hold_delete)).
///
/// Self-contained, so a held delete is sent later without the library: the manifest's
/// canonical CBOR (the server's new chain head is its hash), the envelope projected from it,
/// and the sealed metadata blob whenever the manifest binds one. `None` for an asset whose head
/// is not a delete, or one the library does not hold.
///
/// # Errors
///
/// [`LifecycleError::Cbor`] when the record does not encode, which is a corrupt library.
pub fn delete_op(
    workspace: &Workspace,
    asset_id: &Uuid,
) -> Result<Option<Vec<u8>>, LifecycleError> {
    use base64::Engine as _;
    let Some(asset) = workspace.asset(asset_id) else {
        return Ok(None);
    };
    let Some(head) = asset.chain.records().last() else {
        return Ok(None);
    };
    let core = &head.manifest.core;
    if core.action != capsule_core::crypto::provenance::Action::Delete {
        return Ok(None);
    }
    let manifest = capsule_core::cbor::to_canonical_vec(head)
        .map_err(|e| LifecycleError::Cbor(format!("encoding the provenance record: {e}")))?;
    let base64 = base64::engine::general_purpose::STANDARD;
    let request = capsule_sdk::rest::types::OpRequest {
        manifest_envelope: op_envelope(core),
        manifest_cbor: base64.encode(&manifest),
        metadata_blob: core
            .metadata_blob_hash
            .map(|_| base64.encode(&asset.metadata_blob)),
    };
    serde_json::to_vec(&request)
        .map(Some)
        .map_err(|e| LifecycleError::Cbor(format!("encoding the lifecycle op: {e}")))
}

/// The server-visible projection of a lifecycle manifest: every field verbatim, naming the
/// original's ciphertext, and no derivative (the server refuses the pair on a lifecycle write).
fn op_envelope(
    core: &capsule_core::crypto::provenance::ManifestCore,
) -> capsule_sdk::rest::types::ManifestEnvelope {
    let wire = |value: serde_json::Result<serde_json::Value>| {
        value
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default()
    };
    capsule_sdk::rest::types::ManifestEnvelope {
        crypto_suite_id: i64::from(core.crypto_suite_id),
        protocol_version: core.protocol_version.clone(),
        album_id: Some(core.album_id.to_string()),
        file_id: core.file_id.to_string(),
        amk_version: i64::from(core.amk_version.0),
        ciphertext_hash: core.ciphertext_hash.to_hex(),
        plaintext_size: i64::try_from(core.plaintext_size).unwrap_or(i64::MAX),
        chunk_size: i64::from(core.chunk_size),
        key_mode: wire(serde_json::to_value(core.key_mode)),
        metadata_blob_hash: core.metadata_blob_hash.map(|h| h.to_hex()),
        original_blob_hash: None,
        derivative_role: None,
        derivative_format: None,
        created_by_user: core.created_by_user.to_string(),
        created_by_device: core.created_by_device.to_string(),
        client_version: core.client_version.clone(),
        timestamp: core.timestamp.clone(),
        action: wire(serde_json::to_value(core.action)),
        prior_provenance_hash: core.prior_provenance_hash.map(|h| h.to_hex()),
        retention_until: core.retention_until.clone(),
    }
}

/// What the server did with one lifecycle op.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OpApplied {
    /// The action it applied.
    pub action: String,
    /// The feed position it occupies.
    pub sync_seq: u64,
    /// Whether the server had applied it already (a replay is not an error).
    pub replayed: bool,
}

/// Why a lifecycle op was not applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OpFailure {
    /// The server's stable `error.*` code, when it gave one.
    pub code: Option<String>,
    /// The English detail.
    pub message: String,
}

/// Send one lifecycle op [`delete_op`] encoded, under `session`.
///
/// # Errors
///
/// [`OpFailure`] when the bytes are not a lifecycle op, or the server refused or could not be
/// reached.
#[instrument(skip_all)]
pub(crate) async fn send_op(
    remote: &RemoteConfig,
    session: Session,
    op: &[u8],
) -> Result<OpApplied, OpFailure> {
    let failure = |message: String| OpFailure {
        code: None,
        message,
    };
    let request: capsule_sdk::rest::types::OpRequest = serde_json::from_slice(op)
        .map_err(|error| failure(format!("the held op does not parse: {error}")))?;
    let album_id = request
        .manifest_envelope
        .album_id
        .clone()
        .ok_or_else(|| failure("the held op names no album".to_owned()))?;
    let client = AuthenticatedClient::new(&remote.sync_endpoint, session)
        .map_err(|error| failure(error.to_string()))?;
    let applied = client
        .album_lifecycle_op(album_id, &remote.protocol_version, None, &request)
        .await
        .map_err(|error| {
            let code = op_refusal_code(&error);
            tracing::warn!(?code, %error, "lifecycle op refused");
            OpFailure {
                code,
                message: error.to_string(),
            }
        })?
        .into_inner();
    tracing::info!(
        asset_id = %applied.asset_id,
        action = %applied.action,
        sync_seq = applied.sync_seq,
        replayed = applied.replayed,
        "lifecycle op applied"
    );
    Ok(OpApplied {
        action: applied.action,
        sync_seq: u64::try_from(applied.sync_seq).unwrap_or_default(),
        replayed: applied.replayed,
    })
}

/// The server's stable `error.*` code on a refused lifecycle op: every status the operation
/// declares carries a problem body.
fn op_refusal_code(
    error: &capsule_sdk::rest::Error<capsule_sdk::rest::AlbumLifecycleOpError>,
) -> Option<String> {
    use capsule_sdk::rest::AlbumLifecycleOpError as Refusal;
    let capsule_sdk::rest::Error::Api(response) = error else {
        return None;
    };
    Some(match response.inner() {
        Refusal::Status409(problem) => problem.code.clone(),
        Refusal::Status426(problem) => problem.code.clone(),
        Refusal::Status400(problem)
        | Refusal::Status401(problem)
        | Refusal::Status403(problem)
        | Refusal::Status413(problem)
        | Refusal::Status415(problem)
        | Refusal::Status422(problem)
        | Refusal::Status500(problem) => problem.code.clone(),
    })
}

// ─── Move release (#671) ─────────────────────────────────────────────────────

/// A source file `capsule import --move` left in place, and the library asset that holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MovedSource {
    /// The source file.
    pub source: PathBuf,
    /// The asset whose signed original it must match.
    pub asset_id: Uuid,
}

/// What happened to one moved source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MoveReleaseOutcome {
    /// The source file.
    pub source: String,
    /// The asset that holds it.
    pub asset_id: String,
    /// Whether the source was deleted.
    pub released: bool,
    /// Why it was kept, for a kept source only: `not_durable`, `receipt_missing`,
    /// `verify_unavailable`, `receipt_unavailable`, `read_back_mismatch` or `unknown_asset`.
    pub kept_because: Option<&'static str>,
}

/// The outcome of releasing a Move import's sources.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct MoveReleaseSummary {
    /// Sources deleted on server custody.
    pub released: usize,
    /// Sources kept.
    pub kept: usize,
    /// One entry per source, in the order given.
    pub outcomes: Vec<MoveReleaseOutcome>,
}

/// The stable name `--json`-shaped output and the human line give a retain reason.
fn kept_because(reason: &RetainReason) -> &'static str {
    match reason {
        RetainReason::NotDurable => "not_durable",
        RetainReason::ReceiptMissing => "receipt_missing",
        RetainReason::VerifyUnavailable => "verify_unavailable",
        RetainReason::ReceiptUnavailable => "receipt_unavailable",
        RetainReason::ReadBackMismatch => "read_back_mismatch",
    }
}

/// Delete each of `moved` that the server holds — the Move half of `capsule import --move`.
///
/// Invariant 7: a source is deleted only on server custody. The server's facts come through
/// [`ReleaseCoordinator::resolve_custody`] (a verdict fresh within the re-verify window, and a
/// verified receipt from the asset's log for its original and metadata blob), and each source
/// then goes through [`Workspace::release_move_source`], which reads both copies back against
/// the signed original before the gate may delete anything. A source whose asset is not
/// confirmed is kept and reported; a re-run releases it once the server holds the asset.
///
/// # Errors
///
/// [`RemoteError::NotAuthenticated`] with no stored session, and
/// [`RemoteError::AttestationKeys`] when the server's keys cannot be fetched, and
/// [`RemoteError::AttestationKeyPin`] when they contradict the history pinned for the server.
/// Either way no source has been touched.
#[instrument(skip(store, workspace, moved), fields(sources = moved.len()))]
pub async fn release_moved_sources(
    remote: &RemoteConfig,
    store: &SessionStore,
    workspace: &Workspace,
    moved: &[MovedSource],
) -> Result<MoveReleaseSummary, RemoteError> {
    let session = resume_session(remote, store).await?;
    let outcome = release_moved_with(remote, workspace, session.clone(), moved).await;
    checkpoint(store, &session).await;
    outcome
}

/// The release itself, over a session somebody else is responsible for persisting.
async fn release_moved_with(
    remote: &RemoteConfig,
    workspace: &Workspace,
    session: Session,
    moved: &[MovedSource],
) -> Result<MoveReleaseSummary, RemoteError> {
    // `sync_endpoint` is the bare server origin; the storage and receipt routes hang off `/v1`.
    let verify = StorageVerifyClient::new(VerifyTransport::with_session(
        session,
        format!("{}/v1", remote.sync_endpoint.trim_end_matches('/')),
    ));
    let attestation_keys = pinned_attestation_keys(remote, workspace, &verify).await?;
    let coordinator = ReleaseCoordinator::new(verify, attestation_keys);
    let verifier = coordinator
        .resolve_custody(&held_releases(workspace, moved))
        .await;
    Ok(release_sources(workspace, &verifier, moved))
}

/// The custody question for each distinct asset behind `moved`. An asset whose bundle cannot be
/// built is left out, so the verifier cannot answer for it and its source is kept.
fn held_releases(workspace: &Workspace, moved: &[MovedSource]) -> Vec<HeldRelease> {
    let mut seen = HashSet::new();
    moved
        .iter()
        .filter(|m| seen.insert(m.asset_id))
        .filter_map(|m| match workspace.upload_bundle(&m.asset_id) {
            Ok(bundle) => Some(HeldRelease::for_original(&bundle)),
            Err(error) => {
                tracing::warn!(asset_id = %m.asset_id, %error, "move release: no bundle; the source is kept");
                None
            }
        })
        .collect()
}

/// Put every source in `moved` through the release gate over `verifier`, and tally the result.
fn release_sources<V: StorageVerifier>(
    workspace: &Workspace,
    verifier: &V,
    moved: &[MovedSource],
) -> MoveReleaseSummary {
    let mut summary = MoveReleaseSummary::default();
    for m in moved {
        let kept = match workspace.release_move_source(verifier, m.asset_id, &m.source) {
            Ok(ReleaseDecision::Release) => None,
            Ok(ReleaseDecision::Retain(reason)) => Some(kept_because(&reason)),
            Err(error) => {
                tracing::warn!(asset_id = %m.asset_id, %error, "move release: the library does not hold the asset");
                Some("unknown_asset")
            }
        };
        if kept.is_some() {
            summary.kept += 1;
        } else {
            summary.released += 1;
        }
        summary.outcomes.push(MoveReleaseOutcome {
            source: m.source.display().to_string(),
            asset_id: m.asset_id.to_string(),
            released: kept.is_none(),
            kept_because: kept,
        });
    }
    tracing::info!(
        released = summary.released,
        kept = summary.kept,
        "move release complete"
    );
    summary
}

/// The line after a Move release: how many sources were deleted and how many kept.
#[must_use]
pub fn move_release_line(summary: &MoveReleaseSummary) -> String {
    format!(
        "Deleted {} source files the server holds; kept {}.",
        summary.released, summary.kept
    )
}

/// One kept source, as the line the human output prints for it.
#[must_use]
pub fn kept_source_line(outcome: &MoveReleaseOutcome) -> String {
    format!(
        "kept {}: {}",
        outcome.source,
        outcome.kept_because.unwrap_or("not released")
    )
}

/// The line `capsule import --move` prints when it keeps every source because no server was
/// asked: without `--push` the import stays offline, and the local copy is not custody.
#[must_use]
pub fn sources_kept_offline_line(count: usize) -> String {
    format!(
        "Kept {count} source files: --move deletes a source only once the server holds it. \
         Re-run with --push to upload and release them."
    )
}

// ─── Verify (#747) ───────────────────────────────────────────────────────────

/// `capsule verify`: ask the server whether it durably holds each asset in `options`, and with
/// `options.signed` keep its signed answer in each asset's attestation log.
///
/// The orchestration is `verify::verify_assets`; this function supplies its network legs over
/// the SDK. A signed run first checks the server's attestation-key history against the pin, as
/// a push does, and every attestation is verified under the pinned history, bound to a nonce drawn fresh per request
/// ([`ReleaseCoordinator::fetch_verified_attestation`]). An attestation that does not verify is
/// reported against its assets and never stored.
///
/// # Errors
///
/// [`RemoteError::NotAuthenticated`] with no stored session, and, on a signed run,
/// [`RemoteError::AttestationKeys`] when the server's keys cannot be fetched, or
/// [`RemoteError::AttestationKeyPin`] when they contradict the history pinned for the server.
/// Either way nothing has been stored.
#[instrument(skip(store, workspace), fields(signed = options.signed, deep = options.deep))]
pub async fn verify_storage(
    remote: &RemoteConfig,
    store: &SessionStore,
    workspace: &Workspace,
    options: &VerifyOptions,
) -> Result<VerifySummary, RemoteError> {
    let session = resume_session(remote, store).await?;
    let outcome = verify_storage_with(remote, workspace, session.clone(), options).await;
    checkpoint(store, &session).await;
    outcome
}

/// The verify itself, over a session somebody else is responsible for persisting.
async fn verify_storage_with(
    remote: &RemoteConfig,
    workspace: &Workspace,
    session: Session,
    options: &VerifyOptions,
) -> Result<VerifySummary, RemoteError> {
    // `sync_endpoint` is the bare server origin; the storage route hangs off `/v1`.
    let client = StorageVerifyClient::new(VerifyTransport::with_session(
        session,
        format!("{}/v1", remote.sync_endpoint.trim_end_matches('/')),
    ));
    let coordinator = if options.signed {
        let keys = pinned_attestation_keys(remote, workspace, &client).await?;
        Some(ReleaseCoordinator::new(client.clone(), keys))
    } else {
        None
    };
    let source = SdkVerdictSource {
        client,
        coordinator,
    };
    Ok(verify::verify_assets(workspace, &source, options).await)
}

/// [`verify::VerdictSource`] over the SDK's storage-verify client.
struct SdkVerdictSource {
    client: StorageVerifyClient,
    /// Present on a signed run: it holds the attestation keys and draws each nonce.
    coordinator: Option<ReleaseCoordinator>,
}

impl verify::VerdictSource for SdkVerdictSource {
    async fn verdicts(
        &self,
        assets: &[AssetQuery],
        deep: bool,
    ) -> Result<Vec<StorageVerdict>, VerifyError> {
        self.client.verify(assets, deep).await
    }

    async fn attestation(
        &self,
        assets: &[AssetQuery],
        deep: bool,
    ) -> Result<VerifiedAttestation, VerifyError> {
        let coordinator = self.coordinator.as_ref().ok_or_else(|| {
            VerifyError::Malformed("a signed verdict needs the attestation keys".to_owned())
        })?;
        coordinator.fetch_verified_attestation(assets, deep).await
    }
}

/// Query the sync-fed local store — the client-side library `capsule list` renders.
#[instrument(skip(db))]
pub async fn list<C: ConnectionTrait>(
    db: &C,
    include_tombstoned: bool,
) -> Result<Vec<SyncedAssetView>, RemoteError> {
    Ok(syncstore::list_assets(db, include_tombstoned).await?)
}

#[cfg(test)]
mod tests;
