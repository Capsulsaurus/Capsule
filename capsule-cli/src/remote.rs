//! The CLI's networked commands — `auth login/logout`, `sync`, `list` — expressed
//! entirely over `capsule-sdk` (slice `S-D5`).
//!
//! The CLI is a client: it **never hand-rolls a network flow**. Every request here
//! goes through an SDK primitive — [`AuthClient`]/[`Session`] for auth, and
//! [`SyncConsumer`]/[`SyncState`] for the feed — so token refresh, the protocol
//! handshake, the `401`/`Unauthenticated` retry, and the anti-rewind contract are
//! the SDK's, not re-implemented here. These functions are the testable core the
//! binary's command arms call and the E2E round-trip drives directly.

use std::collections::HashSet;

use capsule_core::crypto::keys::MasterKey;
use capsule_core::crypto::primitives::{Argon2Params, DeviceTier};
use capsule_core::import::UploadPolicy;
use capsule_core::lifecycle::{LifecycleError, Workspace};
use capsule_sdk::albums::{AlbumClient, AlbumTransport};
use capsule_sdk::auth::{AuthClient, AuthError, LoginOutcome, Session};
use capsule_sdk::client::AuthenticatedClient;
use capsule_sdk::directory::{DirectoryClient, DirectoryError};
use capsule_sdk::net::ConnectionClass;
use capsule_sdk::push::{self, PushError};
use capsule_sdk::recovery::{MintedSecret, RecoveryClient, RecoveryError};
use capsule_sdk::staged::{StagedScheduler, TierSessionOutcome, held_from_feed};
use capsule_sdk::sync::{SyncConsumer, SyncCursor, SyncError, SyncState};
use capsule_sdk::upload::{UploadClient, UploadTransport};
use capsule_sdk::verify::{StorageVerifyClient, VerifyError, VerifyTransport};
use sea_orm::{ConnectionTrait, TransactionTrait};
use secrecy::SecretString;
use serde::Serialize;
use thiserror::Error;
use tracing::instrument;
use uuid::Uuid;

use crate::session::{SessionError, SessionStore};
use crate::syncstore::{self, SyncStoreError, SyncedAssetView};

/// The client's default max-known protocol version — the top of the dev sync
/// server's negotiation window.
pub const DEFAULT_PROTOCOL_VERSION: &str = "2026-12-31";

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
    /// there (slice `S-C25`), and the lifecycle-write surface hangs under
    /// `{albums_endpoint}/{album_id}/ops`.
    pub albums_endpoint: String,
    /// The client's max-known protocol version (`YYYY-MM-DD`), sent on the
    /// handshake and used as the forward-version ceiling.
    pub protocol_version: String,
}

/// The default server origin — one host, one port, matching `mise run serve-memory`.
pub const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:3000";

