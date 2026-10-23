//! SHA-256 content hashing — the one hash algorithm Capsule uses everywhere
//! (content addressing, integrity verification, provenance chaining).
//!
//! SSoT: [Cryptography — Primitives § Cryptographic Hash]. The same SHA-256 value is
//! reused across layers rather than recomputed — the content-addressing hash is the
//! value the signed manifest commits to and the upload protocol declares and verifies.
//!
//! Hashing is **streaming** (incremental over chunks): the ciphertext hash of a large
//! asset is computed as the STREAM chunks are produced, never by buffering the whole
//! file. [`Sha256Hasher`] is the incremental interface; [`hash_bytes`] / [`hash_reader`]
//! are the one-shot conveniences built on top of it.
//!
//! [Cryptography — Primitives § Cryptographic Hash]: https://docs/design/cryptography/primitives/#cryptographic-hash

use std::io::{self, Read};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use sha2::{Digest, Sha256};

/// Length of a SHA-256 digest in bytes. Pinned by the `crypto_suite_id` inventory.
pub const SHA256_LEN: usize = 32;

/// A 32-byte SHA-256 digest.
///
/// Serializes as a CBOR **byte string** (major type 2), matching the `hash: bytes`
/// fields in the manifest and sidecar schemas — never as an array of integers — so
/// canonical encodings are byte-identical across implementations.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Hash32(pub [u8; SHA256_LEN]);

impl Hash32 {
    /// Wrap raw digest bytes.
    #[inline]
    pub const fn from_bytes(bytes: [u8; SHA256_LEN]) -> Self {
        Self(bytes)
    }

    /// Borrow the raw digest bytes.
    #[inline]
    pub const fn as_bytes(&self) -> &[u8; SHA256_LEN] {
        &self.0
    }

    /// Lowercase hex encoding (64 chars).
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Parse a 64-char lowercase/uppercase hex string into a digest.
    pub fn from_hex(s: &str) -> Result<Self, FromHexError> {
        let v = hex::decode(s).map_err(|_| FromHexError)?;
        let arr: [u8; SHA256_LEN] = v.as_slice().try_into().map_err(|_| FromHexError)?;
        Ok(Self(arr))
    }
}

/// The provided string was not a valid 32-byte hex digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FromHexError;

impl std::fmt::Display for FromHexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid 32-byte hex digest")
    }
}
impl std::error::Error for FromHexError {}

impl std::fmt::Debug for Hash32 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Hash32({})", self.to_hex())
    }
}

impl std::fmt::Display for Hash32 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl Serialize for Hash32 {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        // CBOR byte string, not a sequence of integers.
        s.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for Hash32 {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> de::Visitor<'de> for V {
            type Value = Hash32;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a 32-byte SHA-256 digest")
            }
            fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<Hash32, E> {
                let arr: [u8; SHA256_LEN] = v
                    .try_into()
                    .map_err(|_| E::invalid_length(v.len(), &"32 bytes"))?;
                Ok(Hash32(arr))
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Hash32, A::Error> {
                // Tolerate decoders that surface a byte string as a sequence.
                let mut arr = [0u8; SHA256_LEN];
                for (i, slot) in arr.iter_mut().enumerate() {
                    *slot = seq
                        .next_element()?
                        .ok_or_else(|| de::Error::invalid_length(i, &"32 bytes"))?;
                }
                Ok(Hash32(arr))
            }
        }
        d.deserialize_bytes(V)
    }
}

/// Incremental SHA-256 hasher: feed bytes with [`update`](Self::update), then
/// [`finalize`](Self::finalize). Used to hash STREAM ciphertext as chunks are produced.
#[derive(Clone, Default)]
pub struct Sha256Hasher {
    inner: Sha256,
}

impl Sha256Hasher {
    /// A fresh hasher over the empty input.
    pub fn new() -> Self {
        Self::default()
    }

    /// Absorb a chunk of input.
    #[inline]
    pub fn update(&mut self, bytes: &[u8]) {
        self.inner.update(bytes);
    }

    /// Consume the hasher and produce the digest.
    pub fn finalize(self) -> Hash32 {
        Hash32(self.inner.finalize().into())
    }
}

/// One-shot SHA-256 of a byte slice.
pub fn hash_bytes(bytes: &[u8]) -> Hash32 {
    let mut h = Sha256Hasher::new();
    h.update(bytes);
    h.finalize()
}

/// Stream a reader to completion, hashing in 64 KiB blocks without buffering the whole input.
pub fn hash_reader<R: Read>(mut reader: R) -> io::Result<Hash32> {
    let mut hasher = Sha256Hasher::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize())
}

#[cfg(test)]
mod tests;
