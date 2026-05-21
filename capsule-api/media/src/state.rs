use std::sync::Arc;

use sea_orm::DatabaseConnection;

use crate::config::MediaServerConfig;

#[derive(Clone)]
pub struct AppState {
    inner: Arc<AppStateInner>,
}

pub struct AppStateInner {
    pub conn: DatabaseConnection,
    pub config: MediaServerConfig,
}

impl AppState {
    pub fn new(conn: DatabaseConnection, config: MediaServerConfig) -> Self {
        Self {
            inner: Arc::new(AppStateInner { conn, config }),
        }
    }
}

impl std::ops::Deref for AppState {
    type Target = AppStateInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
