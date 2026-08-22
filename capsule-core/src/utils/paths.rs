//! Path helpers with no knowledge of the library layout.
//!
//! [`tmp_path`] lives here rather than in [`crate::library::paths`] because it encodes no
//! layout at all — it appends `.tmp` to whatever it is given. Keeping it here lets
//! [`crate::sidecar`] do atomic writes without importing `library`, which was the one real
//! `sidecar -> library` edge (the rest of that pair is rustdoc links).

use std::path::{Path, PathBuf};

/// Appends `.tmp` to any path.
///
/// The write-then-rename partner for atomic file replacement: write to `tmp_path(p)`, fsync,
/// then rename onto `p` so a reader never observes a half-written file.
#[must_use]
pub fn tmp_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmp_path_appends_the_suffix_without_touching_the_extension() {
        // Not `set_extension`: a sidecar is `{uuid}.cbor`, and its temp file must be
        // `{uuid}.cbor.tmp`, never `{uuid}.tmp` — otherwise two different sidecars for the
        // same asset would collide on one temp path.
        assert_eq!(
            tmp_path(Path::new("/a/b/c.cbor")),
            PathBuf::from("/a/b/c.cbor.tmp")
        );
    }

    #[test]
    fn tmp_path_handles_a_path_with_no_extension() {
        assert_eq!(tmp_path(Path::new("/a/b/c")), PathBuf::from("/a/b/c.tmp"));
    }
}
