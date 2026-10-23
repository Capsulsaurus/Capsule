//! The closed lifecycle-action set (SSoT: [Authorization — The Closed Action Set]).
//!
//! Every lifecycle transition is an [`AssetManifest`](super::manifest::AssetManifest) whose
//! `action` is one of these. A value outside the set is a **structural error**, never a
//! "future value to ignore" — adding a value bumps `protocol_version` and old albums never
//! see it. Deserializing an unknown action string fails (closed-enum rejection).
//!
//! [Authorization — The Closed Action Set]: https://docs/design/authorization/#the-closed-action-set

use serde::{Deserialize, Serialize};

/// The seven authorized lifecycle actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    /// First write of an asset; `prior_provenance_hash` is null.
    Create,
    /// Replace the original bytes (e.g. re-encryption under a new AMK epoch).
    Replace,
    /// Soft-delete; the asset enters trash with a retention window.
    Delete,
    /// Edit to the encrypted metadata blob or sidecar fields.
    MetadataUpdate,
    /// Add a thumbnail, preview, or embedding.
    DerivativeAdd,
    /// Replace an existing derivative — the only authorized path; silent overwrite rejected.
    DerivativeReplace,
    /// Recover a soft-deleted asset from trash within its retention window.
    TrashRestore,
}

impl Action {
    /// Whether this action is the first link in an asset's chain (`prior_provenance_hash`
    /// must be null iff this is true).
    pub fn is_create(self) -> bool {
        matches!(self, Action::Create)
    }

    /// Whether this action mints a new encrypted metadata blob and its manifest therefore
    /// commits to `metadata_blob_hash`: `create | replace | metadata-update` carry the field,
    /// the other four (`delete | derivative-add | derivative-replace | trash-restore`) omit it
    /// (SSoT: [Provenance — Asset Manifest]). The presence-by-action rule is enforced at
    /// [`structural_ok`](super::manifest::AssetManifest::structural_ok).
    ///
    /// [Provenance — Asset Manifest]: https://docs/design/cryptography/provenance/#asset-manifest
    pub fn binds_metadata_blob(self) -> bool {
        matches!(
            self,
            Action::Create | Action::Replace | Action::MetadataUpdate
        )
    }
}

/// The role of a derivative (SSoT: [Provenance — Derivative Provenance]).
///
/// [Provenance — Derivative Provenance]: https://docs/design/cryptography/provenance/#derivative-provenance
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DerivativeRole {
    /// A small thumbnail image.
    Thumbnail,
    /// A larger preview image.
    Preview,
    /// An ML embedding vector.
    Embedding,
}

#[cfg(test)]
mod tests;