impl RemoteConfig {
    /// Build from `CAPSULE_ENDPOINT` (one origin for the whole server), with
    /// `CAPSULE_AUTH_ENDPOINT` / `CAPSULE_SYNC_ENDPOINT` / `CAPSULE_PROTOCOL` as
    /// per-surface overrides for split deployments.
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
            protocol_version: std::env::var("CAPSULE_PROTOCOL")
                .unwrap_or_else(|_| DEFAULT_PROTOCOL_VERSION.to_string()),
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
        // The sign-in is not finished (`S-C55`). Nothing is persisted: there is no session yet,
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
/// 2. fetch the escrow and the sealed identity (`GET /v1/auth/escrow`,
///    `GET /v1/auth/escrow/identity`) — both before any key derivation, so an account that
///    never stored its identity fails at once rather than after an Argon2id pass;
/// 3. unwrap the master key with the recovery secret ([`recovery_secret_candidates`] says how
///    the typed secret is read);
/// 4. create the library at `library` with `Workspace::create_from_master`, which opens the
///    sealed identity under the recovered master key and refuses before writing anything if it
///    does not open;
/// 5. join the account's device directory (#625): fetch the
///    account's signed directory (`GET /v1/auth/devices/directory/{user_id}`), verify it under
///    the identity key the sealed identity opened, append the new device with
///    `Workspace::extend_account_directory` and publish it at the stored version + 1. An
///    account that never published one gets the new library's own, at version 1.
///
/// Step 5 is part of the recovery, not a follow-up: a device the directory does not list signs
/// nothing the account's other devices accept. So a failure there **removes the library step 4
/// created** — it holds nothing but the keys minted for it moments earlier — and returns the
/// error, which leaves the path as it was and makes rerunning the command the retry. Every
/// refusal before step 4 writes nothing.
///
/// # Errors
///
/// [`RemoteError::NotAuthenticated`] with no stored session; [`RemoteError::Recovery`] for an
/// escrow or sealed-identity refusal (`IdentityNotStored` carries its own code);
/// [`RemoteError::WrongRecoverySecret`] when no reading of the secret opens the escrow;
/// [`RemoteError::Lifecycle`] when the library cannot be created, or when the account's
/// directory cannot be extended with the new device; [`RemoteError::Directory`] when the
/// account's directory cannot be fetched, does not verify, or the publish is refused.
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
    tracing::info!(%user_id, "recover: fetched the escrow and the sealed identity");

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
    match join_account_directory(&directory, &mut workspace).await {
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
                "recover: the new device could not join the account's directory; removing the library"
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
///    then store the sealed identity beside it (`PUT /v1/auth/escrow/identity`).
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
    tracing::info!(
        %user_id,
        escrow_replaced = stored.replaced,
        identity_replaced = identity.replaced,
        "escrow: the escrow and the sealed identity are stored"
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
#[derive(Debug, Clone, Default)]
pub struct SyncSummary {
    /// Total feed entries applied across all pages.
    pub applied: usize,
    /// Number of distinct albums touched this run.
    pub albums: usize,
    /// Number of feed pages drained.
    pub pages: usize,
    /// Whether this was a dry run (nothing persisted).
    pub dry_run: bool,
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
    // uses, so there is no second connection to dial and no second transport to configure
    // (`S-D28`).
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

    loop {
        // `pull_into` validates the page against the in-memory `SyncState`
        // (forward-version + anti-rewind) and advances its cursor before returning.
        let page = consumer.pull_into(&mut state, page_size).await?;
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
        }
        summary.pages += 1;
    }

    summary.albums = albums.len();
    tracing::info!(
        applied = summary.applied,
        albums = summary.albums,
        pages = summary.pages,
        dry_run,
        "sync drained the feed"
    );
    Ok(summary)
}

// ─── Push (S-D18) ────────────────────────────────────────────────────────────

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
}

/// Why an asset failed, in the shape `--json` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AssetPushFailure {
    /// A stable name for where the asset stopped: `bundle`, `album`, `upload`, `unknown_blob`,
    /// `verify_unavailable`, `not_durable`, `receipt_missing`, `receipt_rejected` or
    /// `receipt_store`.
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
    ReceiptStore(#[from] capsule_core::library::ReceiptStoreError),
}

impl AssetError {
    fn failure(&self) -> AssetPushFailure {
        let (kind, code) = match self {
            Self::Bundle(_) => ("bundle", None),
            Self::Push(error) => (error.kind(), error.error_code().map(str::to_owned)),
            Self::ReceiptStore(_) => ("receipt_store", None),
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
    /// Distinct albums registered with the server this run (slice `S-C25`). Provisioning is
    /// idempotent, so this counts requests made, not rows created.
    pub provisioned_albums: usize,
    /// How many of those registrations actually created a server-side album row. Zero on a
    /// re-run — the album was already bound.
    pub created_albums: usize,
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
    let consumer = SyncConsumer::with_session(&remote.sync_endpoint, session)?;
    // A scratch state: start cursor, no high-water marks. Nothing is persisted from it.
    let mut state = SyncState::restore(&remote.protocol_version, SyncCursor::start(), Vec::new());

    let mut held = HashSet::new();
    let mut entries = 0usize;
    loop {
        let page = consumer.pull_into(&mut state, page_size).await?;
        if page.entries.is_empty() {
            break;
        }
        for entry in &page.entries {
            entries += 1;
            held.extend(held_from_feed(entry));
        }
    }
    tracing::info!(
        entries,
        blobs = held.len(),
        "push: server truth read off the sync feed"
    );
    Ok(held)
}

/// Push every managed asset of `workspace` to the server.
///
/// The CLI never hand-rolls a network flow: the album is provisioned through
/// `capsule_sdk::push::ensure_album`, the bytes ride `capsule_sdk::upload`'s resumable client,
/// the ordering rides `capsule_sdk::staged`'s scheduler, and the bundle comes from
/// `capsule_core`'s `Workspace::upload_bundle` — this function only stitches them together.
///
/// **Provisioning comes first** (slice `S-C25`). An asset's album id is derived from the
/// account master key, so the server has never heard of it; invariant 6 refuses an upload
/// into an album that does not exist or is not writable by the caller. Each *distinct* album
/// the run touches is registered once, lazily, before its first blob session opens — and
/// because provisioning is idempotent, a second `capsule push` re-registers the same id, gets
/// the same success, and still moves nothing.
///
/// **A transfer is not custody** (issue #670). After an asset's ladder, the run asks the server
/// whether it durably holds every blob the asset relies on and requires a custody receipt per
/// blob that verifies under the server's published attestation key
/// (`capsule_sdk::push::verify_custody`); the verified receipts are appended to the asset's
/// `{uuid}.receipts.cbor` log in the library. Only then is the asset `Verified`.
///
/// **One asset's failure is that asset's.** An asset whose bundle, album, transfer, custody
/// check or receipt write fails is recorded as `Failed` with its error, and the run moves on to
/// the next one; the caller reports every failure and exits non-zero. What fails the whole run
/// is what every asset needs: the session, the feed read, and the attestation keys.
///
/// Re-runnable by construction. The outstanding transfer work comes from server truth, so a
/// second run against an unchanged library uploads nothing; it re-confirms custody of every
/// asset, and re-persisting a receipt already in the log is a no-op.
#[instrument(skip(store, workspace), fields(endpoint = %remote.upload_endpoint))]
pub async fn push(
    remote: &RemoteConfig,
    store: &SessionStore,
    workspace: &Workspace,
    options: PushOptions,
) -> Result<PushSummary, RemoteError> {
    let session = resume_session(remote, store).await?;
    let outcome = push_with(remote, workspace, session.clone(), options).await;
    checkpoint(store, &session).await;
    outcome
}

/// The push itself, over a session somebody else is responsible for persisting.
async fn push_with(
    remote: &RemoteConfig,
    workspace: &Workspace,
    session: Session,
    options: PushOptions,
) -> Result<PushSummary, RemoteError> {
    let asset_ids = workspace.asset_ids();
    let mut summary = PushSummary {
        assets: asset_ids.len(),
        dry_run: options.dry_run,
        ..Default::default()
    };
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
    .with_connection_class(options.connection);
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
            let outcome = plan_asset(workspace, &scheduler, &held, options.force, asset_id);
            summary.record(outcome);
        }
        summary.log();
        return Ok(summary);
    }

