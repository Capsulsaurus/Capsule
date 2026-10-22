//! Re-keying on rewrite — the writers that fold a fresh nonce into the key salt so a
//! same-epoch rewrite re-rolls **both** the key and the nonce, and refuse to reuse the
//! nonce of the record they supersede.
//!
//! A [`replace`] keeps a file's `file_id` and can land in the *same* AMK epoch as the bytes
//! it supersedes; a `metadata-update` keeps a blob's `blob_id`. Because the fresh
//! `nonce_prefix` / blob `nonce` is folded into the key salt
//! ([`Amk::derive_file_key`] / [`Amk::derive_blob_key`]), re-encrypting byte-identical
//! plaintext under the same id and epoch yields a different key, a different nonce, and
//! different ciphertext — so AES-GCM nonce reuse is structurally impossible.
//!
//! These writers **couple the nonce draw to derivation** (the salt depends on the nonce, so
//! the two cannot be separated) and add the defense-in-depth check that a freshly drawn
//! nonce never equals the one being replaced (on top of the CSPRNG draw). A first write
//! passes `replaces = None`; a rewrite passes the superseded nonce. SSoT: [Encryption §
//! Re-keying on Rewrite].
//!
//! [`replace`]: https://docs/design/cryptography/encryption/#re-keying-on-rewrite
//! [Encryption § Re-keying on Rewrite]: https://docs/design/cryptography/encryption/#re-keying-on-rewrite

use std::io::{Read, Write};

use thiserror::Error;
use uuid::Uuid;

use super::blob::{NONCE_LEN, seal_blob_with_nonce};
use super::stream::{AssetEncryption, NONCE_PREFIX_LEN, StreamError, encrypt_asset_with_prefix};
use crate::crypto::keys::Amk;
use crate::crypto::{CryptoError, rng};

/// Encrypt `plaintext` as a first or re-rolled asset write under `amk` scoped to `file_id`:
/// draw a fresh `nonce_prefix`, fold it into the file-key salt, and STREAM-encrypt.
///
/// `replaces` is the `nonce_prefix` of the version being superseded (a same-epoch
/// [`replace`]), if any; the writer refuses to emit that exact prefix again. Returns the
/// [`AssetEncryption`] metadata, the ciphertext, and the derived file key.
///
/// [`replace`]: https://docs/design/cryptography/encryption/#re-keying-on-rewrite
pub fn encrypt_asset_rekey(
    amk: &Amk,
    file_id: &Uuid,
    plaintext: &[u8],
    replaces: Option<[u8; NONCE_PREFIX_LEN]>,
) -> Result<(AssetEncryption, Vec<u8>, [u8; 32]), CryptoError> {
    encrypt_asset_rekey_with_prefix(
        amk,
        file_id,
        plaintext,
        rng::random_array::<NONCE_PREFIX_LEN>(),
        replaces,
    )
}

/// [`encrypt_asset_rekey`] with an explicit `nonce_prefix` — the deterministic, testable
/// core of the fold and the reuse refusal.
pub fn encrypt_asset_rekey_with_prefix(
    amk: &Amk,
    file_id: &Uuid,
    plaintext: &[u8],
    nonce_prefix: [u8; NONCE_PREFIX_LEN],
    replaces: Option<[u8; NONCE_PREFIX_LEN]>,
) -> Result<(AssetEncryption, Vec<u8>, [u8; 32]), CryptoError> {
    let mut ciphertext = Vec::new();
    match encrypt_asset_rekey_stream_with_prefix(
        amk,
        file_id,
        plaintext,
        &mut ciphertext,
        nonce_prefix,
        replaces,
    ) {
        Ok((enc, file_key)) => Ok((enc, ciphertext, file_key)),
        Err(RekeyStreamError::Crypto(error)) => Err(error),
        // A slice reader and a `Vec` writer cannot fail, and every chunk is under 64 KiB, so
        // AES-GCM cannot refuse one either.
        Err(RekeyStreamError::Stream(error)) => {
            unreachable!("in-memory STREAM encryption is infallible: {error}")
        }
    }
}

