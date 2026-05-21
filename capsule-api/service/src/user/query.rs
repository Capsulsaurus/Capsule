use ::entity::{user, user::Entity as User};
use sea_orm::*;

pub struct Query;

impl Query {
    /// Returns user by ID
    pub async fn find_user_by_id(
        db: &DbConn,
        id: &str,
    ) -> Result<Option<model::user::User>, DbErr> {
        let user = User::find_by_id(id).one(db).await?;
        Ok(user.map(Into::into))
    }

    /// Returns user by email
    pub async fn find_user_by_email(
        db: &DbConn,
        email: &str,
    ) -> Result<Option<model::user::User>, DbErr> {
        let user = User::find()
            .filter(user::Column::Email.eq(email))
            .one(db)
            .await?;
        Ok(user.map(Into::into))
    }

    /// Returns user by username
    pub async fn find_user_by_username(
        db: &DbConn,
        username: &str,
    ) -> Result<Option<model::user::User>, DbErr> {
        let user = User::find()
            .filter(user::Column::Username.eq(username))
            .one(db)
            .await?;
        Ok(user.map(Into::into))
    }

    /// Returns email by ID
    /// Returns None if user not found
    pub async fn get_email_by_id(db: &DbConn, id: &str) -> Result<Option<String>, DbErr> {
        let user = User::find_by_id(id).one(db).await?;
        Ok(user.map(|u| u.email))
    }

    /// Returns account verification status of user by ID
    /// Returns None if user not found
    pub async fn get_account_verification_status_by_id(
        db: &DbConn,
        id: &str,
    ) -> Result<Option<bool>, DbErr> {
        let user = User::find_by_id(id).one(db).await?;
        Ok(user.map(|u| u.account_verified))
    }

    /// Returns hashed password by ID
    #[cfg(feature = "auth")]
    pub async fn get_password_hash_by_id(db: &DbConn, id: &str) -> Result<Option<String>, DbErr> {
        let user = User::find_by_id(id)
            .one(db)
            .await?
            .ok_or(DbErr::RecordNotFound("User not found".to_string()))?;
        Ok(Some(user.password_hash))
    }

    /// Returns TOTP secret by ID
    /// Returns None if user not found
    /// Returns Some(None) if user has no TOTP secret
    #[cfg(feature = "auth")]
    pub async fn get_totp_secret_by_id(
        db: &DbConn,
        id: &str,
    ) -> Result<Option<Option<String>>, DbErr> {
        let user = User::find_by_id(id).one(db).await?;
        Ok(user.map(|u| u.totp_secret))
    }

    // /// If ok, returns (user models, num pages).
    // pub async fn find_users_in_page(
    //     db: &DbConn,
    //     page: u64,
    //     users_per_page: u64,
    // ) -> Result<(Vec<user::Model>, u64), DbErr> {
    //     // Setup paginator
    //     let paginator = User::find()
    //         .order_by_asc(user::Column::Id)
    //         .paginate(db, users_per_page);
    //     let num_pages = paginator.num_pages().await?;

    //     // Fetch paginated users
    //     paginator.fetch_page(page - 1).await.map(|p| (p, num_pages))
    // }
    //     paginator.fetch_page(page - 1).await.map(|p| (p, num_pages))
    // }

    /// Returns user by password reset token
    #[cfg(feature = "auth")]
    pub async fn find_user_by_reset_token(
        db: &DbConn,
        token: &str,
    ) -> Result<Option<user::Model>, DbErr> {
        User::find()
            .filter(user::Column::PasswordResetToken.eq(token))
            .one(db)
            .await
    }

    /// Returns failed login attempts count for user
    #[cfg(feature = "auth")]
    pub async fn get_failed_login_attempts(db: &DbConn, id: &str) -> Result<Option<i32>, DbErr> {
        let user = User::find_by_id(id).one(db).await?;
        Ok(user.map(|u| u.failed_login_attempts))
    }

    /// Returns the password reset token for a user by email (used in tests / admin flows)
    #[cfg(feature = "auth")]
    pub async fn get_password_reset_token_by_email(
        db: &DbConn,
        email: &str,
    ) -> Result<Option<Option<String>>, DbErr> {
        let user = User::find()
            .filter(user::Column::Email.eq(email))
            .one(db)
            .await?;
        Ok(user.map(|u| u.password_reset_token))
    }
}
