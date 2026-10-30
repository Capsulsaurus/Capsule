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
use std::fmt::Write as _;
use std::path::PathBuf;

use capsule_core::crypto::keys::{HybridVerifyingKey, MasterKey};
use capsule_core::crypto::primitives::{Argon2Params, DeviceTier};
use capsule_library::import::UploadPolicy;
use capsule_library::library::{
    KeyPinError, ReleaseDecision, RetainReason, StorageVerdict, StorageVerifier,
    pin_attestation_keys,
};
use capsule_library::lifecycle::{LifecycleError, ResealRefusal, Workspace};
use capsule_sdk::albums::{AlbumClient, AlbumTransport, LifecycleOpError};
use capsule_sdk::auth::{AuthClient, AuthError, LoginOutcome, Session};
use capsule_sdk::client::AuthenticatedClient;
use capsule_sdk::directory::{DirectoryClient, DirectoryError};
use capsule_sdk::events::TransferEvents;
use capsule_sdk::fetch::HttpBlobSource;
use capsule_sdk::net::ConnectionClass;
use capsule_sdk::net::path::TransferPlan;
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
/// With none published ([`DirectoryError::NotPublished`]), the workspace's own directory — for
/// a recovered library this device alone at version 1, for an escrowed one every device it
/// lists at the version its account bind signed — is the account's first.
///
/// Both [`auth_recover`] and [`auth_escrow`] join through here, so a library either command
/// leaves behind can author uploads the server accepts.
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
                "directory: the account's directory fetched and verified"
            );
            let Some(extended) = workspace.extend_account_directory(&published)? else {
                tracing::info!(
                    %user_id,
                    directory_version = stored,
                    "directory: the account's directory already lists this device"
                );
                return Ok(stored);
            };
            extended
        }
        Err(DirectoryError::NotPublished) => {
            tracing::info!(
                %user_id,
                "directory: the account has no directory yet; publishing this device's own"
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
/// 5. make this device a member of the account's published device directory (#920), the way
///    [`auth_recover`] does: extend the directory step 1 verified, adopt it unchanged when it
///    already lists this device, or publish the library's own as the account's first. Without
///    it the server refuses every upload this device authored
///    (`error.upload.device_not_authorized`), so `capsule push` would back nothing up;
/// 6. wrap the master key under the secret at `tier` and store it (`PUT /v1/auth/escrow`),
///    then store the sealed identity beside it (`PUT /v1/auth/escrow/identity`) and each
///    album's key escrow the server does not hold current (`PUT /v1/auth/escrow/albums/{id}`,
///    #741), so a recovery from this escrow opens the library's albums.
///
/// Every refusal before step 4 writes nothing, locally or on the server. A failure after it
/// leaves the library bound, which a rerun treats as already done. The directory is joined
/// before the escrow is stored so that a rerun after any later failure finds no escrow in its
/// way and the directory already listing this device, which it adopts without publishing.
///
/// # Errors
///
/// [`RemoteError::NotAuthenticated`] with no stored session; [`RemoteError::Profile`] when the
/// account id cannot be read; the refusals above; [`RemoteError::Directory`] when the account's
/// directory does not verify under the library's identity key (code
/// `error.directory.untrusted_signature`), cannot be fetched, or its publish is refused;
/// [`RemoteError::Lifecycle`] for the bind, the directory extend or the wrap;
/// [`RemoteError::Recovery`] for an escrow or sealed-identity call.
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
    let directory_version = join_account_directory(&directory, workspace).await?;
    tracing::info!(
        %user_id,
        device = %workspace.device_id(),
        directory_version,
        "escrow: this device is listed in the account's directory"
    );

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
/// published at all ([`DirectoryError::NotPublished`], so this escrow publishes the account's
/// first directory once the library is bound), passes. A document that does not verify
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
    // The latest server clock reading among the landed entries: the only instant the retention
    // sweep judges deadlines at.
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
        // one, and sweeping before that page lands would tombstone a restored asset for good.
        // Judged by the server's clock as the drain saw it, never this device's: a run that
        // landed no entry has no reading, so it leaves the sweep to the next one that does.
        if let Some(at) = server_clock {
            syncstore::record_elapsed_purges(db, at).await?;
        } else {
            tracing::debug!("sync: the drain saw no server clock reading; no window is judged");
        }
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
    /// The detected connection class: it sizes the upload chunks, and gates the staged tiers
    /// when there is no `path` plan.
    pub connection: ConnectionClass,
    /// The plan the path snapshot made ([`crate::cache::path_plan`]); when present it gates
    /// the staged tiers on the whole path — power, heat and the background deadline too — not
    /// the class alone.
    pub path: Option<TransferPlan>,
}

impl Default for PushOptions {
    fn default() -> Self {
        Self {
            dry_run: false,
            force: false,
            policy: UploadPolicy::Full,
            connection: ConnectionClass::Unmetered,
            path: None,
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
    /// Skipped: the server already holds the asset's create (its feed lists the asset and holds
    /// the original the create pins), and an edit since replaced the metadata blob that create
    /// binds, so the library cannot rebuild the create and nothing of it is left to upload. Not
    /// a failure; the records after the create reach the server in `live_records`.
    Indexed,
}

/// Why an asset failed, in the shape `--json` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AssetPushFailure {
    /// A stable name for where the asset stopped: `bundle`, `album`, `upload`, `create_refused`,
    /// `reseal`, `unknown_blob`, `verify_unavailable`, `not_durable`, `receipt_missing`,
    /// `receipt_rejected`, `receipt_store` or `album_keys_not_escrowed`.
    pub kind: &'static str,
    /// The server's stable `error.*` code, when it supplied one. For a blob the server refused
    /// at finalization — `error.upload.content_hash_mismatch`, `envelope_rejected`,
    /// `replace_incomplete`, `stale_revival` — it is the code the session's terminal
    /// `failed_processing` state carried (`X-Capsule-Upload-Error`), which the transfer reads
    /// from `HEAD` once the server has finished finalizing.
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
    /// Blobs transferred to their terminal outcome this run: every byte sent and the session
    /// reported `completed` once the server finished finalizing it (planned, on a dry run). A
    /// blob finalization refused is the asset's `error` instead.
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
    /// Assets skipped because the server already holds their create and an edit since replaced
    /// the metadata blob it binds ([`AssetPushStatus::Indexed`]).
    pub indexed_assets: usize,
    /// Assets skipped because they are in the trash: a deleted asset's upload is redacted, so
    /// nothing of it is planned or sent and it has no entry in `outcomes`.
    pub trashed_assets: usize,
    /// What the run did with each trashed asset's lifecycle records the server does not hold
    /// yet: its `delete`, held until the server indexed the asset's create, is sent once the
    /// feed lists it, whichever device uploaded it (Organization — Deletion State Machine). One
    /// entry per trashed asset with a record to send or still held; none for one the server is
    /// level with, and none on a dry run or under `--force`, which read no server truth. A
    /// purged asset's kept chain is delivered the same way, from the tombstone, once the feed
    /// lists it with a head before the chain's.
    pub held_records: Vec<TrashOutcome>,
    /// What the run did with the records after the create of each live asset it considered:
    /// an upload carries only the create, so a delete, a restore or an edit signed since is sent
    /// as a lifecycle op once the server has indexed the create, read off a feed drained after
    /// this run's uploads (Organization — Deletion State Machine). One entry per live asset
    /// whose chain the server is not level with; none for a lone create, none for an asset
    /// handed to another device, and none on a dry run.
    pub live_records: Vec<TrashOutcome>,
    /// Verified custody receipts persisted to the library this run.
    pub receipts: usize,
    /// Blobs transferred and finalized: the server reported each session `completed`.
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
    Ok(blobs_held(&entries))
}

/// The blob content addresses a drain's `entries` say the server durably holds.
fn blobs_held(entries: &[FeedEntry]) -> HashSet<String> {
    let mut held = HashSet::new();
    for entry in entries {
        held.extend(held_from_feed(entry));
    }
    tracing::info!(
        entries = entries.len(),
        blobs = held.len(),
        "push: server truth read off the sync feed"
    );
    held
}

/// Send the lifecycle records of each of `assets` the server does not hold yet, now that
/// `chains` says which creates it has indexed ([`deliver_chain`]). An asset the server is level
/// with has no entry; one whose create is still not indexed is reported held. An outcome
/// carries a `retention_until` only for an asset in the trash.
async fn deliver_held_records(
    remote: &RemoteConfig,
    session: &Session,
    workspace: &Workspace,
    assets: &[Uuid],
    chains: &held::ServerChains,
) -> Vec<TrashOutcome> {
    let mut outcomes = Vec::new();
    for asset_id in assets {
        let Some(asset) = workspace.asset(asset_id) else {
            continue;
        };
        if chains.is_level(asset_id, asset.chain.records()) {
            tracing::debug!(%asset_id, "push: the server holds this asset's head; nothing to send");
            continue;
        }
        let trashed = workspace.is_trashed(asset_id);
        let delivered = deliver_chain(
            remote,
            session,
            workspace,
            asset_id,
            chains,
            jiff::Timestamp::now(),
        )
        .await;
        let outcome = TrashOutcome {
            retention_until: trashed.then(|| trashed_until(asset)).flatten(),
            ..trash_outcome(*asset_id, delivered, trashed_until(asset))
        };
        tracing::info!(%asset_id, trashed, status = ?outcome.status, "push: an asset's held records");
        outcomes.push(outcome);
    }
    outcomes
}

/// Send the records after the create of each `live` asset, once this run's uploads are done:
/// the feed is drained afresh, so a create this run uploaded is indexed by the time its later
/// records are judged ([`deliver_held_records`]). A feed that cannot be read sends nothing, and
/// each asset fails as `unavailable`, which a re-run clears.
async fn deliver_live_records(
    remote: &RemoteConfig,
    session: &Session,
    workspace: &Workspace,
    live: &[Uuid],
) -> Vec<TrashOutcome> {
    if live.is_empty() {
        return Vec::new();
    }
    tracing::info!(
        assets = live.len(),
        "push: delivering the records after each live asset's create"
    );
    match server_chains(remote, session.clone()).await {
        Ok(chains) => deliver_held_records(remote, session, workspace, live, &chains).await,
        Err(message) => live
            .iter()
            .map(|asset_id| {
                failed_trash(
                    *asset_id,
                    TrashFailure {
                        kind: "unavailable",
                        code: None,
                        message: format!(
                            "the server's feed could not be read after the uploads, so whether \
                             it has indexed the asset is unknown; its later records are held in \
                             the library and nothing was sent: {message}"
                        ),
                        permanent: false,
                        retention_until: None,
                    },
                )
            })
            .collect(),
    }
}

/// Send the stretch of each `purged` asset's kept chain the server lacks, from the tombstone
/// ([`deliver_chain`]): the held `delete`, and the shorter-retention re-issue an early purge
/// appended, once another device's upload of the create is indexed. The server then purges by
/// that re-issued window.
///
/// Only an asset the feed lists with a head record, and not the chain's own head, has anything
/// to send. One the feed does not list has no entry: its create may never reach the server, and
/// a purged asset has no upload of its own to wait for, so it is not reported held on every
/// run. One listed by an entry that carries no record (the server purged it too, or an older
/// server) has no entry either: there is no head to chain onto.
async fn deliver_purged_records(
    remote: &RemoteConfig,
    session: &Session,
    workspace: &Workspace,
    purged: &[Uuid],
    chains: &held::ServerChains,
) -> Vec<TrashOutcome> {
    let mut outcomes = Vec::new();
    for asset_id in purged {
        let Some(records) = workspace.purged_chain(asset_id) else {
            continue;
        };
        if !matches!(chains.head(asset_id), Some(Some(_))) {
            tracing::debug!(%asset_id, "push: the feed lists no head for this purged asset; nothing to send");
            continue;
        }
        if chains.is_level(asset_id, records) {
            tracing::debug!(%asset_id, "push: the server holds this purged asset's head; nothing to send");
            continue;
        }
        let until = latest_delete_until(records);
        let delivered = deliver_chain(
            remote,
            session,
            workspace,
            asset_id,
            chains,
            jiff::Timestamp::now(),
        )
        .await;
        let outcome = TrashOutcome {
            retention_until: until.clone(),
            ..trash_outcome(*asset_id, delivered, until)
        };
        tracing::info!(%asset_id, status = ?outcome.status, "push: a purged asset's held records, sent from its tombstone");
        outcomes.push(outcome);
    }
    outcomes
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
    // A purged asset has nothing to upload, but its kept chain may still hold a `delete` (and an
    // early purge's re-issue) the server never saw: another device may upload its create later,
    // and the stretch is then sent from the tombstone. Every tombstone on a whole-library run;
    // only the named ones on a scoped run.
    let (asset_ids, purged): (Vec<Uuid>, Vec<Uuid>) = match run.assets.take() {
        Some(assets) => distinct(assets)
            .into_iter()
            .partition(|asset_id| workspace.purge_record(asset_id).is_none()),
        None => (
            workspace.asset_ids(),
            workspace.tombstones().into_iter().collect(),
        ),
    };
    let mut summary = PushSummary {
        assets: asset_ids.len(),
        dry_run: options.dry_run,
        ..Default::default()
    };
    // A trashed asset's upload is redacted (Organization — Deletion State Machine): this device
    // sends no further byte of it, so none of its tiers is planned, deferred or opened. Its
    // `delete` reaches the server as a lifecycle op, never as an upload: `capsule trash` sends it
    // when the server has indexed the create, and this run sends one held until it has.
    let (trashed, asset_ids): (Vec<Uuid>, Vec<Uuid>) = asset_ids
        .into_iter()
        .partition(|asset_id| workspace.is_trashed(asset_id));
    summary.trashed_assets = trashed.len();
    for asset_id in &trashed {
        tracing::info!(%asset_id, "push: skipped; the asset is in the trash, so its upload is redacted");
    }
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
    let nothing_held = trashed.is_empty() && purged.is_empty();
    if asset_ids.is_empty() && (nothing_held || options.dry_run || options.force) {
        tracing::info!("push: the library holds no assets to push");
        return Ok(summary);
    }

    // Server truth first — the whole resume story. `--force` deliberately reads nothing.
    let (held, indexed) = if options.force {
        tracing::info!("push: --force, re-driving every blob regardless of server truth");
        (HashSet::new(), HashSet::new())
    } else {
        let entries = drain_feed(remote, session.clone(), DEFAULT_SYNC_PAGE_SIZE).await?;
        let held = blobs_held(&entries);
        let chains = held::ServerChains::from_feed(&entries);
        if !options.dry_run {
            summary.held_records =
                deliver_held_records(remote, &session, workspace, &trashed, &chains).await;
            summary.held_records.extend(
                deliver_purged_records(remote, &session, workspace, &purged, &chains).await,
            );
        }
        let indexed = creates_on_server(workspace, &asset_ids, &chains, &held);
        (held, indexed)
    };
    if asset_ids.is_empty() {
        summary.log();
        return Ok(summary);
    }

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
        session.clone(),
        format!("{}/v1", remote.sync_endpoint.trim_end_matches('/')),
    ));
    let scheduler = match &options.path {
        Some(plan) => StagedScheduler::from_plan(plan, options.policy),
        None => StagedScheduler::new(options.policy, options.connection),
    };

    if options.dry_run {
        for asset_id in asset_ids {
            let outcome = if run.delegated.contains(&asset_id) {
                delegated(asset_id)
            } else {
                plan_asset(
                    workspace,
                    &scheduler,
                    &held,
                    &indexed,
                    options.force,
                    asset_id,
                )
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

    // An upload carries only an asset's create, so every live asset whose chain moved past it
    // (a delete and a restore, or an edit) has records to deliver as lifecycle ops once the
    // server indexes the create, whether this run uploads it or another device did. An asset
    // handed to another device is left to its hand-off.
    let later_records: Vec<Uuid> = asset_ids
        .iter()
        .filter(|asset_id| !run.delegated.contains(asset_id))
        .filter(|asset_id| {
            workspace
                .asset(asset_id)
                .is_some_and(|asset| asset.chain.records().len() > 1)
        })
        .copied()
        .collect();

    let mut pusher = AssetPusher {
        workspace,
        upload,
        albums,
        verify,
        events: run.events.clone(),
        attestation_keys,
        scheduler,
        held,
        indexed,
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
    summary.live_records =
        deliver_live_records(remote, &session, pusher.workspace, &later_records).await;
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
    indexed: &HashSet<Uuid>,
    force: bool,
    asset_id: Uuid,
) -> AssetPushOutcome {
    let mut outcome = AssetPushOutcome::new(asset_id, AssetPushStatus::Planned);
    match create_bundle(workspace, indexed, asset_id) {
        Ok(None) => outcome.status = AssetPushStatus::Indexed,
        Ok(Some(bundle)) => {
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

/// The assets of `asset_ids` whose create the server already holds: its feed lists the asset
/// and holds the original that create pins. Nothing of such a create is left to upload.
fn creates_on_server<H: std::hash::BuildHasher>(
    workspace: &Workspace,
    asset_ids: &[Uuid],
    chains: &held::ServerChains,
    held: &HashSet<String, H>,
) -> HashSet<Uuid> {
    let indexed: HashSet<Uuid> = asset_ids
        .iter()
        .filter(|asset_id| chains.head(asset_id).is_some())
        .filter(|asset_id| {
            workspace.asset(asset_id).is_some_and(|asset| {
                asset.chain.records().first().is_some_and(|create| {
                    held.contains(&create.manifest.core.ciphertext_hash.to_hex())
                })
            })
        })
        .copied()
        .collect();
    tracing::debug!(
        assets = asset_ids.len(),
        indexed = indexed.len(),
        "push: assets whose create the server already holds"
    );
    indexed
}

/// `asset_id`'s upload bundle, or `None` when there is nothing of its create to upload: the
/// server already holds it (`indexed`), and an edit since replaced the metadata blob it binds,
/// so the library cannot rebuild it ([`LifecycleError::CreateMetadataSuperseded`]). The records
/// after the create are delivered as lifecycle ops ([`deliver_live_records`]), so such an asset
/// is never a failure. One the server does not hold yet still fails: its create has to be
/// uploaded, and cannot be.
fn create_bundle(
    workspace: &Workspace,
    indexed: &HashSet<Uuid>,
    asset_id: Uuid,
) -> Result<Option<capsule_library::lifecycle::UploadBundle>, LifecycleError> {
    match workspace.upload_bundle(&asset_id) {
        Ok(bundle) => Ok(Some(bundle)),
        Err(LifecycleError::CreateMetadataSuperseded(_)) if indexed.contains(&asset_id) => {
            tracing::info!(
                %asset_id,
                "push: skipped; the server holds this asset's create, which an edit here has \
                 superseded, so nothing of it is uploaded"
            );
            Ok(None)
        }
        Err(error) => Err(error),
    }
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
    /// Assets whose create the server already holds ([`creates_on_server`]).
    indexed: HashSet<Uuid>,
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
        let Some(bundle) = create_bundle(self.workspace, &self.indexed, asset_id)? else {
            outcome.status = AssetPushStatus::Indexed;
            return Ok(());
        };
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
            AssetPushStatus::Indexed => self.indexed_assets += 1,
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
            indexed_assets = self.indexed_assets,
            trashed_assets = self.trashed_assets,
            held_records = self.held_records.len(),
            live_records = self.live_records.len(),
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
        let _ = write!(
            line,
            " Skipped {} handed to another device; `capsule peer status` follows them.",
            summary.delegated_assets
        );
    }
    if summary.indexed_assets > 0 {
        let _ = write!(
            line,
            " Skipped {} the server already holds; an edit here replaced what their upload \
             carried, so only the records after it are sent.",
            summary.indexed_assets
        );
    }
    if summary.trashed_assets > 0 {
        let _ = write!(
            line,
            " Skipped {} in the trash; nothing of a deleted asset is uploaded.",
            summary.trashed_assets
        );
    }
    let count = |outcomes: &[TrashOutcome], status| {
        outcomes
            .iter()
            .filter(|outcome| outcome.status == status)
            .count()
    };
    let (sent, held, failed) = (
        count(&summary.held_records, TrashStatus::Applied),
        count(&summary.held_records, TrashStatus::Held),
        count(&summary.held_records, TrashStatus::Failed),
    );
    if sent > 0 {
        let _ = write!(
            line,
            " Sent the held delete of {sent} deleted assets the server now indexes."
        );
    }
    if held > 0 {
        let _ = write!(
            line,
            " {held} deleted assets keep their delete held in the library."
        );
    }
    if failed > 0 {
        let _ = write!(
            line,
            " {failed} deleted assets' held records were not applied; `--json` says why."
        );
    }
    let (sent, held, failed) = (
        count(&summary.live_records, TrashStatus::Applied),
        count(&summary.live_records, TrashStatus::Held),
        count(&summary.live_records, TrashStatus::Failed),
    );
    if sent > 0 {
        let _ = write!(
            line,
            " Sent the records after the create of {sent} assets the server now indexes."
        );
    }
    if held > 0 {
        let _ = write!(
            line,
            " {held} assets keep the records after their create held in the library."
        );
    }
    if failed > 0 {
        let _ = write!(
            line,
            " {failed} assets' records after their create were not applied; `--json` says why."
        );
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
    let Some(asset) = workspace.asset(asset_id) else {
        return Ok(None);
    };
    let Some(head) = asset.chain.records().last() else {
        return Ok(None);
    };
    if head.manifest.core.action != capsule_core::crypto::provenance::Action::Delete {
        return Ok(None);
    }
    encode_op(asset_id, Some(&asset.metadata_blob), head).map(Some)
}

/// The `POST /v1/albums/{album}/ops` bodies that carry `asset_id`'s chain from the record whose
/// hash is `after` up to the library's head, in chain order, when that stretch holds a
/// `delete`: what a delegated asset's origin ledger holds until the create's custody exists.
///
/// The server's chain head once the delegate uploads is the record the hand-off sealed
/// (`after`), and invariant 17 refuses an op that does not chain onto it. So a delete made
/// after an edit, or after a restore of an earlier delete, is sent with every record between
/// the sealed head and it, each chaining onto the one before, rather than alone. With `after`
/// unknown or not in the chain, only a head `delete` is returned (as [`delete_op`]).
///
/// Each op is [`delete_op`]'s shape. A record that binds a metadata blob carries the library's
/// blob only when it is the one the record commits to: the library keeps the latest blob, so an
/// edit superseded by a later edit goes without it, and the server refuses that op for good.
///
/// # Errors
///
/// [`LifecycleError::Cbor`] when a record does not encode, which is a corrupt library.
pub fn lifecycle_ops_after(
    workspace: &Workspace,
    asset_id: &Uuid,
    after: Option<capsule_core::crypto::hash::Hash32>,
) -> Result<Vec<Vec<u8>>, LifecycleError> {
    use capsule_core::crypto::provenance::Action;
    // A purged asset's stretch is read from the chain its purge kept, the tombstone.
    let Some(chain) = held::local_chain(workspace, asset_id) else {
        return Ok(Vec::new());
    };
    let tail = held::after_head(chain.records, after);
    if !tail
        .iter()
        .any(|r| r.manifest.core.action == Action::Delete)
    {
        return Ok(Vec::new());
    }
    tracing::debug!(%asset_id, records = tail.len(), "lifecycle ops: the stretch after the sealed head");
    tail.iter()
        .map(|record| encode_op(asset_id, chain.metadata_blob, record))
        .collect()
}

/// One provenance record of `asset_id` as the JSON body of a lifecycle op. `metadata_blob` is
/// the sealed blob the library holds for the asset (`None` once a purge removed it); a record
/// that binds a blob carries it only when it is the one the record commits to.
fn encode_op(
    asset_id: &Uuid,
    metadata_blob: Option<&[u8]>,
    record: &capsule_core::crypto::provenance::ProvenanceRecord,
) -> Result<Vec<u8>, LifecycleError> {
    use base64::Engine as _;
    let core = &record.manifest.core;
    let manifest = capsule_core::cbor::to_canonical_vec(record)
        .map_err(|e| LifecycleError::Cbor(format!("encoding the provenance record: {e}")))?;
    let base64 = base64::engine::general_purpose::STANDARD;
    let blob = core.metadata_blob_hash.and_then(|hash| {
        let held = metadata_blob
            .filter(|blob| capsule_core::crypto::encryption::blob_ciphertext_hash(blob) == hash);
        if held.is_none() {
            tracing::warn!(
                %asset_id,
                action = ?core.action,
                purged = metadata_blob.is_none(),
                "lifecycle op: the library no longer holds the metadata blob this record binds; sent without it"
            );
        }
        held.map(|blob| base64.encode(blob))
    });
    let request = capsule_sdk::rest::types::OpRequest {
        manifest_envelope: op_envelope(core),
        manifest_cbor: base64.encode(&manifest),
        metadata_blob: blob,
    };
    serde_json::to_vec(&request)
        .map_err(|e| LifecycleError::Cbor(format!("encoding the lifecycle op: {e}")))
}

/// The hash of the provenance record a lifecycle op ([`lifecycle_ops_after`]'s shape) carries,
/// or `None` for bytes that are not such an op.
///
/// This is the op's identity: the same record encodes to different bytes once a later edit
/// supersedes the metadata blob it binds (the blob is then left out), so held ops are told
/// apart by it rather than by their bytes.
#[must_use]
pub fn op_record_hash(op: &[u8]) -> Option<capsule_core::crypto::hash::Hash32> {
    use base64::Engine as _;
    let request: capsule_sdk::rest::types::OpRequest = serde_json::from_slice(op).ok()?;
    let manifest = base64::engine::general_purpose::STANDARD
        .decode(request.manifest_cbor)
        .ok()?;
    let record: capsule_core::crypto::provenance::ProvenanceRecord =
        capsule_core::cbor::from_slice(&manifest).ok()?;
    Some(record.record_hash())
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
    /// A stable name for why: one of [`LifecycleOpError::kind`]'s (`retention_elapsed`,
    /// `stale_revival`, `refused`, `forbidden`, `unauthenticated`, `upgrade_required`,
    /// `unavailable`, `malformed`), or `not_an_op` for held bytes that do not parse as one.
    pub kind: &'static str,
    /// The server's stable `error.*` code, when it gave one.
    pub code: Option<String>,
    /// The English detail.
    pub message: String,
    /// Whether sending the same bytes again can only be refused again: the server judged the op
    /// itself (`400`, `409`, `413`, `415`, `422` — a stale chain, a regressed epoch, a closed
    /// retention window, a malformed op), or the held bytes are not an op. An unreachable server,
    /// a session it did not accept, an upgrade it asked for and a server fault are not.
    pub permanent: bool,
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
    let not_an_op = |message: String| OpFailure {
        kind: "not_an_op",
        code: None,
        message,
        permanent: true,
    };
    let request: capsule_sdk::rest::types::OpRequest = serde_json::from_slice(op)
        .map_err(|error| not_an_op(format!("the held op does not parse: {error}")))?;
    // Checked here as well as in the SDK so that bytes no resend can fix stay permanent.
    if request.manifest_envelope.album_id.is_none() {
        return Err(not_an_op("the held op names no album".to_owned()));
    }
    let client = AlbumClient::new(AlbumTransport::with_session(
        session,
        remote.albums_endpoint.clone(),
    ));
    let applied = client
        .apply_op(&request)
        .await
        .map_err(|error| op_failure(&error))?;
    Ok(OpApplied {
        action: applied.action,
        sync_seq: applied.sync_seq,
        replayed: applied.replayed,
    })
}

/// A refused or undelivered lifecycle op, as the CLI reports it.
fn op_failure(error: &LifecycleOpError) -> OpFailure {
    OpFailure {
        kind: error.kind(),
        code: error.error_code().map(str::to_owned),
        message: error.to_string(),
        permanent: error.is_permanent(),
    }
}

// ─── Trash (`capsule trash delete|restore`) ─────────────────────────────────

/// Which way a `capsule trash` run moves its assets. Serialized as the lifecycle action the
/// run signs and sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrashAction {
    /// Move each asset to the trash: a signed `delete` carrying its retention floor.
    Delete,
    /// Bring each asset back out of the trash: a signed `trash-restore`.
    TrashRestore,
}

impl TrashAction {
    /// The provenance action this run's records carry.
    fn action(self) -> capsule_core::crypto::provenance::Action {
        match self {
            Self::Delete => capsule_core::crypto::provenance::Action::Delete,
            Self::TrashRestore => capsule_core::crypto::provenance::Action::TrashRestore,
        }
    }
}

/// One `capsule trash` run's inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashRequest {
    /// Which lifecycle op every asset gets.
    pub action: TrashAction,
    /// The assets, in the order given; repeats are dropped.
    pub assets: Vec<Uuid>,
    /// The retention window, in days, a fresh delete signs. Unused by a restore, and by an
    /// asset whose latest record already is a delete: that record is resent with its own
    /// window, which the outcome's `retention_until` reports.
    pub retain_days: i64,
}

/// Where one asset ended up after a `capsule trash` run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrashStatus {
    /// The server applied the op (or had applied it already: see `replayed`).
    Applied,
    /// The record is signed in the library and held there, not sent; `held` says why. Not a
    /// failure: a later `capsule trash` or `capsule push` sends it once it may be sent.
    Held,
    /// The op was not applied; `error` says why.
    Failed,
}

/// Why one asset's records were held rather than sent, in the shape `--json` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrashHold {
    /// `not_indexed` (the server has not indexed the asset's create, so it may see no lifecycle
    /// record of it yet) or `restore_too_late` (the restore undoes a delete the server has not
    /// seen and whose window has closed; withheld for good, the server keeps the asset live).
    pub reason: HoldReason,
    /// For `restore_too_late`, the closed window (RFC 3339) of the delete the restore undoes.
    pub retention_until: Option<String>,
}

/// Why one asset's op was not applied, in the shape `--json` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrashFailure {
    /// A stable name for why. The server's: `retention_elapsed`, `stale_revival`, `refused`,
    /// `forbidden`, `unauthenticated`, `upgrade_required`, `unavailable`, `malformed`
    /// ([`LifecycleOpError::kind`]). The library's: `not_found` (the library holds no such
    /// asset), `not_in_trash` (a restore of an asset that is not in the trash), `library` (the
    /// record could not be signed or written).
    pub kind: &'static str,
    /// The stable `error.*` code, when there is one: the server's, or
    /// `error.upload.retention_elapsed` when the library already knew the window had closed.
    pub code: Option<String>,
    /// The English detail.
    pub message: String,
    /// Whether running the same command again can only fail the same way. A permanent failure
    /// is not retried; a `retention_elapsed` one is final for the asset, which stays in the
    /// trash on the server until the purge removes it.
    pub permanent: bool,
    /// On a `retention_elapsed` failure, the signed retention floor the restore came too late
    /// for (RFC 3339): the one the delete it would undo carries, which is the one the server
    /// judges.
    pub retention_until: Option<String>,
}

/// What a `capsule trash` run did for one asset — one line of the `--json` report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrashOutcome {
    /// The asset id.
    pub asset_id: String,
    /// Where the asset ended up.
    pub status: TrashStatus,
    /// The feed position the applied op occupies.
    pub sync_seq: Option<u64>,
    /// Whether the server had applied this op already (a resend after a lost answer).
    pub replayed: bool,
    /// Why the op was not applied, for a [`TrashStatus::Failed`] asset only.
    pub error: Option<TrashFailure>,
    /// For a delete, the signed retention floor (RFC 3339) of the delete record that was sent.
    /// An asset whose latest record already is a delete (a rerun, or a delete `capsule cull
    /// --sweep` signed) has that record resent with the window it was signed with, so this can
    /// differ from what `--retain-days` asked for. `None` for a restore.
    pub retention_until: Option<String>,
    /// For a delete the library signed, what aborting the asset's open upload sessions did.
    /// `None` for a restore, and for an asset whose delete was not signed.
    pub redaction: Option<TrashRedaction>,
    /// Why the records were held, for a [`TrashStatus::Held`] asset only.
    pub held: Option<TrashHold>,
}

/// What a delete did to its asset's in-flight upload — the redaction the deletion state
/// machine requires before the `delete` op is sent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct TrashRedaction {
    /// Open upload sessions of the asset the server cancelled.
    pub cancelled: usize,
    /// Sessions whose finalization was already running, so the server would not cancel them
    /// (`409`). Their blob lands, and the `delete` sent after supersedes the create.
    pub finalizing: usize,
    /// Why the sessions could not be listed or cancelled, when they could not. The delete is
    /// still sent: this device uploads nothing more of a trashed asset either way, and a
    /// session nobody drives is evicted by the server.
    pub error: Option<String>,
}

