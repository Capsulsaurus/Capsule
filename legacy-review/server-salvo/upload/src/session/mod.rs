use std::collections::HashMap;
use std::time::Duration;

use bb8_redis::RedisConnectionManager;
use bb8_redis::bb8::Pool;
use bb8_redis::redis::{AsyncCommands, Script};
use jiff::Timestamp;

use crate::error::UploadError;
use crate::models::session::{BlobRole, UploadSession, UploadSessionStatus};

/// The global progress index: a sorted set of active session ids scored by the epoch
/// second of their last accepted chunk. Pressure eviction reads it least-recently-
/// progressed first; the ≥1-hour survival floor is a score filter over it.
const PROGRESS_INDEX_KEY: &str = "upload:progress_index";

#[derive(Clone)]
pub struct UploadSessionManager {
    pool: Pool<RedisConnectionManager>,
    expiration: Duration,
}

impl UploadSessionManager {
    pub async fn new(valkey_url: &str) -> Result<Self, UploadError> {
        let manager = RedisConnectionManager::new(valkey_url)?;
        let pool = Pool::builder().build(manager).await?;
        Ok(Self {
            pool,
            expiration: Duration::from_hours(24), // 24 hours default
        })
    }

    fn key(&self, upload_id: &str) -> String {
        format!("upload:session:{upload_id}")
    }

    /// The uploader-scoped session index — keyed by `upload_user_id` (the resuming party),
    /// so `GET /upload/sessions` lists exactly the sessions that uploader can resume.
    fn uploader_index_key(&self, upload_user_id: &str) -> String {
        format!("upload:uploader_sessions:{upload_user_id}")
    }

    /// The per-session chunk-replay store: `(offset -> "{chunk_hash}:{next_offset}")`, the
    /// durable half of the `(upload_id, offset, chunk_hash)` idempotency tuple.
    fn chunks_key(&self, upload_id: &str) -> String {
        format!("upload:chunks:{upload_id}")
    }

    /// Create a new upload session in Redis using HSET.
    /// This sets all fields at once during session creation.
    pub async fn create(&self, session: &UploadSession) -> Result<(), UploadError> {
        let mut conn = self.pool.get().await?;
        let key = self.key(&session.id);

        // Build field-value pairs for HSET
        let mut fields: Vec<(&str, Vec<u8>)> = vec![
            ("id", session.id.as_bytes().to_vec()),
            ("asset_id", session.asset_id.as_bytes().to_vec()),
            ("owner_id", session.owner_id.as_bytes().to_vec()),
            ("upload_user_id", session.upload_user_id.as_bytes().to_vec()),
            ("total_size", session.total_size.to_string().into_bytes()),
            (
                "received_bytes",
                session.received_bytes.to_string().into_bytes(),
            ),
            ("expected_hash", session.expected_hash.clone().into_bytes()),
            (
                "crypto_suite_id",
                session.crypto_suite_id.to_string().into_bytes(),
            ),
            (
                "protocol_version",
                session.protocol_version.clone().into_bytes(),
            ),
            ("blob_role", session.blob_role.as_str().as_bytes().to_vec()),
            (
                "manifest_envelope",
                session.manifest_envelope.clone().into_bytes(),
            ),
            (
                "status",
                serde_json::to_string(&session.status)
                    .unwrap_or_else(|_| "\"Pending\"".to_string())
                    .into_bytes(),
            ),
            ("created_at", session.created_at.to_string().into_bytes()),
            (
                "last_progress_at",
                session.last_progress_at.to_string().into_bytes(),
            ),
            ("expires_at", session.expires_at.to_string().into_bytes()),
        ];

        // Store optional fields if present
        if let Some(album_id) = &session.album_id {
            fields.push(("album_id", album_id.as_bytes().to_vec()));
        }
        if let Some(content_type) = &session.content_type {
            fields.push(("content_type", content_type.as_bytes().to_vec()));
        }
        if let Some(intent_id) = &session.intent_id {
            fields.push(("intent_id", intent_id.as_bytes().to_vec()));
        }

        // Use HSET with multiple fields
        let mut cmd = bb8_redis::redis::cmd("HSET");
        cmd.arg(&key);
        for (field, value) in fields {
            cmd.arg(field).arg(value);
        }
        let _: () = cmd.query_async(&mut *conn).await?;

        // Set expiration
        let _: () = conn
            .expire(
                &key,
                i64::try_from(self.expiration.as_secs()).unwrap_or(i64::MAX),
            )
            .await?;

        // Add to the uploader-scoped index for session listing.
        let uploader_index_key = self.uploader_index_key(&session.upload_user_id);
        let _: () = conn.sadd(&uploader_index_key, &session.id).await?;
        let _: () = conn
            .expire(
                &uploader_index_key,
                i64::try_from(self.expiration.as_secs()).unwrap_or(i64::MAX) * 2, // Keep index longer
            )
            .await?;

        // Seed the progress index at the creation time (no chunk yet).
        let _: () = conn
            .zadd(
                PROGRESS_INDEX_KEY,
                &session.id,
                session.last_progress_at.as_second(),
            )
            .await?;

        Ok(())
    }

