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
