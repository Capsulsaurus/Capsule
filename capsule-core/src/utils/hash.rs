use std::{fs, io, path::Path};

/// Get BLAKE3 hash of a file as a 64-char lowercase hex string.
// TODO: switch to streaming version for large files
pub fn get_file_hash(path: &Path) -> io::Result<String> {
    let bytes = fs::read(path)?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}