    // Every receipt is checked under the server's published key history, so without it no
    // asset could be confirmed: that is a failure of the run, not of an asset.
    let attestation_keys = verify
        .fetch_attestation_keys()
        .await
        .map_err(RemoteError::AttestationKeys)?;
    tracing::info!(
        keys = attestation_keys.len(),
        "push: attestation keys fetched"
    );

    let mut run = AssetPusher {
        workspace,
        upload,
        albums,
        verify,
        attestation_keys,
        scheduler,
        held,
        force: options.force,
        provisioned: HashSet::new(),
    };
    for asset_id in asset_ids {
        let mut outcome = AssetPushOutcome::new(asset_id, AssetPushStatus::Verified);
        if let Err(error) = run.push(asset_id, &mut outcome, &mut summary).await {
            tracing::warn!(%asset_id, %error, "push: asset failed; continuing with the next");
            outcome.status = AssetPushStatus::Failed;
            outcome.error = Some(error.failure());
        }
        summary.record(outcome);
    }
    summary.log();
    Ok(summary)
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

/// Everything a run shares across its assets.
struct AssetPusher<'a> {
    workspace: &'a Workspace,
    upload: UploadClient,
    albums: AlbumClient,
    verify: StorageVerifyClient,
    attestation_keys: Vec<capsule_core::crypto::keys::HybridVerifyingKey>,
    scheduler: StagedScheduler,
    held: HashSet<String>,
    force: bool,
    /// Albums registered this run. Provisioning is idempotent server-side, so this only saves
    /// making the same request once per asset; an album whose registration failed is not in
    /// it, so the next asset in that album tries again.
    provisioned: HashSet<Uuid>,
}

impl AssetPusher<'_> {
    /// Push one asset, confirm custody, and persist its receipts, filling `outcome` once each
    /// step completes. A transfer that fails partway leaves `uploaded`, `merged` and `bytes` at
    /// zero for the asset even when some of its blobs landed, because [`push::push_bundle`]
    /// returns only its error; a re-run derives the remaining work from server truth.
    #[instrument(skip(self, outcome, summary), fields(%asset_id))]
    async fn push(
        &mut self,
        asset_id: Uuid,
        outcome: &mut AssetPushOutcome,
        summary: &mut PushSummary,
    ) -> Result<(), AssetError> {
        let bundle = self.workspace.upload_bundle(&asset_id)?;

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

        let report = push::push_bundle(
            &self.upload,
            &self.scheduler,
            &bundle,
            &self.held,
            self.force,
        )
        .await?;
        outcome.already_held = report.already_held;
        outcome.deferred = report.deferred;
        outcome.bytes = report.bytes;
        for blob in &report.pushed {
            match blob.outcome {
                TierSessionOutcome::Uploaded { .. } => outcome.uploaded += 1,
                TierSessionOutcome::AlreadyStored { .. } => outcome.merged += 1,
            }
        }

        let custody = push::verify_custody(
            &self.verify,
            &self.attestation_keys,
            &bundle,
            &report,
            jiff::Timestamp::now().as_second(),
        )
        .await?;

        // The receipt log lives beside the provenance chain, in the same capture-month shard.
        let capture_utc = self
            .workspace
            .asset(&asset_id)
            .map(|asset| asset.capture_utc);
        for receipt in &custody.receipts {
            capsule_core::library::append_receipt(self.workspace.root(), capture_utc, receipt)?;
            outcome.receipts += 1;
        }
        tracing::info!(
            receipts = outcome.receipts,
            checked_at = %custody.checked_at,
            "push: custody verified and receipts persisted"
        );
        Ok(())
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
            pushed_assets = self.pushed_assets,
            verified_assets = self.verified_assets,
            failed_assets = self.failed_assets,
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
    format!(
        "Custody verified for {} of {} assets; {} receipts stored in the library.",
        summary.verified_assets, summary.assets, summary.receipts
    )
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

/// Query the sync-fed local store — the client-side library `capsule list` renders.
#[instrument(skip(db))]
pub async fn list<C: ConnectionTrait>(
    db: &C,
    include_tombstoned: bool,
) -> Result<Vec<SyncedAssetView>, RemoteError> {
    Ok(syncstore::list_assets(db, include_tombstoned).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The write-back that closes the CLI's session-persistence bug.
    ///
    /// The server rotates on refresh and closes the old session, so a command that refreshed and
    /// then dropped the rotated pair left the store holding a dead refresh token — and roughly
    /// fifteen minutes after signing in every command demanded an interactive login. This is the
    /// plumbing half: whatever the session holds when the work is done reaches the store.
    #[tokio::test]
    async fn a_checkpoint_writes_the_sessions_current_pair_back_to_the_store() {
        let dir =
            std::env::temp_dir().join(format!("capsule-cli-checkpoint-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let store = SessionStore::new(dir.join("session.json"));

        let persisted = capsule_sdk::auth::PersistedSession {
            access_token: "access-1".to_owned().into(),
            refresh_token: "refresh-1".to_owned().into(),
            access_expires_at_unix: jiff::Timestamp::now().as_second() + 3600,
        };
        store.save(&persisted).expect("the store writes");

        let client = AuthClient::new("http://127.0.0.1:1/v1/auth").expect("a base url");
        let expected_refresh = {
            use secrecy::ExposeSecret as _;
            persisted.refresh_token.expose_secret().to_owned()
        };
        let session = client.resume(persisted).expect("a resumed session");
        checkpoint(&store, &session).await;

        let reloaded = store
            .load()
            .expect("the store reads")
            .expect("a session is stored");
        use secrecy::ExposeSecret as _;
        assert_eq!(reloaded.refresh_token.expose_secret(), expected_refresh);

        // And a session with nothing left to export leaves the store as it was, rather than
        // clearing it: logging out is `auth_logout`'s job and nobody else's.
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `from_env` reads process-wide state, so these run under one lock and restore what they
    /// touched — otherwise a parallel test would see another's variables.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Clear every endpoint variable, run `f`, and restore the prior values.
    fn with_clean_env<T>(f: impl FnOnce() -> T) -> T {
        const VARS: [&str; 5] = [
            "CAPSULE_ENDPOINT",
            "CAPSULE_AUTH_ENDPOINT",
            "CAPSULE_SYNC_ENDPOINT",
            "CAPSULE_UPLOAD_ENDPOINT",
            "CAPSULE_PROTOCOL",
        ];
        let guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let saved: Vec<_> = VARS.iter().map(|k| (*k, std::env::var(k).ok())).collect();
        for k in VARS {
            unsafe { std::env::remove_var(k) };
        }
        let out = f();
        for (k, v) in saved {
            match v {
                Some(v) => unsafe { std::env::set_var(k, v) },
                None => unsafe { std::env::remove_var(k) },
            }
        }
        drop(guard);
        out
    }

    #[test]
    fn endpoint_defaults_target_a_single_origin() {
        let cfg = with_clean_env(RemoteConfig::from_env);
        // The old defaults were :8080 and :8081 — two ports that exist only in the
        // integration harness. A real server serves everything from one.
        assert_eq!(cfg.auth_endpoint, "http://127.0.0.1:3000/v1/auth");
        assert_eq!(cfg.sync_endpoint, "http://127.0.0.1:3000");
        assert_eq!(cfg.upload_endpoint, "http://127.0.0.1:3000/v1/upload");
        assert_eq!(cfg.protocol_version, DEFAULT_PROTOCOL_VERSION);
    }

    #[test]
    fn a_single_base_derives_every_surface() {
        let cfg = with_clean_env(|| {
            unsafe { std::env::set_var("CAPSULE_ENDPOINT", "https://capsule.example.com") };
            RemoteConfig::from_env()
        });
        assert_eq!(cfg.auth_endpoint, "https://capsule.example.com/v1/auth");
        assert_eq!(cfg.sync_endpoint, "https://capsule.example.com");
        assert_eq!(cfg.upload_endpoint, "https://capsule.example.com/v1/upload");
    }

    #[test]
    fn a_trailing_slash_does_not_double_up() {
        let cfg = with_clean_env(|| {
            unsafe { std::env::set_var("CAPSULE_ENDPOINT", "http://host:3000/") };
            RemoteConfig::from_env()
        });
        assert_eq!(cfg.auth_endpoint, "http://host:3000/v1/auth");
        assert_eq!(cfg.sync_endpoint, "http://host:3000");
        assert_eq!(cfg.upload_endpoint, "http://host:3000/v1/upload");
    }

    /// Split deployments (auth behind one ingress, sync behind another) stay expressible.
    #[test]
    fn per_surface_overrides_win_over_the_base() {
        let cfg = with_clean_env(|| {
            unsafe {
                std::env::set_var("CAPSULE_ENDPOINT", "http://ignored:1");
                std::env::set_var("CAPSULE_AUTH_ENDPOINT", "https://auth.example.com/v1/auth");
                std::env::set_var("CAPSULE_SYNC_ENDPOINT", "https://sync.example.com");
                std::env::set_var(
                    "CAPSULE_UPLOAD_ENDPOINT",
                    "https://upload.example.com/v1/upload",
                );
            }
            RemoteConfig::from_env()
        });
        assert_eq!(cfg.auth_endpoint, "https://auth.example.com/v1/auth");
        assert_eq!(cfg.sync_endpoint, "https://sync.example.com");
        assert_eq!(cfg.upload_endpoint, "https://upload.example.com/v1/upload");
    }

    /// The sync endpoint must stay a bare origin. tonic's `AddOrigin` keeps only the scheme
    /// and authority of the endpoint URI and lets the generated stub write the path, so any
    /// prefix here is silently dropped and the request lands on a path the server does not
    /// serve. This is the regression guard for that.
    #[test]
    fn the_default_sync_endpoint_carries_no_path() {
        let cfg = with_clean_env(RemoteConfig::from_env);
        let after_scheme = cfg
            .sync_endpoint
            .split_once("://")
            .expect("sync endpoint has a scheme")
            .1;
        assert!(
            !after_scheme.contains('/'),
            "sync endpoint must be a bare origin, got {}",
            cfg.sync_endpoint
        );
    }

    /// A minted secret is shown as hex and wrapped under its bytes, so the decoded bytes are
    /// tried first; the text as typed is still tried, because a chosen passphrase may happen
    /// to be 64 hex characters.
    #[test]
    fn a_hex_recovery_secret_is_tried_decoded_and_then_as_typed() {
        let hex = "00ff".repeat(16);
        let candidates = recovery_secret_candidates(&format!("  {hex}\n"));
        assert_eq!(
            candidates,
            vec![[0x00, 0xff].repeat(16), hex.as_bytes().to_vec()]
        );
    }

    /// A library laid out the way a recovery leaves one: nested directories and files.
    fn lay_out_a_library(root: &std::path::Path) {
        std::fs::create_dir_all(root.join(".library/keys")).expect("the skeleton creates");
        std::fs::write(root.join(".library/keys/device.key"), b"k").expect("a key file writes");
        std::fs::write(root.join("library.toml"), b"name").expect("a top-level file writes");
    }

    /// A recovery that created its root removes it whole when the directory join fails, so the
    /// path is absent again and a rerun starts from nothing.
    #[test]
    fn a_failed_join_removes_the_root_the_recovery_created() {
        let parent = std::env::temp_dir().join(format!(
            "capsule-cli-discard-created-{}",
            std::process::id()
        ));
        let root = parent.join("recovered");
        lay_out_a_library(&root);

        discard_new_library(&root, false);

        assert!(!root.exists(), "the created root is gone");
        assert!(parent.exists(), "and nothing above it is touched");
        std::fs::remove_dir_all(&parent).ok();
    }

    /// A recovery into an empty directory the user made leaves that directory, empty, in place.
    #[test]
    fn a_failed_join_empties_a_root_that_existed_and_keeps_it() {
        let root = std::env::temp_dir().join(format!(
            "capsule-cli-discard-existing-{}",
            std::process::id()
        ));
        lay_out_a_library(&root);

        discard_new_library(&root, true);

        assert!(root.is_dir(), "the user's directory stays");
        assert_eq!(
            std::fs::read_dir(&root).expect("it reads").count(),
            0,
            "and is empty again, as it was before the recovery"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_chosen_recovery_secret_is_tried_as_typed_only() {
        assert_eq!(
            recovery_secret_candidates("seven words the user wrote down"),
            vec![b"seven words the user wrote down".to_vec()]
        );
        // Hex of the wrong length is not a minted secret.
        assert_eq!(recovery_secret_candidates("00ff"), vec![b"00ff".to_vec()]);
        // 64 characters that are not hex.
        let not_hex = "z".repeat(64);
        assert_eq!(
            recovery_secret_candidates(&not_hex),
            vec![not_hex.as_bytes().to_vec()]
        );
    }

    /// A verified asset and a failed one, as a run that carried on past the failure records them.
    fn mixed_run() -> PushSummary {
        let mut summary = PushSummary {
            assets: 2,
            ..Default::default()
        };
        let mut verified = AssetPushOutcome::new(Uuid::from_u128(1), AssetPushStatus::Verified);
        verified.uploaded = 3;
        verified.bytes = 4096;
        verified.receipts = 3;
        summary.record(verified);

        let mut failed = AssetPushOutcome::new(Uuid::from_u128(2), AssetPushStatus::Failed);
        failed.uploaded = 2;
        failed.error = Some(
            AssetError::from(PushError::NotDurable {
                asset_id: Uuid::from_u128(2).to_string(),
                not_durable: vec!["ab".repeat(32)],
            })
            .failure(),
        );
        summary.record(failed);
        summary
    }

    /// One asset's failure is that asset's: the run's totals count both, the failure is listed,
    /// and the run is reported as failed — which is what makes the command exit non-zero.
    #[test]
    fn a_failed_asset_is_counted_listed_and_fails_the_run() {
        let summary = mixed_run();
        assert_eq!(summary.verified_assets, 1);
        assert_eq!(summary.failed_assets, 1);
        assert_eq!(summary.pushed_assets, 2, "both opened sessions");
        assert_eq!(summary.uploaded_blobs, 5);
        assert_eq!(summary.receipts, 3);

        let failures: Vec<_> = summary.failures().collect();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].asset_id, Uuid::from_u128(2).to_string());
        assert!(failure_line(failures[0]).contains("does not durably hold"));
        assert_eq!(
            failed_run_reason(&summary).as_deref(),
            Some("1 of 2 assets did not reach verified custody")
        );
        assert_eq!(
            custody_line(&summary),
            "Custody verified for 1 of 2 assets; 3 receipts stored in the library."
        );
    }

    /// A run with no failure exits zero.
    #[test]
    fn a_run_with_no_failure_is_not_failed() {
        let mut summary = PushSummary {
            assets: 1,
            ..Default::default()
        };
        summary.record(AssetPushOutcome::new(
            Uuid::from_u128(1),
            AssetPushStatus::Verified,
        ));
        assert_eq!(failed_run_reason(&summary), None);
    }

    /// **The `--json` contract.** One document: the totals, and per asset its status, counts,
    /// receipts, and — for a failed one — a stable `kind`, the server's code, and the detail.
    #[test]
    fn the_json_report_lists_each_assets_outcome() {
        let json = serde_json::to_value(mixed_run()).expect("the summary serializes");
        assert_eq!(json["assets"], 2);
        assert_eq!(json["verified_assets"], 1);
        assert_eq!(json["failed_assets"], 1);
        assert_eq!(json["receipts"], 3);
        assert_eq!(json["dry_run"], false);

        let outcomes = json["outcomes"].as_array().expect("an outcome per asset");
        assert_eq!(outcomes.len(), 2);
        assert_eq!(outcomes[0]["asset_id"], Uuid::from_u128(1).to_string());
        assert_eq!(outcomes[0]["status"], "verified");
        assert_eq!(outcomes[0]["receipts"], 3);
        assert!(outcomes[0]["error"].is_null());

        assert_eq!(outcomes[1]["status"], "failed");
        assert_eq!(outcomes[1]["error"]["kind"], "not_durable");
        assert!(outcomes[1]["error"]["code"].is_null());
        assert!(
            outcomes[1]["error"]["message"]
                .as_str()
                .unwrap()
                .contains("does not durably hold 1 of its blobs")
        );
    }

    /// A server's stable code rides the failure, so a client can localize it.
    #[test]
    fn a_rejected_upload_carries_the_servers_code() {
        let failure = AssetError::from(PushError::Upload(
            capsule_sdk::upload::UploadError::Rejected {
                status: 500,
                code: Some("error.upload.unavailable".into()),
                message: "the asset index could not answer".into(),
            },
        ))
        .failure();
        assert_eq!(failure.kind, "upload");
        assert_eq!(failure.code.as_deref(), Some("error.upload.unavailable"));
    }
}
