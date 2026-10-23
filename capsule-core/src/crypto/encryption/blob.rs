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
/// Length of the per-blob AES-256-GCM nonce (fresh CSPRNG draw per seal).
pub const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;
/// Minimum valid blob length: header + nonce + empty ciphertext + tag.
pub const MIN_BLOB_LEN: usize = SUITE_LEN + NONCE_LEN + TAG_LEN;

/// Seal `plaintext` (canonical CBOR) into the metadata-blob wire format under `blob_key`,
/// tagging it with the current `crypto_suite_id`. A fresh nonce is drawn per call.
///
/// This is the nonce-agnostic AEAD primitive (also used for the KEM-DEM drop wrap and the
/// share-scope wrap, whose keys are not blob-id-derived). Metadata-blob writers that fold
/// the nonce into a derived key go through [`super::rekey::seal_metadata_blob`].
pub fn seal_blob(blob_key: &[u8; 32], plaintext: &[u8]) -> Vec<u8> {
    seal_blob_with_nonce(blob_key, rng::random_array::<NONCE_LEN>(), plaintext)
}

/// [`seal_blob`] with an explicit `nonce`, so a metadata-blob writer can fold that same
/// nonce into the derived `blob_key` salt before sealing (the salt depends on the nonce).
pub fn seal_blob_with_nonce(
    blob_key: &[u8; 32],
    nonce: [u8; NONCE_LEN],
    plaintext: &[u8],
) -> Vec<u8> {
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

/// The 12-byte nonce a blob carries in its header, needed to re-derive the folded
/// `blob_key` before opening. `None` if the wire is shorter than the fixed prefix.
pub fn blob_nonce(wire: &[u8]) -> Option<[u8; NONCE_LEN]> {
    wire.get(SUITE_LEN..SUITE_LEN + NONCE_LEN)?.try_into().ok()
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
mod tests;