/// The outcome of a `capsule trash` run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrashSummary {
    /// The lifecycle op every asset was sent.
    pub action: TrashAction,
    /// Assets whose op the server applied.
    pub applied: usize,
    /// Assets whose record is signed and held in the library, not sent; each is in `outcomes`
    /// with why.
    pub held: usize,
    /// Assets whose op was not applied; each is in `outcomes` with its error.
    pub failed: usize,
    /// One entry per asset, in the order given.
    pub outcomes: Vec<TrashOutcome>,
}

impl TrashSummary {
    /// The assets whose op was not applied.
    pub fn failures(&self) -> impl Iterator<Item = &TrashOutcome> {
        self.outcomes
            .iter()
            .filter(|outcome| outcome.status == TrashStatus::Failed)
    }
}

/// Move `request.assets` to the trash, or restore them from it, in the library and on the
/// server: sign the lifecycle record locally, then send it to `POST /v1/albums/{album}/ops`.
///
/// An asset whose chain head already is the record this run would sign (a run whose send did
/// not land) has nothing new signed. No op is retried within the run, and a
/// [`permanent`](TrashFailure::permanent) failure is one a re-run only repeats.
///
/// **A record goes only to a server that has indexed the asset's create** (Organization —
/// Deletion State Machine). The run drains the feed once, fresh: an asset it does not list is
/// [`Held`](TrashStatus::Held) (`not_indexed`), its record kept in the library and sent by a
/// later `capsule trash` or `capsule push` that finds the create indexed, whichever device
/// uploaded it. A listed asset is sent every record after the head the server holds, in chain
/// order, so a delete held earlier reaches the server before the restore that undoes it; where
/// the server already holds the head, the head is resent and answered as a replay. A restore
/// that undoes a held delete whose window has closed is withheld for good (`restore_too_late`).
///
/// **A delete redacts the asset's in-flight upload first** ([`push::redact_asset`]): every open
/// upload session the server lists for it is cancelled before the `delete` op is sent, and a
/// session whose finalization was already running is left to land, so the `delete` supersedes
/// the create it completes. `capsule push` then skips the trashed asset, so no pending tier of
/// it is ever opened; a restore returns it to Live and so to the next push.
///
/// A restore whose signed retention floor has passed by this device's clock is refused before
/// anything is signed (`retention_elapsed`), so the library never holds a restore it knows the
/// server will refuse. The server's clock is the one that decides, so a restore this device
/// still thought was in time can be refused the same way; the library then keeps its signed
/// restore (nothing is lost, and the original stays in the library), while the server keeps the
/// asset in the trash.
///
/// # Errors
///
/// [`RemoteError::NotAuthenticated`] with no stored session. Every per-asset failure is an
/// outcome in the summary, not an error.
#[instrument(skip(store, workspace), fields(action = ?request.action, assets = request.assets.len()))]
pub async fn trash(
    remote: &RemoteConfig,
    store: &SessionStore,
    workspace: &mut Workspace,
    request: &TrashRequest,
) -> Result<TrashSummary, RemoteError> {
    let session = resume_session(remote, store).await?;
    let summary = trash_with(remote, workspace, session.clone(), request).await;
    checkpoint(store, &session).await;
    Ok(summary)
}

