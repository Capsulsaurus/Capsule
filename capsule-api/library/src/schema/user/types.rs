use async_graphql::*;
use chrono::{DateTime, Utc};
use entity::user;
use serde::{Deserialize, Serialize};

#[derive(SimpleObject, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub username: String,
    pub name: String,
    pub email: String,
    pub account_verified: bool,
    pub needs_onboarding: bool,
    pub is_admin: bool,
    pub created_at: DateTime<Utc>,
    pub modified_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    /// How the account was created (e.g. 'invitation'); null for seeded/legacy/unknown
    pub registered_via: Option<String>,
}

impl From<user::Model> for User {
    fn from(user: user::Model) -> Self {
        Self {
            id: user.id,
            username: user.username,
            name: user.name,
            email: user.email,
            account_verified: user.account_verified,
            needs_onboarding: user.needs_onboarding,
            is_admin: user.is_admin,
            created_at: user.created_at,
            modified_at: user.modified_at,
            deleted_at: user.deleted_at,
            registered_via: user.registered_via,
        }
    }
}

/// User statistics
#[derive(SimpleObject, Clone, Serialize, Deserialize)]
pub struct UserStatistics {
    pub total_assets: i64,
    pub total_photos: i64,
    pub total_videos: i64,
    pub total_video_minutes: i64,
    pub total_albums: i64,
    pub total_storage_used: i64,
}
