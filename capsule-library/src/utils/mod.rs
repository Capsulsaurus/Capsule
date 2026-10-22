pub mod paths;

use std::io;
use std::path::Path;

use capsule_core::crypto::hash::{Hash32, hash_reader};

/// SHA-256 of a file's contents, streamed in fixed blocks.
///
/// Opens `path` and feeds it through [`hash_reader`] rather than reading the whole file
/// into memory, so arbitrarily large originals hash with bounded memory. The digest itself is
/// `capsule-core`'s; only the file handle is the library's.
pub fn hash_file(path: &Path) -> io::Result<Hash32> {
    let file = std::fs::File::open(path)?;
    hash_reader(io::BufReader::new(file))
}

#[cfg(test)]
mod tests;
