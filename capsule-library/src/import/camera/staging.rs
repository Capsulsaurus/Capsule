//! Where received camera objects are staged on local disk.
//!
//! ```text
//! <staging root>/
//!   objects/<storage:08x>/<parent:08x>/<camera file name>   complete, verified objects
//!   partial/<storage:08x>-<handle:08x>-<key>.part           objects still arriving
//! ```
//!
//! Complete objects keep the camera's own file name under a directory per camera folder, so a
//! RAW+JPEG pair (`DSC00001.ARW`, `DSC00001.JPG`) lands side by side and the scanner's stem
//! grouping stacks it exactly as it would from a card reader. Two DCIM folders can each hold a
//! `DSC00001.JPG`; the parent folder's handle keeps them apart.
//!
//! A partial file's name carries a key derived from the object's parent, name, size and capture
//! date as well as its handle. PTP promises handles only within a session, so a resumed transfer
//! continues a partial file only when every one of those still matches the object it is about to
//! read — a handle the camera reassigned to another object starts a fresh file.

use std::path::{Path, PathBuf};

use capsule_core::crypto::hash::hash_bytes;

use super::CameraObject;

/// The directory holding complete objects: the root the import pipeline scans.
pub(crate) fn objects_dir(root: &Path) -> PathBuf {
    root.join("objects")
}

/// The directory holding objects still arriving. Never scanned.
pub(crate) fn partial_dir(root: &Path) -> PathBuf {
    root.join("partial")
}

/// Where `object` lives once it is complete and verified.
pub(crate) fn object_path(root: &Path, object: &CameraObject) -> PathBuf {
    let name = safe_file_name(&object.filename)
        .map_or_else(|| format!("object-{:08x}", object.handle), str::to_owned);
    objects_dir(root)
        .join(format!("{:08x}", object.storage))
        .join(format!("{:08x}", object.parent))
        .join(name)
}

/// Where `object`'s bytes accumulate while it is arriving.
pub(crate) fn partial_path(root: &Path, object: &CameraObject) -> PathBuf {
    let identity = format!(
        "{}\u{0}{}\u{0}{:?}\u{0}{:?}",
        object.parent, object.filename, object.size, object.captured_at
    );
    let key = &hash_bytes(identity.as_bytes()).to_hex()[..16];
    partial_dir(root).join(format!(
        "{:08x}-{:08x}-{key}.part",
        object.storage, object.handle
    ))
}

/// The camera's file name, if it is safe to use as one path component: non-empty, not `.` or
/// `..`, and free of separators, NUL and other control characters. A camera is an untrusted
/// peer; a name that fails is replaced by one derived from the handle rather than trusted.
pub(crate) fn safe_file_name(name: &str) -> Option<&str> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed == "."
        || trimmed == ".."
        || trimmed.len() > 255
        || name
            .chars()
            .any(|c| c == '/' || c == '\\' || c.is_control())
    {
        return None;
    }
    Some(name)
}

#[cfg(test)]
mod tests;
