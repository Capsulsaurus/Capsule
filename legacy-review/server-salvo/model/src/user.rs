use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub struct CreateUser {
    pub username: String,
    pub name: String,
    pub email: String,
    pub password_hash: String,
}

pub struct UpdateUser {
    pub username: Option<String>,
    pub name: Option<String>,
    pub email: Option<String>,
    pub password_hash: Option<String>,
}

/// Represents a user within the system.
///
/// Omits sensitive information (e.g. credentials)
/// This is a shared model used across the application layers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    /// User ID
    pub id: String,

    /// Username
    pub username: String,

    /// User's full name or display name
    pub name: String,

    /// Primary contact email.
    /// Note: uniqueness is enforced by a case-insensitive database index on LOWER(email).
    pub email: String,

    /// URL to the user's profile image/avatar.
    pub profile_image_url: Option<String>,

    /// Indicates if the user still needs to complete the initial setup process.
    pub needs_onboarding: bool,

    /// Flag indicating if the user has administrative privileges.
    pub is_admin: bool,

    /// Timestamp of when the user record was originally created.
    pub created_at: DateTime<Utc>,

    /// Timestamp of the last time the user record was updated.
    pub modified_at: DateTime<Utc>,

    /// Timestamp of when the user was soft-deleted. If None, the user is active.
    pub deleted_at: Option<DateTime<Utc>>,

    /// How the account was created (e.g. 'invitation'); None for seeded/legacy/unknown.
    pub registered_via: Option<String>,
}

impl From<entity::user::Model> for User {
    fn from(model: entity::user::Model) -> Self {
        let profile_image_url = model.profile_image_url();
        let entity::user::Model {
            id,
            username,
            name,
            email,
            profile_image_url: _,
            account_verified: _,
            needs_onboarding,
            password_hash: _,
            totp_secret: _,
            totp_verified: _,
            password_reset_token: _,
            password_reset_expires_at: _,
            last_login_at: _,
            failed_login_attempts: _,
            is_admin,
            created_at,
            modified_at,
            deleted_at,
            registered_via,
        } = model;

        User {
            id,
            username,
            name,
            email,
            profile_image_url,
            needs_onboarding,
            is_admin,
            created_at,
            modified_at,
            deleted_at,
            registered_via,
        }
    }
}
