use config::AuthConfig;
use salvo::cors::{AllowOrigin, Cors};
use salvo::http::Method;
use salvo::prelude::*;
use sea_orm::DatabaseConnection;
use state::AppState;

pub mod claims;
pub mod config;
pub mod constants;
pub mod errors;
#[cfg(feature = "server")]
pub mod models;
pub mod oidc;
pub mod roles;
#[cfg(feature = "server")]
mod routes;
pub mod service;
#[cfg(feature = "server")]
pub mod session;
#[cfg(feature = "server")]
pub mod state;
pub mod utils;
#[cfg(feature = "server")]
pub mod validation;

/// Build a router from an existing `AppState` (useful for testing).
#[cfg(feature = "server")]
pub fn get_router_with_state(state: AppState) -> Router {
    routes::get_router(state)
}

#[cfg(feature = "server")]
pub async fn get_router<C: Into<AuthConfig>>(
    conn: DatabaseConnection,
    config: C,
) -> eyre::Result<Router> {
    let config = config.into();

    let session_manager = session::SessionManager::new(
        config.valkey_url.clone(),
        std::time::Duration::from_secs(config.jwt_refresh_token_duration_seconds),
    )
    .await
    .map_err(|e| e.0)?;

    // Initialize Passkey Service
    let rp_id = config.domain.clone();
    let rp_origin = webauthn_rs::prelude::Url::parse(&format!("https://{}", config.domain))?;

    let builder = webauthn_rs::prelude::WebauthnBuilder::new(&rp_id, &rp_origin)
        .map_err(|e| eyre::eyre!(e))?;
    let webauthn = std::sync::Arc::new(builder.build().map_err(|e| eyre::eyre!(e))?);
    let passkey_service = service::PasskeyService::new(conn.clone(), webauthn);

    // CORS configuration
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
            Method::OPTIONS,
        ])
        .allow_headers("*")
        .into_handler();

    let state = AppState::new(conn, config, session_manager, passkey_service);

    let router = routes::get_router(state);

    // Wrap with CORS
    let router = Router::new().hoop(cors).push(router);

    Ok(router)
}
