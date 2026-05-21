use jsonwebtoken::{DecodingKey, EncodingKey};
use ring::signature::{Ed25519KeyPair, KeyPair};

/// Convert a PKCS#8 v1 DER-encoded ED25519 key to corresponding jsonwebtoken EdDSA keys
pub fn convert_ed25519_der_to_jwt_keys(
    der: &[u8],
) -> Result<(EncodingKey, DecodingKey), ring::error::KeyRejected> {
    let pair = Ed25519KeyPair::from_pkcs8_maybe_unchecked(der)?;

    Ok((
        EncodingKey::from_ed_der(der),
        DecodingKey::from_ed_der(pair.public_key().as_ref()),
    ))
}
// TODO: Test ^

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

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
}
