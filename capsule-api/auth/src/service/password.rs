use chrono::{Duration, Utc};
use model::errors::InternalServerError;
use sea_orm::DatabaseConnection;
use service::user as UserService;
use std::time::Instant;

pub struct PasswordService {
    min_operation_time: std::time::Duration,
}

impl PasswordService {
    pub fn new(min_operation_time_ms: u64) -> Self {
        Self {
            min_operation_time: std::time::Duration::from_millis(min_operation_time_ms),
        }
    }

    pub async fn request_reset(
        &self,
        conn: &DatabaseConnection,
        email: &str,
    ) -> Result<(), InternalServerError> {
        let start = Instant::now();

        // Find user by email
        let user_result = UserService::Query::find_user_by_email(conn, email).await;

        let result = match user_result {
            Ok(Some(user)) => {
                // Generate token
                let token = nanoid::nanoid!();
                let expires_at = Utc::now() + Duration::hours(1);

                // Update user with token
                UserService::Mutation::update_password_reset_token(
                    conn, &user.id, &token, expires_at,
                )
                .await
                .map(|_| ())
                .map_err(InternalServerError::from)
            }
            Ok(None) => {
                // User not found - pretend success
                Ok(())
            }
            Err(e) => Err(InternalServerError::from(e)),
        };

        // Ensure minimum time elapsed
        let elapsed = start.elapsed();
        if elapsed < self.min_operation_time {
            tokio::time::sleep(self.min_operation_time - elapsed).await;
        }

        result
    }
}
