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
use capsule_sdk::sync::{SyncConsumer, SyncCursor, SyncError, SyncState};
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

/// The default server origin — one host, one port, matching `mise run serve-api`.
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
            sync_endpoint: std::env::var("CAPSULE_SYNC_ENDPOINT").unwrap_or(base),
            protocol_version: std::env::var("CAPSULE_PROTOCOL")
                .unwrap_or_else(|_| DEFAULT_PROTOCOL_VERSION.to_string()),
        }
    }

    /// The auth surface hangs off `/v1/auth` on the shared origin.
    fn auth_endpoint_for(base: &str) -> String {
        format!("{base}/v1/auth")
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
    username: &str,
    name: &str,
    email: &str,
    password: &str,
) -> Result<(), RemoteError> {
    let client = AuthClient::new(&remote.auth_endpoint)?;
    let session = client.register(username, name, email, password).await?;
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
    let persisted = store.load()?.ok_or(RemoteError::NotAuthenticated)?;
    let client = AuthClient::new(&remote.auth_endpoint)?;
    let session = client.resume(persisted)?;

    let channel = SyncConsumer::connect(remote.sync_endpoint.clone()).await?;
    let mut consumer =
        SyncConsumer::with_session(channel, session, remote.protocol_version.clone());

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

    /// `from_env` reads process-wide state, so these run under one lock and restore what they
    /// touched — otherwise a parallel test would see another's variables.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Clear every endpoint variable, run `f`, and restore the prior values.
    fn with_clean_env<T>(f: impl FnOnce() -> T) -> T {
        const VARS: [&str; 4] = [
            "CAPSULE_ENDPOINT",
            "CAPSULE_AUTH_ENDPOINT",
            "CAPSULE_SYNC_ENDPOINT",
            "CAPSULE_PROTOCOL",
        ];
        let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    }

    #[test]
    fn a_trailing_slash_does_not_double_up() {
        let cfg = with_clean_env(|| {
            unsafe { std::env::set_var("CAPSULE_ENDPOINT", "http://host:3000/") };
            RemoteConfig::from_env()
        });
        assert_eq!(cfg.auth_endpoint, "http://host:3000/v1/auth");
        assert_eq!(cfg.sync_endpoint, "http://host:3000");
    }

    /// Split deployments (auth behind one ingress, sync behind another) stay expressible.
    #[test]
    fn per_surface_overrides_win_over_the_base() {
        let cfg = with_clean_env(|| {
            unsafe {
                std::env::set_var("CAPSULE_ENDPOINT", "http://ignored:1");
                std::env::set_var("CAPSULE_AUTH_ENDPOINT", "https://auth.example.com/v1/auth");
                std::env::set_var("CAPSULE_SYNC_ENDPOINT", "https://sync.example.com");
            }
            RemoteConfig::from_env()
        });
        assert_eq!(cfg.auth_endpoint, "https://auth.example.com/v1/auth");
        assert_eq!(cfg.sync_endpoint, "https://sync.example.com");
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
}
