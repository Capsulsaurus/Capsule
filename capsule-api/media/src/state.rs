use std::sync::Arc;

use sea_orm::DatabaseConnection;

use crate::config::MediaServerConfig;
use crate::service::verify::VerificationService;

#[derive(Clone)]
pub struct AppState {
    inner: Arc<AppStateInner>,
}

pub struct AppStateInner {
    pub conn: DatabaseConnection,
    pub config: MediaServerConfig,
    /// The storage-verification engine (slice `S-C3`), shared across requests so its
    /// deep-scan coalesce cache and per-user rate budget are process-global.
    pub(crate) verify: VerificationService,
}

impl AppState {
    pub fn new(conn: DatabaseConnection, config: MediaServerConfig) -> Self {
        let verify = VerificationService::new(config.upload_dir.clone());
        Self {
            inner: Arc::new(AppStateInner {
                conn,
                config,
                verify,
            }),
        }
    }
}

impl std::ops::Deref for AppState {
    type Target = AppStateInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
