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

/// State for the `/albums` route tree, mounted at the API root (not under `/upload`) so the
/// transport paths match the authorization contract. It backs two surfaces:
///
/// - `POST /albums/{album_id}/ops` — the generic lifecycle-write surface (slice `S-C16`),
///   served by the [`OpService`](crate::service::ops::OpService);
/// - `POST /albums` — album provisioning (slice `S-C25`), which talks to the database
///   directly through `service::album::Mutation` and so needs the connection.
///
/// The config carries the JWT decode key both handlers authenticate with.
#[derive(Clone)]
pub(crate) struct OpsState {
    inner: Arc<OpsStateInner>,
}

pub(crate) struct OpsStateInner {
    pub conn: DatabaseConnection,
    pub config: UploadServerConfig,
    pub ops_service: crate::service::ops::OpService,
}

impl OpsState {
    pub(crate) fn new(
        conn: DatabaseConnection,
        config: UploadServerConfig,
        ops_service: crate::service::ops::OpService,
    ) -> Self {
        Self {
            inner: Arc::new(OpsStateInner {
                conn,
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
