//! The visibility gate and the `original_held` derivation (the staged-uploads contract).
//!
//! The server gates an asset's visibility on *which blob of the bundle just finalized*, and it
//! can do that without a key: every session declares its [`BlobRole`] at creation, so bundle
//! completeness is answerable from the roles alone.
//!
//! Both facts are **derived**, never stored: `original_held` in particular is a function of
//! whether the original-role blob has finalized, and a second column carrying it would be a
//! second source of truth that can disagree with the first.
//!
//! # What this module does *not* do here
//!
//! Flipping the durable pending-asset row is the asset index's business, and this crate has
//! no asset index yet (see [`crate::upload`]'s "What this port does not carry"). What lands
//! here is the definition, exercised by the finalization path's log line, so that when the
//! index arrives it consumes one definition rather than inventing a second.

use crate::store::BlobRole;

/// Whether finalizing a blob of this role flips its asset from pending to **visible**.
///
/// Visibility flips on the index tier: the manifest travels with every blob's envelope, so the
/// gating event is the **metadata** blob landing. A derivative, an original, a provenance blob
/// or a backup never flips visibility on its own — the original in particular may still be
/// staged on the device, which is the whole point of staged uploads.
pub fn finalization_makes_visible(role: BlobRole) -> bool {
    matches!(role, BlobRole::Metadata)
}

/// Derive `original_held` from the finalization state of the asset's original blob.
///
/// A one-line function on purpose: it is the *definition*, and the callers that will need it
/// (the sync feed, the media-serving path) must share it rather than each re-deciding what
/// "held" means.
pub fn derive_original_held(original_finalized: bool) -> bool {
    original_finalized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_metadata_finalization_flips_visibility() {
        assert!(finalization_makes_visible(BlobRole::Metadata));
        for role in [
            BlobRole::Original,
            BlobRole::Derivative,
            BlobRole::Provenance,
            BlobRole::Backup,
        ] {
            assert!(
                !finalization_makes_visible(role),
                "{role:?} must not make an asset visible on its own"
            );
        }
    }

    #[test]
    fn original_held_tracks_the_original_blob() {
        assert!(!derive_original_held(false));
        assert!(derive_original_held(true));
    }
}
