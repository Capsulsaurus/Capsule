//! HKDF-SHA512 key derivation (SSoT: [Cryptography — Primitives § Key Derivation]).
//!
//! The wider SHA-512 keeps the post-quantum posture (a 256-bit hash falls to ~128-bit
//! under Grover; SHA-512 retains ~256-bit), and KDFs are off the hot path. Every
//! derivation includes a versioned `info` label (see [`super::primitives::info`]) and a
//! scope-unique salt (`album_id`, `file_id`, `blob_id`) — salts are never reused across
//! scopes. The 512-bit output is truncated to 32 bytes for AES-256 keys.
//!
//! [Cryptography — Primitives § Key Derivation]: https://docs/design/cryptography/primitives/#key-derivation

use hkdf::Hkdf;
use sha2::Sha512;

/// HKDF-SHA512 extract-then-expand, producing `out_len` bytes.
///
/// `out_len` must be ≤ 255 × 64 (the HKDF-SHA512 ceiling), which every Capsule
/// derivation satisfies; an out-of-range request is a programming error and panics.
pub fn hkdf_sha512(ikm: &[u8], salt: &[u8], info: &[u8], out_len: usize) -> Vec<u8> {
    let hk = Hkdf::<Sha512>::new(Some(salt), ikm);
    let mut okm = vec![0u8; out_len];
    hk.expand(info, &mut okm)
        .expect("HKDF-SHA512 output length within bounds");
    okm
}

/// Derive a 32-byte (AES-256) key. The canonical derivation for file and metadata-blob keys.
pub fn derive_key32(ikm: &[u8], salt: &[u8], info: &[u8]) -> [u8; 32] {
    let okm = hkdf_sha512(ikm, salt, info, 32);
    let mut key = [0u8; 32];
    key.copy_from_slice(&okm);
    key
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::primitives::info;

    #[test]
    fn derivation_is_deterministic() {
        let ikm = [7u8; 32];
        let a = derive_key32(&ikm, b"file-123", info::ASSET_FILE_V1);
        let b = derive_key32(&ikm, b"file-123", info::ASSET_FILE_V1);
        assert_eq!(a, b, "same (ikm, salt, info) must produce identical output");
    }

    #[test]
    fn scope_uniqueness_by_salt_and_info() {
        let ikm = [7u8; 32];
        let base = derive_key32(&ikm, b"file-123", info::ASSET_FILE_V1);
        // Different salt (file_id) → different key.
        assert_ne!(base, derive_key32(&ikm, b"file-124", info::ASSET_FILE_V1));
        // Different info label (domain separation) → different key.
        assert_ne!(
            base,
            derive_key32(&ikm, b"file-123", info::METADATA_BLOB_V1)
        );
        // Different ikm (AMK) → different key.
        assert_ne!(
            base,
            derive_key32(&[8u8; 32], b"file-123", info::ASSET_FILE_V1)
        );
    }

    #[test]
    fn truncation_is_a_prefix_of_the_full_expand() {
        // Deriving 32 bytes must equal the first 32 bytes of a 64-byte expansion, so the
        // 512→256 truncation is well-defined and stable across platforms.
        let ikm = [3u8; 32];
        let k32 = derive_key32(&ikm, b"album-1", info::ASSET_FILE_V1);
        let k64 = hkdf_sha512(&ikm, b"album-1", info::ASSET_FILE_V1, 64);
        assert_eq!(&k32[..], &k64[..32]);
    }

    #[test]
    fn golden_vector_pins_the_wiring() {
        // Pins the exact HKDF-SHA512 construction (extract+expand, label, salt) so an
        // accidental algorithm/label change is caught. Cross-checked once on first run.
        let key = derive_key32(&[0u8; 32], b"salt", info::ASSET_FILE_V1);
        assert_eq!(
            hex::encode(key),
            "cb54d8d1556baf00a1fb03103adaa63d7cd5fb108a4e418d76b5a5d724f75587"
        );
    }
}