/// The trash run itself, over a session somebody else is responsible for persisting.
async fn trash_with(
    remote: &RemoteConfig,
    workspace: &mut Workspace,
    session: Session,
    request: &TrashRequest,
) -> TrashSummary {
    let mut summary = TrashSummary {
        action: request.action,
        applied: 0,
        held: 0,
        failed: 0,
        outcomes: Vec::new(),
    };
    let assets = distinct(request.assets.clone());
    if assets.is_empty() {
        return summary;
    }
    let upload = UploadClient::new(UploadTransport::with_session(
        session.clone(),
        &remote.upload_endpoint,
        &remote.protocol_version,
    ));
    // Which assets the server has indexed, and the head it holds for each: read once, fresh, so
    // a record goes only to a server that has the create it continues.
    let chains = server_chains(remote, session.clone()).await;
    for asset_id in assets {
        let now = jiff::Timestamp::now();
        let outcome = match prepare_trash_op(workspace, &asset_id, request, now) {
            Err(failure) => failed_trash(asset_id, failure),
            Ok(prepared) => {
                // The window a delete actually carries: the sent record's, which a resend
                // keeps whatever `--retain-days` says.
                let sent_until = (request.action == TrashAction::Delete)
                    .then(|| prepared.retention_until.clone())
                    .flatten();
                // Redaction first: no further byte of a deleted asset leaves this device, and a
                // session already finalizing lands before the delete that supersedes it.
                let redaction = if request.action == TrashAction::Delete {
                    Some(redact_upload(&upload, asset_id).await)
                } else {
                    None
                };
                let delivered = match &chains {
                    Ok(chains) => {
                        deliver_chain(remote, &session, workspace, &asset_id, chains, now).await
                    }
                    Err(message) => Delivered::Failed(OpFailure {
                        kind: "unavailable",
                        code: None,
                        message: format!(
                            "the server's feed could not be read, so whether it has indexed the \
                             asset is unknown; the record is held in the library and nothing was \
                             sent: {message}"
                        ),
                        permanent: false,
                    }),
                };
                TrashOutcome {
                    retention_until: sent_until,
                    redaction,
                    ..trash_outcome(asset_id, delivered, prepared.retention_until)
                }
            }
        };
        match outcome.status {
            TrashStatus::Applied => summary.applied += 1,
            TrashStatus::Held => summary.held += 1,
            TrashStatus::Failed => summary.failed += 1,
        }
        summary.outcomes.push(outcome);
    }
    tracing::info!(
        applied = summary.applied,
        held = summary.held,
        failed = summary.failed,
        "trash: run finished"
    );
    summary
}

