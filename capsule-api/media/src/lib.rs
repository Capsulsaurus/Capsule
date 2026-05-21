use config::MediaServerConfig;
use eyre::Result;
use sea_orm::DatabaseConnection;

use salvo::prelude::*;

use crate::state::AppState;

mod config;
mod error;
pub mod routes; // Expose routes module if needed or just functions
mod state;

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
