//! The **scope grammar** — the canonical identity of an import source (SSoT:
//! [Organization — Scope Grammar]).
//!
//! How a local folder, camera roll, or watched directory on each platform maps to a remote
//! album is a formal contract, not per-client improvisation. A [`Scope`] names the source;
//! its [`scope_id`](Scope::scope_id) is a domain-separated canonical-CBOR hash, so two
//! devices of the same platform looking at the same source compute the **same** id with no
//! coordination protocol.
//!
//! The `scope_overrides` / `source_kind_defaults` mapping table these ids key is deferred
//! post-v1 with the library-settings document; v1 carries the scope on the import so the
//! destination is explainable, and the override rows plug in later without changing this
//! grammar (see [`resolve_default_album`](super::default_album::resolve_default_album)).
//!
//! [Organization — Scope Grammar]: https://docs/design/organization/#scope-grammar-local-source--album-mapping

use ciborium::value::Value;

use crate::cbor;
use crate::cohort::PlatformTag;
use crate::crypto::hash::{Hash32, hash_bytes};

/// Domain-separation label for [`Scope::scope_id`] (versioned; a construction change bumps
/// the suffix, exactly as for the device-cohort hash).
pub const IMPORT_SCOPE_V1: &str = "capsule-import-scope/v1";

/// What kind of source an import came from (closed enum; the wire value is the lowercase
/// snake-case name, and a new kind is an additive protocol change).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceKind {
    /// The platform's primary photo library (iOS camera roll, Android `DCIM/Camera`).
    CameraRoll,
    /// The platform's screenshots collection.
    Screenshots,
    /// An app-owned collection (an iOS user collection, an Android app bucket).
    AppCollection,
    /// A plain directory the user pointed at once.
    Folder,
    /// A directory watched for new files.
    WatchedDir,
    /// A mounted card or external disk, identified by volume UUID.
    RemovableVolume,
}

impl SourceKind {
    /// The canonical wire string — the value that enters the [`scope_id`](Scope::scope_id)
    /// preimage. Stable: changing one changes every derived id.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::CameraRoll => "camera_roll",
            SourceKind::Screenshots => "screenshots",
            SourceKind::AppCollection => "app_collection",
            SourceKind::Folder => "folder",
            SourceKind::WatchedDir => "watched_dir",
            SourceKind::RemovableVolume => "removable_volume",
        }
    }
}

/// The canonical identity of an import source.
///
/// `locator` is the platform's canonical, reinstall-stable locator per the owner doc's
/// table — the iOS smart-album subtype name or collection `localIdentifier`, the Android
/// MediaStore **relative path** (never `BUCKET_ID`), the desktop canonicalized path, or a
/// removable volume's UUID plus relative path. Producing it is the platform adapter's job;
/// this type only fixes how the three parts become an id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Scope {
    /// The platform the source lives on (shared closed enum with device cohorts).
    pub platform: PlatformTag,
    /// What kind of source it is.
    pub source_kind: SourceKind,
    /// The platform's canonical locator for this source.
    pub locator: String,
}

impl Scope {
    /// Build a scope from its three parts.
    pub fn new(platform: PlatformTag, source_kind: SourceKind, locator: impl Into<String>) -> Self {
        Self {
            platform,
            source_kind,
            locator: locator.into(),
        }
    }

    /// The scope id:
    /// `SHA-256( canonical-CBOR([ "capsule-import-scope/v1", platform, source_kind, locator ]) )`.
    ///
    /// Domain-separated canonical CBOR — never naive concatenation — so a locator that
    /// happens to contain a separator can never collide with a different scope, and the id
    /// is byte-reproducible on any device and any platform implementation.
    #[must_use]
    pub fn scope_id(&self) -> Hash32 {
        let array = Value::Array(vec![
            Value::Text(IMPORT_SCOPE_V1.to_string()),
            Value::Text(self.platform.as_str().to_string()),
            Value::Text(self.source_kind.as_str().to_string()),
            Value::Text(self.locator.clone()),
        ]);
        hash_bytes(&cbor::value_to_canonical_vec(&array))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ios_roll() -> Scope {
        Scope::new(
            PlatformTag::Ios,
            SourceKind::CameraRoll,
            "smartAlbumUserLibrary",
        )
    }

    /// Two devices of the same platform looking at the same source compute the same id —
    /// the property that lets the mapping table need no coordination protocol.
    #[test]
    fn scope_id_is_deterministic_across_invocations() {
        assert_eq!(ios_roll().scope_id(), ios_roll().scope_id());
    }

    /// Every part of the triple is load-bearing: changing any one changes the id.
    #[test]
    fn every_component_changes_the_scope_id() {
        let base = ios_roll().scope_id();
        assert_ne!(
            base,
            Scope::new(
                PlatformTag::Android,
                SourceKind::CameraRoll,
                "smartAlbumUserLibrary"
            )
            .scope_id(),
            "platform must be bound into the id"
        );
        assert_ne!(
            base,
            Scope::new(
                PlatformTag::Ios,
                SourceKind::Screenshots,
                "smartAlbumUserLibrary"
            )
            .scope_id(),
            "source kind must be bound into the id"
        );
        assert_ne!(
            base,
            Scope::new(PlatformTag::Ios, SourceKind::CameraRoll, "DCIM/Camera").scope_id(),
            "locator must be bound into the id"
        );
    }

    /// Domain-separated canonical CBOR, not concatenation: a locator carrying a separator
    /// cannot impersonate a different (source_kind, locator) split.
    #[test]
    fn separator_in_locator_cannot_forge_another_scope() {
        let a = Scope::new(PlatformTag::Linux, SourceKind::Folder, "photos/2026");
        let b = Scope::new(PlatformTag::Linux, SourceKind::Folder, "photos").scope_id();
        assert_ne!(a.scope_id(), b);
        // And the concatenation of the parts is not the preimage.
        assert_ne!(
            a.scope_id(),
            hash_bytes(b"capsule-import-scope/v1linuxfolderphotos/2026")
        );
    }

    /// The wire strings are the doc's closed value set; changing one silently re-keys every
    /// override row, so they are pinned here.
    #[test]
    fn source_kind_wire_values_are_the_catalog_strings() {
        assert_eq!(SourceKind::CameraRoll.as_str(), "camera_roll");
        assert_eq!(SourceKind::Screenshots.as_str(), "screenshots");
        assert_eq!(SourceKind::AppCollection.as_str(), "app_collection");
        assert_eq!(SourceKind::Folder.as_str(), "folder");
        assert_eq!(SourceKind::WatchedDir.as_str(), "watched_dir");
        assert_eq!(SourceKind::RemovableVolume.as_str(), "removable_volume");
    }
}
