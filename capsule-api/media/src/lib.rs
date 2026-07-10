use config::MediaServerConfig;
use eyre::Result;
use salvo::prelude::*;
use sea_orm::DatabaseConnection;

use crate::drop_state::DropState;
use crate::share_state::ShareState;
use crate::state::AppState;

mod config;
mod drop_state;
mod error;
pub mod routes; // Expose routes module if needed or just functions
mod service;
mod share_state;
mod state;

#[cfg(test)]
mod tests;

pub async fn get_router<C: Into<MediaServerConfig>>(
    conn: DatabaseConnection,
    config: C,
) -> Result<Router> {
    let config = config.into();
    let state = AppState::new(conn, config);

    Ok(Router::new().push(routes::get_router(state)))
}

/// Public share-link serve router (`/s/{opaque-id}` metadata + blob + wrapped-secret). Slice
/// `S-C4`.
pub async fn get_share_router<C: Into<MediaServerConfig>>(
    conn: DatabaseConnection,
    config: C,
) -> Result<Router> {
    let config = config.into();
    let state = ShareState::new(conn, config);

    Ok(routes::get_share_router(state))
}

/// Storage-verification router (`POST /storage/verify`, incl. signed attestation). Slices
/// `S-C3` / `S-C15`.
pub async fn get_storage_router<C: Into<MediaServerConfig>>(
    conn: DatabaseConnection,
    config: C,
) -> Result<Router> {
    let state = AppState::new(conn, config.into());
    Ok(routes::get_storage_router(state))
}

/// Durable custody-receipt router (`GET /assets/{asset_id}/receipts`). Slice `S-C15`.
pub async fn get_receipts_router<C: Into<MediaServerConfig>>(
    conn: DatabaseConnection,
    config: C,
) -> Result<Router> {
    let state = AppState::new(conn, config.into());
    Ok(routes::get_receipts_router(state))
}

/// Attestation-key publication router (`GET /.well-known/capsule/attestation-keys`). Slice
/// `S-C15`.
pub async fn get_well_known_router<C: Into<MediaServerConfig>>(
    conn: DatabaseConnection,
    config: C,
) -> Result<Router> {
    let state = AppState::new(conn, config.into());
    Ok(routes::get_well_known_router(state))
}

/// Guest drop-session router (`/u/{opaque-id}/drop`) — slice `S-C5`.
pub async fn get_drop_link_router<C: Into<MediaServerConfig>>(
    conn: DatabaseConnection,
    config: C,
) -> Result<Router> {
    let state = DropState::new(conn, config.into()).await?;
    Ok(routes::get_drop_link_router(state))
}

/// Owner drop-inbox router (`/drops`) — slice `S-C5`.
pub async fn get_drops_router<C: Into<MediaServerConfig>>(
    conn: DatabaseConnection,
    config: C,
) -> Result<Router> {
    let state = DropState::new(conn, config.into()).await?;
    Ok(routes::get_drops_router(state))
}
