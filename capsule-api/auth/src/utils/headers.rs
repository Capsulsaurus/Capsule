use crate::claims::Claims;
use salvo::http::HeaderMap;
use secrecy::{ExposeSecret, SecretString};

use crate::errors::ClaimValidationError;

/// Get the token from the Authorization header
pub fn get_token_from_headers(headers: &HeaderMap) -> Result<SecretString, ClaimValidationError> {
    let auth_header = headers
        .get("Authorization")
        .ok_or(ClaimValidationError::TokenMissing)?
        .to_str()
        .map_err(|_| ClaimValidationError::UnexpectedHeaderFormat)?;

    // Check if it's a Bearer token
    if !auth_header.starts_with("Bearer ") {
        return Err(ClaimValidationError::UnexpectedHeaderFormat);
    }

    // Extract the token part
    Ok(SecretString::from(&auth_header[7..]))
}

/// Validates access token from headers
///
/// Returns user ID if valid
pub fn validate_user_from_headers(
    headers: &HeaderMap,
    decoding_key: &jsonwebtoken::DecodingKey,
) -> Result<String, ClaimValidationError> {
    let token_secret = get_token_from_headers(headers)?;
    let token_data = Claims::decode(token_secret.expose_secret(), decoding_key)?;
    let claims = token_data.claims;

    // Validate token
    claims.validate_access_token()?;

    // Note: We do not need a particular scope for access tokens
    Ok(claims.sub) // Return user ID
}

#[cfg(test)]
mod tests {
    use salvo::http::HeaderValue;

    use super::*;

    #[test]
    fn test_get_token_valid() {
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", HeaderValue::from_static("Bearer my_token"));

        let token = get_token_from_headers(&headers).unwrap();
        assert_eq!(token.expose_secret(), "my_token");
    }

    #[test]
    fn test_get_token_missing_header() {
        let headers = HeaderMap::new();
        let result = get_token_from_headers(&headers);
        assert!(matches!(result, Err(ClaimValidationError::TokenMissing)));
    }

    #[test]
    fn test_get_token_invalid_format() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "Authorization",
            HeaderValue::from_static("Basic base64code"),
        );

        let result = get_token_from_headers(&headers);
        assert!(matches!(
            result,
            Err(ClaimValidationError::UnexpectedHeaderFormat)
        ));
    }

    #[test]
    fn test_get_token_malformed() {
        let mut headers = HeaderMap::new();
        // Just "Bearer" w/o space or token
        headers.insert("Authorization", HeaderValue::from_static("Bearer"));
        let result = get_token_from_headers(&headers);
        // "Bearer".len() is 6. [7..] would panic if we didn't check starts_with("Bearer ") which includes space (len 7)
        // CODE CHECK:
        // if !auth_header.starts_with("Bearer ")

        // "Bearer" (len 6) -> starts_with("Bearer ") is false.
        assert!(matches!(
            result,
            Err(ClaimValidationError::UnexpectedHeaderFormat)
        ));
    }
}
