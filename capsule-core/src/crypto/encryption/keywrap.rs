//! `asset-keywrap/v1` — sealing an externally-chosen file key `K` under the album's AMK.
//!
//! An asset a member did *not* author — an [adopted web-upload drop] — arrives already
//! STREAM-encrypted under a random key `K` the guest chose, which no member can re-derive
//! from the AMK. The adopting client therefore **carries** `K` wrapped under the AMK
//! instead of deriving it. The wrapped bytes travel in the signed
//! [manifest](crate::crypto::provenance)'s `wrapped_file_key` field under `key_mode =
//! wrapped`; a reader holding the AMK unwraps `K` and runs the unchanged
//! [STREAM construction](super::stream) to decrypt.
//!
//! Wire format (fixed length, opaque to the server):
//!
//! ```text
//! +--------------------+----------------------------+----------------+
//! | wrap_nonce (12)    | AES-256-GCM(K) (32)        | tag (16)       |
//! +--------------------+----------------------------+----------------+
//! ```
//!
//! The wrap key is `HKDF(ikm=AMK, salt=file_id || wrap_nonce, info="asset-keywrap/v1")`
//! ([`Amk::derive_wrap_key`]). The fresh `wrap_nonce` is folded into the salt exactly as a
//! derived key's `nonce_prefix` is, so no `(wrap_key, wrap_nonce)` pair repeats and AES-GCM
//! nonce reuse is structurally impossible. This is the sole case where a per-file key is
//! stored rather than recomputed. SSoT: [Encryption § Asset Key Derivation].
//!
//! [adopted web-upload drop]: https://docs/design/web-upload/#why-adopt-in-place
//! [Encryption § Asset Key Derivation]: https://docs/design/cryptography/encryption/#asset-key-derivation

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use uuid::Uuid;

use crate::crypto::CryptoError;
use crate::crypto::keys::Amk;

/// Length of the fresh per-wrap nonce (the AES-256-GCM nonce for the wrap).
pub const WRAP_NONCE_LEN: usize = 12;
/// Length of the wrapped file key: `wrap_nonce(12) ‖ ciphertext(32) ‖ tag(16)`.
pub const WRAPPED_FILE_KEY_LEN: usize = WRAP_NONCE_LEN + 32 + 16;

/// Seal an externally-chosen 32-byte file key `K` under `amk`, scoped to `file_id`.
///
/// Draws a fresh `wrap_nonce`, derives the wrap key with the nonce folded into the salt,
/// and returns `wrap_nonce ‖ AES-256-GCM(K) ‖ tag` — the bytes stored verbatim in the
/// manifest's `wrapped_file_key`. Wrap key derivation is deferred to the AMK so nonce
/// generation and derivation stay coupled (the salt depends on the nonce).
pub fn seal_file_key(amk: &Amk, file_id: &Uuid, file_key: &[u8; 32]) -> Vec<u8> {
    let wrap_nonce = crate::crypto::rng::random_array::<WRAP_NONCE_LEN>();
    let wrap_key = amk.derive_wrap_key(file_id, &wrap_nonce);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&wrap_key));
    let ct_and_tag = cipher
        .encrypt(Nonce::from_slice(&wrap_nonce), file_key.as_slice())
        .expect("AES-256-GCM seal is infallible for a valid key/nonce");

    let mut out = Vec::with_capacity(WRAPPED_FILE_KEY_LEN);
    out.extend_from_slice(&wrap_nonce);
    out.extend_from_slice(&ct_and_tag);
    debug_assert_eq!(out.len(), WRAPPED_FILE_KEY_LEN);
    out
}

/// Recover the file key `K` from a `wrapped_file_key` under `amk`, scoped to `file_id`.
///
/// Fail-closed: a wrong length is [`CryptoError::Malformed`]; a wrong AMK / `file_id`, a
/// tampered nonce, or a tampered ciphertext all fail AEAD authentication
/// ([`CryptoError::Auth`]) — the same terminal rejection any tampered signed field earns.
pub fn unseal_file_key(amk: &Amk, file_id: &Uuid, wrapped: &[u8]) -> Result<[u8; 32], CryptoError> {
    if wrapped.len() != WRAPPED_FILE_KEY_LEN {
        return Err(CryptoError::Malformed("wrapped file key wrong length"));
    }
    let (wrap_nonce, ct_and_tag) = wrapped.split_at(WRAP_NONCE_LEN);
    let wrap_key = amk.derive_wrap_key(file_id, wrap_nonce);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&wrap_key));
    let plaintext = cipher
        .decrypt(Nonce::from_slice(wrap_nonce), ct_and_tag)
        .map_err(|_| CryptoError::Auth("wrapped file key authentication failed"))?;
    // A valid GCM open of a 32-byte plaintext is exactly 32 bytes; guard defensively.
    let key: [u8; 32] = plaintext
        .as_slice()
        .try_into()
        .map_err(|_| CryptoError::Malformed("unwrapped file key wrong length"))?;
    Ok(key)
}