fn failed_trash(asset_id: Uuid, failure: TrashFailure) -> TrashOutcome {
    TrashOutcome {
        asset_id: asset_id.to_string(),
        status: TrashStatus::Failed,
        sync_seq: None,
        replayed: false,
        error: Some(failure),
        retention_until: None,
        redaction: None,
        held: None,
    }
}

/// One asset's outcome from what delivering its chain did. `trashed_until` is the signed
/// window of the delete the asset is in the trash under, which a `retention_elapsed` refusal
/// reports.
fn trash_outcome(
    asset_id: Uuid,
    delivered: Delivered,
    trashed_until: Option<String>,
) -> TrashOutcome {
    match delivered {
        Delivered::Applied(applied) => TrashOutcome {
            asset_id: asset_id.to_string(),
            status: TrashStatus::Applied,
            sync_seq: Some(applied.sync_seq),
            replayed: applied.replayed,
            error: None,
            retention_until: None,
            redaction: None,
            held: None,
        },
        Delivered::Held(hold) => TrashOutcome {
            asset_id: asset_id.to_string(),
            status: TrashStatus::Held,
            sync_seq: None,
            replayed: false,
            error: None,
            retention_until: None,
            redaction: None,
            held: Some(hold),
        },
        Delivered::Failed(failure) => {
            let retention_until = (failure.kind == "retention_elapsed")
                .then_some(trashed_until)
                .flatten();
            failed_trash(
                asset_id,
                TrashFailure {
                    kind: failure.kind,
                    code: failure.code,
                    message: failure.message,
                    permanent: failure.permanent,
                    retention_until,
                },
            )
        }
    }
}

