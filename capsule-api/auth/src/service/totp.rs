use sea_orm::DatabaseConnection;

use crate::{
    errors::{TotpEnrollError, TotpVerificationError},
    utils::totp::{generate_secret, get_totp_generator, verify_token},
};

/// TOTP service for generating and verifying Time-based One-Time Passwords
pub struct TotpService {
    conn: DatabaseConnection,
    issuer: String,
}

impl TotpService {
    /// Create a new TOTP service
    pub fn new(conn: impl Into<DatabaseConnection>, issuer: impl Into<String>) -> Self {
        Self {
            conn: conn.into(),
            issuer: issuer.into(),
        }
    }

    /// Enroll user in TOTP
    ///
    /// Returns the provisioning URI for the user
    /// Throws error if user is already enrolled
    pub async fn enroll(&self, user_id: &str) -> Result<String, TotpEnrollError> {
        // Check if TOTP is already enabled
        match service::user::Query::get_totp_secret_by_id(&self.conn, user_id).await {
            Ok(Some(Some(_))) => return Err(TotpEnrollError::AlreadyEnabled),
            Ok(Some(None)) => {} // Good. User exists but not enrolled
            Ok(None) => return Err(TotpEnrollError::UserNotFound),
            Err(e) => return Err(TotpEnrollError::Db(e)),
        }
        // let user = match service::user::Query::find_user_by_id(&self.conn, user_id).await {
        //     Ok(Some(u)) => u,
        //     Ok(None) => {
        //         return Err(TotpEnrollError::UserNotFound);
        //     }
        //     Err(e) => return Err(TotpEnrollError::Db(e)),
        // };

        // if user.totp_secret.is_some() {
        //     return Err(TotpEnrollError::AlreadyEnabled);
        // }

        // Generate TOTP secret
        let secret = generate_secret();
        // Fetch user's email
        let email = match service::user::Query::get_email_by_id(&self.conn, user_id).await {
            Ok(Some(email)) => email,
            Ok(None) => return Err(TotpEnrollError::UserNotFound),
            Err(e) => return Err(TotpEnrollError::Db(e)),
        };
        let provisioning_uri = match self.get_provisioning_uri(&secret, &email) {
            Ok(uri) => uri,
            Err(e) => {
                return Err(e.into());
            }
        };

        // Store secret in database (but not set as verified yet)
        service::user::Mutation::set_totp_secret(&self.conn, user_id, Some(secret)).await?;

        Ok(provisioning_uri)
    }

    /// Get TOTP secret by user ID
    pub async fn get_secret_for_user(
        &self,
        user_id: &str,
    ) -> Result<String, TotpVerificationError> {
        match service::user::Query::get_totp_secret_by_id(&self.conn, user_id).await {
            Ok(Some(Some(secret))) => Ok(secret),
            Ok(Some(None)) => Err(TotpVerificationError::NotEnabled),
            Ok(None) => Err(TotpVerificationError::UserNotFound),
            Err(e) => Err(TotpVerificationError::Unexpected(e.into())),
        }
    }

    /// Verify TOTP token for user
    pub async fn verify_token(
        &self,
        user_id: &str,
        token: &str,
    ) -> Result<bool, TotpVerificationError> {
        // Get TOTP secret for user
        let secret = match self.get_secret_for_user(user_id).await {
            Ok(secret) => secret,
            Err(e) => return Err(e),
        };

        verify_token(&secret, token).map_err(|e| e.into())
    }

    /// Verify enrollment for user
    pub async fn verify_enrollment(
        &self,
        user_id: &str,
        token: &str,
    ) -> Result<bool, TotpVerificationError> {
        // Verify TOTP token
        let verified = self.verify_token(user_id, token).await?;
        // If verified, update user's TOTP secret as verified
        if verified {
            service::user::Mutation::set_totp_verified(&self.conn, user_id, true)
                .await
                .map_err(|e| TotpVerificationError::Unexpected(e.into()))?;
        }

        Ok(verified)
    }

    /// Clear TOTP secret for user
    pub async fn clear_enrollment(
        &self,
        user_id: &str,
        token: &str,
    ) -> Result<(), TotpVerificationError> {
        // Verify token is valid
        match self.verify_token(user_id, token).await {
            Ok(true) => {
                // Clear TOTP fields
                service::user::Mutation::clear_totp_secret(&self.conn, user_id)
                    .await
                    .map_err(|e| TotpVerificationError::Unexpected(e.into()))?;
            }
            Ok(false) => return Err(TotpVerificationError::InvalidCode),
            Err(e) => return Err(e),
        }

        Ok(())
    }

    /// Clear TOTP secret for user
    #[allow(dead_code)]
    async fn clear_secret(&self, user_id: &str) -> eyre::Result<()> {
        service::user::Mutation::clear_totp_secret(&self.conn, user_id).await?;
        Ok(())
    }

    /// Get the provisioning URI for authenticator apps
    ///
    /// This URI can be used by clients to generate QR codes
    /// Format: otpauth://totp/{issuer}:{email}?secret={secret}&issuer={issuer}
    fn get_provisioning_uri(&self, secret: &str, email: &str) -> eyre::Result<String> {
        // Validate secret format
        _ = get_totp_generator(secret)?;

        // Generate the full provisioning URI with issuer and account
        let account = format!("{}:{}", self.issuer, email);
        Ok(format!(
            "otpauth://totp/{}?secret={}&issuer={}",
            account, secret, self.issuer
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get_test_db_connection() -> DatabaseConnection {
        DatabaseConnection::Disconnected
    }

    #[test]
    fn test_provisioning_uri_format() {
        let service = TotpService::new(get_test_db_connection(), "my-capsule-issuer");
        let secret = generate_secret();
        let uri = service.get_provisioning_uri(&secret, "test@example.com");

        assert!(uri.is_ok());
        let uri = uri.unwrap();
        assert!(uri.starts_with("otpauth://totp/"));
        assert!(uri.contains(&secret));
        assert!(uri.contains("my-capsule-issuer"));
    }
}
