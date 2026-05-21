use crate::config::UploadServerConfig;
use crate::error::UploadError;
use crate::models::session::{UploadSession, UploadSessionStatus};
use crate::service::processing::ProcessingService;
use crate::service::storage::StorageService;
use crate::session::UploadSessionManager;
use chrono::Utc;
use nanoid::nanoid;
use capsule_core::utils::hash::get_file_hash;
use sea_orm::{DatabaseConnection, TransactionTrait};
use std::clone::Clone;
use tokio::fs;

use entity::asset;
use service::album as AlbumService;
use service::asset as AssetService;

#[derive(Clone)]
pub struct UploadService {
    config: UploadServerConfig,
    storage: StorageService,
    session_manager: UploadSessionManager,
    processing_service: ProcessingService,
    conn: DatabaseConnection,
}

// TODO: Update all methods to use more explicit error types
impl UploadService {
    pub fn new(
        config: UploadServerConfig,
        storage: StorageService,
        session_manager: UploadSessionManager,
        conn: DatabaseConnection,
    ) -> Self {
        Self {
            config,
            storage,
            session_manager,
            processing_service: ProcessingService::new(),
            conn,
        }
    }

    /// Create a new upload session with asset record in Postgres
    #[allow(clippy::too_many_arguments)]
    pub async fn create_session(
        &self,
        owner_id: &str,
        upload_user_id: &str,
        content_type: Option<String>,
        total_size: u64,
        expected_hash: String,
        album_id: Option<String>,
        original_filename: String,
    ) -> Result<UploadSession, UploadError> {
        let upload_id = nanoid!();

        // Validate Album access if provided
        if let Some(album_id) = &album_id {
            match AlbumService::Query::get_album_access(&self.conn, owner_id, album_id).await {
                Ok(access) => {
                    if !access.is_some_and(|a| a.is_write()) {
                        return Err(UploadError::InvalidUpload(
                            "Album access denied".to_string(),
                        ));
                    }
                }
                Err(e) => {
                    return Err(UploadError::InvalidUpload(e.to_string()));
                }
            }
        }

        // Check for duplicate hash - asset with same hash already exists for this user
        if let Some(existing) =
            AssetService::Query::find_by_hash_for_user(&self.conn, upload_user_id, &expected_hash)
                .await
                .map_err(|e| UploadError::Unknown(e.to_string()))?
        {
            return Err(UploadError::InvalidUpload(format!(
                "Asset with this hash already exists: {}",
                existing.id
            )));
        }

        // Determine asset type from content_type
        let asset_type = content_type
            .as_ref()
            .map(|ct| {
                if ct.starts_with("image/") {
                    asset::AssetType::Photo
                } else if ct.starts_with("video/") {
                    asset::AssetType::Video
                } else {
                    asset::AssetType::Photo
                }
            })
            .unwrap_or(asset::AssetType::Photo);

        // Create pending asset in Postgres with uploaded=false
        let asset = AssetService::Mutation::create_pending(
            &self.conn,
            owner_id.to_string(),
            upload_user_id.to_string(),
            album_id.clone(),
            asset_type,
            original_filename,
            total_size as i64,
            expected_hash.clone(),
            content_type
                .clone()
                .unwrap_or_else(|| "application/octet-stream".to_string()),
            None, // date will be extracted on finalize
        )
        .await
        .map_err(|e| UploadError::Unknown(e.to_string()))?;

        let session = UploadSession {
            id: upload_id.clone(),
            asset_id: asset.id.clone(),
            owner_id: owner_id.to_string(),
            upload_user_id: upload_user_id.to_string(),
            album_id,
            content_type,
            expected_hash,
            received_bytes: 0,
            total_size,
            status: UploadSessionStatus::Pending,
            created_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::hours(24),
        };

        // Create session in Redis (atomic HSET)
        self.session_manager.create(&session).await?;

        Ok(session)
    }

    pub async fn get_session(&self, upload_id: &str) -> Result<Option<UploadSession>, UploadError> {
        self.session_manager.get(upload_id).await
    }

    /// List sessions by owner ID
    pub async fn list_sessions_by_owner(
        &self,
        owner_id: &str,
    ) -> Result<Vec<UploadSession>, UploadError> {
        let session_ids = self.session_manager.list_by_owner(owner_id).await?;
        let mut sessions = Vec::with_capacity(session_ids.len());

        for id in session_ids {
            if let Some(session) = self.session_manager.get(&id).await? {
                // Only return active sessions
                if session.status.is_active() {
                    sessions.push(session);
                }
            }
        }

        Ok(sessions)
    }

