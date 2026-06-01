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
    let mut buf = [0u8; 64 * 1024];
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
mod tests {
    use super::*;

    // NIST FIPS 180-2 / RFC 6234 known-answer vectors.
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    // SHA-256 of one million 'a' characters.
    const MILLION_A: &str = "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0";

    #[test]
    fn kat_abc() {
        assert_eq!(hash_bytes(b"abc").to_hex(), ABC);
    }

    #[test]
    fn kat_empty() {
        assert_eq!(hash_bytes(b"").to_hex(), EMPTY);
    }

    #[test]
    fn kat_million_a() {
        let data = vec![b'a'; 1_000_000];
        assert_eq!(hash_bytes(&data).to_hex(), MILLION_A);
    }

    #[test]
    fn chunked_equals_one_shot() {
        let data = vec![0xABu8; 65_520 * 3 + 17];
        let one_shot = hash_bytes(&data);

        // Feed in irregular pieces to prove order/chunking does not affect the digest.
        let mut h = Sha256Hasher::new();
        for chunk in data.chunks(65_520) {
            h.update(chunk);
        }
        assert_eq!(h.finalize(), one_shot);

        // And via the reader path.
        assert_eq!(hash_reader(&data[..]).unwrap(), one_shot);
    }

    #[test]
    fn hex_round_trip() {
        let h = hash_bytes(b"capsule");
        let parsed = Hash32::from_hex(&h.to_hex()).unwrap();
        assert_eq!(h, parsed);
        assert_eq!(parsed.as_bytes(), h.as_bytes());
    }

    #[test]
    fn from_hex_rejects_bad_length() {
        assert_eq!(Hash32::from_hex("dead"), Err(FromHexError));
        assert_eq!(
            Hash32::from_hex("zz".repeat(32).as_str()),
            Err(FromHexError)
        );
    }

    #[test]
    fn serde_emits_cbor_byte_string() {
        // Major type 2 (byte string) of length 32 has initial byte 0x58 0x20.
        let h = hash_bytes(b"abc");
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&h, &mut buf).unwrap();
        assert_eq!(buf[0], 0x58, "expected CBOR byte-string major type");
        assert_eq!(buf[1], 0x20, "expected length 32");
        assert_eq!(&buf[2..], h.as_bytes());

        let decoded: Hash32 = ciborium::de::from_reader(buf.as_slice()).unwrap();
        assert_eq!(decoded, h);
    }
}
