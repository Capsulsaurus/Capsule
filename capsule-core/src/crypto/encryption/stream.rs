//! Authenticated asset encryption with the AES-256-GCM **STREAM** construction
//! (Hoang-Reyhanitabar-Rogaway-Vizár 2015), via RustCrypto `aead::stream`.
//!
//! Plaintext is split into 65,520-byte chunks, each sealed with AES-256-GCM under a
//! structured nonce `prefix(7) ‖ counter_be32(4) ‖ last_flag(1)`, producing 64 KiB
//! ciphertext chunks (16-byte tag each). STREAM detects truncation, reordering, and chunk
//! deletion. Because each chunk's nonce is derived deterministically from its index, any
//! chunk decrypts **independently** ([`decrypt_chunk`]) for ranged reads.
//!
//! A fresh per-file key (see [`crate::crypto::keys::Amk::derive_file_key`]) lets the
//! counter safely start at zero. SSoT: [Cryptography — Encryption § STREAM Construction].
//!
//! [Cryptography — Encryption § STREAM Construction]: https://docs/design/cryptography/encryption/#stream-construction

use std::io::{self, Read, Write};

use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::aead::stream::{DecryptorBE32, EncryptorBE32};
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use thiserror::Error;

use crate::crypto::hash::{Hash32, Sha256Hasher};
use crate::crypto::rng;

/// Plaintext bytes per STREAM chunk.
pub const PLAINTEXT_CHUNK: usize = 65_520;
/// Ciphertext bytes per full STREAM chunk (plaintext chunk + 16-byte GCM tag = 64 KiB).
pub const CIPHERTEXT_CHUNK: usize = PLAINTEXT_CHUNK + TAG_LEN;
/// STREAM nonce prefix length (12-byte nonce − 4-byte counter − 1-byte last flag).
pub const NONCE_PREFIX_LEN: usize = 7;
/// GCM authentication tag length.
pub const TAG_LEN: usize = 16;

/// Result of encrypting an asset: everything the [signed manifest] commits to.
///
/// [signed manifest]: crate::crypto::provenance
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetEncryption {
    /// SHA-256 over the full produced ciphertext (the content address).
    pub ciphertext_hash: Hash32,
    /// Total plaintext byte length.
    pub plaintext_size: u64,
    /// Plaintext bytes per chunk (always [`PLAINTEXT_CHUNK`]).
    pub chunk_size: u32,
    /// The random STREAM nonce prefix for this file.
    pub nonce_prefix: [u8; NONCE_PREFIX_LEN],
}

/// Errors from streaming encrypt/decrypt.
#[derive(Debug, Error)]
pub enum StreamError {
    /// Underlying reader/writer I/O failure.
    #[error("stream io error: {0}")]
    Io(#[from] io::Error),
    /// A chunk failed AEAD authentication (tamper, wrong key, truncation, reorder).
    #[error("stream authentication failed at chunk {0}")]
    Auth(u32),
}

fn read_chunk<R: Read>(reader: &mut R, n: usize) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    let mut filled = 0;
    while filled < n {
        let k = reader.read(&mut buf[filled..])?;
        if k == 0 {
            break;
        }
        filled += k;
    }
    buf.truncate(filled);
    Ok(buf)
}

/// Encrypt `reader` to `writer` under `file_key`, returning the content metadata.
/// Computes the ciphertext hash incrementally — the whole file is never buffered.
pub fn encrypt_asset<R: Read, W: Write>(
    file_key: &[u8; 32],
    mut reader: R,
    mut writer: W,
) -> Result<AssetEncryption, StreamError> {
    let nonce_prefix = rng::random_array::<NONCE_PREFIX_LEN>();
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(file_key));
    let mut enc =
        EncryptorBE32::<Aes256Gcm>::from_aead(cipher, GenericArray::from_slice(&nonce_prefix));

    let mut hasher = Sha256Hasher::new();
    let mut plaintext_size: u64 = 0;
    let mut index: u32 = 0;

    // Read one chunk ahead so we know which chunk is the last (it gets the last-block flag).
    let mut cur = read_chunk(&mut reader, PLAINTEXT_CHUNK)?;
    loop {
        let next = read_chunk(&mut reader, PLAINTEXT_CHUNK)?;
        plaintext_size += cur.len() as u64;
        if next.is_empty() {
            let ct = enc
                .encrypt_last(cur.as_slice())
                .map_err(|_| StreamError::Auth(index))?;
            hasher.update(&ct);
            writer.write_all(&ct)?;
            break;
        }
        let ct = enc
            .encrypt_next(cur.as_slice())
            .map_err(|_| StreamError::Auth(index))?;
        hasher.update(&ct);
        writer.write_all(&ct)?;
        cur = next;
        index += 1;
    }

    Ok(AssetEncryption {
        ciphertext_hash: hasher.finalize(),
        plaintext_size,
        chunk_size: PLAINTEXT_CHUNK as u32,
        nonce_prefix,
    })
}

