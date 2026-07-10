//! The CLI's networked commands — `auth login/logout`, `sync`, `list` — expressed
//! entirely over `capsule-sdk` (slice `S-D5`).
//!
//! The CLI is a client: it **never hand-rolls a network flow**. Every request here
//! goes through an SDK primitive — [`AuthClient`]/[`Session`] for auth, and
//! [`SyncConsumer`]/[`SyncState`] for the feed — so token refresh, the protocol
//! handshake, the `401`/`Unauthenticated` retry, and the anti-rewind contract are
//! the SDK's, not re-implemented here. These functions are the testable core the
//! binary's command arms call and the E2E round-trip drives directly.

use capsule_sdk::auth::{AuthClient, AuthError};
use capsule_sdk::sync::{SyncConsumer, SyncError};
use sea_orm::{ConnectionTrait, TransactionTrait};
use thiserror::Error;
use tracing::instrument;

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
    /// The client's max-known protocol version (`YYYY-MM-DD`), sent on the
    /// handshake and used as the forward-version ceiling.
    pub protocol_version: String,
}

impl RemoteConfig {
    /// Build from `CAPSULE_AUTH_ENDPOINT`, `CAPSULE_SYNC_ENDPOINT`, and
    /// `CAPSULE_PROTOCOL`, each with a local dev-server default.
    pub fn from_env() -> Self {
        Self {
            auth_endpoint: std::env::var("CAPSULE_AUTH_ENDPOINT")
                .unwrap_or_else(|_| "http://127.0.0.1:8080".to_string()),
            sync_endpoint: std::env::var("CAPSULE_SYNC_ENDPOINT")
                .unwrap_or_else(|_| "http://127.0.0.1:8081".to_string()),
            protocol_version: std::env::var("CAPSULE_PROTOCOL")
                .unwrap_or_else(|_| DEFAULT_PROTOCOL_VERSION.to_string()),
        }
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
    /// A command needing an authenticated session found none stored.
    #[error("not authenticated")]
    NotAuthenticated,
    /// The freshly-logged-in session held no exportable tokens (should not happen).
    #[error("login produced no session tokens")]
    EmptySession,
}

impl RemoteError {
    /// The stable `error.*` catalog code a client localizes, when one applies.
    pub fn error_code(&self) -> Option<&str> {
        match self {
            Self::Auth(error) => error.error_code(),
            Self::Sync(error) => error.error_code(),
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
) -> Result<(), RemoteError> {
    let client = AuthClient::new(&remote.auth_endpoint)?;
    let session = client.login(email, password).await?;
    let persisted = session.export().await.ok_or(RemoteError::EmptySession)?;
    store.save(&persisted)?;
    tracing::info!("login persisted to the session store");
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
/// Auth rides the resumed [`Session`](capsule_sdk::auth::Session) (pre-flight
/// refresh + one refresh-and-retry on `Unauthenticated`); the feed and the
/// anti-rewind contract ride [`SyncConsumer::pull_into`] over a [`SyncState`]
/// rehydrated from the local store. Each validated page is persisted in one
/// transaction (unless `dry_run`), and the cursor + high-water marks survive to the
/// next run.
#[instrument(skip(store, db))]
pub async fn sync<C: ConnectionTrait + TransactionTrait>(
    remote: &RemoteConfig,
    store: &SessionStore,
    db: &C,
    page_size: u32,
    dry_run: bool,
) -> Result<SyncSummary, RemoteError> {
    let persisted = store.load()?.ok_or(RemoteError::NotAuthenticated)?;
    let client = AuthClient::new(&remote.auth_endpoint)?;
    let session = client.resume(persisted)?;

    let channel = SyncConsumer::connect(remote.sync_endpoint.clone()).await?;
    let mut consumer =
        SyncConsumer::with_session(channel, session, remote.protocol_version.clone());

    let mut state = syncstore::load_sync_state(db, &remote.protocol_version).await?;

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

/// Query the sync-fed local store — the client-side library `capsule list` renders.
#[instrument(skip(db))]
pub async fn list<C: ConnectionTrait>(
    db: &C,
    include_tombstoned: bool,
) -> Result<Vec<SyncedAssetView>, RemoteError> {
    Ok(syncstore::list_assets(db, include_tombstoned).await?)
}
