use std::sync::Arc;

use sea_orm::DatabaseConnection;

use crate::config::UploadServerConfig;

#[derive(Clone)]
pub(crate) struct AppState {
    inner: Arc<AppStateInner>,
}

pub(crate) struct AppStateInner {
    pub conn: DatabaseConnection,
    pub config: UploadServerConfig,
    pub upload_service: crate::service::upload::UploadService,
}

impl AppState {
    pub(crate) fn new(
        conn: DatabaseConnection,
        config: UploadServerConfig,
        upload_service: crate::service::upload::UploadService,
    ) -> Self {
        Self {
            inner: Arc::new(AppStateInner {
                conn,
                config,
                upload_service,
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

/// State for the generic lifecycle-write surface (`POST /albums/{album_id}/ops`, slice
/// `S-C16`). Mounted at the API root (not under `/upload`) so the transport path matches the
/// authorization contract; it carries only the config (for the JWT decode key + quota limits)
/// and the [`OpService`](crate::service::ops::OpService).
#[derive(Clone)]
pub(crate) struct OpsState {
    inner: Arc<OpsStateInner>,
}

pub(crate) struct OpsStateInner {
    pub config: UploadServerConfig,
    pub ops_service: crate::service::ops::OpService,
}

impl OpsState {
    pub(crate) fn new(
        config: UploadServerConfig,
        ops_service: crate::service::ops::OpService,
    ) -> Self {
        Self {
            inner: Arc::new(OpsStateInner {
                config,
                ops_service,
            }),
        }
    }
}

impl std::ops::Deref for OpsState {
    type Target = OpsStateInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
