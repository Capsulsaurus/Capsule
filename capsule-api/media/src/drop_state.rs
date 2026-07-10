//! Shared state for the web-upload drop server (slice `S-C5`).
//!
//! The drop endpoints (guest `/u/{opaque-id}/drop` + owner `/drops`) reuse the S-C1 chunk
//! transport: a Valkey [`UploadSessionManager`] session and a content-addressed
//! [`StorageService`] blob, driven directly (a drop never creates an `assets` row until
//! adoption). This state carries those plus the DB handle, the media config, and the
//! per-`{opaque-id}`/per-IP drop-session rate limiter (invariant 31).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sea_orm::DatabaseConnection;
use upload::transport::{StorageService, UploadSessionManager};

use crate::config::MediaServerConfig;

#[derive(Clone)]
pub struct DropState {
    inner: Arc<DropStateInner>,
}

pub struct DropStateInner {
    pub conn: DatabaseConnection,
    pub config: MediaServerConfig,
    pub session_manager: UploadSessionManager,
    pub storage: StorageService,
    pub limiter: RateLimiter,
}

impl DropState {
    /// Build the drop state, initialising the Valkey session manager and the content-addressed
    /// blob store from the media config.
    pub async fn new(conn: DatabaseConnection, config: MediaServerConfig) -> eyre::Result<Self> {
        let session_manager = UploadSessionManager::new(&config.valkey_url)
            .await
            .map_err(|e| eyre::eyre!("failed to init drop session manager: {e}"))?;
        let storage = StorageService::with_upload_dir(config.upload_dir.clone());
        let limiter = RateLimiter::new(
            config.drop_rate_limit_max,
            Duration::from_secs(config.drop_rate_limit_window_secs),
        );
        Ok(Self {
            inner: Arc::new(DropStateInner {
                conn,
                config,
                session_manager,
                storage,
                limiter,
            }),
        })
    }
}

impl std::ops::Deref for DropState {
    type Target = DropStateInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// A small in-process fixed-window rate limiter for the drop-session serve path (invariant 31,
/// per-`{opaque-id}` and per-source-IP). In-process (per node) by design for this slice; a
/// shared Valkey limiter is a future hardening (noted in `SLICES.md`).
#[derive(Clone)]
pub struct RateLimiter {
    max: u32,
    window: Duration,
    windows: Arc<Mutex<HashMap<String, (Instant, u32)>>>,
}

impl RateLimiter {
    fn new(max: u32, window: Duration) -> Self {
        Self {
            max,
            window,
            windows: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Record a hit for `key`; returns `true` if it is within the window budget, `false` if the
    /// limit is exceeded (the caller returns `429`).
    pub fn check(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut map = self.windows.lock().expect("rate-limiter mutex poisoned");
        let entry = map.entry(key.to_string()).or_insert((now, 0));
        if now.duration_since(entry.0) > self.window {
            *entry = (now, 0);
        }
        entry.1 += 1;
        entry.1 <= self.max
    }
}
