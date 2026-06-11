use std::fs::File;
use std::io;
use std::path::Path;

use crate::crypto::hash::{hash_bytes as hash32_bytes, hash_reader};

/// SHA-256 hash of a byte slice as a 64-char lowercase hex string.
pub fn hash_bytes(bytes: &[u8]) -> String {
    hash32_bytes(bytes).to_hex()
}

/// SHA-256 hash of a file as a 64-char lowercase hex string.
///
/// Streams the file in fixed blocks via [`crate::crypto::hash`] rather than reading the
/// whole file into memory, so arbitrarily large originals hash with bounded memory.
pub fn get_file_hash(path: &Path) -> io::Result<String> {
    let file = File::open(path)?;
    Ok(hash_reader(io::BufReader::new(file))?.to_hex())
}
