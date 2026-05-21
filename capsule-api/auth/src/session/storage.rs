use async_trait::async_trait;
use bb8_redis::RedisConnectionManager;
use bb8_redis::bb8::Pool;
use redis::AsyncCommands;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

use model::errors::InternalServerError;

/// Result of a rate limit check: count of requests in current window and remaining TTL (secs).
pub struct RateLimitResult {
    pub count: i64,
    pub window_ttl_secs: u64,
}

#[async_trait::async_trait]
pub trait SessionStorage: Send + Sync {
    /// Saves a session to storage with the given session ID and data.
    ///
    /// # Arguments
    /// * `sid` - The session identifier
    /// * `session_data` - The serialized session data to store
    /// * `ttl` - Time-to-live duration for the session
    ///
    /// # Returns
    /// Returns `Ok(())` on success, or an `InternalServerError` if the operation fails.
    async fn save_session(
        &self,
        sid: &str,
        session_data: String,
        ttl: Duration,
    ) -> Result<(), InternalServerError>;

    /// Retrieves session data for the given session ID.
    ///
    /// # Arguments
    /// * `sid` - The session identifier to look up
    ///
    /// # Returns
    /// Returns `Ok(Some(session_data))` if the session exists, `Ok(None)` if not found,
    /// or an `InternalServerError` if the operation fails.
    async fn get_session(&self, sid: &str) -> Result<Option<String>, InternalServerError>;

    /// Deletes a session from storage.
    ///
    /// # Arguments
    /// * `sid` - The session identifier to delete
    ///
    /// # Returns
    /// Returns `Ok(())` on success, or an `InternalServerError` if the operation fails.
    async fn delete_session(&self, sid: &str) -> Result<(), InternalServerError>;

    /// Associates a session with a user, allowing tracking of all active sessions for a user.
    ///
    /// # Arguments
    /// * `user_id` - The user identifier
    /// * `sid` - The session identifier to associate with the user
    /// * `ttl` - Time-to-live duration for this user-session association
    ///
    /// # Returns
    /// Returns `Ok(())` on success, or an `InternalServerError` if the operation fails.
    async fn add_user_session(
        &self,
        user_id: &str,
        sid: &str,
        ttl: Duration,
    ) -> Result<(), InternalServerError>;

    /// Retrieves all active session IDs for a given user.
    ///
    /// # Arguments
    /// * `user_id` - The user identifier
    ///
    /// # Returns
    /// Returns `Ok(Vec<session_ids>)` with all active session IDs for the user,
    /// or an `InternalServerError` if the operation fails.
    async fn get_user_sessions(&self, user_id: &str) -> Result<Vec<String>, InternalServerError>;

    /// Deletes the user sessions key, effectively removing the association between a user and their sessions.
    ///
    /// Note: This does not delete the individual sessions themselves, only the tracking key.
    ///
    /// # Arguments
    /// * `user_id` - The user identifier
    ///
    /// # Returns
    /// Returns `Ok(())` on success, or an `InternalServerError` if the operation fails.
    async fn delete_user_sessions_key(&self, user_id: &str) -> Result<(), InternalServerError>;

    // MFA attempt tracking
    /// Increments the MFA attempt counter for a given MFA token.
    ///
    /// # Arguments
    /// * `mfa_token_jti` - The JWT ID (jti) of the MFA token
    ///
    /// # Returns
    /// Returns `Ok(attempt_count)` with the new attempt count after incrementing,
    /// or an `InternalServerError` if the operation fails.
    async fn increment_mfa_attempt(&self, mfa_token_jti: &str) -> Result<i32, InternalServerError>;

    /// Retrieves the current MFA attempt count for a given MFA token.
    ///
    /// # Arguments
    /// * `mfa_token_jti` - The JWT ID (jti) of the MFA token
    ///
    /// # Returns
    /// Returns `Ok(attempt_count)` with the current attempt count (0 if no attempts recorded),
    /// or an `InternalServerError` if the operation fails.
    async fn get_mfa_attempts(&self, mfa_token_jti: &str) -> Result<i32, InternalServerError>;

