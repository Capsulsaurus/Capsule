//! Content-addressed blob store addressing.
//!
//! The single source of the on-disk path a committed ciphertext blob lives at, so the
//! **writer** (upload finalization, slice `S-C1`) and every **reader** (storage
//! verification, slice `S-C3`; the media servers) reason over one layout and never fork
//! the addressing. A finalized blob is committed to `{upload_dir}/blobs/{hash}.bin`, where
//! `hash` is its lowercase-hex ciphertext content address.
//!
//! SSoT: the [blob store layout](../../../../capsule-docs/src/content/docs/design/filesystem/server.md).

use std::path::{Path, PathBuf};

/// A SHA-256 ciphertext content address is exactly 64 lowercase-hex characters.
pub const CONTENT_HASH_LEN: usize = 64;

/// Whether `hash` is a well-formed lowercase-hex ciphertext content address (64 hex chars).
///
/// The single shape check every reader validates before interpolating a hash into a path or
/// a query — a malformed address can address no committed blob, so callers treat a `false`
/// here as "unknown content address" rather than a server error.
#[must_use]
pub fn is_content_hash(hash: &str) -> bool {
    hash.len() == CONTENT_HASH_LEN
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The content-addressed blob store directory under `upload_dir`.
#[must_use]
pub fn blobs_dir(upload_dir: &Path) -> PathBuf {
    upload_dir.join("blobs")
}

/// The content-addressed path a finalized blob is committed to (`blobs/{hash}.bin`).
///
/// `hash` is the blob's lowercase-hex ciphertext content address; the caller is
/// responsible for having validated its shape (the address is never interpolated from
/// untrusted input without a hex/length check upstream).
#[must_use]
pub fn blob_path(upload_dir: &Path, hash: &str) -> PathBuf {
    blobs_dir(upload_dir).join(format!("{hash}.bin"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_path_is_content_addressed_under_blobs_dir() {
        let root = Path::new("/srv/capsule/upload");
        let hash = "a".repeat(64);
        assert_eq!(blobs_dir(root), Path::new("/srv/capsule/upload/blobs"));
        assert_eq!(
            blob_path(root, &hash),
            blobs_dir(root).join(format!("{hash}.bin")),
        );
    }
}
