//! The visibility gate and the `original_held` derivation (staged-uploads contract; slice
//! `S-C1`). SSoT: [Download & Sync — Upload Tiering](https://docs/design/import/download-sync/#upload-tiering-staged-uploads).
//!
//! The server gates visibility on the pending-asset row: an asset becomes visible to other
//! devices once its **manifest and metadata blob** are finalized — not when the (possibly
//! large, possibly staged) original lands. Whether the original is held yet is a *derived*
//! per-asset fact, `original_held`, carried on the sync feed (the feed field itself is
//! S-C2); it is never stored as a second source of truth. An asset with
//! `original_held = false` is in the derived `awaiting-original` state.
//!
//! These are pure predicates over the blob role + finalization state, unit-tested here so
//! the finalization path and the (S-C2) sync feed reason over one definition.

use crate::models::session::BlobRole;

/// Whether finalizing a blob of this role flips the asset from pending to **visible**.
///
/// Visibility flips on the index tier (T0): the signed manifest travels with every blob's
/// envelope, so the gating event is the **metadata** blob landing. Derivative, original,
/// provenance, and backup blobs never flip visibility on their own — the original in
/// particular may still be staged.
pub(crate) fn finalization_makes_visible(role: BlobRole) -> bool {
    matches!(role, BlobRole::Metadata)
}

/// Derive `original_held` for an asset from the finalization state of its original blob.
///
/// `original_finalized` is whether the asset's original-role blob has reached `Completed`.
/// The fact is *always* derived — the caller must never persist it as a second column.
pub(crate) fn derive_original_held(original_finalized: bool) -> bool {
    original_finalized
}

/// Whether an asset is in the derived `awaiting-original` state: visible (its metadata
/// finalized) but its original blob not yet held. Fetching such an asset's original
/// returns the transient `error.blob.pending_upload`, never `410` (owned by S-C10).
#[allow(dead_code)]
pub(crate) fn is_awaiting_original(visible: bool, original_held: bool) -> bool {
    visible && !original_held
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_metadata_finalization_flips_visibility() {
        assert!(finalization_makes_visible(BlobRole::Metadata));
        assert!(!finalization_makes_visible(BlobRole::Original));
        assert!(!finalization_makes_visible(BlobRole::Derivative));
        assert!(!finalization_makes_visible(BlobRole::Provenance));
        assert!(!finalization_makes_visible(BlobRole::Backup));
    }

    #[test]
    fn original_held_tracks_the_original_blob() {
        assert!(!derive_original_held(false));
        assert!(derive_original_held(true));
    }

    #[test]
    fn awaiting_original_is_visible_without_the_original() {
        // Metadata finalized (visible) but the original still on device → awaiting-original.
        assert!(is_awaiting_original(true, false));
        // Original held → no longer awaiting.
        assert!(!is_awaiting_original(true, true));
        // Not yet visible → not awaiting-original (it is simply pending).
        assert!(!is_awaiting_original(false, false));
    }
}
