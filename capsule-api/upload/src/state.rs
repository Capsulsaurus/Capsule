use std::sync::Arc;

use sea_orm::DatabaseConnection;

use crate::config::UploadServerConfig;

#[derive(Clone)]
pub struct AppState {
    inner: Arc<AppStateInner>,
}

pub struct AppStateInner {
    pub conn: DatabaseConnection,
    pub config: UploadServerConfig,
    pub upload_service: crate::service::upload::UploadService,
}

impl AppState {
    pub fn new(
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
