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
mod tests {
    use super::*;

    #[test]
    fn actions_use_the_exact_wire_strings() {
        let cases = [
            (Action::Create, "create"),
            (Action::Replace, "replace"),
            (Action::Delete, "delete"),
            (Action::MetadataUpdate, "metadata-update"),
            (Action::DerivativeAdd, "derivative-add"),
            (Action::DerivativeReplace, "derivative-replace"),
            (Action::TrashRestore, "trash-restore"),
        ];
        for (action, wire) in cases {
            let bytes = crate::cbor::to_canonical_vec(&action).unwrap();
            let decoded: Action = crate::cbor::from_slice(&bytes).unwrap();
            assert_eq!(decoded, action);
            // The encoded value is exactly the kebab-case text string.
            let as_text: String = crate::cbor::from_slice(&bytes).unwrap();
            assert_eq!(as_text, wire);
        }
    }

    #[test]
    fn unknown_action_value_is_rejected() {
        // A closed enum: an unknown string fails to decode (not "ignored as future").
        let bytes = crate::cbor::to_canonical_vec(&"future-action-not-yet-defined").unwrap();
        assert!(crate::cbor::from_slice::<Action>(&bytes).is_err());
    }

    #[test]
    fn only_create_is_a_chain_root() {
        assert!(Action::Create.is_create());
        for a in [
            Action::Replace,
            Action::Delete,
            Action::MetadataUpdate,
            Action::DerivativeAdd,
            Action::DerivativeReplace,
            Action::TrashRestore,
        ] {
            assert!(!a.is_create());
        }
    }
}