    /// Clears the MFA attempt counter for a given MFA token.
    ///
    /// This should be called after successful MFA verification.
    ///
    /// # Arguments
    /// * `mfa_token_jti` - The JWT ID (jti) of the MFA token
    ///
    /// # Returns
    /// Returns `Ok(())` on success, or an `InternalServerError` if the operation fails.
    async fn clear_mfa_attempts(&self, mfa_token_jti: &str) -> Result<(), InternalServerError>;

    /// Increments a rate-limit counter for `key` and returns current count + window TTL.
    /// Uses a fixed window of `window_secs`. On first request in the window, sets expiry.
    async fn increment_rate_limit(
        &self,
        key: &str,
        window_secs: u64,
    ) -> Result<RateLimitResult, InternalServerError>;

    // Ephemeral/Temporary data storage (for flows like Passkey)
    async fn save_temp_data(
        &self,
        key: &str,
        value: String,
        ttl: Duration,
    ) -> Result<(), InternalServerError>;

    async fn get_temp_data(&self, key: &str) -> Result<Option<String>, InternalServerError>;

    async fn delete_temp_data(&self, key: &str) -> Result<(), InternalServerError>;
}

#[derive(Clone)]
pub struct RedisSessionStorage {
    client: Pool<RedisConnectionManager>,
}

