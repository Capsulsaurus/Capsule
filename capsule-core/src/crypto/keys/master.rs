//! The account master key: the single backed-up root of the hierarchy.
//!
//! It does **not** encrypt assets directly. Its jobs are (1) to wrap the per-device
//! identity private keys and (2) to anchor the encrypted backup that escrows album keys.
//! It also derives one *identifier* — the [default album]'s `album_id` — via HKDF with a
//! dedicated label, so any device can recompute the default album from the master key
//! alone after recovery (SSoT: [Cryptography — Keys § Key Chain]).
//!
//! [default album]: https://docs/design/organization/#the-default-album
//! [Cryptography — Keys § Key Chain]: https://docs/design/cryptography/keys/#key-chain

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use uuid::Uuid;

use crate::crypto::primitives::info;
use crate::crypto::{CryptoError, kdf, rng};

const NONCE_LEN: usize = 12;

/// A 32-byte account master key.
#[derive(Clone)]
pub struct MasterKey([u8; 32]);

impl MasterKey {
    /// Generate a fresh master key from the OS CSPRNG (client-side at account creation).
    pub fn generate() -> Self {
        Self(rng::random_array::<32>())
    }

    /// Wrap raw key bytes (e.g. after unwrapping the escrow blob).
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow the raw key bytes (for escrow wrapping).
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Derive the default album's `album_id` deterministically. This derives an *ID*,
    /// never a key — the default album has its own random per-epoch AMK like any album.
    pub fn derive_default_album_id(&self) -> Uuid {
        let okm = kdf::hkdf_sha512(&self.0, b"", info::DEFAULT_ALBUM_ID_V1, 16);
        let mut b = [0u8; 16];
        b.copy_from_slice(&okm);
        // A v8 (custom) UUID: deterministic, unguessable before creation, recomputable.
        uuid::Builder::from_custom_bytes(b).into_uuid()
    }

    /// Symmetrically seal `plaintext` under the master key (AES-256-GCM, random nonce).
    /// Used to wrap device identity private keys. Output is `nonce(12) ‖ ciphertext+tag`.
    pub fn seal(&self, plaintext: &[u8]) -> Vec<u8> {
        let nonce = rng::random_array::<NONCE_LEN>();
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&self.0));
        let ct = cipher
            .encrypt(Nonce::from_slice(&nonce), plaintext)
            .expect("AES-256-GCM seal is infallible for a valid key/nonce");
        let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        out
    }

    /// Open a blob produced by [`seal`](Self::seal). Returns [`CryptoError::Auth`] on a
    /// wrong key or tampered ciphertext.
    pub fn open(&self, blob: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if blob.len() < NONCE_LEN {
            return Err(CryptoError::Malformed("sealed blob shorter than nonce"));
        }
        let (nonce, ct) = blob.split_at(NONCE_LEN);
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&self.0));
        cipher
            .decrypt(Nonce::from_slice(nonce), ct)
            .map_err(|_| CryptoError::Auth("master-key unseal failed"))
    }
}

impl std::fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print key material.
        f.write_str("MasterKey(****)")
    }
}

#[cfg(test)]
mod tests;
