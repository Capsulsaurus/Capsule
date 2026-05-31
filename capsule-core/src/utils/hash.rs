use std::{fs, io, path::Path};

use sha2::{Digest, Sha256};

/// SHA-256 hash of a byte slice as a 64-char lowercase hex string.
pub fn hash_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Get SHA-256 hash of a file as a 64-char lowercase hex string.
// TODO: switch to streaming version for large files
pub fn get_file_hash(path: &Path) -> io::Result<String> {
    let bytes = fs::read(path)?;
    Ok(hash_bytes(&bytes))
}
