use std::{
    fs::File,
    io::{self, Read},
    path::Path,
};

use data_encoding::HEXLOWER;
use sha2::{Digest, Sha256};

/// Read-buffer size for streamed file hashing.
const HASH_CHUNK_SIZE: usize = 64 * 1024;

/// Compute the SHA-256 hash of `bytes` as a 64-char lowercase hex string.
pub fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    HEXLOWER.encode(&hasher.finalize())
}

/// Compute the SHA-256 hash of a file as a 64-char lowercase hex string.
///
/// The file is streamed in fixed-size chunks so large media never has to be
/// held in memory in full. SHA-256 is hardware-accelerated on modern CPUs
/// (ARMv8 SHA-2 / x86 SHA-NI) via the `sha2` crate's runtime feature detection.
pub fn get_file_hash(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; HASH_CHUNK_SIZE];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(HEXLOWER.encode(&hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// SHA-256 of the empty input — RFC 6234 known-answer test.
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn test_hash_bytes_empty() {
        assert_eq!(hash_bytes(b""), EMPTY_SHA256);
    }

    #[test]
    fn test_hash_bytes_known_answer() {
        // SHA-256("abc") — FIPS 180-4 example.
        assert_eq!(
            hash_bytes(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn test_get_file_hash_matches_hash_bytes() {
        let mut file = NamedTempFile::new().unwrap();
        let content = b"capsule sha-256 streaming test content";
        file.write_all(content).unwrap();
        file.flush().unwrap();
        assert_eq!(get_file_hash(file.path()).unwrap(), hash_bytes(content));
    }

    #[test]
    fn test_get_file_hash_large_file_streams() {
        // A file spanning several read chunks must hash identically to the
        // single-shot in-memory path.
        let mut file = NamedTempFile::new().unwrap();
        let content = vec![0xABu8; HASH_CHUNK_SIZE * 3 + 17];
        file.write_all(&content).unwrap();
        file.flush().unwrap();
        assert_eq!(get_file_hash(file.path()).unwrap(), hash_bytes(&content));
    }
}
