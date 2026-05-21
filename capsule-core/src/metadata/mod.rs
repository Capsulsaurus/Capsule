use std::io;
use std::path::Path;

use crate::constants::SIDECAR_EXTENSIONS;

mod file;
mod filter;
mod types;

pub use file::*;
pub use filter::*;
pub use types::*;

/// Detect if file is a sidecar file. Automatically returns false if it is not a
/// file.
pub fn is_sidecar_file(path: &Path) -> io::Result<bool> {
    if !path.is_file() {
        return Ok(false);
    }

    // Check if the file has a recognized sidecar extension
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();

    let is_sidecar = SIDECAR_EXTENSIONS.iter().any(|&e| e == ext);

    Ok(is_sidecar)
}
