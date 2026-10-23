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
    recover_file_key_parts(
        amk,
        &core.file_id,
        &core.nonce_prefix,
        core.key_mode,
        core.wrapped_file_key.as_ref(),
    )
}

/// [`recover_file_key`] over the manifest fields it reads, for a reader that holds those
/// fields but not the manifest: the share viewer, which is served an asset's key-free crypto
/// parameters and never its signed manifest (that would carry the device and account ids the
/// export strip removes). Same dispatch and the same fail-closed errors.
pub fn recover_file_key_parts(
    amk: &Amk,
    file_id: &Uuid,
    nonce_prefix: &[u8],
    key_mode: crate::crypto::provenance::KeyMode,
    wrapped_file_key: Option<&crate::crypto::provenance::WrappedFileKey>,
) -> Result<[u8; 32], CryptoError> {
    use crate::crypto::provenance::KeyMode;
    match key_mode {
        KeyMode::Derived => Ok(amk.derive_file_key(file_id, nonce_prefix)),
        KeyMode::Wrapped => {
            let wrapped = wrapped_file_key.ok_or(CryptoError::Malformed(
                "wrapped key mode without a wrapped_file_key",
            ))?;
            unseal_file_key(amk, file_id, &wrapped.0)
        }
    }
}

#[cfg(test)]
mod tests;
