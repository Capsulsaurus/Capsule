//! Free-space probe + streaming-import recommendation — the storage-constrained-import
//! seam (SSoT:
//! [Import — Pipeline](https://docs/design/import/pipeline/)).
//!
//! The probe is I/O and runs *outside* the pure planner: it is attached at plan
//! confirmation, so the planner stays deterministic. The recommendation predicate below
//! is pure and is the contract the executor's streaming drive mode keys off. The
//! `total_size` accounting and the plan-level `streaming_recommended` attachment live on
//! [`ImportActionPlan`](crate::import::ImportActionPlan); this module owns the
//! probe and the two pure verdicts (`streaming_recommended`, `largest_asset_fits`) they
//! feed.

use std::path::Path;

use super::error::LibraryError;

/// The library volume's available bytes — a thin `statvfs` / `GetDiskFreeSpaceEx`
/// wrapper over the filesystem holding `library_root`.
///
/// Reports the space available **to an unprivileged process** (POSIX `f_bavail`, not the
/// root-reserved `f_bfree`), so the number matches what an import can actually write. On
/// Unix it is `f_bavail × f_frsize`; on Windows it is `GetDiskFreeSpaceExW`'s
/// free-bytes-available-to-caller. Any other target is a compile-time error rather than a
/// silent wrong answer.
#[tracing::instrument(level = "debug", skip_all, fields(root = %library_root.display()))]
pub fn available_bytes(library_root: &Path) -> Result<u64, LibraryError> {
    let bytes = platform::available_bytes(library_root)?;
    tracing::debug!(available_bytes = bytes, "library free-space probe");
    Ok(bytes)
}

#[cfg(unix)]
mod platform {
    use std::path::Path;

    use super::LibraryError;

    /// `statvfs(2)`: blocks available to an unprivileged caller × the fragment size.
    pub(super) fn available_bytes(library_root: &Path) -> Result<u64, LibraryError> {
        let stat = rustix::fs::statvfs(library_root).map_err(std::io::Error::from)?;
        Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
    }
}

#[cfg(windows)]
mod platform {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    use super::LibraryError;

    /// `GetDiskFreeSpaceExW`: bytes available to the calling user on the volume holding
    /// `library_root`.
    pub(super) fn available_bytes(library_root: &Path) -> Result<u64, LibraryError> {
        // A NUL-terminated wide string for the Win32 boundary.
        let mut wide: Vec<u16> = library_root.as_os_str().encode_wide().collect();
        wide.push(0);
        let mut free_to_caller: u64 = 0;
        // SAFETY: `wide` is a valid NUL-terminated UTF-16 buffer; `free_to_caller` is a live
        // local out-pointer; the other two out-params are optional and passed null.
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut free_to_caller,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(LibraryError::Io(std::io::Error::last_os_error()));
        }
        Ok(free_to_caller)
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    use std::path::Path;

    use super::LibraryError;

    pub(super) fn available_bytes(_library_root: &Path) -> Result<u64, LibraryError> {
        compile_error!("available_bytes: no free-space probe for this target");
    }
}

/// The pure streaming-recommendation predicate: streaming mode is recommended when the
/// plan's total size, plus a configurable headroom margin, meets or exceeds the volume's
/// available bytes. Attached to the plan at confirmation (never computed inside the pure
/// planner).
pub fn streaming_recommended(total_size: u64, available: u64, headroom_margin: u64) -> bool {
    total_size.saturating_add(headroom_margin) >= available
}

/// The pure minimum-headroom verdict for a *streaming* import: streaming bounds peak disk to
/// the in-flight window, but a single asset must still fully materialize locally (original +
/// derivatives + metadata) before its upload and release, so the **largest single asset** in
/// the plan must fit within available space minus the headroom margin. When even one asset
/// cannot fit, streaming cannot proceed and the plan surfaces a hard error rather than stall
/// mid-stream — streaming cannot make a single file smaller than the disk.
pub fn largest_asset_fits(largest_asset: u64, available: u64, headroom_margin: u64) -> bool {
    largest_asset.saturating_add(headroom_margin) <= available
}

#[cfg(test)]
mod tests;