/// The server's chain heads off a fresh feed drain, or why the feed could not be read.
pub(crate) async fn server_chains(
    remote: &RemoteConfig,
    session: Session,
) -> Result<held::ServerChains, String> {
    match drain_feed(remote, session, DEFAULT_SYNC_PAGE_SIZE).await {
        Ok(entries) => Ok(held::ServerChains::from_feed(&entries)),
        Err(error) => {
            tracing::warn!(%error, "held records: the feed could not be read; nothing is sent");
            Err(error.to_string())
        }
    }
}

/// What delivering one asset's chain did.
#[derive(Debug)]
pub(crate) enum Delivered {
    /// Every record sent was applied; the last one's answer.
    Applied(OpApplied),
    /// Nothing was sent.
    Held(TrashHold),
    /// A record was not applied; those after it were not sent.
    Failed(OpFailure),
}

/// Send what of `asset_id`'s chain may go to the server now ([`held::plan`]): nothing while the
/// server has not indexed the asset's create, otherwise every record after the head it holds,
/// in chain order, each once the one before it was applied. The chain is the library's
/// ([`held::local_chain`]): a purged asset's is the one its purge kept, so the held `delete` and
/// the re-issue an early purge appended are sent from the tombstone. A library holding neither
/// is a failure that is not [`permanent`](OpFailure::permanent): nothing shows the records
/// cannot be delivered by a later run.
#[instrument(skip(remote, session, workspace, chains, now))]
pub(crate) async fn deliver_chain(
    remote: &RemoteConfig,
    session: &Session,
    workspace: &Workspace,
    asset_id: &Uuid,
    chains: &held::ServerChains,
    now: jiff::Timestamp,
) -> Delivered {
    let library_failure = |message: String| {
        Delivered::Failed(OpFailure {
            kind: "library",
            code: None,
            message,
            permanent: true,
        })
    };
    // A purged asset's chain is the one its purge kept, so its held records outlive the purge.
    // A library that holds neither the asset nor its tombstone (one this build could not read,
    // say) has nothing to send now, but the records it owes the server are not shown to be
    // undeliverable: the failure is not permanent, so a caller that keeps work for a later run
    // keeps it rather than dropping a delete the server never received.
    let Some(chain) = held::local_chain(workspace, asset_id) else {
        tracing::warn!(%asset_id, "held records: the library holds neither the asset nor its tombstone; nothing sent");
        return Delivered::Failed(OpFailure {
            kind: "library",
            code: None,
            message: format!("the library holds no asset {asset_id}"),
            permanent: false,
        });
    };
    let records = chain.records;
    let stretch = match held::plan(records, chains.head(asset_id), chains.judging_clock(now)) {
        held::Delivery::Held {
            reason,
            retention_until,
        } => {
            tracing::info!(%asset_id, ?reason, retention_until = ?retention_until, "held records: nothing sent");
            return Delivered::Held(TrashHold {
                reason,
                retention_until,
            });
        }
        held::Delivery::Send(stretch) => stretch,
    };
    let mut last = None;
    for record in stretch {
        let op = match encode_op(asset_id, chain.metadata_blob, record) {
            Ok(op) => op,
            Err(error) => return library_failure(error.to_string()),
        };
        match send_op(remote, session.clone(), &op).await {
            Ok(applied) => {
                tracing::info!(%asset_id, action = %applied.action, sync_seq = applied.sync_seq, replayed = applied.replayed, "held records: op applied");
                last = Some(applied);
            }
            Err(failure) => {
                tracing::warn!(%asset_id, kind = failure.kind, code = ?failure.code, permanent = failure.permanent, error = %failure.message, "held records: op not applied; the rest of the stretch is not sent");
                return Delivered::Failed(failure);
            }
        }
    }
    last.map_or_else(
        || library_failure(format!("asset {asset_id} has no provenance")),
        Delivered::Applied,
    )
}