    /// Atomically increment received_bytes using HINCRBY.
    /// Returns the new value of received_bytes.
    pub async fn increment_received_bytes(
        &self,
        upload_id: &str,
        bytes: u64,
    ) -> Result<u64, UploadError> {
        let mut conn = self.pool.get().await?;
        let key = self.key(upload_id);

        let new_value: i64 = conn.hincr(&key, "received_bytes", bytes as i64).await?;

        Ok(new_value as u64)
    }

    /// Set `received_bytes` to an absolute value. Used only by the startup scrub to
    /// reconcile the session counter up to the on-disk file length (the file is the truth)
    /// after a crash between the durable append and the counter increment.
    pub(crate) async fn set_received_bytes(
        &self,
        upload_id: &str,
        value: u64,
    ) -> Result<(), UploadError> {
        let mut conn = self.pool.get().await?;
        let key = self.key(upload_id);
        let _: () = conn.hset(&key, "received_bytes", value.to_string()).await?;
        Ok(())
    }

    /// Record chunk progress: refreshes `last_progress_at` (the anchor of the ≥1-hour
    /// survival floor) on the session hash **and** its score in the progress index.
    pub async fn touch_progress(&self, upload_id: &str) -> Result<(), UploadError> {
        let mut conn = self.pool.get().await?;
        let key = self.key(upload_id);
        let now = Timestamp::now();
        let _: () = conn.hset(&key, "last_progress_at", now.to_string()).await?;
        let _: () = conn
            .zadd(PROGRESS_INDEX_KEY, upload_id, now.as_second())
            .await?;
        Ok(())
    }

    /// Atomically update the upload status.
    pub async fn update_status(
        &self,
        upload_id: &str,
        status: UploadSessionStatus,
    ) -> Result<(), UploadError> {
        let mut conn = self.pool.get().await?;
        let key = self.key(upload_id);

        let status_json = serde_json::to_string(&status)?;
        let _: () = conn.hset(&key, "status", status_json).await?;

        // A terminal session leaves the progress index — its bytes are already gone, so it
        // is exempt from pressure eviction (the receipt is retained until the 24-hour cap).
        if status.is_inactive() {
            let _: () = conn.zrem(PROGRESS_INDEX_KEY, upload_id).await?;
        }

        Ok(())
    }

    /// Atomic compare-and-set into `WaitingForProcessing` (the finalization CAS). Only a
    /// session currently `Pending` or `Uploading` transitions; two racing finalizers cannot
    /// both win. Returns `true` for the winner, `false` for a loser (which returns
    /// `finalize_in_progress`). Removes the winner from the progress index — a finalizing
    /// session is never evicted out from under the finalizer.
    pub async fn begin_finalize_cas(&self, upload_id: &str) -> Result<bool, UploadError> {
        let mut conn = self.pool.get().await?;
        let key = self.key(upload_id);
        let script = Script::new(
            r#"
            local cur = redis.call('HGET', KEYS[1], 'status')
            if cur == '"Pending"' or cur == '"Uploading"' then
                redis.call('HSET', KEYS[1], 'status', '"WaitingForProcessing"')
                redis.call('ZREM', KEYS[2], ARGV[1])
                return 1
            else
                return 0
            end
            "#,
        );
        let won: i64 = script
            .key(&key)
            .key(PROGRESS_INDEX_KEY)
            .arg(upload_id)
            .invoke_async(&mut *conn)
            .await?;
        Ok(won == 1)
    }

