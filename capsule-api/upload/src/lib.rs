use config::UploadServerConfig;
use eyre::Result;
use sea_orm::DatabaseConnection;

use salvo::cors::{AllowOrigin, Cors};
use salvo::http::Method;
use salvo::prelude::*;
use tracing::info;

use crate::{config::validate_config, state::AppState};

mod config;
mod error;
mod models;
mod routes;
mod service;
mod session;
mod state;

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

    let state = AppState::new(conn, config, upload_service);

    Ok(Router::new().hoop(cors).push(routes::get_router(state)))
}