/// Abort `asset_id`'s open upload sessions before its `delete` is sent, so this device sends no
/// further byte of it. Best-effort: a failure is reported in the outcome, never a reason to
/// withhold the delete.
async fn redact_upload(upload: &UploadClient, asset_id: Uuid) -> TrashRedaction {
    match push::redact_asset(upload, &asset_id.to_string()).await {
        Ok(redaction) => TrashRedaction {
            cancelled: redaction.cancelled.len(),
            finalizing: redaction.finalizing.len(),
            error: None,
        },
        Err(error) => {
            tracing::warn!(%asset_id, %error, "trash: the asset's open upload sessions could not be aborted; sending the delete anyway");
            TrashRedaction {
                error: Some(error.to_string()),
                ..TrashRedaction::default()
            }
        }
    }
}

/// One asset's record, signed in the library and ready to deliver.
#[derive(Debug)]
struct PreparedTrashOp {
    /// The signed retention floor of the delete the asset is in the trash under, when it is.
    retention_until: Option<String>,
}

/// Sign `request.action` for `asset_id` in the library, unless its chain head already is such
/// a record. `now` is this device's clock, against which a restore's retention floor is checked
/// before anything is signed. What of the chain is then sent is [`deliver_chain`]'s to decide.
fn prepare_trash_op(
    workspace: &mut Workspace,
    asset_id: &Uuid,
    request: &TrashRequest,
    now: jiff::Timestamp,
) -> Result<PreparedTrashOp, TrashFailure> {
    use capsule_core::crypto::provenance::Action;
    let local = |kind: &'static str, message: String| TrashFailure {
        kind,
        code: None,
        message,
        permanent: true,
        retention_until: None,
    };
    let Some(asset) = workspace.asset(asset_id) else {
        return Err(local(
            "not_found",
            format!("the library holds no asset {asset_id}"),
        ));
    };
    let head = asset
        .chain
        .records()
        .last()
        .map(|record| record.manifest.core.action);
    let retention_until = trashed_until(asset);
    let wanted = request.action.action();
    if head == Some(wanted) {
        // A delete head keeps the window it was signed with: `retain_days` signs only a fresh
        // delete, and a second delete over the first would not chain onto the server's head.
        tracing::debug!(%asset_id, action = ?wanted, retention_until = ?retention_until, "trash: the head already is this record; resending it");
    } else {
        let signed = match request.action {
            TrashAction::Delete => workspace.soft_delete(asset_id, request.retain_days),
            TrashAction::TrashRestore => {
                if head != Some(Action::Delete) {
                    return Err(local(
                        "not_in_trash",
                        format!("asset {asset_id} is not in the trash"),
                    ));
                }
                let elapsed = retention_until
                    .as_deref()
                    .and_then(|until| until.parse::<jiff::Timestamp>().ok())
                    .is_some_and(|until| now >= until);
                if elapsed {
                    let until = retention_until.unwrap_or_default();
                    tracing::warn!(%asset_id, retention_until = %until, "trash: the retention window has closed; nothing signed or sent");
                    return Err(TrashFailure {
                        kind: "retention_elapsed",
                        code: Some(capsule_i18n::error_codes::UPLOAD_RETENTION_ELAPSED.to_owned()),
                        message: format!(
                            "asset {asset_id}'s retention window closed at {until}, so it can no \
                             longer be restored"
                        ),
                        permanent: true,
                        retention_until: Some(until),
                    });
                }
                workspace.restore(asset_id)
            }
        };
        signed.map_err(|error| local("library", error.to_string()))?;
        tracing::info!(%asset_id, action = ?wanted, "trash: signed the record in the library");
    }
    let asset = workspace.asset(asset_id).ok_or_else(|| {
        local(
            "not_found",
            format!("the library holds no asset {asset_id}"),
        )
    })?;
    Ok(PreparedTrashOp {
        retention_until: trashed_until(asset),
    })
}

