use super::{CreateUserArgs, UpdateUserArgs};
use ::entity::{user, user::Entity as User};
use sea_orm::*;

pub struct Mutation;

impl Mutation {
    /// Creates a new user
    pub async fn create_user(db: &DbConn, user: CreateUserArgs) -> Result<user::Model, DbErr> {
        let CreateUserArgs {
            username,
            name,
            email,
            password_hash,
            registered_via,
        } = user;

        user::ActiveModel {
            username: Set(username),
            name: Set(name),
            email: Set(email),
            password_hash: Set(password_hash),
            registered_via: Set(registered_via),
            ..Default::default()
        }
        .insert(db)
        .await
    }

    /// Updates an existing user
    pub async fn update_user(
        db: &DbConn,
        id: &str,
        user: UpdateUserArgs,
    ) -> Result<user::Model, DbErr> {
        let UpdateUserArgs {
            username,
            name,
            email,
            password_hash,
        } = user;

        let user: user::ActiveModel = User::find_by_id(id)
            .one(db)
            .await?
            .ok_or(DbErr::RecordNotFound("User not found".to_string()))?
            .into();

        let mut user = user;

        if let Some(username) = username {
            user.username = Set(username);
        }
        if let Some(name) = name {
            user.name = Set(name);
        }
        if let Some(email) = email {
            user.email = Set(email);
        }
        if let Some(password_hash) = password_hash {
            user.password_hash = Set(password_hash);
        }

        user.update(db).await
    }

    /// Tracks a successful login for a user
    pub async fn track_login_success(db: &DbConn, id: &str) -> Result<user::Model, DbErr> {
        let user_model = User::find_by_id(id)
            .one(db)
            .await?
            .ok_or(DbErr::RecordNotFound("User not found".to_string()))?;

        let mut user: user::ActiveModel = user_model.into();
        user.last_login_at = Set(Some(chrono::Utc::now()));
        user.failed_login_attempts = Set(0);

        user.update(db).await
    }

    /// Tracks a failed login attempt for a user
    pub async fn track_login_failure(db: &DbConn, id: &str) -> Result<user::Model, DbErr> {
        let user_model = User::find_by_id(id)
            .one(db)
            .await?
            .ok_or(DbErr::RecordNotFound("User not found".to_string()))?;

        // Increment failed attempts
        let current_attempts = user_model.failed_login_attempts;
        let mut user: user::ActiveModel = user_model.into();
        user.failed_login_attempts = Set(current_attempts + 1);

        user.update(db).await
    }

    /// Updates a user's password reset token
    pub async fn update_password_reset_token(
        db: &DbConn,
        id: &str,
        token: &str,
        expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<user::Model, DbErr> {
        let user_model = User::find_by_id(id)
            .one(db)
            .await?
            .ok_or(DbErr::RecordNotFound("User not found".to_string()))?;

        let mut user: user::ActiveModel = user_model.into();
        user.password_reset_token = Set(Some(token.to_string()));
        user.password_reset_expires_at = Set(Some(expires_at));

        user.update(db).await
    }

    /// Confirms a password reset for a user
    pub async fn confirm_password_reset(
        db: &DbConn,
        id: &str,
        new_password_hash: &str,
    ) -> Result<user::Model, DbErr> {
        let user_model = User::find_by_id(id)
            .one(db)
            .await?
            .ok_or(DbErr::RecordNotFound("User not found".to_string()))?;

        let mut user: user::ActiveModel = user_model.into();
        user.password_hash = Set(new_password_hash.to_string());
        user.password_reset_token = Set(None);
        user.password_reset_expires_at = Set(None);

        user.update(db).await
    }

    /// Set TOTP fields. If verified is None, it will not be updated.
    ///
    /// Returns true if the fields were set successfully
    pub async fn set_totp(
        db: &DbConn,
        id: &str,
        secret: Option<String>,
        verified: Option<bool>,
    ) -> Result<bool, DbErr> {
        let user_model = match User::find_by_id(id).one(db).await? {
            Some(m) => m,
            None => return Ok(false),
        };

        let mut user: user::ActiveModel = user_model.into();
        user.totp_secret = Set(secret);
        if let Some(verified) = verified {
            user.totp_verified = Set(Some(verified));
        }

        user.update(db).await?;

        Ok(true)
    }
    /// Set TOTP fields
    /// Returns true if the secret was set successfully
    pub async fn set_totp_secret(
        db: &DbConn,
        id: &str,
        secret: Option<String>,
    ) -> Result<bool, DbErr> {
        Self::set_totp(db, id, secret, None).await
    }

    /// Set TOTP secret as verified
    /// Returns true if the secret was set successfully
    pub async fn set_totp_verified(db: &DbConn, id: &str, verified: bool) -> Result<bool, DbErr> {
        let user_model = match User::find_by_id(id).one(db).await? {
            Some(m) => m,
            None => return Ok(false),
        };

        let mut user: user::ActiveModel = user_model.into();
        user.totp_verified = Set(Some(verified));

        user.update(db).await?;

        Ok(true)
    }

    /// Clears a user's TOTP secret and verification status
    pub async fn clear_totp_secret(db: &DbConn, id: &str) -> Result<bool, DbErr> {
        Self::set_totp(db, id, None, Some(false)).await
    }
}
