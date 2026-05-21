/// Returns the JWT token issuer. Reads from CAPSULE_ISSUER env var if set.
pub fn get_auth_issuer() -> String {
    std::env::var("CAPSULE_ISSUER").unwrap_or_else(|_| crate::constants::ISSUER.to_string())
}