/// The signed retention floor of the latest `delete` in `asset`'s chain: the one a restore
/// undoes, and so the one the server judges it against.
fn trashed_until(asset: &capsule_library::lifecycle::AssetState) -> Option<String> {
    latest_delete_until(asset.chain.records())
}

/// The signed `retention_until` of the latest `delete` in `records` (a chain, oldest first).
fn latest_delete_until(
    records: &[capsule_core::crypto::provenance::ProvenanceRecord],
) -> Option<String> {
    records
        .iter()
        .rev()
        .find(|record| {
            record.manifest.core.action == capsule_core::crypto::provenance::Action::Delete
        })
        .and_then(|record| record.manifest.core.retention_until.clone())
}

/// The one-line human summary of a `capsule trash` run.
#[must_use]
pub fn trash_line(summary: &TrashSummary) -> String {
    let verb = match summary.action {
        TrashAction::Delete => "moved to the trash",
        TrashAction::TrashRestore => "restored from the trash",
    };
    let noun = if summary.applied == 1 {
        "asset"
    } else {
        "assets"
    };
    let mut line = format!("{} {noun} {verb} on the server", summary.applied);
    if summary.held > 0 {
        let _ = write!(
            line,
            ", {} held in the library until the server may take it",
            summary.held
        );
    }
    if summary.failed > 0 {
        let _ = write!(line, ", {} failed", summary.failed);
    }
    line
}

