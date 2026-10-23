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