/// Decrypt a STREAM ciphertext from `reader` to `writer`. Every chunk's tag is verified;
/// any tamper/truncation/reorder yields [`StreamError::Auth`].
pub fn decrypt_asset<R: Read, W: Write>(
    file_key: &[u8; 32],
    nonce_prefix: &[u8; NONCE_PREFIX_LEN],
    mut reader: R,
    mut writer: W,
) -> Result<(), StreamError> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(file_key));
    let mut dec =
        DecryptorBE32::<Aes256Gcm>::from_aead(cipher, GenericArray::from_slice(nonce_prefix));

    let mut index: u32 = 0;
    let mut cur = read_chunk(&mut reader, CIPHERTEXT_CHUNK)?;
    loop {
        let next = read_chunk(&mut reader, CIPHERTEXT_CHUNK)?;
        if next.is_empty() {
            let pt = dec
                .decrypt_last(cur.as_slice())
                .map_err(|_| StreamError::Auth(index))?;
            writer.write_all(&pt)?;
            break;
        }
        let pt = dec
            .decrypt_next(cur.as_slice())
            .map_err(|_| StreamError::Auth(index))?;
        writer.write_all(&pt)?;
        cur = next;
        index += 1;
    }
    Ok(())
}

/// Decrypt a single ciphertext chunk in isolation (ranged read). `index` is the 0-based
/// chunk number; `is_last` must be true only for the final chunk of the file (it carries
/// the STREAM last-block flag). Returns [`crate::crypto::CryptoError::Auth`] on tamper.
pub fn decrypt_chunk(
    file_key: &[u8; 32],
    nonce_prefix: &[u8; NONCE_PREFIX_LEN],
    index: u32,
    is_last: bool,
    ct_chunk: &[u8],
) -> Result<Vec<u8>, crate::crypto::CryptoError> {
    let mut nonce = [0u8; 12];
    nonce[..NONCE_PREFIX_LEN].copy_from_slice(nonce_prefix);
    nonce[NONCE_PREFIX_LEN..11].copy_from_slice(&index.to_be_bytes());
    nonce[11] = u8::from(is_last);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(file_key));
    cipher
        .decrypt(Nonce::from_slice(&nonce), ct_chunk)
        .map_err(|_| crate::crypto::CryptoError::Auth("STREAM chunk authentication failed"))
}

/// Convenience: encrypt a whole slice in memory, returning (metadata, ciphertext).
pub fn encrypt_asset_vec(file_key: &[u8; 32], plaintext: &[u8]) -> AssetEncryption {
    let mut out = Vec::new();
    // Slice reader + Vec writer are infallible, so the only error path is unreachable.
    encrypt_asset(file_key, plaintext, &mut out).expect("in-memory encryption is infallible")
}

/// Convenience: encrypt a whole slice in memory and also return the ciphertext bytes.
pub fn encrypt_asset_vec_full(file_key: &[u8; 32], plaintext: &[u8]) -> (AssetEncryption, Vec<u8>) {
    let mut out = Vec::new();
    let meta = encrypt_asset(file_key, plaintext, &mut out).expect("in-memory encryption");
    (meta, out)
}

