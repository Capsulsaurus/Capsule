use std::clone::Clone;

use capsule_core::utils::hash::get_file_hash;
use chrono::Utc;
use entity::asset;
use nanoid::nanoid;
use sea_orm::{DatabaseConnection, TransactionTrait};
use service::{album as AlbumService, asset as AssetService};

use crate::config::UploadServerConfig;
use crate::error::UploadError;
use crate::models::requests::CreateUploadRequest;
use crate::models::session::{UploadSession, UploadSessionStatus};
use crate::service::processing::ProcessingService;
use crate::service::storage::StorageService;
use crate::session::UploadSessionManager;

#[derive(Clone)]
pub(crate) struct UploadService {
    config: UploadServerConfig,
    storage: StorageService,
    session_manager: UploadSessionManager,
    processing_service: ProcessingService,
    conn: DatabaseConnection,
}

impl UploadService {
    pub(crate) fn new(
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

    /// Create a new upload session with asset record in Postgres.
    ///
    /// The session record captures the full creation request (crypto/protocol
    /// pins, blob role, manifest envelope) so finalization needs no further
    /// client input. Deep envelope validation (invariants 1–8) is the envelope
    /// gate's job (S-C1); this path enforces shape and access only.
    pub(crate) async fn create_session(
        &self,
        request: &CreateUploadRequest,
        owner_id: &str,
        upload_user_id: &str,
    ) -> Result<UploadSession, UploadError> {
        let upload_id = nanoid!();

        // Validate Album access if provided
        if let Some(album_id) = &request.album_id {
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

        // Duplicate hash: a finalized asset with this hash already exists for the
        // user. Per the protocol's idempotency contract this is a 409 carrying the
        // existing asset reference — the client's merge trigger, not a 500.
        // (Returning an *active* session for the tuple, and closing the TOCTOU
        // race with a single SELECT..FOR UPDATE transaction, lands with S-C1.)
        if let Some(existing) =
            AssetService::Query::find_by_hash_for_user(&self.conn, upload_user_id, &request.hash)
                .await
                .map_err(|e| UploadError::Unknown(e.to_string()))?
        {
            return Err(UploadError::DuplicateBlob {
                asset_id: existing.id,
            });
        }

        // Determine asset type from content_type
        let asset_type = if request.content_type.starts_with("video/") {
            asset::AssetType::Video
        } else {
            asset::AssetType::Photo
        };

        // Create pending asset in Postgres with uploaded=false.
        // LEGACY-PLAINTEXT (frozen, S-C1/S-G3): the plaintext-era `original_filename`
        // column gets the opaque upload id — the wire request deliberately carries
        // no filename (plaintext metadata rides the encrypted metadata blob).
        let asset = AssetService::Mutation::create_pending(
            &self.conn,
            owner_id.to_string(),
            upload_user_id.to_string(),
            request.album_id.clone(),
            asset_type,
            upload_id.clone(),
            request.size as i64,
            request.hash.clone(),
            request.content_type.clone(),
            None,
        )
        .await
        .map_err(|e| UploadError::Unknown(e.to_string()))?;

        let now = Utc::now();
        let session = UploadSession {
            id: upload_id.clone(),
            asset_id: asset.id.clone(),
            owner_id: owner_id.to_string(),
            upload_user_id: upload_user_id.to_string(),
            album_id: request.album_id.clone(),
            content_type: Some(request.content_type.clone()),
            expected_hash: request.hash.clone(),
            crypto_suite_id: request.crypto_suite_id,
            protocol_version: request.protocol_version.clone(),
            blob_role: request.blob_role,
            intent_id: request.intent_id.clone(),
            manifest_envelope: serde_json::to_string(&request.manifest_envelope)?,
            received_bytes: 0,
            total_size: request.size,
            status: UploadSessionStatus::Pending,
            created_at: now,
            last_progress_at: now,
            expires_at: now + chrono::Duration::hours(24),
        };

        // Create session in Redis (atomic HSET)
        self.session_manager.create(&session).await?;

        Ok(session)
    }

    pub(crate) async fn get_session(
        &self,
        upload_id: &str,
    ) -> Result<Option<UploadSession>, UploadError> {
        self.session_manager.get(upload_id).await
    }

    /// List active sessions for a caller (the uploader-scoped index is S-C1; the
    /// current index keys by owner, which coincides for self-uploads).
    pub(crate) async fn list_sessions_by_owner(
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

    pub(crate) async fn append_chunk(
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

        if session.status.is_inactive()
            || session.status == UploadSessionStatus::WaitingForProcessing
        {
            return Err(UploadError::SessionNotActive);
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
            return Err(UploadError::SizeExceeded);
        }
        if new_size > self.config.max_file_size as u64 {
            return Err(UploadError::FileTooLarge);
        }

        // Append to the session's single upload file (durability before ACK; the
        // file length is cross-checked against the offset inside).
        self.storage.append_at(upload_id, offset, data).await?;

        // First accepted chunk transitions Pending -> Uploading (observable via HEAD).
        if session.received_bytes == 0 && session.status == UploadSessionStatus::Pending {
            self.session_manager
                .update_status(upload_id, UploadSessionStatus::Uploading)
                .await?;
        }

        // Atomically increment received_bytes and refresh the survival-floor anchor.
        let new_received_bytes = self
            .session_manager
            .increment_received_bytes(upload_id, chunk_len)
            .await?;
        self.session_manager.touch_progress(upload_id).await?;

        let updated_session = UploadSession {
            received_bytes: new_received_bytes,
            status: UploadSessionStatus::Uploading,
            ..session
        };

        Ok(updated_session)
    }

    pub(crate) async fn finalize_upload(
        &self,
        upload_id: &str,
    ) -> Result<asset::Model, UploadError> {
        let session = self
            .get_session(upload_id)
            .await?
            .ok_or(UploadError::SessionNotFound)?;

        match session.status {
            UploadSessionStatus::WaitingForProcessing => {
                return Err(UploadError::FinalizeInProgress);
            }
            status if status.is_inactive() => return Err(UploadError::SessionNotActive),
            _ => {}
        }

        if session.total_size > 0 && session.received_bytes != session.total_size {
            return Err(UploadError::InvalidUpload(format!(
                "Upload not complete: received {} of {}",
                session.received_bytes, session.total_size
            )));
        }

        // Mark session as processing. (An atomic status compare-and-set replaces
        // this read-then-write guard with S-C1.)
        self.session_manager
            .update_status(upload_id, UploadSessionStatus::WaitingForProcessing)
            .await?;

        // The session's single upload file is already the complete blob — no
        // assembly step. Verify its hash on the blocking pool.
        let final_path = self.storage.get_upload_path(upload_id);
        let hash_path = final_path.clone();
        let actual_hash = tokio::task::spawn_blocking(move || get_file_hash(&hash_path))
            .await
            .map_err(|e| UploadError::ProcessingError(e.to_string()))?
            .map_err(|e| UploadError::ProcessingError(e.to_string()))?;

        if actual_hash != session.expected_hash {
            // Hash mismatch — treated as corruption: delete the upload file and
            // the pending asset row, fail the session.
            if let Err(e) = self.storage.remove(upload_id).await {
                tracing::warn!("Failed to delete file after hash mismatch: {}", e);
            }

            let _ = AssetService::Mutation::delete(&self.conn, &session.asset_id).await;

            self.session_manager
                .update_status(upload_id, UploadSessionStatus::FailedProcessing)
                .await?;

            return Err(UploadError::ContentHashMismatch {
                expected: session.expected_hash,
                actual: actual_hash,
            });
        }

        // Envelope re-validation inside the finalization transaction (invariant 15)
        // is wired by the envelope gate slice (S-C1); the envelope is on the session.

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

    pub(crate) async fn cancel_upload(&self, upload_id: &str) -> Result<(), UploadError> {
        // Get session to find asset_id
        let session = self.get_session(upload_id).await?;

        // Finalization is not interruptible.
        if let Some(session) = &session
            && session.status == UploadSessionStatus::WaitingForProcessing
        {
            return Err(UploadError::SessionNotActive);
        }

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

        // Delete the upload file from disk
        if let Err(e) = self.storage.remove(upload_id).await {
            tracing::warn!(
                "Failed to delete upload file for upload {}: {}",
                upload_id,
                e
            );
        }

        // Remove session from Redis
        self.session_manager.delete(upload_id).await?;

        Ok(())
    }
}
