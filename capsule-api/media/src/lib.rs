use config::MediaServerConfig;
use eyre::Result;
use salvo::prelude::*;
use sea_orm::DatabaseConnection;

use crate::drop_state::DropState;
use crate::state::AppState;

mod config;
mod drop_state;
mod error;
pub mod routes; // Expose routes module if needed or just functions
mod service;
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

pub async fn get_share_router<C: Into<MediaServerConfig>>(
    conn: DatabaseConnection,
    config: C,
) -> Result<Router> {
    let config = config.into();
    let state = AppState::new(conn, config);

    Ok(routes::get_share_router(state))
}

/// Storage-verification router (`POST /storage/verify`). Skeleton — slice `S-C3`.
pub async fn get_storage_router<C: Into<MediaServerConfig>>(
    conn: DatabaseConnection,
    config: C,
) -> Result<Router> {
    let state = AppState::new(conn, config.into());
    Ok(routes::get_storage_router(state))
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
