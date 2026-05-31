//! Standalone AEAD for metadata blobs — a single contiguous byte string with a fixed
//! wire format. Used for the encrypted CBOR sidecar / metadata blob the server stores.
//!
//! **Implementations MUST produce and consume exactly this layout** so two correct
//! implementations compute identical content hashes byte-for-byte:
//!
//! ```text
//! +---------------------+------------------+----------------------+----------------+
//! | crypto_suite_id (2) | nonce (12 bytes) | ciphertext (variable)| tag (16 bytes) |
//! | big-endian u16      | fresh CSPRNG     | AES-256-GCM(plaintext)| GCM tag        |
//! +---------------------+------------------+----------------------+----------------+
//! ```
//!
//! The key is derived per blob via [`crate::crypto::keys::Amk::derive_blob_key`]. The
//! `ciphertext_hash` committed to by the manifest is computed over the **full** byte
//! string. SSoT: [Cryptography — Encryption § Metadata Blob Wire Format].
//!
//! [Cryptography — Encryption § Metadata Blob Wire Format]: https://docs/design/cryptography/encryption/#metadata-blob-wire-format

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};

use crate::crypto::hash::{self, Hash32};
use crate::crypto::primitives::{CRYPTO_SUITE_ID, SuiteId};
use crate::crypto::{CryptoError, rng};

const SUITE_LEN: usize = 2;
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;
/// Minimum valid blob length: header + nonce + empty ciphertext + tag.
pub const MIN_BLOB_LEN: usize = SUITE_LEN + NONCE_LEN + TAG_LEN;

/// Seal `plaintext` (canonical CBOR) into the metadata-blob wire format under `blob_key`,
/// tagging it with the current `crypto_suite_id`. A fresh nonce is drawn per call.
pub fn seal_blob(blob_key: &[u8; 32], plaintext: &[u8]) -> Vec<u8> {
    let nonce = rng::random_array::<NONCE_LEN>();
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(blob_key));
    let ct_and_tag = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .expect("AES-256-GCM seal is infallible for a valid key/nonce");

    let mut out = Vec::with_capacity(SUITE_LEN + NONCE_LEN + ct_and_tag.len());
    out.extend_from_slice(&CRYPTO_SUITE_ID.to_be_bytes());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct_and_tag);
    out
}

/// Open a metadata-blob wire string under `blob_key`. Rejects an unknown `crypto_suite_id`
/// (fail-closed) before attempting decryption, and rejects tampered ciphertext.
pub fn open_blob(blob_key: &[u8; 32], wire: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if wire.len() < MIN_BLOB_LEN {
        return Err(CryptoError::Malformed("metadata blob shorter than minimum"));
    }
    let suite = u16::from_be_bytes([wire[0], wire[1]]);
    if SuiteId::from_u16(suite).is_none() {
        return Err(CryptoError::UnknownSuite(suite));
    }
    let nonce = &wire[SUITE_LEN..SUITE_LEN + NONCE_LEN];
    let ct_and_tag = &wire[SUITE_LEN + NONCE_LEN..];
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(blob_key));
    cipher
        .decrypt(Nonce::from_slice(nonce), ct_and_tag)
        .map_err(|_| CryptoError::Auth("metadata blob authentication failed"))
}

/// The content hash of a metadata blob: SHA-256 over the **full** wire byte string
/// (header ‖ nonce ‖ ciphertext ‖ tag). This is the value the manifest commits to.
pub fn blob_ciphertext_hash(wire: &[u8]) -> Hash32 {
    hash::hash_bytes(wire)
}

/// The `crypto_suite_id` declared in a blob's header, without decrypting.
pub fn blob_suite_id(wire: &[u8]) -> Option<u16> {
    (wire.len() >= SUITE_LEN).then(|| u16::from_be_bytes([wire[0], wire[1]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [0x42; 32];

    #[test]
    fn seal_open_round_trip() {
        let plaintext = b"deterministic CBOR sidecar bytes";
        let wire = seal_blob(&KEY, plaintext);
        assert_eq!(open_blob(&KEY, &wire).unwrap(), plaintext);
    }

    #[test]
    fn wire_format_layout_is_exact() {
        let wire = seal_blob(&KEY, b"abc");
        // suite(2) big-endian = 0x0001.
        assert_eq!(&wire[0..2], &[0x00, 0x01]);
        // total = 2 + 12 + 3 (ct) + 16 (tag).
        assert_eq!(wire.len(), 2 + 12 + 3 + 16);
        assert_eq!(blob_suite_id(&wire), Some(0x0001));
    }

    #[test]
    fn empty_plaintext_is_well_formed() {
        let wire = seal_blob(&KEY, b"");
        assert_eq!(wire.len(), MIN_BLOB_LEN);
        assert_eq!(open_blob(&KEY, &wire).unwrap(), b"");
    }

    #[test]
    fn ciphertext_hash_covers_entire_wire() {
        let wire = seal_blob(&KEY, b"payload");
        assert_eq!(blob_ciphertext_hash(&wire), hash::hash_bytes(&wire));
        // Flipping any byte (even in the header) changes the content hash.
        let mut t = wire.clone();
        t[0] ^= 0x01;
        assert_ne!(blob_ciphertext_hash(&t), blob_ciphertext_hash(&wire));
    }

    #[test]
    fn fresh_nonce_per_seal() {
        let a = seal_blob(&KEY, b"same");
        let b = seal_blob(&KEY, b"same");
        assert_ne!(a, b, "each seal must draw a fresh nonce");
        // Nonce field differs.
        assert_ne!(&a[2..14], &b[2..14]);
    }

    #[test]
    fn tamper_and_wrong_key_rejected() {
        let wire = seal_blob(&KEY, b"secret");
        let mut t = wire.clone();
        let last = t.len() - 1;
        t[last] ^= 0x01;
        assert!(open_blob(&KEY, &t).is_err());
        assert!(open_blob(&[0u8; 32], &wire).is_err());
    }

    #[test]
    fn unknown_suite_id_fails_closed_before_decrypt() {
        let mut wire = seal_blob(&KEY, b"x");
        wire[0] = 0xff;
        wire[1] = 0xff;
        assert_eq!(
            open_blob(&KEY, &wire),
            Err(CryptoError::UnknownSuite(0xffff))
        );
    }

    #[test]
    fn too_short_is_malformed() {
        assert!(matches!(
            open_blob(&KEY, &[0u8; 5]),
            Err(CryptoError::Malformed(_))
        ));
    }
}
