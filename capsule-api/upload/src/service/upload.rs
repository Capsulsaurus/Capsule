use std::clone::Clone;

use capsule_core::crypto::hash::Hash32;
use capsule_core::utils::hash::get_file_hash;
use entity::{asset, user};
use jiff::{SignedDuration, Timestamp};
use nanoid::nanoid;
use sea_orm::{
    ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QuerySelect, TransactionTrait,
};
use service::attestation::{self, ReceiptInput};
use service::quota::{self, WriteClass};
use service::{album as AlbumService, asset as AssetService, sync as SyncFeed};

use crate::config::UploadServerConfig;
use crate::envelope::{revalidate_envelope, validate_create_envelope};
use crate::error::UploadError;
use crate::models::requests::{CreateUploadRequest, ManifestEnvelope};
use crate::models::session::{BlobRole, UploadSession, UploadSessionStatus};
use crate::service::processing::ProcessingService;
use crate::service::storage::StorageService;
use crate::session::UploadSessionManager;
use crate::visibility::{derive_original_held, finalization_makes_visible};

/// A device-directory `added_at` floor for a user whose row is missing (the JWT would be
/// invalid anyway; this keeps the pure envelope battery from spuriously failing invariant 7).
const EPOCH_RFC3339: &str = "1970-01-01T00:00:00Z";

/// Upper bound on the metadata blob bytes inlined onto a sync feed entry (S-C2). Larger
/// metadata blobs travel by content-address reference only; the design keeps them small.
const MAX_INLINE_METADATA: u64 = 1024 * 1024;

/// The outcome of a `POST /upload`: a freshly created session (`201`) or the active session
/// already open for the `(owner_id, hash, album_id)` idempotency tuple (`200`).
pub(crate) enum CreateOutcome {
    Created(UploadSession),
    Existing(UploadSession),
}