    /// Record an accepted chunk in the replay store, keyed by its offset. TTL-bounded with
    /// the session. `next_offset` is the received-byte count after this chunk.
    pub async fn record_chunk(
        &self,
        upload_id: &str,
        offset: u64,
        chunk_hash: &str,
        next_offset: u64,
    ) -> Result<(), UploadError> {
        let mut conn = self.pool.get().await?;
        let key = self.chunks_key(upload_id);
        let _: () = conn
            .hset(
                &key,
                offset.to_string(),
                format!("{chunk_hash}:{next_offset}"),
            )
            .await?;
        let _: () = conn
            .expire(
                &key,
                i64::try_from(self.expiration.as_secs()).unwrap_or(i64::MAX),
            )
            .await?;
        Ok(())
    }

    /// Look up a recorded chunk at `offset`, returning `(chunk_hash, next_offset)` if this
    /// offset was previously accepted.
    pub async fn get_chunk(
        &self,
        upload_id: &str,
        offset: u64,
    ) -> Result<Option<(String, u64)>, UploadError> {
        let mut conn = self.pool.get().await?;
        let key = self.chunks_key(upload_id);
        let raw: Option<String> = conn.hget(&key, offset.to_string()).await?;
        Ok(raw.and_then(|v| {
            let (hash, next) = v.rsplit_once(':')?;
            Some((hash.to_string(), next.parse().ok()?))
        }))
    }

    /// Get a session by ID using HGETALL.
    pub async fn get(&self, upload_id: &str) -> Result<Option<UploadSession>, UploadError> {
        let mut conn = self.pool.get().await?;
        let key = self.key(upload_id);

        let fields: HashMap<String, Vec<u8>> = conn.hgetall(&key).await?;

        if fields.is_empty() {
            return Ok(None);
        }

        // Parse fields into UploadSession
        let session = self.parse_session_from_hash(upload_id, fields)?;
        Ok(Some(session))
    }

    /// List sessions by the uploader (`upload_user_id`). Returns session IDs.
    pub(crate) async fn list_by_uploader(
        &self,
        upload_user_id: &str,
    ) -> Result<Vec<String>, UploadError> {
        let mut conn = self.pool.get().await?;
        let index_key = self.uploader_index_key(upload_user_id);

        let session_ids: Vec<String> = conn.smembers(&index_key).await?;
        Ok(session_ids)
    }

    /// All session ids currently in the progress index (active, non-terminal sessions).
    /// Used by the pressure sweeper and the startup scrub.
    pub(crate) async fn list_progress_ids(&self) -> Result<Vec<String>, UploadError> {
        let mut conn = self.pool.get().await?;
        let ids: Vec<String> = conn.zrange(PROGRESS_INDEX_KEY, 0, -1).await?;
        Ok(ids)
    }

    /// Candidates eligible for pressure eviction: active sessions whose last progress is
    /// older than `floor_epoch` (i.e. beyond the ≥1-hour survival floor), least-recently-
    /// progressed first. Returns `(id, last_progress_epoch)` pairs in ascending score order.
    #[allow(dead_code)]
    pub(crate) async fn evictable_candidates(
        &self,
        floor_epoch: i64,
    ) -> Result<Vec<(String, i64)>, UploadError> {
        let mut conn = self.pool.get().await?;
        // Scores strictly below the floor: progress older than the survival window.
        let raw: Vec<(String, i64)> = conn
            .zrangebyscore_withscores(PROGRESS_INDEX_KEY, "-inf", format!("({floor_epoch}"))
            .await?;
        Ok(raw)
    }

