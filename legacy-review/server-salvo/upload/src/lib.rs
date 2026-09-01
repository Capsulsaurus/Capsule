use config::UploadServerConfig;
use eyre::Result;
use salvo::cors::{AllowOrigin, Cors};
use salvo::http::Method;
use salvo::prelude::*;
use sea_orm::DatabaseConnection;
use tracing::info;

use crate::config::validate_config;
use crate::state::AppState;

mod config;
mod envelope;
mod error;
mod models;
mod routes;
mod service;
mod session;
mod state;
mod visibility;

#[cfg(test)]
mod tests;

/// Reusable upload-transport primitives shared with the media drop server (slice `S-C5`).
///
/// A web-upload **drop** reuses S-C1's chunk mechanics verbatim: the drop server drives its
/// own Valkey [`UploadSessionManager`] session and content-addressed [`StorageService`] blob
/// through these, then stages the finalized blob into the owner's drop **inbox** (never the
/// `assets` index — a drop is not a library asset until adoption). Exposing the transport
/// here keeps the chunk state machine single-sourced in the upload crate; the drop-specific
/// store, inbox, and atomic adoption live in `capsule-api-media::drops`.
pub mod transport {
    pub use crate::config::{
        DEFAULT_CONTENT_TYPES, DEFAULT_DRIFT_DAYS, DEFAULT_PROTOCOL_MAX, DEFAULT_PROTOCOL_MIN,
        DEFAULT_QUOTA_GRACE_DAYS, DEFAULT_QUOTA_HARD_LIMIT, DEFAULT_QUOTA_SOFT_LIMIT,
    };
    pub use crate::error::UploadError;
    pub use crate::models::session::{BlobRole, UploadSession, UploadSessionStatus};
    pub use crate::service::storage::StorageService;
    pub use crate::session::UploadSessionManager;
}

pub async fn get_router<C: Into<UploadServerConfig>>(
    conn: DatabaseConnection,
    config: C,
) -> Result<Router> {
    let config = config.into();
    let config_warnings = validate_config(&config).map_err(|e| {
        eyre::eyre!(
            "Upload server configuration is invalid: {}. Please fix the configuration and try again.",
            e
        )
    })?;
    if !config_warnings.is_empty() {
        info!("Upload server config warnings: {:?}", config_warnings);
    }

    // Initialize Upload Session Manager
    let session_manager = session::UploadSessionManager::new(&config.valkey_url)
        .await
        .map_err(|e| eyre::eyre!("Failed to initialize session manager: {}", e))?;

    // Initialize Storage Service
    let storage = service::storage::StorageService::new(config.clone());

    // Startup scrub: reconcile disk against the session store before serving traffic
    // (orphan upload files deleted; length-diverged sessions failed). Recoverable by
    // construction — no permanently orphaned upload files or pending rows survive a boot.
    let discard = service::discard::DiscardService::new(
        session_manager.clone(),
        storage.clone(),
        conn.clone(),
    );
    match discard.scrub().await {
        Ok(report) => info!(
            orphan_files_deleted = report.orphan_files_deleted,
            length_diverged_failed = report.length_diverged_failed,
            "upload startup scrub complete"
        ),
        Err(e) => tracing::warn!("upload startup scrub failed: {}", e),
    }

    // Initialize Upload Service
    let upload_service = service::upload::UploadService::new(
        config.clone(),
        storage.clone(),
        session_manager.clone(),
        conn.clone(),
    );

    let allow_origin = if config.allowed_origins.iter().any(|o| o == "*") {
        AllowOrigin::any()
    } else {
        AllowOrigin::from(&config.allowed_origins)
    };
    let cors = Cors::new()
        .allow_origin(allow_origin)
        .allow_methods(vec![
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::PATCH,
            Method::HEAD,
            Method::OPTIONS,
        ])
        .allow_headers("*")
        .into_handler();

    let protocol_min = config.protocol_min.clone();
    let protocol_max = config.protocol_max.clone();
    let state = AppState::new(conn, config, upload_service);

    Ok(Router::new()
        .hoop(cors)
        .push(routes::get_router(state, protocol_min, protocol_max)))
}

/// Build the `albums/` router mounted at the API root, carrying two surfaces:
///
/// - `POST /albums` — **album provisioning** (slice `S-C25`): binds a client's derived album
///   UUID to the authenticated owner, idempotently, so invariant 6 can pass for an album the
///   client named. A plain authenticated write; no manifest, no bytes.
/// - `POST /albums/{album_id}/ops` — the singular non-upload lifecycle surface (slice
///   `S-C16`). Reuses the upload server's `EnvelopeGate` (protocol handshake) and the key-free
///   envelope battery; no blob bytes ever move through it (a write that moves bytes is an
///   upload). See [Authorization — The Lifecycle Write Surface].
///
/// [Authorization — The Lifecycle Write Surface]: https://docs/design/authorization/#the-lifecycle-write-surface
pub async fn get_ops_router<C: Into<UploadServerConfig>>(
    conn: DatabaseConnection,
    config: C,
) -> Result<Router> {
    let config = config.into();
    validate_config(&config).map_err(|e| {
        eyre::eyre!(
            "Upload server configuration is invalid: {}. Please fix and retry.",
            e
        )
    })?;

    let protocol_min = config.protocol_min.clone();
    let protocol_max = config.protocol_max.clone();
    let ops_service = crate::service::ops::OpService::new(config.clone(), conn.clone());
    let state = crate::state::OpsState::new(conn, config, ops_service);

    Ok(routes::get_ops_router(state, protocol_min, protocol_max))
}

/// The upload route tree with no injected state, for OpenAPI schema extraction only
/// (slice `S-D8`). Reuses [`routes::route_tree`] — the exact `#[endpoint]` set the live
/// [`get_router`] mounts — so the deterministic schema dump can never drift from the
/// served routes, while needing no Valkey session manager, storage service, database, or
/// startup scrub to build. The protocol window is the declared default; it affects only
/// the `EnvelopeGate` response headers, never the schema.
pub fn openapi_router() -> Router {
    routes::route_tree(
        config::DEFAULT_PROTOCOL_MIN.to_string(),
        config::DEFAULT_PROTOCOL_MAX.to_string(),
    )
}

/// The `albums/` route tree (`POST /albums`, `POST /albums/{album_id}/ops`) with no injected
/// state, for OpenAPI schema extraction only (slice `S-D8`). Mirrors [`get_ops_router`].
pub fn openapi_ops_router() -> Router {
    routes::ops_route_tree(
        config::DEFAULT_PROTOCOL_MIN.to_string(),
        config::DEFAULT_PROTOCOL_MAX.to_string(),
    )
}