/// The outcome of a `PATCH` chunk append: a newly accepted chunk, or an idempotent replay of
/// an already-accepted `(offset, chunk_hash)` tuple returning the same offset (no re-write,
/// no re-finalize).
pub(crate) enum AppendOutcome {
    Accepted(Box<UploadSession>),
    Replay { new_offset: u64 },
}

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

    /// The uploader's device-authorization floor: the account-creation time, standing in for
    /// the device-directory `added_at` (invariant 7) until the directory table lands.
    async fn uploader_added_at(&self, user_id: &str) -> String {
        user::Entity::find_by_id(user_id)
            .one(&self.conn)
            .await
            .ok()
            .flatten()
            .map_or_else(|| EPOCH_RFC3339.to_string(), |u| u.created_at.to_rfc3339())
    }

    /// Create a new upload session with a pending asset row in Postgres.
    ///
    /// The envelope gate runs the refuse-by-default battery (invariants 1–8, 15 family)
    /// ahead of any write; album write-capability (invariant 6, DB half) and the
    /// `(owner_id, hash, album_id)` dedup (idempotency) are enforced in a single
    /// `SELECT … FOR UPDATE` + `INSERT` transaction that closes the TOCTOU race.
    #[tracing::instrument(skip(self, request), fields(owner_id, upload_user_id))]
    pub(crate) async fn create_session(
        &self,
        request: &CreateUploadRequest,
        owner_id: &str,
        upload_user_id: &str,
    ) -> Result<CreateOutcome, UploadError> {
        let now = Timestamp::now();
        let added_at = self.uploader_added_at(upload_user_id).await;

        // Refuse-by-default envelope battery (invariants 1–8, 15 family) BEFORE any write.
        validate_create_envelope(request, &self.config, &added_at, &now.to_string())?;

        // Moderation (S-C8): a suspended account cannot upload. This is an account-level gate,
        // distinct from quota and permission — refuse session creation with the structural
        // `error.moderation.account_suspended` code (403) before any album/quota work.
        match service::moderation::Suspension::is_suspended(&self.conn, upload_user_id).await {
            Ok(true) => {
                tracing::info!(upload_user_id, "upload session refused: account suspended");
                return Err(UploadError::AccountSuspended);
            }
            Ok(false) => {}
            Err(service::moderation::ModerationError::Db(e)) => {
                return Err(UploadError::DbError(e));
            }
            Err(e) => return Err(UploadError::Unknown(e.to_string())),
        }

        // Invariant 6 (DB half): album exists and the user has write capability on it.
        if let Some(album_id) = &request.album_id {
            match AlbumService::Query::get_album_access(&self.conn, owner_id, album_id).await {
                Ok(access)
                    if access
                        .as_ref()
                        .is_some_and(capsule_core::models::album::AlbumAccess::is_write) => {}
                Ok(_) => return Err(UploadError::AlbumAccessDenied),
                Err(e) => {
                    tracing::warn!("album access lookup failed: {}", e);
                    return Err(UploadError::AlbumAccessDenied);
                }
            }
        }

        let upload_id = nanoid!();

        // One transaction: lock the (owner, hash) rows, split by state, insert the pending
        // row only if creating a fresh session.
        let txn = self.conn.begin().await?;
        let existing_rows = asset::Entity::find()
            .filter(asset::Column::OwnerId.eq(owner_id))
            .filter(asset::Column::FileHash.eq(&request.hash))
            .filter(asset::Column::DeletedAt.is_null())
            .lock_exclusive()
            .all(&txn)
            .await?;

        // Finalized hash for this tuple → 409 duplicate_blob (the client's merge trigger).
        if let Some(finalized) = existing_rows
            .iter()
            .find(|a| a.uploaded && a.album_id == request.album_id)
        {
            txn.rollback().await?;
            tracing::info!(asset_id = %finalized.id, "duplicate blob: hash already finalized");
            return Err(UploadError::DuplicateBlob {
                asset_id: finalized.id.clone(),
            });
        }

        // Active pending session for this tuple → return it as-is (no second session). The
        // pending row records its upload id in `original_filename` (legacy plaintext column).
        if let Some(pending) = existing_rows
            .iter()
            .find(|a| !a.uploaded && a.album_id == request.album_id)
        {
            let existing_id = pending.original_filename.clone();
            if let Some(session) = self.session_manager.get(&existing_id).await?
                && session.status.is_active()
            {
                // No second session: return the active one (releasing the row lock).
                txn.rollback().await?;
                tracing::info!(upload_id = %existing_id, "returning active session for duplicate create");
                return Ok(CreateOutcome::Existing(session));
            }
            // The pending row is stale (its session expired). Fall through on the same
            // transaction and create a fresh session.
        }

        // Quota enforcement (S-C6): the single hard gate. This is a genuinely new blob (the
        // dedup/merge cases returned above), so charge its declared size against the
        // uploader's quota and refuse before any pending row is written if it crosses the
        // hard limit. The declared size becomes the reservation — the pending asset row this
        // transaction is about to insert — and is released if the session is cancelled
        // (the row is deleted) or expires.
        if let Err(e) = quota::Mutation::check(
            &txn,
            upload_user_id,
            request.size,
            WriteClass::UploadSession,
            &self.config.quota_limits(),
        )
        .await
        {
            txn.rollback().await?;
            return Err(match e {
                quota::QuotaError::Exceeded { .. } => UploadError::QuotaExceeded,
                quota::QuotaError::Db(db) => UploadError::DbError(db),
                // GraceLocked / PeerBudgetExceeded are not reachable for an UploadSession check.
                other => UploadError::Unknown(other.to_string()),
            });
        }

        // Insert the pending asset row inside this transaction.
        let asset_type = if request.content_type.starts_with("video/") {
            asset::AssetType::Video
        } else {
            asset::AssetType::Photo
        };
        // LEGACY-PLAINTEXT (frozen, S-C1/S-G3): the plaintext-era `original_filename` column
        // gets the opaque upload id — the wire request carries no filename.
        let asset = AssetService::Mutation::create_pending(
            &txn,
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
        txn.commit().await?;

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
            expires_at: now + SignedDuration::from_hours(24),
        };

        self.session_manager.create(&session).await?;
        tracing::info!(upload_id = %upload_id, asset_id = %asset.id, "upload session created");
        Ok(CreateOutcome::Created(session))
    }

    pub(crate) async fn get_session(
        &self,
        upload_id: &str,
    ) -> Result<Option<UploadSession>, UploadError> {
        self.session_manager.get(upload_id).await
    }

    /// List active sessions for the uploader (`upload_user_id`) — the party that resumes.
    pub(crate) async fn list_sessions_by_uploader(
        &self,
        upload_user_id: &str,
    ) -> Result<Vec<UploadSession>, UploadError> {
        let session_ids = self
            .session_manager
            .list_by_uploader(upload_user_id)
            .await?;
        let mut sessions = Vec::with_capacity(session_ids.len());

        for id in session_ids {
            if let Some(session) = self.session_manager.get(&id).await?
                && session.status.is_active()
            {
                sessions.push(session);
            }
        }

        Ok(sessions)
    }

    /// Append a chunk at `offset` whose SHA-256 is `chunk_hash` (already verified against the
    /// body). Honors the `(upload_id, offset, chunk_hash)` idempotency tuple: a replay of an
    /// accepted tuple is a no-op returning the same offset; the same offset with a different
    /// hash is a `chunk_conflict`.
    #[tracing::instrument(skip(self, data), fields(upload_id, offset, len = data.len()))]
    pub(crate) async fn append_chunk(
        &self,
        upload_id: &str,
        data: bytes::Bytes,
        offset: u64,
        chunk_hash: &str,
    ) -> Result<AppendOutcome, UploadError> {
        let session = self
            .get_session(upload_id)
            .await?
            .ok_or(UploadError::SessionNotFound)?;

        if session.status.is_inactive()
            || session.status == UploadSessionStatus::WaitingForProcessing
        {
            return Err(UploadError::SessionNotActive);
        }

        // Replay / conflict: an offset at or below the acknowledged region.
        if offset < session.received_bytes {
            return match self.session_manager.get_chunk(upload_id, offset).await? {
                Some((recorded_hash, next_offset)) if recorded_hash == chunk_hash => {
                    tracing::debug!(upload_id, offset, "idempotent chunk replay (no-op)");
                    Ok(AppendOutcome::Replay {
                        new_offset: next_offset,
                    })
                }
                Some(_) => Err(UploadError::ChunkConflict),
                // An already-acked offset with no record is an ordinary stale offset.
                None => Err(UploadError::InvalidOffset {
                    expected: session.received_bytes,
                    actual: offset,
                }),
            };
        }

        // Gapped / ahead-of-EOF offset.
        if offset != session.received_bytes {
            return Err(UploadError::InvalidOffset {
                expected: session.received_bytes,
                actual: offset,
            });
        }

        let chunk_len = data.len() as u64;
        let new_size = session.received_bytes + chunk_len;

        // Cumulative bounds (invariant 11): a violation is unsalvageable — the declaration
        // was broken — so the session moves to FailedProcessing and is cleaned up.
        if (session.total_size > 0 && new_size > session.total_size)
            || new_size > self.config.max_file_size as u64
        {
            let over_file_limit = new_size > self.config.max_file_size as u64;
            self.fail_session(&session).await;
            return Err(if over_file_limit {
                UploadError::FileTooLarge
            } else {
                UploadError::SizeExceeded
            });
        }

        // Durability before ACK; the file length is cross-checked against the offset inside.
        self.storage.append_at(upload_id, offset, data).await?;

        // First accepted chunk transitions Pending -> Uploading (observable via HEAD).
        if session.received_bytes == 0 && session.status == UploadSessionStatus::Pending {
            self.session_manager
                .update_status(upload_id, UploadSessionStatus::Uploading)
                .await?;
        }

        let new_received_bytes = self
            .session_manager
            .increment_received_bytes(upload_id, chunk_len)
            .await?;
        self.session_manager.touch_progress(upload_id).await?;
        // Record the accepted chunk for idempotent replay of a lost ACK.
        self.session_manager
            .record_chunk(upload_id, offset, chunk_hash, new_received_bytes)
            .await?;

        Ok(AppendOutcome::Accepted(Box::new(UploadSession {
            received_bytes: new_received_bytes,
            status: UploadSessionStatus::Uploading,
            ..session
        })))
    }

    /// Finalize a completed transfer: CAS into `WaitingForProcessing`, verify the ciphertext
    /// hash, re-validate the envelope (invariant 15), atomically commit the blob into the
    /// content-addressed store, then flip `uploaded` in one Postgres transaction.
    #[tracing::instrument(skip(self), fields(upload_id))]
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

        // Invariant 13: total received equals the declared size.
        if session.total_size > 0 && session.received_bytes != session.total_size {
            return Err(UploadError::InvalidUpload(format!(
                "Upload not complete: received {} of {}",
                session.received_bytes, session.total_size
            )));
        }

        // Atomic status CAS: only one finalizer wins; the loser observes the transition.
        if !self.session_manager.begin_finalize_cas(upload_id).await? {
            return Err(UploadError::FinalizeInProgress);
        }

        // Invariant 14: recompute the ciphertext hash on the blocking pool.
        let final_path = self.storage.get_upload_path(upload_id);
        let hash_path = final_path.clone();
        let actual_hash = tokio::task::spawn_blocking(move || get_file_hash(&hash_path))
            .await
            .map_err(|e| UploadError::ProcessingError(e.to_string()))?
            .map_err(|e| UploadError::ProcessingError(e.to_string()))?;

        if actual_hash != session.expected_hash {
            tracing::warn!(
                upload_id,
                "content hash mismatch on finalization; failing session"
            );
            self.fail_session(&session).await;
            return Err(UploadError::ContentHashMismatch {
                expected: session.expected_hash,
                actual: actual_hash,
            });
        }

        // Invariant 15: re-validate the envelope inside the finalization path, catching an
        // out-of-drift clock or a closed album since creation.
        let added_at = self.uploader_added_at(&session.upload_user_id).await;
        let now = Timestamp::now();
        if let Err(e) = revalidate_envelope(
            &session.manifest_envelope,
            &session.protocol_version,
            &self.config,
            &added_at,
            &now.to_string(),
        ) {
            tracing::warn!(upload_id, "envelope re-validation failed on finalization");
            self.fail_session(&session).await;
            return Err(e);
        }
        // Re-check album write-capability (invariant 6 re-validation).
        if let Some(album_id) = &session.album_id {
            let ok = AlbumService::Query::get_album_access(&self.conn, &session.owner_id, album_id)
                .await
                .ok()
                .flatten()
                .as_ref()
                .is_some_and(capsule_core::models::album::AlbumAccess::is_write);
            if !ok {
                self.fail_session(&session).await;
                return Err(UploadError::EnvelopeRejected(
                    "album write capability lost since creation".to_string(),
                ));
            }
        }

        // Best-effort server-side metadata: the server holds no key, so ciphertext blobs do
        // not decode — a decode failure is expected, not fatal (dimensions stay 0).
        let (width, height, date) =
            match self.processing_service.extract_metadata(&final_path).await {
                Ok(m) => (m.width, m.height, m.date),
                Err(_) => (0, 0, None),
            };

        // Atomic rename into the content-addressed blob store, then commit `uploaded` in one
        // Postgres transaction (the custody-receipt insert joins this txn in S-C15). A
        // failure after the rename un-finalizes the bundle: the blob is GC'd and the session
        // fails, per the asset-bundle atomicity invariant.
        self.storage
            .commit_blob(upload_id, &session.expected_hash)
            .await?;

        // Prepare the sync feed payload (S-C2) before opening the txn: the manifest as
        // canonical CBOR, the per-role blob refs, the inlined metadata blob (metadata role
        // only), and the derived `original_held` fact. A prep failure un-finalizes the bundle
        // exactly like a commit failure — the blob is GC'd and the session fails.
        let original_held = derive_original_held(session.blob_role == BlobRole::Original);
        let feed_input = if let Some(album_id) = &session.album_id {
            match self
                .prepare_feed_input(&session, album_id, original_held)
                .await
            {
                Ok(input) => Some(input),
                Err(e) => {
                    tracing::warn!(
                        upload_id,
                        "sync feed prep failed; rolling back bundle: {}",
                        e
                    );
                    let _ = self.storage.remove_blob(&session.expected_hash).await;
                    self.fail_session(&session).await;
                    return Err(e);
                }
            }
        } else {
            tracing::debug!(
                upload_id,
                "session has no album; no sync feed entry emitted"
            );
            None
        };

        // The custody-receipt input (S-C15): built from what the server itself recomputed or
        // verified at this commit — the ciphertext hash it re-hashed (== expected), the
        // declared size, and the envelope hash it re-serialized — never echoed from the
        // client. Prepared before the txn; a malformed stored envelope un-finalizes the
        // bundle exactly like a feed-prep failure.
        let receipt_input = match self.build_receipt_input(&session, now) {
            Ok(input) => input,
            Err(e) => {
                tracing::warn!(upload_id, "receipt prep failed; rolling back bundle: {}", e);
                let _ = self.storage.remove_blob(&session.expected_hash).await;
                self.fail_session(&session).await;
                return Err(e);
            }
        };

        let txn = self.conn.begin().await?;
        // The `sync_seq` mint (S-C2) and the `CustodyReceipt` (S-C15) both join THIS
        // finalization transaction: `mark_uploaded` flips `uploaded`, `record_finalization`
        // mints the per-album `sync_seq` and appends the feed row, and `issue_receipt` mints
        // the per-server `receipt_seq`, chains + signs the receipt, and inserts it —
        // atomically. Any failure rolls back all three (no receipt without durable custody,
        // no custody-marking without a receipt) and GCs the blob.
        let committed = async {
            let asset =
                AssetService::Mutation::mark_uploaded(&txn, &session.asset_id, width, height, date)
                    .await?;
            if let Some(input) = feed_input {
                SyncFeed::Mutation::record_finalization(&txn, input).await?;
            }
            attestation::Mutation::issue_receipt(&txn, &self.config.attestation, receipt_input)
                .await?;
            Ok::<_, sea_orm::DbErr>(asset)
        }
        .await;

        let asset = match committed {
            Ok(asset) => {
                txn.commit().await?;
                asset
            }
            Err(e) => {
                let _ = txn.rollback().await;
                tracing::warn!(
                    upload_id,
                    "finalization commit failed; rolling back bundle: {}",
                    e
                );
                let _ = self.storage.remove_blob(&session.expected_hash).await;
                let _ = AssetService::Mutation::delete(&self.conn, &session.asset_id).await;
                self.session_manager
                    .update_status(upload_id, UploadSessionStatus::FailedProcessing)
                    .await?;
                return Err(UploadError::Unknown(e.to_string()));
            }
        };

        self.session_manager
            .update_status(upload_id, UploadSessionStatus::Completed)
            .await?;

        // Visibility gate + original_held derivation (staged-uploads contract): visibility
        // flips on the metadata (T0) tier; the original-held fact is derived, never stored.
        let visible = finalization_makes_visible(session.blob_role);
        tracing::info!(
            upload_id,
            asset_id = %session.asset_id,
            blob_role = ?session.blob_role,
            flips_visibility = visible,
            original_held,
            "upload finalized (Completed)"
        );

        Ok(asset)
    }

    /// Build the sync feed entry payload (S-C2) for a finalized blob: the signed manifest as
    /// canonical CBOR (re-serialized from the stored envelope), the per-role blob refs, the
    /// inlined metadata blob (metadata role only), and the derived `original_held` fact.
    ///
    /// `original_held` is derived here via S-C1's `visibility::derive_original_held` — one
    /// definition, no second source of truth.
    async fn prepare_feed_input(
        &self,
        session: &UploadSession,
        album_id: &str,
        original_held: bool,
    ) -> Result<SyncFeed::FeedEntryInput, UploadError> {
        // The signed manifest travels as opaque canonical CBOR; the server holds only the
        // envelope projection, so re-serialize that canonically (never re-modeled on the wire).
        let envelope: ManifestEnvelope = serde_json::from_str(&session.manifest_envelope)?;
        let manifest_cbor = capsule_core::cbor::to_canonical_vec(&envelope)
            .map_err(|e| UploadError::ProcessingError(format!("manifest cbor: {e}")))?;

        // Inline the metadata blob only when this blob *is* the metadata blob and it is small.
        let metadata_blob = if session.blob_role == BlobRole::Metadata {
            let bytes = self
                .storage
                .read_committed_blob(&session.expected_hash)
                .await?;
            if bytes.len() as u64 <= MAX_INLINE_METADATA {
                Some(bytes)
            } else {
                tracing::warn!(
                    upload_id = %session.id,
                    "metadata blob too large to inline on the feed; carrying ref only"
                );
                None
            }
        } else {
            None
        };

        let blob_ref = SyncFeed::FeedBlobRef {
            ciphertext_hash: session.expected_hash.clone(),
            role: session.blob_role.as_str().to_string(),
            format: session.content_type.clone().unwrap_or_default(),
            size: session.total_size,
        };
        let blobs = if session.blob_role == BlobRole::Original {
            SyncFeed::FeedBlobManifest {
                original: Some(blob_ref),
                derivatives: Vec::new(),
            }
        } else {
            SyncFeed::FeedBlobManifest {
                original: None,
                derivatives: vec![blob_ref],
            }
        };

        Ok(SyncFeed::FeedEntryInput {
            album_id: album_id.to_string(),
            protocol_version: session.protocol_version.clone(),
            kind: SyncFeed::ChangeKind::Created,
            asset_id: session.asset_id.clone(),
            manifest_cbor,
            metadata_blob,
            blobs,
            original_held,
        })
    }

    /// Build the [`ReceiptInput`] for a finalized blob (S-C15): the server-recomputed
    /// ciphertext hash (== the verified `expected_hash`), declared size, the envelope hash
    /// the server re-serializes from the stored envelope (binding the receipt to the asset's
    /// provenance-chain position), and the uploading device from the envelope.
    fn build_receipt_input(
        &self,
        session: &UploadSession,
        received_at: Timestamp,
    ) -> Result<ReceiptInput, UploadError> {
        let ciphertext_hash = Hash32::from_hex(&session.expected_hash).map_err(|_| {
            UploadError::ProcessingError("finalized hash is not valid hex".to_string())
        })?;

        // Re-serialize the stored envelope canonically and hash it — the same projection the
        // sync feed carries, so the receipt's `envelope_hash` binds to the committed manifest.
        let envelope: ManifestEnvelope = serde_json::from_str(&session.manifest_envelope)?;
        let envelope_cbor = capsule_core::cbor::to_canonical_vec(&envelope)
            .map_err(|e| UploadError::ProcessingError(format!("envelope cbor: {e}")))?;
        let envelope_hash = Some(capsule_core::crypto::hash::hash_bytes(&envelope_cbor));
        let uploaded_by_device = Some(envelope.created_by_device.clone());

        Ok(ReceiptInput {
            protocol_version: session.protocol_version.clone(),
            upload_id: session.id.clone(),
            asset_id: session.asset_id.clone(),
            blob_role: session.blob_role.as_str().to_string(),
            ciphertext_hash,
            size: session.total_size,
            envelope_hash,
            uploaded_by_user: session.upload_user_id.clone(),
            uploaded_by_device,
            received_at,
        })
    }

    pub(crate) async fn cancel_upload(&self, upload_id: &str) -> Result<(), UploadError> {
        let session = self.get_session(upload_id).await?;

        // Finalization is not interruptible.
        if let Some(session) = &session
            && session.status == UploadSessionStatus::WaitingForProcessing
        {
            return Err(UploadError::SessionNotActive);
        }

        // Quota release (S-C6): deleting the pending asset row drops the reservation — the
        // uploader's `quota_used` counts every present `assets` row (pending or finalized),
        // so removing it frees the reserved-but-uncommitted bytes, and the next quota check
        // sees the lower usage. No separate ledger write is needed for originals.
        if let Some(session) = &session
            && let Err(e) = AssetService::Mutation::delete(&self.conn, &session.asset_id).await
        {
            tracing::warn!(
                "Failed to delete asset {} from Postgres: {}",
                session.asset_id,
                e
            );
        }

        if let Err(e) = self.storage.remove(upload_id).await {
            tracing::warn!(
                "Failed to delete upload file for upload {}: {}",
                upload_id,
                e
            );
        }

        self.session_manager.delete(upload_id).await?;

        Ok(())
    }

    /// Move a session to `FailedProcessing` and clean up its upload file and pending row.
    async fn fail_session(&self, session: &UploadSession) {
        if let Err(e) = self.storage.remove(&session.id).await {
            tracing::warn!("failed to remove upload file on failure: {}", e);
        }
        let _ = AssetService::Mutation::delete(&self.conn, &session.asset_id).await;
        if let Err(e) = self
            .session_manager
            .update_status(&session.id, UploadSessionStatus::FailedProcessing)
            .await
        {
            tracing::warn!("failed to mark session FailedProcessing: {}", e);
        }
    }
}
