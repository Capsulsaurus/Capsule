//! Session discard machinery (slice `S-C1`; SSoT: [Upload Protocol — Session Lifetime and
//! Discard](https://docs/design/import/upload-protocol/#session-lifetime-and-discard)).
//!
//! Two responsibilities:
//!
//! - **Pressure eviction.** Between the ≥1-hour survival floor and the 24-hour cap the
//!   server MAY discard sessions to reclaim space, least-recently-progressed first (ties
//!   broken toward the most on-disk bytes). A session that made progress within the floor
//!   is never evicted, under any pressure.
//! - **Startup scrub.** On boot the server reconciles disk against the session store: an
//!   upload file with no session is deleted; a session whose file is shorter than its
//!   recorded received-byte count is moved to `FailedProcessing` (the file is authoritative
//!   — an ACK the disk cannot back must not stand). Both delete the pending asset row.

use jiff::Timestamp;
use sea_orm::DatabaseConnection;
use service::asset as AssetService;

use crate::error::UploadError;
use crate::models::session::UploadSessionStatus;
use crate::service::storage::StorageService;
use crate::session::UploadSessionManager;

/// The guaranteed survival floor: a session that made progress in the last hour is never
/// evicted under pressure. (Exercised by the S-C1 discard-floor test; the sweeper that
/// invokes [`DiscardService::evict_for_pressure`] under `max_cache_size` pressure is the
/// relief valve named by the upload-protocol Backpressure section.)
#[allow(dead_code)]
pub(crate) const SURVIVAL_FLOOR_SECS: i64 = 3600;

/// Outcome of a startup scrub, for structured logging and test assertions.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScrubReport {
    /// Orphan `incoming/*.bin` files (no backing session) deleted.
    pub orphan_files_deleted: usize,
    /// Sessions whose on-disk file was shorter than the recorded offset, failed.
    pub length_diverged_failed: usize,
    /// Sessions whose counter was reconciled forward to a longer on-disk file (a crash
    /// between the durable append and the counter increment).
    pub reconciled_forward: usize,
}

#[derive(Clone)]
pub(crate) struct DiscardService {
    session_manager: UploadSessionManager,
    storage: StorageService,
    conn: DatabaseConnection,
}

impl DiscardService {
    pub(crate) fn new(
        session_manager: UploadSessionManager,
        storage: StorageService,
        conn: DatabaseConnection,
    ) -> Self {
        Self {
            session_manager,
            storage,
            conn,
        }
    }

    /// Evict least-recently-progressed sessions (past the survival floor) until at least
    /// `needed_bytes` of on-disk space is reclaimed or no evictable session remains.
    /// Returns the number of bytes reclaimed. Sessions within the floor are excluded by the
    /// score filter, so a live-but-slow upload keeps its session. This is the relief valve
    /// the backpressure sweeper invokes when `max_cache_size` is hit.
    #[allow(dead_code)]
    #[tracing::instrument(skip(self))]
    pub(crate) async fn evict_for_pressure(&self, needed_bytes: u64) -> Result<u64, UploadError> {
        let floor_epoch = Timestamp::now().as_second() - SURVIVAL_FLOOR_SECS;
        let candidates = self
            .session_manager
            .evictable_candidates(floor_epoch)
            .await?;

        // Enrich with on-disk bytes and order: least-recently-progressed first (ascending
        // score), ties broken toward the most on-disk bytes (reclaim the most space).
        let mut enriched: Vec<(String, i64, u64)> = Vec::with_capacity(candidates.len());
        for (id, score) in candidates {
            let bytes = self.storage.file_len(&id).await?.unwrap_or(0);
            enriched.push((id, score, bytes));
        }
        enriched.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| b.2.cmp(&a.2)));

        let mut reclaimed = 0u64;
        for (id, _, bytes) in enriched {
            if reclaimed >= needed_bytes {
                break;
            }
            self.discard(&id).await?;
            reclaimed = reclaimed.saturating_add(bytes);
            tracing::info!(upload_id = %id, bytes, "evicted stalled upload session under pressure");
        }
        Ok(reclaimed)
    }

    /// Reconcile disk against the session store on boot. Idempotent; safe to run at startup.
    #[tracing::instrument(skip(self))]
    pub(crate) async fn scrub(&self) -> Result<ScrubReport, UploadError> {
        let mut report = ScrubReport::default();

        // Orphan files: an upload file with no session record is deleted.
        for id in self.storage.list_upload_ids().await? {
            if self.session_manager.get(&id).await?.is_none() {
                self.storage.remove(&id).await?;
                report.orphan_files_deleted += 1;
                tracing::info!(upload_id = %id, "scrub deleted orphan upload file");
            }
        }

        // Length divergence: the file is authoritative. A session whose file is shorter than
        // its recorded received-byte count cannot stand — fail it and clean up.
        for id in self.session_manager.list_progress_ids().await? {
            let Some(session) = self.session_manager.get(&id).await? else {
                continue;
            };
            let on_disk = self.storage.file_len(&id).await?.unwrap_or(0);
            if on_disk < session.received_bytes {
                tracing::warn!(
                    upload_id = %id,
                    on_disk,
                    recorded = session.received_bytes,
                    "scrub found on-disk length below recorded offset; failing session"
                );
                self.session_manager
                    .update_status(&id, UploadSessionStatus::FailedProcessing)
                    .await?;
                self.storage.remove(&id).await?;
                let _ = AssetService::Mutation::delete(&self.conn, &session.asset_id).await;
                report.length_diverged_failed += 1;
            } else if on_disk > session.received_bytes {
                // A crash between the durable append and the counter increment: the file is
                // the truth, so reconcile the counter forward and let HEAD report the
                // on-disk offset. The re-sent chunk simply re-aligns.
                tracing::info!(
                    upload_id = %id,
                    on_disk,
                    recorded = session.received_bytes,
                    "scrub reconciled session counter forward to on-disk length"
                );
                self.session_manager
                    .set_received_bytes(&id, on_disk)
                    .await?;
                report.reconciled_forward += 1;
            }
        }

        Ok(report)
    }

    /// Discard one session: delete its upload file, its pending asset row, and its record.
    #[allow(dead_code)]
    async fn discard(&self, upload_id: &str) -> Result<(), UploadError> {
        if let Some(session) = self.session_manager.get(upload_id).await? {
            let _ = AssetService::Mutation::delete(&self.conn, &session.asset_id).await;
        }
        self.storage.remove(upload_id).await?;
        self.session_manager.delete(upload_id).await?;
        Ok(())
    }
}
