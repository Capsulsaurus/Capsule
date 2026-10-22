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
//! **Bounded memory.** Nothing here needs a whole file in memory. [`encrypt_asset`] and
//! [`decrypt_asset`] run reader-to-writer one chunk at a time; [`CiphertextReader`] re-derives
//! any byte range of a ciphertext from its plaintext (a chunk's ciphertext depends only on its
//! own plaintext, its index and whether it is last), which is what lets an upload resume at any
//! offset without holding the ciphertext; and [`StreamDecryptor`] opens a ciphertext that
//! arrives in arbitrary pieces, holding at most two chunks. ADR-0012 records why originals keep
//! this format rather than a new segmented one.
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

/// The number of STREAM chunks a `plaintext_size`-byte plaintext encrypts to. An empty
/// plaintext is still one (empty, last) chunk: its ciphertext is a bare tag.
#[must_use]
pub const fn chunk_count(plaintext_size: u64) -> u64 {
    if plaintext_size == 0 {
        1
    } else {
        plaintext_size.div_ceil(PLAINTEXT_CHUNK as u64)
    }
}

/// The exact ciphertext length a `plaintext_size`-byte plaintext encrypts to: the plaintext
/// plus one [`TAG_LEN`] tag per chunk.
#[must_use]
pub const fn ciphertext_len(plaintext_size: u64) -> u64 {
    plaintext_size + chunk_count(plaintext_size) * TAG_LEN as u64
}

/// The STREAM-BE32 nonce of chunk `index`: `prefix(7) ‖ counter_be32(4) ‖ last_flag(1)`.
fn chunk_nonce(nonce_prefix: &[u8; NONCE_PREFIX_LEN], index: u32, is_last: bool) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[..NONCE_PREFIX_LEN].copy_from_slice(nonce_prefix);
    nonce[NONCE_PREFIX_LEN..11].copy_from_slice(&index.to_be_bytes());
    nonce[11] = u8::from(is_last);
    nonce
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
/// Draws a fresh random nonce prefix.
pub fn encrypt_asset<R: Read, W: Write>(
    file_key: &[u8; 32],
    reader: R,
    writer: W,
) -> Result<AssetEncryption, StreamError> {
    encrypt_asset_with_prefix(
        file_key,
        rng::random_array::<NONCE_PREFIX_LEN>(),
        reader,
        writer,
    )
}

/// As [`encrypt_asset`] but with an explicit nonce prefix, so a client can **regenerate the
/// exact ciphertext** from the plaintext + file key + the prefix recorded in the manifest
/// (the client stores plaintext locally; ciphertext is derived for upload/backup).
pub fn encrypt_asset_with_prefix<R: Read, W: Write>(
    file_key: &[u8; 32],
    nonce_prefix: [u8; NONCE_PREFIX_LEN],
    mut reader: R,
    mut writer: W,
) -> Result<AssetEncryption, StreamError> {
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
    let nonce = chunk_nonce(nonce_prefix, index, is_last);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(file_key));
    cipher
        .decrypt(Nonce::from_slice(&nonce), ct_chunk)
        .map_err(|_| crate::crypto::CryptoError::Auth("STREAM chunk authentication failed"))
}

/// Encrypt a single plaintext chunk in isolation: the inverse of [`decrypt_chunk`], and
/// byte-identical to chunk `index` of [`encrypt_asset_with_prefix`]'s output for the same key
/// and prefix. `pt_chunk` must be [`PLAINTEXT_CHUNK`] bytes unless `is_last`.
#[must_use]
pub fn encrypt_chunk(
    file_key: &[u8; 32],
    nonce_prefix: &[u8; NONCE_PREFIX_LEN],
    index: u32,
    is_last: bool,
    pt_chunk: &[u8],
) -> Vec<u8> {
    let nonce = chunk_nonce(nonce_prefix, index, is_last);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(file_key));
    cipher
        .encrypt(Nonce::from_slice(&nonce), pt_chunk)
        .expect("AES-GCM encryption of a chunk under 64 KiB cannot fail")
}

/// Random-access re-derivation of a STREAM ciphertext from its plaintext.
///
/// A chunk's ciphertext is a function of the key, the nonce prefix, the chunk's plaintext, its
/// index and whether it is the last chunk, so any byte range of the ciphertext can be produced
/// by reading and encrypting only the plaintext chunks that range covers. That is what lets an
/// upload send — and re-send, on a resume or a realign — any window of an original without the
/// ciphertext ever existing in memory or on disk.
///
/// `plaintext_size` is the size the signed manifest committed to. The reader never reads past
/// it, and a source shorter than it is an [`io::ErrorKind::UnexpectedEof`] rather than a short
/// ciphertext.
pub struct CiphertextReader<R> {
    file_key: [u8; 32],
    nonce_prefix: [u8; NONCE_PREFIX_LEN],
    plaintext: R,
    plaintext_size: u64,
}

impl<R> std::fmt::Debug for CiphertextReader<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CiphertextReader")
            .field("file_key", &"<redacted>")
            .field("plaintext_size", &self.plaintext_size)
            .finish_non_exhaustive()
    }
}

impl<R: Read + io::Seek> CiphertextReader<R> {
    /// A reader over `plaintext`, which holds `plaintext_size` bytes.
    pub fn new(
        file_key: [u8; 32],
        nonce_prefix: [u8; NONCE_PREFIX_LEN],
        plaintext: R,
        plaintext_size: u64,
    ) -> Self {
        Self {
            file_key,
            nonce_prefix,
            plaintext,
            plaintext_size,
        }
    }

    /// The ciphertext's total length ([`ciphertext_len`] of the plaintext size).
    #[must_use]
    pub fn ciphertext_size(&self) -> u64 {
        ciphertext_len(self.plaintext_size)
    }

