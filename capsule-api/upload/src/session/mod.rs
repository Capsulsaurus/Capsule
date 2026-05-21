use crate::error::UploadError;
use crate::models::session::{UploadSession, UploadSessionStatus};
use bb8_redis::redis::AsyncCommands;
use bb8_redis::{RedisConnectionManager, bb8::Pool};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::time::Duration;

// TODO: Validate this code

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
            expiration: Duration::from_secs(24 * 60 * 60), // 24 hours default
        })
    }

    fn key(&self, upload_id: &str) -> String {
        format!("upload:session:{}", upload_id)
    }

    fn owner_index_key(&self, owner_id: &str) -> String {
        format!("upload:owner_sessions:{}", owner_id)
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
            (
                "expected_hash",
                session.expected_hash.to_string().into_bytes(),
            ),
            (
                "status",
                serde_json::to_string(&session.status)
                    .unwrap_or_else(|_| "\"Pending\"".to_string())
                    .into_bytes(),
            ),
            ("created_at", session.created_at.to_rfc3339().into_bytes()),
            ("expires_at", session.expires_at.to_rfc3339().into_bytes()),
        ];

        // Store optional fields if present
        if let Some(album_id) = &session.album_id {
            fields.push(("album_id", album_id.as_bytes().to_vec()));
        }
        if let Some(content_type) = &session.content_type {
            fields.push(("content_type", content_type.as_bytes().to_vec()));
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

        // Add to owner index (SADD for session listing by owner)
        let owner_index_key = self.owner_index_key(&session.owner_id);
        let _: () = conn.sadd(&owner_index_key, &session.id).await?;
        let _: () = conn
            .expire(
                &owner_index_key,
                i64::try_from(self.expiration.as_secs()).unwrap_or(i64::MAX) * 2, // Keep index longer
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

        Ok(())
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

    /// List sessions by owner ID. Returns active session IDs.
    pub async fn list_by_owner(&self, owner_id: &str) -> Result<Vec<String>, UploadError> {
        let mut conn = self.pool.get().await?;
        let owner_index_key = self.owner_index_key(owner_id);

        let session_ids: Vec<String> = conn.smembers(&owner_index_key).await?;
        Ok(session_ids)
    }

    /// Delete a session from Redis if it exists.
    /// Does not return error if it does not exist.
    pub async fn delete(&self, upload_id: &str) -> Result<(), UploadError> {
        let mut conn = self.pool.get().await?;
        let key = self.key(upload_id);

        // Get owner_id before deleting to clean up the index
        let owner_id: Option<String> = conn.hget(&key, "owner_id").await.ok();

        let _: () = conn.del(&key).await?;

        // Remove from owner index if we found the owner
        if let Some(owner_id) = owner_id {
            let owner_index_key = self.owner_index_key(&owner_id);
            let _: () = conn.srem(&owner_index_key, upload_id).await?;
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
                .map_err(|e| UploadError::Unknown(format!("Invalid UTF-8 in field {name}: {}", e)))
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
            .map_err(|e| UploadError::Unknown(format!("Invalid received_bytes: {}", e)))?;
        let total_size: u64 = get_string("total_size")?
            .parse()
            .map_err(|e| UploadError::Unknown(format!("Invalid total_size: {}", e)))?;
        let expected_hash: String = get_string("expected_hash")?;

        let status_str = get_string("status")?;
        let status: UploadSessionStatus = serde_json::from_str(&status_str)
            .map_err(|e| UploadError::Unknown(format!("Invalid status '{}': {}", status_str, e)))?;

        let created_at: DateTime<Utc> = get_string("created_at")?
            .parse()
            .map_err(|e| UploadError::Unknown(format!("Invalid created_at: {}", e)))?;

        let expires_at: DateTime<Utc> = get_string("expires_at")?
            .parse()
            .map_err(|e| UploadError::Unknown(format!("Invalid expires_at: {}", e)))?;

        Ok(UploadSession {
            id,
            asset_id,
            owner_id,
            upload_user_id,
            album_id,
            content_type,
            expected_hash,
            received_bytes,
            total_size,
            status,
            created_at,
            expires_at,
        })
    }
}
