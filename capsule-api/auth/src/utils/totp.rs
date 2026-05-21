use eyre::Context;
use totp_rs::{Algorithm, Secret, TOTP};

/// Generate a new Base32-encoded TOTP secret
pub fn generate_secret() -> String {
    Secret::generate_secret().to_encoded().to_string()
}

/// Returns TOTP generator
pub fn get_totp_generator(secret: &str) -> eyre::Result<TOTP> {
    TOTP::new(
        Algorithm::SHA1,
        6,  // 6 digits
        1,  // 1 step (skew tolerance)
        30, // 30 second period
        Secret::Encoded(secret.to_string())
            .to_bytes()
            .wrap_err("Failed to parse secret")?,
        None,
        "capsule".to_string(),
    )
    .wrap_err("Failed to create TOTP")
}
// TODO: Test that it fails if secret is invalid

/// Verify a TOTP token against a secret
///
/// This accounts for time skew tolerance (±1 period = ±30 seconds)
pub fn verify_token(secret: &str, token: &str) -> eyre::Result<bool> {
    let totp = get_totp_generator(secret)?;

    totp.check_current(token).wrap_err("Failed to verify token")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_secret() {
        let secret = generate_secret();
        assert!(!secret.is_empty());
        // Base32 secrets should only contain A-Z, 2-7, and =
        assert!(
            secret
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '=')
        );
    }

    #[test]
    fn test_verify_valid_token() {
        let secret = generate_secret();

        // Generate current token
        let totp = get_totp_generator(&secret).unwrap();

        let token = totp.generate_current().unwrap();

        // Verify the token
        let result = verify_token(&secret, &token);
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn test_verify_invalid_token() {
        let secret = generate_secret();
        let invalid_token = "000000";

        let result = verify_token(&secret, invalid_token);
        // Should return Ok(false) for invalid token
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }
}