/// Recover the file key of the **original** a manifest describes, honouring its `key_mode`.
///
/// `amk` is the AMK of the epoch the manifest was written under (`core.amk_version`); the
/// caller resolves it, because where the epoch keys live differs per reader (a workspace's
/// album, a backup artifact's ledger, a share link's album grant). A `derived` original
/// re-derives `HKDF(AMK, file_id ‖ nonce_prefix)`; a `wrapped` one — an adopted web-upload
/// drop, whose key a guest chose and nobody can derive — unwraps `wrapped_file_key` under the
/// AMK. Every reader that recovers an original's key goes through here, so no reader can
/// silently assume `derived`.
///
/// Fail-closed: a `wrapped` manifest without a `wrapped_file_key` is
/// [`CryptoError::Malformed`]; a key that does not open is [`CryptoError::Auth`].
pub fn recover_file_key(
    amk: &Amk,
    core: &crate::crypto::provenance::ManifestCore,
) -> Result<[u8; 32], CryptoError> {
    use crate::crypto::provenance::KeyMode;
    match core.key_mode {
        KeyMode::Derived => Ok(amk.derive_file_key(&core.file_id, &core.nonce_prefix)),
        KeyMode::Wrapped => {
            let wrapped = core
                .wrapped_file_key
                .as_ref()
                .ok_or(CryptoError::Malformed(
                    "wrapped key mode without a wrapped_file_key",
                ))?;
            unseal_file_key(amk, &core.file_id, &wrapped.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::encryption::{decrypt_asset_vec, encrypt_asset_vec_with_prefix};

    const K: [u8; 32] = [0x77; 32];

    #[test]
    fn seal_unseal_round_trip() {
        let amk = Amk::from_bytes([0x11; 32]);
        let file_id = Uuid::from_u128(0xF11E);
        let wrapped = seal_file_key(&amk, &file_id, &K);
        assert_eq!(wrapped.len(), WRAPPED_FILE_KEY_LEN);
        assert_eq!(unseal_file_key(&amk, &file_id, &wrapped).unwrap(), K);
    }

    #[test]
    fn fresh_wrap_nonce_per_seal_still_unseals() {
        let amk = Amk::from_bytes([0x11; 32]);
        let file_id = Uuid::from_u128(1);
        let a = seal_file_key(&amk, &file_id, &K);
        let b = seal_file_key(&amk, &file_id, &K);
        // Distinct wrap_nonce → distinct wrapped bytes even under a constant file_id + K.
        assert_ne!(a, b, "each seal must draw a fresh wrap_nonce");
        assert_ne!(&a[..WRAP_NONCE_LEN], &b[..WRAP_NONCE_LEN]);
        assert_eq!(unseal_file_key(&amk, &file_id, &a).unwrap(), K);
        assert_eq!(unseal_file_key(&amk, &file_id, &b).unwrap(), K);
    }

    #[test]
    fn tampered_nonce_or_ciphertext_fails_auth() {
        let amk = Amk::from_bytes([0x11; 32]);
        let file_id = Uuid::from_u128(1);
        let wrapped = seal_file_key(&amk, &file_id, &K);

        // Flip a ciphertext/tag byte.
        let mut t = wrapped.clone();
        let last = t.len() - 1;
        t[last] ^= 0x01;
        assert!(matches!(
            unseal_file_key(&amk, &file_id, &t),
            Err(CryptoError::Auth(_))
        ));

        // Flip a nonce byte → a different wrap key is derived → open fails.
        let mut t = wrapped.clone();
        t[0] ^= 0x01;
        assert!(matches!(
            unseal_file_key(&amk, &file_id, &t),
            Err(CryptoError::Auth(_))
        ));
    }

    #[test]
    fn wrong_amk_or_file_id_rejects() {
        let amk = Amk::from_bytes([0x11; 32]);
        let file_id = Uuid::from_u128(1);
        let wrapped = seal_file_key(&amk, &file_id, &K);
        // Wrong AMK epoch.
        assert!(matches!(
            unseal_file_key(&Amk::from_bytes([0x99; 32]), &file_id, &wrapped),
            Err(CryptoError::Auth(_))
        ));
        // Wrong file_id → salt divergence → wrong wrap key.
        assert!(matches!(
            unseal_file_key(&amk, &Uuid::from_u128(2), &wrapped),
            Err(CryptoError::Auth(_))
        ));
    }

    #[test]
    fn malformed_length_is_rejected_before_decrypt() {
        let amk = Amk::from_bytes([0x11; 32]);
        let file_id = Uuid::from_u128(1);
        assert!(matches!(
            unseal_file_key(&amk, &file_id, &[0u8; 10]),
            Err(CryptoError::Malformed(_))
        ));
        assert!(matches!(
            unseal_file_key(&amk, &file_id, &[0u8; WRAPPED_FILE_KEY_LEN + 1]),
            Err(CryptoError::Malformed(_))
        ));
    }

    /// The doc's positive case: a member holding the AMK unwraps `K` and STREAM-decrypts the
    /// unchanged ciphertext — decryption is identical to the derived case once `K` is back.
    #[test]
    fn member_unwrap_then_stream_decrypt_round_trip() {
        let amk = Amk::from_bytes([0x5A; 32]);
        let file_id = Uuid::from_u128(0xF11E);
        // A guest chose a random K and STREAM-encrypted the asset under it.
        let plaintext = b"adopted web-upload drop bytes, sealed under a guest-chosen key";
        let (enc, ct) = encrypt_asset_vec_with_prefix(&K, [1, 2, 3, 4, 5, 6, 7], plaintext);
        let wrapped = seal_file_key(&amk, &file_id, &K);

        // The adopting member recovers K and decrypts the unchanged ciphertext.
        let recovered = unseal_file_key(&amk, &file_id, &wrapped).unwrap();
        assert_eq!(recovered, K);
        let back = decrypt_asset_vec(&recovered, &enc.nonce_prefix, &ct).unwrap();
        assert_eq!(back, plaintext);
    }

    fn manifest_core(
        file_id: Uuid,
        key_mode: crate::crypto::provenance::KeyMode,
        wrapped: Option<Vec<u8>>,
    ) -> crate::crypto::provenance::ManifestCore {
        use crate::crypto::hash::Hash32;
        use crate::crypto::keys::AmkVersion;
        use crate::crypto::primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION};
        use crate::crypto::provenance::{
            ASSET_MANIFEST_VERSION, Action, ManifestCore, WrappedFileKey,
        };
        ManifestCore {
            version: ASSET_MANIFEST_VERSION.into(),
            crypto_suite_id: CRYPTO_SUITE_ID,
            protocol_version: PROTOCOL_VERSION.into(),
            file_id,
            album_id: Uuid::from_u128(0xA1),
            amk_version: AmkVersion(1),
            ciphertext_hash: Hash32([0xCC; 32]),
            plaintext_size: 1,
            chunk_size: 65_520,
            nonce_prefix: [1, 2, 3, 4, 5, 6, 7],
            key_mode,
            wrapped_file_key: wrapped.map(WrappedFileKey),
            metadata_blob_hash: None,
            created_by_user: Uuid::from_u128(0x05E2),
            created_by_device: Uuid::from_u128(0xD1),
            client_version: "t".into(),
            timestamp: "2026-05-31T12:00:00Z".into(),
            action: Action::Create,
            prior_provenance_hash: None,
            retention_until: None,
            upgraded_from: None,
        }
    }

    /// `recover_file_key` follows the manifest's `key_mode`: `derived` re-derives under the
    /// folded nonce prefix, `wrapped` unwraps the carried key, and a `wrapped` manifest that
    /// lost its key — or carries one sealed under another AMK — fails closed.
    #[test]
    fn recover_file_key_honours_key_mode() {
        use crate::crypto::provenance::KeyMode;
        let amk = Amk::from_bytes([0x5A; 32]);
        let file_id = Uuid::from_u128(0xF11E);

        let derived = manifest_core(file_id, KeyMode::Derived, None);
        assert_eq!(
            recover_file_key(&amk, &derived).unwrap(),
            amk.derive_file_key(&file_id, &derived.nonce_prefix),
        );

        let wrapped = manifest_core(
            file_id,
            KeyMode::Wrapped,
            Some(seal_file_key(&amk, &file_id, &K)),
        );
        assert_eq!(recover_file_key(&amk, &wrapped).unwrap(), K);
        assert_ne!(K, amk.derive_file_key(&file_id, &wrapped.nonce_prefix));
        assert!(matches!(
            recover_file_key(&Amk::from_bytes([0x99; 32]), &wrapped),
            Err(CryptoError::Auth(_))
        ));

        let keyless = manifest_core(file_id, KeyMode::Wrapped, None);
        assert!(matches!(
            recover_file_key(&amk, &keyless),
            Err(CryptoError::Malformed(_))
        ));
    }
}
