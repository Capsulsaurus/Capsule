//! File-path EXIF reading over `capsule-core`'s reader-generic extractor.
//!
//! The parse is [`extract_exif_from_reader`], which takes any seekable reader; this module owns
//! only opening the file, and re-exports the rest of `capsule_core::exif` so callers name one
//! path.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

pub use capsule_core::exif::*;

/// Open `path` and extract its EXIF fields.
///
/// A file that cannot be opened is an error; a readable file that holds no EXIF container is an
/// all-`None` [`ExifExtract`], read only as far as the container sniff needs.
pub fn extract_exif(path: &Path) -> Result<ExifExtract, Box<dyn std::error::Error + Send + Sync>> {
    let mut reader = BufReader::new(File::open(path)?);
    Ok(extract_exif_from_reader(&mut reader))
}

#[cfg(test)]
mod tests;
