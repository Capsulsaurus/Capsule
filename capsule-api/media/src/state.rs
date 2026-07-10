use std::sync::Arc;

use sea_orm::DatabaseConnection;

use crate::config::MediaServerConfig;
use crate::service::serve::BlobServeService;
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
    /// The key-free media-serving engine (slice `S-C10`) over the content-addressed blob tree.
    pub(crate) serve: BlobServeService,
}

impl AppState {
    pub fn new(conn: DatabaseConnection, config: MediaServerConfig) -> Self {
        let verify = VerificationService::new(config.upload_dir.clone());
        let serve = BlobServeService::new(config.upload_dir.clone());
        Self {
            inner: Arc::new(AppStateInner {
                conn,
                config,
                verify,
                serve,
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
