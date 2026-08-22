use std::sync::Arc;
use std::time::Duration;

use bb8_redis::RedisConnectionManager;
use bb8_redis::bb8::Pool;
use model::errors::InternalServerError;
use serde::{Deserialize, Serialize};

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
    /// The advisory device-cohort hash asserted at session creation (slice `S-C13`), stored
    /// verbatim so the listing surface can group this session with others from the same
    /// physical device. **Advisory-only:** it is never read by any authorization path — the
    /// JWT [`crate::claims::Claims`] that drive every authz decision carry no cohort field, so
    /// this record is legibility metadata, never a capability input.
    #[serde(default)]
    pub cohort_hash: Option<String>,
    /// The device this session was opened from — the security-bearing `device_id` (a UUID)
    /// the client asserted at session creation, or `None` when it asserted none (slice
    /// `S-N3`).
    ///
    /// It is a **separate identifier space** from [`Self::cohort_hash`]: the cohort groups a
    /// physical device's *re-enrollments*, while `device_id` names one directory device.
    /// Both are carried purely so the session listing can emit the support bundle's
    /// `(device_id, session_id)` pairs — no authorization path reads either (the JWT
    /// [`crate::claims::Claims`] carry neither field).
    #[serde(default)]
    pub device_id: Option<String>,
}

/// The client-asserted provenance a login ceremony attaches to the session it opens.
///
/// Every ceremony — password login, registration, TOTP second factor, passkey, and token
/// refresh — carries the same pair, so a session opened by any of them groups in the devices
/// view instead of showing up as an unknown device (slice `S-N3`). Both fields are
/// **legibility metadata only**: they ride the session record and the listing surface, never
/// the JWT claims, so no authorization or capability decision can read them.
///
/// Values reach the store only through [`SessionContext::normalized`], so an absent, empty,
/// over-long, or malformed assertion is indistinguishable from no assertion at all.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionContext {
    /// The advisory device-cohort hash (slice `S-C13`), normalized by
    /// [`service::cohort::normalize`].
    pub cohort_hash: Option<String>,
    /// The asserted directory `device_id`, normalized by [`normalize_device_id`].
    pub device_id: Option<String>,
}

impl SessionContext {
    /// Build a context from the raw values a request body carried.
    pub fn new(cohort_hash: Option<String>, device_id: Option<String>) -> Self {
        Self {
            cohort_hash,
            device_id,
        }
    }

    /// The context with both fields normalized — the only form that reaches the session
    /// store. Normalization never fails: an unusable value becomes `None`.
    #[must_use]
    pub fn normalized(self) -> Self {
        Self {
            cohort_hash: service::cohort::normalize(self.cohort_hash),
            device_id: normalize_device_id(self.device_id),
        }
    }
}

/// Normalize a client-asserted `device_id` into the canonical form that may be stored, or
/// `None`.
///
/// Unlike the opaque cohort hash, `device_id` has a defined shape — the directory's UUID — so
/// normalization parses it and re-renders the canonical lowercase hyphenated form, making the
/// support bundle's ids comparable to directory entries regardless of how the client spelled
/// them. Anything that is not a UUID, and the nil UUID (which names no device), is dropped:
/// the session then simply carries no device id, exactly as if none had been asserted.
///
/// This is a *shape* check, not an authenticity check. The value is client-asserted and the
/// server does not verify that the caller controls the named device — which is why nothing
/// here may gate authorization. It is surfaced only so a support report can say which
/// directory device a session claims to belong to.
pub fn normalize_device_id(raw: Option<String>) -> Option<String> {
    let raw = raw?;
    let parsed = uuid::Uuid::parse_str(raw.trim()).ok()?;
    if parsed.is_nil() {
        return None;
    }
    Some(parsed.to_string())
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

    /// Open a session for `user_id`, recording the ceremony's client-asserted
    /// [`SessionContext`] on the record so the listing surface can emit it.
    ///
    /// `context` is normalized here rather than at the call sites, so every ceremony stores
    /// the same shapes and no caller can smuggle an unnormalized value past the boundary.
    pub async fn create_session(
        &self,
        user_id: String,
        user_agent: Option<String>,
        ip_address: Option<String>,
        context: SessionContext,
    ) -> Result<String, InternalServerError> {
        let sid = nanoid::nanoid!();
        let now = jiff::Timestamp::now().as_second();
        let SessionContext {
            cohort_hash,
            device_id,
        } = context.normalized();
        let session = Session {
            user_id: user_id.clone(),
            created_at: now,
            user_agent,
            ip_address,
            last_active_at: now,
            cohort_hash,
            device_id,
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

#[cfg(test)]
mod tests {
    use super::*;

    const UUID_V4: &str = "1f2e3d4c-5b6a-4978-8899-aabbccddeeff";

    #[test]
    fn device_id_normalizes_to_canonical_form() {
        assert_eq!(
            normalize_device_id(Some(UUID_V4.to_uppercase())),
            Some(UUID_V4.to_string()),
            "case is normalized so bundle ids compare against directory entries"
        );
        assert_eq!(
            normalize_device_id(Some(format!("  {UUID_V4}  "))),
            Some(UUID_V4.to_string())
        );
        // The braced/urn spellings a platform SDK may emit collapse to the same id.
        assert_eq!(
            normalize_device_id(Some(format!("urn:uuid:{UUID_V4}"))),
            Some(UUID_V4.to_string())
        );
    }

    #[test]
    fn device_id_drops_absent_malformed_and_nil() {
        assert_eq!(normalize_device_id(None), None);
        assert_eq!(normalize_device_id(Some(String::new())), None);
        assert_eq!(normalize_device_id(Some("   ".to_string())), None);
        assert_eq!(normalize_device_id(Some("not-a-uuid".to_string())), None);
        assert_eq!(
            normalize_device_id(Some("00000000-0000-0000-0000-000000000000".to_string())),
            None,
            "the nil UUID names no device and is treated as absent"
        );
    }

    #[test]
    fn session_context_normalizes_both_halves_independently() {
        let ctx = SessionContext::new(
            Some("  deadbeef  ".to_string()),
            Some("garbage".to_string()),
        )
        .normalized();
        assert_eq!(ctx.cohort_hash, Some("deadbeef".to_string()));
        assert_eq!(ctx.device_id, None, "a malformed device id is dropped");

        let ctx = SessionContext::new(None, Some(UUID_V4.to_string())).normalized();
        assert_eq!(ctx.cohort_hash, None);
        assert_eq!(ctx.device_id, Some(UUID_V4.to_string()));

        assert_eq!(
            SessionContext::default().normalized(),
            SessionContext::default()
        );
    }

    #[tokio::test]
    async fn created_session_carries_the_asserted_device_id_and_cohort() {
        let sm = SessionManager::new_with_storage(
            Box::new(InMemorySessionStorage::new()),
            Duration::from_secs(3600),
        );
        let sid = sm
            .create_session(
                "user-1".to_string(),
                None,
                None,
                SessionContext::new(Some("cohort-a".to_string()), Some(UUID_V4.to_uppercase())),
            )
            .await
            .expect("session created");

        let session = sm
            .get_session(&sid)
            .await
            .expect("session read")
            .expect("session exists");
        assert_eq!(session.cohort_hash, Some("cohort-a".to_string()));
        assert_eq!(session.device_id, Some(UUID_V4.to_string()));
        assert_ne!(sid, UUID_V4, "the session id is its own identifier");
    }
}