impl RedisSessionStorage {
    pub fn new(client: Pool<RedisConnectionManager>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl SessionStorage for RedisSessionStorage {
    async fn save_session(
        &self,
        sid: &str,
        session_data: String,
        ttl: Duration,
    ) -> Result<(), InternalServerError> {
        let mut con = self
            .client
            .get()
            .await
            .map_err(|e| InternalServerError::from(eyre::eyre!(e)))?;
        let key = format!("capsule:session:{}", sid);
        let _: () = con
            .set_ex(&key, session_data, ttl.as_secs())
            .await
            .map_err(|e| InternalServerError::from(eyre::eyre!(e)))?;
        Ok(())
    }

    async fn get_session(&self, sid: &str) -> Result<Option<String>, InternalServerError> {
        let mut con = self
            .client
            .get()
            .await
            .map_err(|e| InternalServerError::from(eyre::eyre!(e)))?;
        let key = format!("capsule:session:{}", sid);
        let data: Option<String> = con
            .get(&key)
            .await
            .map_err(|e| InternalServerError::from(eyre::eyre!(e)))?;
        Ok(data)
    }

    async fn delete_session(&self, sid: &str) -> Result<(), InternalServerError> {
        let mut con = self
            .client
            .get()
            .await
            .map_err(|e| InternalServerError::from(eyre::eyre!(e)))?;
        let key = format!("capsule:session:{}", sid);
        let _: () = con
            .del(&key)
            .await
            .map_err(|e| InternalServerError::from(eyre::eyre!(e)))?;
        Ok(())
    }

    async fn add_user_session(
        &self,
        user_id: &str,
        sid: &str,
        ttl: Duration,
    ) -> Result<(), InternalServerError> {
        let mut con = self
            .client
            .get()
            .await
            .map_err(|e| InternalServerError::from(eyre::eyre!(e)))?;
        let user_key = format!("capsule:user_sessions:{}", user_id);
        let _: () = con
            .sadd(&user_key, sid)
            .await
            .map_err(|e| InternalServerError::from(eyre::eyre!(e)))?;
        let _: () = con
            .expire(&user_key, ttl.as_secs() as i64)
            .await
            .map_err(|e| InternalServerError::from(eyre::eyre!(e)))?;
        Ok(())
    }

    async fn get_user_sessions(&self, user_id: &str) -> Result<Vec<String>, InternalServerError> {
        let mut con = self
            .client
            .get()
            .await
            .map_err(|e| InternalServerError::from(eyre::eyre!(e)))?;
        let user_key = format!("capsule:user_sessions:{}", user_id);
        let sessions: Vec<String> = con
            .smembers(&user_key)
            .await
            .map_err(|e| InternalServerError::from(eyre::eyre!(e)))?;
        Ok(sessions)
    }

    async fn delete_user_sessions_key(&self, user_id: &str) -> Result<(), InternalServerError> {
        let mut con = self
            .client
            .get()
            .await
            .map_err(|e| InternalServerError::from(eyre::eyre!(e)))?;
        let user_key = format!("capsule:user_sessions:{}", user_id);
        let _: () = con
            .del(&user_key)
            .await
            .map_err(|e| InternalServerError::from(eyre::eyre!(e)))?;
        Ok(())
    }

    async fn increment_mfa_attempt(&self, mfa_token_jti: &str) -> Result<i32, InternalServerError> {
        let mut con = self
            .client
            .get()
            .await
            .map_err(|e| InternalServerError::from(eyre::eyre!(e)))?;
        let key = format!("capsule:mfa_attempts:{}", mfa_token_jti);
        let count: i32 = con
            .incr(&key, 1)
            .await
            .map_err(|e| InternalServerError::from(eyre::eyre!(e)))?;
        // Set expiry to 5 minutes if this is the first attempt
        if count == 1 {
            let _: () = con
                .expire(&key, 300) // 5 minutes
                .await
                .map_err(|e| InternalServerError::from(eyre::eyre!(e)))?;
        }
        Ok(count)
    }

    async fn get_mfa_attempts(&self, mfa_token_jti: &str) -> Result<i32, InternalServerError> {
        let mut con = self
            .client
            .get()
            .await
            .map_err(|e| InternalServerError::from(eyre::eyre!(e)))?;
        let key = format!("capsule:mfa_attempts:{}", mfa_token_jti);
        let count: Option<i32> = con
            .get(&key)
            .await
            .map_err(|e| InternalServerError::from(eyre::eyre!(e)))?;
        Ok(count.unwrap_or(0))
    }

    async fn clear_mfa_attempts(&self, mfa_token_jti: &str) -> Result<(), InternalServerError> {
        let mut con = self
            .client
            .get()
            .await
            .map_err(|e| InternalServerError::from(eyre::eyre!(e)))?;
        let key = format!("capsule:mfa_attempts:{}", mfa_token_jti);
        let _: () = con
            .del(&key)
            .await
            .map_err(|e| InternalServerError::from(eyre::eyre!(e)))?;
        Ok(())
    }

    async fn increment_rate_limit(
        &self,
        key: &str,
        window_secs: u64,
    ) -> Result<RateLimitResult, InternalServerError> {
        let mut con = self
            .client
            .get()
            .await
            .map_err(|e| InternalServerError::from(eyre::eyre!(e)))?;
        let redis_key = format!("capsule:rate_limit:{}", key);
        let count: i64 = con
            .incr(&redis_key, 1)
            .await
            .map_err(|e| InternalServerError::from(eyre::eyre!(e)))?;
        if count == 1 {
            let _: () = con
                .expire(&redis_key, window_secs as i64)
                .await
                .map_err(|e| InternalServerError::from(eyre::eyre!(e)))?;
        }
        let ttl: i64 = con
            .ttl(&redis_key)
            .await
            .map_err(|e| InternalServerError::from(eyre::eyre!(e)))?;
        Ok(RateLimitResult {
            count,
            window_ttl_secs: ttl.max(0) as u64,
        })
    }

    async fn save_temp_data(
        &self,
        key: &str,
        value: String,
        ttl: Duration,
    ) -> Result<(), InternalServerError> {
        let mut con = self
            .client
            .get()
            .await
            .map_err(|e| InternalServerError::from(eyre::eyre!(e)))?;
        let redis_key = format!("capsule:temp:{}", key);
        let _: () = con
            .set_ex(&redis_key, value, ttl.as_secs())
            .await
            .map_err(|e| InternalServerError::from(eyre::eyre!(e)))?;
        Ok(())
    }

    async fn get_temp_data(&self, key: &str) -> Result<Option<String>, InternalServerError> {
        let mut con = self
            .client
            .get()
            .await
            .map_err(|e| InternalServerError::from(eyre::eyre!(e)))?;
        let redis_key = format!("capsule:temp:{}", key);
        let data: Option<String> = con
            .get(&redis_key)
            .await
            .map_err(|e| InternalServerError::from(eyre::eyre!(e)))?;
        Ok(data)
    }

    async fn delete_temp_data(&self, key: &str) -> Result<(), InternalServerError> {
        let mut con = self
            .client
            .get()
            .await
            .map_err(|e| InternalServerError::from(eyre::eyre!(e)))?;
        let redis_key = format!("capsule:temp:{}", key);
        let _: () = con
            .del(&redis_key)
            .await
            .map_err(|e| InternalServerError::from(eyre::eyre!(e)))?;
        Ok(())
    }
}

/// (count, reset_at_unix_secs)
type RateLimitEntry = (i64, u64);

pub struct InMemorySessionStorage {
    sessions: Arc<RwLock<HashMap<String, String>>>,
    user_sessions: Arc<RwLock<HashMap<String, Vec<String>>>>,
    mfa_attempts: Arc<RwLock<HashMap<String, i32>>>,
    temp_data: Arc<RwLock<HashMap<String, String>>>,
    rate_limits: Arc<RwLock<HashMap<String, RateLimitEntry>>>,
}

impl Default for InMemorySessionStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemorySessionStorage {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            user_sessions: Arc::new(RwLock::new(HashMap::new())),
            mfa_attempts: Arc::new(RwLock::new(HashMap::new())),
            temp_data: Arc::new(RwLock::new(HashMap::new())),
            rate_limits: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl SessionStorage for InMemorySessionStorage {
    async fn save_session(
        &self,
        sid: &str,
        session_data: String,
        _ttl: Duration,
    ) -> Result<(), InternalServerError> {
        self.sessions
            .write()
            .await
            .insert(sid.to_string(), session_data);
        Ok(())
    }

    async fn get_session(&self, sid: &str) -> Result<Option<String>, InternalServerError> {
        Ok(self.sessions.read().await.get(sid).cloned())
    }

    async fn delete_session(&self, sid: &str) -> Result<(), InternalServerError> {
        self.sessions.write().await.remove(sid);
        Ok(())
    }

    async fn add_user_session(
        &self,
        user_id: &str,
        sid: &str,
        _ttl: Duration,
    ) -> Result<(), InternalServerError> {
        let mut user_sessions = self.user_sessions.write().await;
        user_sessions
            .entry(user_id.to_string())
            .or_default()
            .push(sid.to_string());
        Ok(())
    }

    async fn get_user_sessions(&self, user_id: &str) -> Result<Vec<String>, InternalServerError> {
        Ok(self
            .user_sessions
            .read()
            .await
            .get(user_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn delete_user_sessions_key(&self, user_id: &str) -> Result<(), InternalServerError> {
        self.user_sessions.write().await.remove(user_id);
        Ok(())
    }

    async fn increment_mfa_attempt(&self, mfa_token_jti: &str) -> Result<i32, InternalServerError> {
        let mut attempts = self.mfa_attempts.write().await;
        let count = attempts.entry(mfa_token_jti.to_string()).or_insert(0);
        *count += 1;
        Ok(*count)
    }

    async fn get_mfa_attempts(&self, mfa_token_jti: &str) -> Result<i32, InternalServerError> {
        Ok(*self
            .mfa_attempts
            .read()
            .await
            .get(mfa_token_jti)
            .unwrap_or(&0))
    }

    async fn clear_mfa_attempts(&self, mfa_token_jti: &str) -> Result<(), InternalServerError> {
        self.mfa_attempts.write().await.remove(mfa_token_jti);
        Ok(())
    }

    async fn increment_rate_limit(
        &self,
        key: &str,
        window_secs: u64,
    ) -> Result<RateLimitResult, InternalServerError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut map = self.rate_limits.write().await;
        let entry = map.entry(key.to_string()).or_insert((0, now + window_secs));
        if now >= entry.1 {
            // Window expired — reset
            *entry = (1, now + window_secs);
        } else {
            entry.0 += 1;
        }
        let (count, reset_at) = *entry;
        Ok(RateLimitResult {
            count,
            window_ttl_secs: reset_at.saturating_sub(now),
        })
    }

    async fn save_temp_data(
        &self,
        key: &str,
        value: String,
        _ttl: Duration,
    ) -> Result<(), InternalServerError> {
        self.temp_data.write().await.insert(key.to_string(), value);
        Ok(())
    }

    async fn get_temp_data(&self, key: &str) -> Result<Option<String>, InternalServerError> {
        Ok(self.temp_data.read().await.get(key).cloned())
    }

    async fn delete_temp_data(&self, key: &str) -> Result<(), InternalServerError> {
        self.temp_data.write().await.remove(key);
        Ok(())
    }
}