/// The human line for one asset whose op was not applied.
#[must_use]
pub fn trash_failure_line(outcome: &TrashOutcome) -> String {
    let Some(error) = &outcome.error else {
        return format!("{}: failed", outcome.asset_id);
    };
    let mut line = format!("{}: {} ({})", outcome.asset_id, error.message, error.kind);
    if error.permanent {
        line.push_str("; not retried");
    }
    line
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

/// The custody question for each distinct asset behind `moved`, read off its chain head as the
/// release gate declares it ([`HeldRelease::for_head`]), so an asset edited since its create is
/// asked about like any other. An asset the library does not hold is left out, so the verifier
/// cannot answer for it and its source is kept.
fn held_releases(workspace: &Workspace, moved: &[MovedSource]) -> Vec<HeldRelease> {
    let mut seen = HashSet::new();
    moved
        .iter()
        .filter(|m| seen.insert(m.asset_id))
        .filter_map(|m| {
            let release = workspace.asset(&m.asset_id).map(HeldRelease::for_head);
            if release.is_none() {
                tracing::warn!(asset_id = %m.asset_id, "move release: the library does not hold the asset; the source is kept");
            }
            release
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

pub(crate) mod held;

pub use held::HoldReason;

#[cfg(test)]
pub(crate) mod tests;
