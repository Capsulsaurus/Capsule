//! Free-space probe + streaming-import recommendation — the storage-constrained-import
//! seam (slice `S-B3` in the repo-root `SLICES.md`; SSoT:
//! [Import — Pipeline](https://docs/design/import/pipeline/)).
//!
//! The probe is I/O and runs *outside* the pure planner: it is attached at plan
//! confirmation, so the planner stays deterministic. The recommendation predicate below
//! is pure and is the contract the executor's streaming drive mode keys off. Note the
//! current planner emits per-candidate decisions and counts only — the `total_size`
//! accounting and the plan-level `streaming_recommended` attachment are part of `S-B3`.

use std::path::Path;

use super::error::LibraryError;

/// The library volume's available bytes — a thin `statvfs` / `GetDiskFreeSpaceEx`
/// wrapper over the filesystem holding `library_root`.
///
/// # Panics
/// Unimplemented skeleton (slice `S-B3`).
pub fn available_bytes(library_root: &Path) -> Result<u64, LibraryError> {
    let _ = library_root;
    todo!("S-B3: free-space probe — see SLICES.md")
}

/// The pure streaming-recommendation predicate: streaming mode is recommended when the
/// plan's total size, plus a configurable headroom margin, meets or exceeds the volume's
/// available bytes. Attached to the plan at confirmation (never computed inside the pure
/// planner).
pub fn streaming_recommended(total_size: u64, available: u64, headroom_margin: u64) -> bool {
    total_size.saturating_add(headroom_margin) >= available
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_recommended_thresholds() {
        // Comfortably under free space: no streaming.
        assert!(!streaming_recommended(100, 1_000, 50));
        // Within the headroom margin of free space: streaming.
        assert!(streaming_recommended(960, 1_000, 50));
        // Over free space outright: streaming.
        assert!(streaming_recommended(2_000, 1_000, 0));
        // Saturating: absurd sizes never overflow.
        assert!(streaming_recommended(u64::MAX, 1_000, u64::MAX));
    }

    /// `S-B3` acceptance: the probe reports the real volume's free bytes (within I/O
    /// slack) on every supported platform, and the executor's streaming drive mode keys
    /// off `streaming_recommended` fed by it.
    #[test]
    #[ignore = "S-B3 contract: free-space probe not yet implemented"]
    fn available_bytes_reports_real_free_space() {
        unimplemented!("implemented by slice S-B3");
    }
}
