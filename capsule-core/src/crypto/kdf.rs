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
mod tests;