    pub async fn append_chunk(
        &self,
        upload_id: &str,
        data: bytes::Bytes,
        offset: u64,
    ) -> Result<UploadSession, UploadError> {
        // Get current session state atomically
        let session = self
            .get_session(upload_id)
            .await?
            .ok_or(UploadError::SessionNotFound)?;

        if session.status.is_inactive() {
            return Err(UploadError::UploadComplete);
        }

        // Validate offset matches current received_bytes
        if offset != session.received_bytes {
            return Err(UploadError::InvalidOffset {
                expected: session.received_bytes,
                actual: offset,
            });
        }

        let chunk_len = data.len() as u64;

        // Validate size limit before writing
        let new_size = session.received_bytes + chunk_len;
        if session.total_size > 0 && new_size > session.total_size {
            return Err(UploadError::InvalidUpload(
                "Upload exceeds declared total size".to_string(),
            ));
        }
        if new_size > self.config.max_file_size as u64 {
            return Err(UploadError::FileTooLarge);
        }

        // Count existing chunks
        let chunk_count = self.storage.count_chunks(upload_id).await?;

        // Write chunk to disk
        let chunk_path = self.storage.get_chunk_path(upload_id, chunk_count);
        fs::write(&chunk_path, &data).await?;

        // Atomically increment received_bytes in Redis
        let new_received_bytes = self
            .session_manager
            .increment_received_bytes(upload_id, chunk_len)
            .await?;

        // Re-fetch session to get updated state
        let updated_session = UploadSession {
            received_bytes: new_received_bytes,
            ..session
        };

        Ok(updated_session)
    }

    pub async fn finalize_upload(&self, upload_id: &str) -> Result<asset::Model, UploadError> {
        let session = self
            .get_session(upload_id)
            .await?
            .ok_or(UploadError::SessionNotFound)?;

        match session.status {
            status if status.is_inactive() => return Err(UploadError::UploadComplete),
            UploadSessionStatus::FailedProcessing => {
                return Err(UploadError::UploadInstanceConflict);
            }
            _ => {}
        }

        if session.total_size > 0 && session.received_bytes != session.total_size {
            return Err(UploadError::InvalidUpload(format!(
                "Upload not complete: received {} of {}",
                session.received_bytes, session.total_size
            )));
        }

        // Mark session as processing
        self.session_manager
            .update_status(upload_id, UploadSessionStatus::WaitingForProcessing)
            .await?;

        // Combine chunks
        let num_chunks = self.storage.count_chunks(upload_id).await?;

        let final_path = self.storage.combine_chunks(upload_id, num_chunks).await?;

        // Verify hash (run sync hash in blocking task)
        let hash_path = final_path.clone();
        let actual_hash = tokio::task::spawn_blocking(move || get_file_hash(&hash_path))
            .await
            .map_err(|e| UploadError::ProcessingError(e.to_string()))?
            .map_err(|e| UploadError::ProcessingError(e.to_string()))?;

        if actual_hash != session.expected_hash {
            // Hash mismatch - clean up and delete asset
            if let Err(e) = fs::remove_file(&final_path).await {
                tracing::warn!("Failed to delete file after hash mismatch: {}", e);
            }

            // Delete the pending asset
            let _ = AssetService::Mutation::delete(&self.conn, &session.asset_id).await;

            // Update session status
            self.session_manager
                .update_status(upload_id, UploadSessionStatus::FailedProcessing)
                .await?;

            return Err(UploadError::ChecksumMismatch {
                expected: session.expected_hash,
                actual: actual_hash,
            });
        }

        // Extract Metadata
        let metadata = self
            .processing_service
            .extract_metadata(&final_path)
            .await
            .map_err(|e| UploadError::ProcessingError(e.to_string()))?;

        // Update asset in Postgres with uploaded=true and metadata
        let txn = self.conn.begin().await?;

        let asset = AssetService::Mutation::mark_uploaded(
            &txn,
            &session.asset_id,
            metadata.width,
            metadata.height,
            metadata.date,
        )
        .await
        .map_err(|e| UploadError::Unknown(e.to_string()))?;

        txn.commit().await?;

        // Mark session as complete
        self.session_manager
            .update_status(upload_id, UploadSessionStatus::Completed)
            .await?;

        Ok(asset)
    }

    pub async fn cancel_upload(&self, upload_id: &str) -> Result<(), UploadError> {
        // Get session to find asset_id
        let session = self.get_session(upload_id).await?;

        // Delete asset from Postgres if session exists
        if let Some(session) = &session
            && let Err(e) = AssetService::Mutation::delete(&self.conn, &session.asset_id).await
        {
            tracing::warn!(
                "Failed to delete asset {} from Postgres: {}",
                session.asset_id,
                e
            );
        }

        // Delete chunks from disk
        if let Err(e) = self.storage.delete_chunks(upload_id).await {
            tracing::warn!("Failed to delete chunks for upload {}: {}", upload_id, e);
        }

        // Remove session from Redis
        self.session_manager.delete(upload_id).await?;

        Ok(())
    }
}
