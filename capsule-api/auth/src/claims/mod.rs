use std::sync::OnceLock;
use std::time::Duration;

use chrono::Utc;
use environment::constants::{ACCESS_TOKEN_EXPIRY, REFRESH_TOKEN_EXPIRY};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, TokenData, Validation, decode, encode};
use serde::{Deserialize, Serialize};

use crate::constants::ISSUER;
use crate::errors::ClaimValidationError;
use crate::roles::UserRole;

pub mod issuer;
mod scope;
pub use scope::*;

static VALIDATION: OnceLock<Validation> = OnceLock::new();

fn get_validation() -> &'static Validation {
    VALIDATION.get_or_init(|| {
        let mut validation = Validation::new(Algorithm::EdDSA);
        validation.set_issuer(&[ISSUER]);
        validation
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claims {
    // Standard JWT claims
    /// Subject - the user ID
    pub sub: String,
    /// Expiration time (as UTC timestamp)
    pub exp: u64,
    /// Issued at (as UTC timestamp)
    pub iat: u64,
    /// JWT ID - Unique identifier for this token
    pub jti: String,
    /// Issuer (e.g. "api.capsule.com")
    pub iss: String,

    // Custom claims
    /// Session ID (optional, used for refresh tokens linked to sessions)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sid: Option<String>,
    /// User type
    pub role: UserRole,
    /// Permissions/scopes granted to this token
    pub scopes: Vec<Scope>,
}

impl Claims {
    /// Creates a standard Claims
    pub fn new<D: Into<Duration>>(
        user_id: String,
        user_role: UserRole,
        expiry_duration: D,
        scopes: Vec<Scope>,
        sid: Option<String>,
    ) -> Self {
        let expiry_duration = expiry_duration.into();
        let iat = Utc::now().timestamp() as u64;
        let exp: u64 = iat + expiry_duration.as_secs();

        Self {
            sub: user_id,
            exp,
            iat,
            jti: uuid::Uuid::new_v4().to_string(),
            iss: ISSUER.to_string(),

            sid,
            role: user_role,
            scopes,
        }
    }

    /// Returns new access token claims
    pub fn new_access_token(user_id: String, sid: Option<String>) -> Self {
        Self::new(
            user_id,
            UserRole::User,
            Duration::from_secs(ACCESS_TOKEN_EXPIRY),
            vec![Scope::AccessToken],
            sid,
        )
    }

    /// Returns new refresh token claims
    pub fn new_refresh_token(user_id: String, sid: String) -> Self {
        Self::new(
            user_id,
            UserRole::User,
            Duration::from_secs(REFRESH_TOKEN_EXPIRY),
            vec![Scope::RefreshToken],
            Some(sid),
        )
    }

    /// Returns new MFA token claims (for TOTP verification)
    /// These tokens have a short TTL (3 minutes) and no authorization scopes
    pub fn new_mfa_token(user_id: String) -> Self {
        Self::new(
            user_id,
            UserRole::User,
            Duration::from_secs(180), // 3 minutes
            vec![Scope::MfaToken],    // No scopes - cannot be used for authorization
            None,                     // No session ID
        )
    }

    /// Returns true if the token has not expired
    pub fn has_expired(&self) -> bool {
        self.exp <= Utc::now().timestamp() as u64
    }

    /// Returns true if the token has valid issuer
    pub fn has_valid_issuer(&self) -> bool {
        self.iss == ISSUER
    }

    /// Validates the token
    pub fn validate(&self, required_scopes: &[Scope]) -> Result<(), ClaimValidationError> {
        if self.has_expired() {
            return Err(ClaimValidationError::TokenExpired);
        }

        if !self.has_valid_issuer() {
            return Err(ClaimValidationError::TokenInvalid(format!(
                "Invalid issuer. Expected {ISSUER}"
            )));
        }

        if !self.has_scopes(required_scopes) {
            return Err(ClaimValidationError::TokenInvalid(format!(
                "Invalid scopes. Expected {:?}, got {:?}",
                required_scopes, self.scopes
            )));
        }

        Ok(())
    }

    /// Returns whether token is valid
    ///
    /// Does not check for scopes.
    /// Does not describe the error if any.
    pub fn is_valid(&self) -> bool {
        self.validate(&[]).is_ok()
    }

    /// Returns nothing. Throws error if invalid access token.
    pub fn validate_access_token(&self) -> Result<(), ClaimValidationError> {
        self.validate(&[Scope::AccessToken])
    }

    /// Returns nothing. Throws error if invalid refresh token.
    pub fn validate_refresh_token(&self) -> Result<(), ClaimValidationError> {
        self.validate(&[Scope::RefreshToken])
    }

    /// Decode from a token string
    /// Assumes `key` uses EdDSA
    pub fn decode(
        token: &str,
        key: &DecodingKey,
    ) -> Result<TokenData<Self>, jsonwebtoken::errors::Error> {
        decode::<Self>(token, key, get_validation())
    }

    /// Encode to a token string
    /// Assumes `key` uses EdDSA
    pub fn encode(&self, key: &EncodingKey) -> Result<String, jsonwebtoken::errors::Error> {
        encode(&jsonwebtoken::Header::new(Algorithm::EdDSA), &self, key)
    }

    /// Check if [Claims] has all required scopes
    pub fn has_scopes(&self, required_scopes: &[Scope]) -> bool {
        required_scopes
            .iter()
            .all(|scope| self.scopes.contains(scope))
    }

    /// Checks if a specific scope is present
    pub fn has_scope(&self, scope: &Scope) -> bool {
        self.has_scopes(&[*scope])
    }
}
// TODO: Test ^^

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    use super::*;

    fn get_test_keys() -> (EncodingKey, DecodingKey) {
        let doc: Vec<u8> = BASE64
            .decode("MC4CAQAwBQYDK2VwBCIEIG73KilXg8qazIq8mNGzuPEHYPLY3WXR1uOS7ZxNkefV")
            .unwrap();
        let pair = Ed25519KeyPair::from_pkcs8_maybe_unchecked(&doc).unwrap();
        let encoding_key = EncodingKey::from_ed_der(&doc);
        let decoding_key = DecodingKey::from_ed_der(pair.public_key().as_ref());

        (encoding_key, decoding_key)
    }

    /// Test we encode and decode a token correctly
    #[test]
    fn test_encode_decode() {
        // Generate test keypair
        let (encoding_key, decoding_key) = get_test_keys();

        let user_id = nanoid::nanoid!().to_string();
        let expiry_duration = Duration::from_secs(1000);
        let scopes = vec![Scope::WriteUser, Scope::ReadUser];

        // Create claims
        let claims = Claims::new(user_id, UserRole::User, expiry_duration, scopes, None);

        // Encode and decode
        let token = claims.encode(&encoding_key).unwrap();
        let decoded = Claims::decode(&token, &decoding_key).unwrap();

        // Check header and claims
        assert_eq!(decoded.header.alg, Algorithm::EdDSA);
        assert_eq!(decoded.claims, claims);
    }

    #[test]
    #[should_panic]
    fn test_token_expired() {
        let (encoding_key, decoding_key) = get_test_keys();

        let user_id = nanoid::nanoid!().to_string();
        let scopes = vec![Scope::WriteUser, Scope::ReadUser];

        let claims = Claims {
            sub: user_id,
            exp: Utc::now().timestamp() as u64 - 100,
            iat: Utc::now().timestamp() as u64,
            jti: uuid::Uuid::new_v4().to_string(),
            iss: ISSUER.to_string(),
            sid: None, // Added sid
            role: UserRole::User,
            scopes,
        };

        let token = claims.encode(&encoding_key).unwrap();
        // Decoded token should be invalid
        let _decoded = Claims::decode(&token, &decoding_key).unwrap();
    }

    #[test]
    #[should_panic]
    fn test_token_invalid_issuer() {
        let (encoding_key, decoding_key) = get_test_keys();

        let user_id = nanoid::nanoid!().to_string();
        let scopes = vec![Scope::WriteUser, Scope::ReadUser];

        let mut claims = Claims::new(
            user_id,
            UserRole::User,
            Duration::from_secs(1000),
            scopes,
            None,
        );
        claims.iss = "my-invalid-issuer".to_string();
        let token = claims.encode(&encoding_key).unwrap();
        let _decoded = Claims::decode(&token, &decoding_key).unwrap();
    }

    #[test]
    fn test_has_scopes() {
        let user_id = nanoid::nanoid!().to_string();

        let claims = Claims::new(
            user_id,
            UserRole::User,
            Duration::from_secs(1000),
            vec![Scope::WriteUser, Scope::ReadUser],
            None,
        );
        assert!(claims.has_scopes(&[Scope::WriteUser]));
        assert!(claims.has_scopes(&[Scope::ReadUser]));
        assert!(claims.has_scopes(&[Scope::WriteUser, Scope::ReadUser]));
        assert!(!claims.has_scopes(&[Scope::WriteUser, Scope::ReadUser, Scope::RefreshToken]));
        assert!(!claims.has_scopes(&[Scope::WriteUser, Scope::RefreshToken]));
    }
}

// TODO: Bench core functions (being used many times for authorization)
