//! Shared state for the public share-link serve router (slice `S-C4`).
//!
//! Backs the public serve router (`/s/{opaque-id}` metadata + blob + wrapped-secret). It carries
//! the content-addressed [`StorageService`] (serving ciphertext blobs by content address) and the
//! [`ShareServeService`] serve engine (the two rate limiters + fail-closed revocation cache +
//! mandatory privacy strip). The issuer publish surface is the service-level
//! [`service::share::Mutation`] (mirroring the drop store's provision step), not an HTTP route.

use std::sync::Arc;

use sea_orm::DatabaseConnection;
use upload::transport::StorageService;

use crate::config::MediaServerConfig;
use crate::service::share::ShareServeService;

#[derive(Clone)]
pub struct ShareState {
    inner: Arc<ShareStateInner>,
}

pub struct ShareStateInner {
    pub(crate) storage: StorageService,
    pub(crate) serve: ShareServeService,
}

impl ShareState {
    /// Build the share state: the content-addressed blob store + the serve engine (keyed to this
    /// server's home-server id) from the media config.
    #[must_use]
    pub fn new(conn: DatabaseConnection, config: MediaServerConfig) -> Self {
        let storage = StorageService::with_upload_dir(config.upload_dir.clone());
        let serve = ShareServeService::new(conn, config.server_id);
        Self {
            inner: Arc::new(ShareStateInner { storage, serve }),
        }
    }
}

impl std::ops::Deref for ShareState {
    type Target = ShareStateInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