/// Why a streaming re-keyed write failed: the reuse refusal, or the reader or writer.
#[derive(Debug, Error)]
pub enum RekeyStreamError {
    /// The re-keying refusal ([`CryptoError::NonceReuse`]), raised before any byte is read.
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    /// The plaintext reader or the ciphertext writer failed part-way.
    #[error(transparent)]
    Stream(#[from] StreamError),
}

/// [`encrypt_asset_rekey`] from a reader to a writer, one STREAM chunk at a time: the same
/// prefix draw, key fold and reuse refusal, without the plaintext or the ciphertext ever
/// being whole in memory. Returns the [`AssetEncryption`] metadata (whose `ciphertext_hash` is
/// computed as the chunks pass) and the derived file key.
///
/// This is how an original of any size is imported: the writer may be [`std::io::sink`],
/// since the ciphertext is re-derivable from the plaintext and the recorded prefix.
pub fn encrypt_asset_rekey_stream<R: Read, W: Write>(
    amk: &Amk,
    file_id: &Uuid,
    reader: R,
    writer: W,
    replaces: Option<[u8; NONCE_PREFIX_LEN]>,
) -> Result<(AssetEncryption, [u8; 32]), RekeyStreamError> {
    encrypt_asset_rekey_stream_with_prefix(
        amk,
        file_id,
        reader,
        writer,
        rng::random_array::<NONCE_PREFIX_LEN>(),
        replaces,
    )
}

/// [`encrypt_asset_rekey_stream`] with an explicit `nonce_prefix` — the one implementation of
/// the fold and the reuse refusal, which [`encrypt_asset_rekey_with_prefix`] drives over a
/// slice, so the two cannot produce different bytes.
pub fn encrypt_asset_rekey_stream_with_prefix<R: Read, W: Write>(
    amk: &Amk,
    file_id: &Uuid,
    reader: R,
    writer: W,
    nonce_prefix: [u8; NONCE_PREFIX_LEN],
    replaces: Option<[u8; NONCE_PREFIX_LEN]>,
) -> Result<(AssetEncryption, [u8; 32]), RekeyStreamError> {
    if replaces == Some(nonce_prefix) {
        return Err(CryptoError::NonceReuse.into());
    }
    let file_key = amk.derive_file_key(file_id, &nonce_prefix);
    let enc = encrypt_asset_with_prefix(&file_key, nonce_prefix, reader, writer)?;
    Ok((enc, file_key))
}

/// Seal `plaintext` (canonical CBOR sidecar) into a metadata blob under `amk` scoped to
/// `blob_id`: draw a fresh blob `nonce`, fold it into the blob-key salt, and AEAD-seal.
///
/// `replaces` is the prior blob's `nonce` on a `metadata-update`, refused if drawn again.
/// Returns the wire bytes and the derived blob key (for the immediate binding self-check).
pub fn seal_metadata_blob(
    amk: &Amk,
    blob_id: &Uuid,
    plaintext: &[u8],
    replaces: Option<[u8; NONCE_LEN]>,
) -> Result<(Vec<u8>, [u8; 32]), CryptoError> {
    seal_metadata_blob_with_nonce(
        amk,
        blob_id,
        plaintext,
        rng::random_array::<NONCE_LEN>(),
        replaces,
    )
}

/// [`seal_metadata_blob`] with an explicit `nonce` — the deterministic, testable core.
pub fn seal_metadata_blob_with_nonce(
    amk: &Amk,
    blob_id: &Uuid,
    plaintext: &[u8],
    nonce: [u8; NONCE_LEN],
    replaces: Option<[u8; NONCE_LEN]>,
) -> Result<(Vec<u8>, [u8; 32]), CryptoError> {
    if replaces == Some(nonce) {
        return Err(CryptoError::NonceReuse);
    }
    let blob_key = amk.derive_blob_key(blob_id, &nonce);
    let wire = seal_blob_with_nonce(&blob_key, nonce, plaintext);
    Ok((wire, blob_key))
}

#[cfg(test)]
mod tests;
