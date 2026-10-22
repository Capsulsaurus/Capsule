//! File-path EXIF reading over `capsule-core`'s byte-level extractor.
//!
//! The parse is [`extract_exif_from_bytes`], which is pure; this module owns only the read of the
//! file, and re-exports the rest of `capsule_core::exif` so callers name one path.

use std::path::Path;

pub use capsule_core::exif::*;

/// Read `path` and extract its EXIF fields.
///
/// An unreadable file is an error; a readable file that holds no EXIF container is an all-`None`
/// [`ExifExtract`], exactly as [`extract_exif_from_bytes`] reports it.
pub fn extract_exif(path: &Path) -> Result<ExifExtract, Box<dyn std::error::Error + Send + Sync>> {
    let bytes = std::fs::read(path)?;
    Ok(extract_exif_from_bytes(&bytes))
}

#[cfg(test)]
mod tests;