    /// Delete a session from Redis if it exists.
    /// Does not return error if it does not exist.
    pub async fn delete(&self, upload_id: &str) -> Result<(), UploadError> {
        let mut conn = self.pool.get().await?;
        let key = self.key(upload_id);

        // Get the uploader before deleting to clean up the index.
        let upload_user_id: Option<String> = conn.hget(&key, "upload_user_id").await.ok();

        let _: () = conn.del(&key).await?;
        let _: () = conn.del(self.chunks_key(upload_id)).await?;
        let _: () = conn.zrem(PROGRESS_INDEX_KEY, upload_id).await?;

        // Remove from the uploader index if we found the uploader.
        if let Some(uid) = upload_user_id {
            let index_key = self.uploader_index_key(&uid);
            let _: () = conn.srem(&index_key, upload_id).await?;
        }

        Ok(())
    }

    /// Parse a HashMap from HGETALL into an UploadSession struct.
    fn parse_session_from_hash(
        &self,
        upload_id: &str,
        fields: HashMap<String, Vec<u8>>,
    ) -> Result<UploadSession, UploadError> {
        let get_string = |name: &str| -> Result<String, UploadError> {
            let bytes = fields.get(name).ok_or_else(|| {
                UploadError::Unknown(format!("Missing field '{name}' in session {upload_id}"))
            })?;
            String::from_utf8(bytes.clone())
                .map_err(|e| UploadError::Unknown(format!("Invalid UTF-8 in field {name}: {e}")))
        };

        let id = get_string("id")?;
        let asset_id = get_string("asset_id")?;
        let owner_id = get_string("owner_id")?;
        let upload_user_id = get_string("upload_user_id")?;

        let album_id = fields
            .get("album_id")
            .and_then(|bytes| String::from_utf8(bytes.clone()).ok());
        let content_type = fields
            .get("content_type")
            .and_then(|bytes| String::from_utf8(bytes.clone()).ok());

        let received_bytes: u64 = get_string("received_bytes")?
            .parse()
            .map_err(|e| UploadError::Unknown(format!("Invalid received_bytes: {e}")))?;
        let total_size: u64 = get_string("total_size")?
            .parse()
            .map_err(|e| UploadError::Unknown(format!("Invalid total_size: {e}")))?;
        let expected_hash: String = get_string("expected_hash")?;

        let crypto_suite_id: u16 = get_string("crypto_suite_id")?
            .parse()
            .map_err(|e| UploadError::Unknown(format!("Invalid crypto_suite_id: {e}")))?;
        let protocol_version = get_string("protocol_version")?;
        let blob_role_str = get_string("blob_role")?;
        let blob_role: BlobRole =
            serde_json::from_str(&format!("\"{blob_role_str}\"")).map_err(|e| {
                UploadError::Unknown(format!("Invalid blob_role '{blob_role_str}': {e}"))
            })?;
        let manifest_envelope = get_string("manifest_envelope")?;
        let intent_id = fields
            .get("intent_id")
            .and_then(|bytes| String::from_utf8(bytes.clone()).ok());

        let status_str = get_string("status")?;
        let status: UploadSessionStatus = serde_json::from_str(&status_str)
            .map_err(|e| UploadError::Unknown(format!("Invalid status '{status_str}': {e}")))?;

        let created_at: Timestamp = get_string("created_at")?
            .parse()
            .map_err(|e| UploadError::Unknown(format!("Invalid created_at: {e}")))?;

        let last_progress_at: Timestamp = get_string("last_progress_at")?
            .parse()
            .map_err(|e| UploadError::Unknown(format!("Invalid last_progress_at: {e}")))?;

        let expires_at: Timestamp = get_string("expires_at")?
            .parse()
            .map_err(|e| UploadError::Unknown(format!("Invalid expires_at: {e}")))?;

        Ok(UploadSession {
            id,
            asset_id,
            owner_id,
            upload_user_id,
            album_id,
            content_type,
            expected_hash,
            crypto_suite_id,
            protocol_version,
            blob_role,
            intent_id,
            manifest_envelope,
            received_bytes,
            total_size,
            status,
            created_at,
            last_progress_at,
            expires_at,
        })
    }
}