    /// The ciphertext bytes `[offset, offset + len)`, clamped to the ciphertext's end. Memory is
    /// the returned window plus one chunk.
    pub fn read_at(&mut self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        let total = self.ciphertext_size();
        let end = offset.saturating_add(len as u64).min(total);
        if offset >= end {
            return Ok(Vec::new());
        }
        let mut out = Vec::with_capacity((end - offset) as usize);
        let last_index = chunk_count(self.plaintext_size) - 1;
        let first = offset / CIPHERTEXT_CHUNK as u64;
        let last = (end - 1) / CIPHERTEXT_CHUNK as u64;
        let mut pt = vec![0u8; PLAINTEXT_CHUNK];
        for index in first..=last {
            let pt_start = index * PLAINTEXT_CHUNK as u64;
            let pt_len = (self.plaintext_size - pt_start).min(PLAINTEXT_CHUNK as u64) as usize;
            self.plaintext.seek(io::SeekFrom::Start(pt_start))?;
            self.plaintext.read_exact(&mut pt[..pt_len])?;
            let counter = u32::try_from(index).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "STREAM chunk counter overflow")
            })?;
            let ct = encrypt_chunk(
                &self.file_key,
                &self.nonce_prefix,
                counter,
                index == last_index,
                &pt[..pt_len],
            );
            let ct_start = index * CIPHERTEXT_CHUNK as u64;
            let from = offset.saturating_sub(ct_start) as usize;
            let to = ((end - ct_start) as usize).min(ct.len());
            out.extend_from_slice(&ct[from..to]);
        }
        Ok(out)
    }
}

/// Incremental STREAM decryption of a ciphertext that arrives in pieces of any size.
///
/// [`update`](Self::update) buffers at most two ciphertext chunks: a chunk is opened as soon as
/// a byte past it has arrived (proving it is not the last), and [`finish`](Self::finish) opens
/// the final chunk under the last-block flag. Truncation, reordering, a chunk swapped in from
/// another file and a toggled last flag all fail authentication, and an empty ciphertext fails
/// at `finish`. After the first failure every later call fails too.
///
/// Plaintext reaches the inner writer chunk by chunk, each chunk authenticated before it is
/// written; the whole file is authentic only once `finish` returns `Ok`. A caller that writes
/// to a file must treat it as unverified, and discard it, until then.
pub struct StreamDecryptor<W> {
    decryptor: Option<DecryptorBE32<Aes256Gcm>>,
    pending: Vec<u8>,
    index: u32,
    inner: W,
}

impl<W> std::fmt::Debug for StreamDecryptor<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamDecryptor")
            .field("index", &self.index)
            .field("pending", &self.pending.len())
            .finish_non_exhaustive()
    }
}

impl<W: Write> StreamDecryptor<W> {
    /// A decryptor writing plaintext to `inner`.
    pub fn new(file_key: &[u8; 32], nonce_prefix: &[u8; NONCE_PREFIX_LEN], inner: W) -> Self {
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(file_key));
        Self {
            decryptor: Some(DecryptorBE32::from_aead(
                cipher,
                GenericArray::from_slice(nonce_prefix),
            )),
            pending: Vec::with_capacity(2 * CIPHERTEXT_CHUNK),
            index: 0,
            inner,
        }
    }

    /// Feed the next ciphertext bytes.
    pub fn update(&mut self, mut data: &[u8]) -> Result<(), StreamError> {
        while !data.is_empty() {
            let take = data.len().min(2 * CIPHERTEXT_CHUNK - self.pending.len());
            self.pending.extend_from_slice(&data[..take]);
            data = &data[take..];
            while self.pending.len() > CIPHERTEXT_CHUNK {
                let decryptor = self
                    .decryptor
                    .as_mut()
                    .ok_or(StreamError::Auth(self.index))?;
                let opened = decryptor.decrypt_next(&self.pending[..CIPHERTEXT_CHUNK]);
                let Ok(pt) = opened else {
                    self.decryptor = None;
                    return Err(StreamError::Auth(self.index));
                };
                self.inner.write_all(&pt)?;
                self.pending.drain(..CIPHERTEXT_CHUNK);
                self.index += 1;
            }
        }
        Ok(())
    }

    /// Open the final chunk and return the inner writer.
    pub fn finish(mut self) -> Result<W, StreamError> {
        let decryptor = self.decryptor.take().ok_or(StreamError::Auth(self.index))?;
        let pt = decryptor
            .decrypt_last(self.pending.as_slice())
            .map_err(|_| StreamError::Auth(self.index))?;
        self.inner.write_all(&pt)?;
        self.inner.flush()?;
        Ok(self.inner)
    }
}

/// `io::Write` over [`StreamDecryptor::update`], so the decryptor can be the sink of anything
/// that writes bytes. An authentication failure surfaces as [`io::ErrorKind::InvalidData`].
impl<W: Write> Write for StreamDecryptor<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.update(buf).map_err(|error| match error {
            StreamError::Io(io) => io,
            auth @ StreamError::Auth(_) => io::Error::new(io::ErrorKind::InvalidData, auth),
        })?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
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

/// Convenience: encrypt a whole slice in memory with an explicit prefix (deterministic).
pub fn encrypt_asset_vec_with_prefix(
    file_key: &[u8; 32],
    nonce_prefix: [u8; NONCE_PREFIX_LEN],
    plaintext: &[u8],
) -> (AssetEncryption, Vec<u8>) {
    let mut out = Vec::new();
    let meta = encrypt_asset_with_prefix(file_key, nonce_prefix, plaintext, &mut out)
        .expect("in-memory encryption");
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
mod tests;