/// Convenience: decrypt a whole ciphertext slice in memory.
pub fn decrypt_asset_vec(
    file_key: &[u8; 32],
    nonce_prefix: &[u8; NONCE_PREFIX_LEN],
    ciphertext: &[u8],
) -> Result<Vec<u8>, StreamError> {
    let mut out = Vec::new();
    decrypt_asset(file_key, nonce_prefix, ciphertext, &mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [0x11; 32];

    fn round_trip(plaintext: &[u8]) {
        let (meta, ct) = encrypt_asset_vec_full(&KEY, plaintext);
        assert_eq!(meta.plaintext_size, plaintext.len() as u64);
        assert_eq!(meta.chunk_size, PLAINTEXT_CHUNK as u32);
        let back = decrypt_asset_vec(&KEY, &meta.nonce_prefix, &ct).unwrap();
        assert_eq!(back, plaintext);
        // Content hash matches a fresh hash of the ciphertext.
        assert_eq!(meta.ciphertext_hash, crate::crypto::hash::hash_bytes(&ct));
    }

    #[test]
    fn round_trip_various_sizes() {
        round_trip(b""); // empty
        round_trip(b"hello"); // < 1 chunk
        round_trip(&[0xABu8; PLAINTEXT_CHUNK]); // exactly one chunk
        round_trip(&[0xCDu8; PLAINTEXT_CHUNK + 1]); // one chunk + 1
        round_trip(&[0x77u8; PLAINTEXT_CHUNK * 3 + 123]); // multi-chunk, partial last
    }

    #[test]
    fn full_chunk_ciphertext_is_64_kib() {
        let (_, ct) = encrypt_asset_vec_full(&KEY, &[0u8; PLAINTEXT_CHUNK * 2]);
        // Two full plaintext chunks → two 64 KiB ciphertext chunks.
        assert_eq!(ct.len(), CIPHERTEXT_CHUNK * 2);
    }

    #[test]
    fn ranged_read_matches_sequential() {
        let plaintext: Vec<u8> = (0..(PLAINTEXT_CHUNK * 3 + 500))
            .map(|i| (i % 251) as u8)
            .collect();
        let (meta, ct) = encrypt_asset_vec_full(&KEY, &plaintext);
        let n_chunks = ct.len().div_ceil(CIPHERTEXT_CHUNK);

        for i in 0..n_chunks {
            let start = i * CIPHERTEXT_CHUNK;
            let end = (start + CIPHERTEXT_CHUNK).min(ct.len());
            let is_last = i == n_chunks - 1;
            let pt = decrypt_chunk(&KEY, &meta.nonce_prefix, i as u32, is_last, &ct[start..end])
                .unwrap();

            let p_start = i * PLAINTEXT_CHUNK;
            let p_end = (p_start + PLAINTEXT_CHUNK).min(plaintext.len());
            assert_eq!(pt, &plaintext[p_start..p_end], "chunk {i} mismatch");
        }
    }

    #[test]
    fn tamper_in_each_chunk_is_detected() {
        let plaintext = [0x5Au8; PLAINTEXT_CHUNK * 2 + 10];
        let (meta, ct) = encrypt_asset_vec_full(&KEY, &plaintext);

        // Bit-flip in the first chunk.
        let mut t = ct.clone();
        t[10] ^= 0x01;
        assert!(decrypt_asset_vec(&KEY, &meta.nonce_prefix, &t).is_err());

        // Bit-flip in the last chunk.
        let mut t = ct.clone();
        let last = t.len() - 1;
        t[last] ^= 0x01;
        assert!(decrypt_asset_vec(&KEY, &meta.nonce_prefix, &t).is_err());
    }

    #[test]
    fn chunk_reorder_and_drop_are_detected() {
        let plaintext = [0x33u8; PLAINTEXT_CHUNK * 2];
        let (meta, ct) = encrypt_asset_vec_full(&KEY, &plaintext);

        // Swap the two chunks (reorder) — STREAM counter mismatch.
        let mut swapped = Vec::new();
        swapped.extend_from_slice(&ct[CIPHERTEXT_CHUNK..]);
        swapped.extend_from_slice(&ct[..CIPHERTEXT_CHUNK]);
        assert!(decrypt_asset_vec(&KEY, &meta.nonce_prefix, &swapped).is_err());

        // Drop the final chunk — last-block flag never seen.
        assert!(decrypt_asset_vec(&KEY, &meta.nonce_prefix, &ct[..CIPHERTEXT_CHUNK]).is_err());
    }

    #[test]
    fn wrong_key_or_prefix_is_rejected() {
        let (meta, ct) = encrypt_asset_vec_full(&KEY, b"secret bytes");
        assert!(decrypt_asset_vec(&[0x22; 32], &meta.nonce_prefix, &ct).is_err());
        let mut bad_prefix = meta.nonce_prefix;
        bad_prefix[0] ^= 0xff;
        assert!(decrypt_asset_vec(&KEY, &bad_prefix, &ct).is_err());
    }

    #[test]
    fn distinct_files_get_distinct_prefixes() {
        let a = encrypt_asset_vec(&KEY, b"x");
        let b = encrypt_asset_vec(&KEY, b"x");
        assert_ne!(a.nonce_prefix, b.nonce_prefix);
    }
}
