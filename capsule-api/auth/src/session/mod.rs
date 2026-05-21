use bb8_redis::RedisConnectionManager;
use bb8_redis::bb8::Pool;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

use model::errors::InternalServerError;

pub mod storage;
pub use self::storage::{
    InMemorySessionStorage, RateLimitResult, RedisSessionStorage, SessionStorage,
};

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Session {
    pub user_id: String,
    pub created_at: i64,
    pub user_agent: Option<String>,
    pub ip_address: Option<String>,
    #[serde(default)]
    pub last_active_at: i64,
}

#[derive(Clone)]
pub struct SessionManager {
    storage: Arc<Box<dyn SessionStorage>>,
    ttl: Duration,
}

impl SessionManager {
    pub async fn new(redis_url: String, ttl: Duration) -> Result<Self, InternalServerError> {
        let manager = RedisConnectionManager::new(redis_url).map_err(InternalServerError::from)?;
        let pool = Pool::builder()
            .build(manager)
            .await
            .map_err(InternalServerError::from)?;

        Ok(Self {
            storage: Arc::new(Box::new(RedisSessionStorage::new(pool))),
            ttl,
        })
    }

    // For testing
    pub fn new_with_storage(storage: Box<dyn SessionStorage>, ttl: Duration) -> Self {
        Self {
            storage: Arc::new(storage),
            ttl,
        }
    }

    pub async fn create_session(
        &self,
        user_id: String,
        user_agent: Option<String>,
        ip_address: Option<String>,
    ) -> Result<String, InternalServerError> {
        let sid = nanoid::nanoid!();
        let now = chrono::Utc::now().timestamp();
        let session = Session {
            user_id: user_id.clone(),
            created_at: now,
            user_agent,
            ip_address,
            last_active_at: now,
        };

        let session_data = serde_json::to_string(&session).map_err(InternalServerError::from)?;

        self.storage
            .save_session(&sid, session_data, self.ttl)
            .await?;
        self.storage
            .add_user_session(&user_id, &sid, self.ttl)
            .await?;

        Ok(sid)
    }

    pub async fn get_session(&self, sid: &str) -> Result<Option<Session>, InternalServerError> {
        let session_data = self.storage.get_session(sid).await?;

        match session_data {
            Some(data) => {
                let session: Session =
                    serde_json::from_str(&data).map_err(InternalServerError::from)?;
                Ok(Some(session))
            }
            None => Ok(None),
        }
    }

    pub async fn revoke_session(&self, sid: &str) -> Result<(), InternalServerError> {
        self.storage.delete_session(sid).await
    }

    /// Returns all active sessions for a user as (session_id, Session) pairs.
    /// Sessions that have expired (not found in storage) are silently skipped.
    pub async fn get_sessions_for_user(
        &self,
        user_id: &str,
    ) -> Result<Vec<(String, Session)>, InternalServerError> {
        let session_ids = self.storage.get_user_sessions(user_id).await?;
        let mut sessions = Vec::new();
        for sid in session_ids {
            if let Some(session) = self.get_session(&sid).await? {
                sessions.push((sid, session));
            }
        }
        Ok(sessions)
    }

    pub async fn revoke_all_for_user(&self, user_id: &str) -> Result<(), InternalServerError> {
        let sessions = self.storage.get_user_sessions(user_id).await?;

        for sid in sessions {
            self.storage.delete_session(&sid).await?;
        }

        self.storage.delete_user_sessions_key(user_id).await?;

        Ok(())
    }

    // MFA attempt tracking methods
    pub async fn increment_mfa_attempt(
        &self,
        mfa_token_jti: &str,
    ) -> Result<i32, InternalServerError> {
        self.storage.increment_mfa_attempt(mfa_token_jti).await
    }

    pub async fn get_mfa_attempts(&self, mfa_token_jti: &str) -> Result<i32, InternalServerError> {
        self.storage.get_mfa_attempts(mfa_token_jti).await
    }

    pub async fn clear_mfa_attempts(&self, mfa_token_jti: &str) -> Result<(), InternalServerError> {
        self.storage.clear_mfa_attempts(mfa_token_jti).await
    }

    // Rate limiting
    pub async fn check_rate_limit(
        &self,
        key: &str,
        _max_requests: i64,
        window_secs: u64,
    ) -> Result<RateLimitResult, InternalServerError> {
        let result = self.storage.increment_rate_limit(key, window_secs).await?;
        Ok(result)
    }

    // Ephemeral data (Passkeys, etc)
    pub async fn save_temp_data<T: Serialize>(
        &self,
        key: &str,
        value: &T,
        ttl: Duration,
    ) -> Result<(), InternalServerError> {
        let json = serde_json::to_string(value).map_err(InternalServerError::from)?;
        self.storage.save_temp_data(key, json, ttl).await
    }

    pub async fn get_temp_data<T: for<'de> Deserialize<'de>>(
        &self,
        key: &str,
    ) -> Result<Option<T>, InternalServerError> {
        let data = self.storage.get_temp_data(key).await?;
        match data {
            Some(json) => {
                let value = serde_json::from_str(&json).map_err(InternalServerError::from)?;
                Ok(Some(value))
            }
            None => Ok(None),
        }
    }

    pub async fn delete_temp_data(&self, key: &str) -> Result<(), InternalServerError> {
        self.storage.delete_temp_data(key).await
    }
}
