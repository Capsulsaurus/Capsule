use jsonwebtoken::{DecodingKey, EncodingKey};
use ring::signature::{Ed25519KeyPair, KeyPair};

/// Convert a PKCS#8 DER-encoded ED25519 key to corresponding jsonwebtoken EdDSA keys
pub(crate) fn convert_ed25519_der_to_jwt_keys(
    der: &[u8],
) -> Result<(EncodingKey, DecodingKey), ring::error::KeyRejected> {
    let pair = Ed25519KeyPair::from_pkcs8_maybe_unchecked(der)?;

    Ok((
        EncodingKey::from_ed_der(der),
        DecodingKey::from_ed_der(pair.public_key().as_ref()),
    ))
}

/// Mint a fresh PKCS#8 DER-encoded Ed25519 signing key for `JWT_ED25519_DER`.
///
/// Generation lives beside [`convert_ed25519_der_to_jwt_keys`] on purpose: this crate owns
/// the key's wire format, so the generator and the parser stay in step and one round-trip
/// test covers both. `capsule-api`'s `keygen` binary is a thin printer over this (slice
/// `S-P7`), which is what makes a from-clean-checkout server bring-up possible without
/// `openssl` on the developer's PATH.
///
/// # Errors
/// Returns [`ring::error::Unspecified`] if the system RNG is unavailable.
pub fn generate_signing_key_der() -> Result<Vec<u8>, ring::error::Unspecified> {
    let rng = ring::rand::SystemRandom::new();
    let doc = Ed25519KeyPair::generate_pkcs8(&rng)?;
    Ok(doc.as_ref().to_vec())
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64;

    use super::*;

    /// Test we parse a DER-encoded ED25519 keypair
    #[test]
    fn test_generate_keypair() {
        let doc = BASE64
            .decode("MC4CAQAwBQYDK2VwBCIEIG73KilXg8qazIq8mNGzuPEHYPLY3WXR1uOS7ZxNkefV")
            .unwrap();
        assert!(convert_ed25519_der_to_jwt_keys(doc.as_ref()).is_ok());
    }

    /// Test we fail to parse a bad DER-encoded ED25519 keypair
    #[test]
    fn test_generate_keypair_bad_der() {
        let doc = BASE64.decode("Ym9ndXMK").unwrap(); // "bogus" in base64
        assert!(convert_ed25519_der_to_jwt_keys(doc.as_ref()).is_err());
    }

    /// A minted key must be accepted by the very parser the server boots through, and must
    /// survive the base64 round trip the operator actually pastes into `.env` (S-P7).
    #[test]
    fn generated_signing_key_is_accepted_by_the_jwt_parser() {
        let der = generate_signing_key_der().expect("system RNG available");
        assert!(convert_ed25519_der_to_jwt_keys(&der).is_ok());

        let round_tripped = BASE64
            .decode(BASE64.encode(&der))
            .expect("our own base64 output decodes");
        assert_eq!(der, round_tripped);
        assert!(convert_ed25519_der_to_jwt_keys(&round_tripped).is_ok());
    }

    /// Two calls must not return the same key — a keygen that emits a constant would hand
    /// every self-hosted deployment the same token-signing secret.
    #[test]
    fn generated_signing_keys_are_distinct() {
        let a = generate_signing_key_der().expect("system RNG available");
        let b = generate_signing_key_der().expect("system RNG available");
        assert_ne!(a, b);
    }
}
