//! Where received camera objects are staged on local disk.
//!
//! ```text
//! <staging root>/
//!   objects/<storage:08x>/<parent:08x>[~<n>]/<camera file name>   complete, verified objects
//!   identity/<storage:08x>/<parent:08x>[~<n>]/<file name>.id      which object each one is
//!   partial/<storage:08x>-<handle:08x>-<key>.part                 objects still arriving
//!   partial/<storage:08x>-<handle:08x>-<key>.part.ckpt            the prefix made durable
//! ```
//!
//! Complete objects keep the camera's own file name under a directory per camera folder, so a
//! RAW+JPEG pair (`DSC00001.ARW`, `DSC00001.JPG`) lands side by side and the scanner's stem
//! grouping stacks it exactly as it would from a card reader. Two DCIM folders can each hold a
//! `DSC00001.JPG`; the parent folder's handle keeps them apart.
//!
//! Every object has an [identity key](identity_key) over its parent, name, size and capture
//! date. A complete object's key is recorded beside it, outside the scanned tree, so a later run
//! takes a staged file as already received only for the same object. A reused staging root can
//! meet a different object under the same path (a reformatted card restarts its file counter);
//! that object goes to the next free *generation* of the folder directory (`<parent>~1`,
//! `<parent>~2`, ...), and the earlier file is never overwritten.
//!
//! A partial file's name carries the identity key as well as its handle. PTP promises handles
//! only within a session, so a resumed transfer continues a partial file only when every one of
//! those still matches the object it is about to read — a handle the camera reassigned to another
//! object starts a fresh file. Its checkpoint records the length and SHA-256 of the prefix last
//! synced to disk; a resume continues only from a prefix that still hashes to it.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use capsule_core::crypto::hash::hash_bytes;

use super::CameraObject;

/// The directory holding complete objects: the root the import pipeline scans.
pub(crate) fn objects_dir(root: &Path) -> PathBuf {
    root.join("objects")
}

/// The directory recording which object each complete file is. Never scanned.
pub(crate) fn identity_dir(root: &Path) -> PathBuf {
    root.join("identity")
}

/// The directory holding objects still arriving. Never scanned.
pub(crate) fn partial_dir(root: &Path) -> PathBuf {
    root.join("partial")
}

/// The identity of `object` across sessions: its parent folder, name, size and capture date,
/// hashed. The handle is left out of it, because a camera may renumber handles between sessions.
pub(crate) fn identity_key(object: &CameraObject) -> String {
    let identity = format!(
        "{}\u{0}{}\u{0}{:?}\u{0}{:?}",
        object.parent, object.filename, object.size, object.captured_at
    );
    hash_bytes(identity.as_bytes()).to_hex()
}

/// The path of `object` in folder `generation`, relative to the objects and identity directories.
fn relative_path(object: &CameraObject, generation: u32) -> PathBuf {
    let name = safe_file_name(&object.filename)
        .map_or_else(|| format!("object-{:08x}", object.handle), Cow::into_owned);
    let folder = if generation == 0 {
        format!("{:08x}", object.parent)
    } else {
        format!("{:08x}~{generation}", object.parent)
    };
    PathBuf::from(format!("{:08x}", object.storage))
        .join(folder)
        .join(name)
}

/// Where `object` lives once it is complete and verified, in folder `generation`: `0` unless an
/// earlier, different object already holds that path.
pub(crate) fn object_path(root: &Path, object: &CameraObject, generation: u32) -> PathBuf {
    objects_dir(root).join(relative_path(object, generation))
}

/// Where the identity key of the object at the same [`object_path`] is recorded.
pub(crate) fn identity_path(root: &Path, object: &CameraObject, generation: u32) -> PathBuf {
    let mut path = identity_dir(root)
        .join(relative_path(object, generation))
        .into_os_string();
    path.push(".id");
    PathBuf::from(path)
}

/// Where `object`'s bytes accumulate while it is arriving.
pub(crate) fn partial_path(root: &Path, object: &CameraObject) -> PathBuf {
    let identity = identity_key(object);
    let key = &identity[..16];
    partial_dir(root).join(format!(
        "{:08x}-{:08x}-{key}.part",
        object.storage, object.handle
    ))
}

/// Where the length and digest of `partial`'s durable prefix are recorded.
pub(crate) fn checkpoint_path(partial: &Path) -> PathBuf {
    let mut path = partial.as_os_str().to_owned();
    path.push(".ckpt");
    PathBuf::from(path)
}

/// Characters no Windows file name may hold, beyond separators and control characters.
const WINDOWS_RESERVED_CHARS: [char; 7] = [':', '*', '?', '"', '<', '>', '|'];

/// Device names Windows reserves, whatever extension follows them.
const WINDOWS_DEVICE_NAMES: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];

/// The camera's file name made safe to use as one path component on every platform, or `None`
/// where it cannot be.
///
/// A camera is an untrusted peer. A name that is empty, `.` or `..`, longer than 255 bytes, or
/// holds a separator, NUL or another control character is refused, and the caller names the file
/// after its handle instead. A name that is otherwise usable but invalid on Windows is repaired so
/// it keeps its extension: each of `: * ? " < > |` becomes `_`, trailing dots and spaces (which
/// Windows strips) become `_`, and a reserved device name (`CON`, `PRN`, `AUX`, `NUL`,
/// `COM1`-`COM9`, `LPT1`-`LPT9`, with or without an extension) gains a leading `_`. A repaired
/// name can equal another object's real name; the identity record keeps the two apart.
pub(crate) fn safe_file_name(name: &str) -> Option<Cow<'_, str>> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed == "."
        || trimmed == ".."
        || name.len() > 255
        || name
            .chars()
            .any(|c| c == '/' || c == '\\' || c.is_control())
    {
        return None;
    }
    let kept = name.trim_end_matches(['.', ' ']);
    let trailing = name.len() - kept.len();
    let stem = kept.split('.').next().unwrap_or(kept).trim_end();
    let device = is_windows_device_name(stem);
    if trailing == 0 && !device && !kept.contains(WINDOWS_RESERVED_CHARS) {
        return Some(Cow::Borrowed(name));
    }
    let mut repaired = String::with_capacity(name.len() + 1);
    if device {
        repaired.push('_');
    }
    repaired.extend(kept.chars().map(|c| {
        if WINDOWS_RESERVED_CHARS.contains(&c) {
            '_'
        } else {
            c
        }
    }));
    repaired.extend(std::iter::repeat_n('_', trailing));
    (repaired.len() <= 255).then_some(Cow::Owned(repaired))
}

/// Whether `stem` is a device name Windows reserves, compared case-insensitively.
fn is_windows_device_name(stem: &str) -> bool {
    let upper = stem.to_ascii_uppercase();
    if WINDOWS_DEVICE_NAMES.contains(&upper.as_str()) {
        return true;
    }
    let Some(digit) = upper
        .strip_prefix("COM")
        .or_else(|| upper.strip_prefix("LPT"))
    else {
        return false;
    };
    matches!(
        digit,
        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "\u{b9}" | "\u{b2}" | "\u{b3}"
    )
}

#[cfg(test)]
mod tests;
