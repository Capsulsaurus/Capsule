use super::library_info_unmigrated;

/// An up-to-date library still shows the row, with a zero, and no migrate pointer.
#[test]
fn a_fully_signed_library_shows_zero_and_no_hint() {
    let (row, hint) = library_info_unmigrated(0);
    assert_eq!(row, "Unmigrated sidecars: 0");
    assert_eq!(hint, None);
}

/// One unsigned sidecar takes the singular hint, still carrying the count; the pointer
/// names the verb verbatim.
#[test]
fn one_unmigrated_sidecar_points_at_the_migrate_verb() {
    let (row, hint) = library_info_unmigrated(1);
    assert_eq!(row, "Unmigrated sidecars: 1");
    assert_eq!(
        hint.as_deref(),
        Some("Run `capsule library migrate` to admit the 1 unsigned sidecar.")
    );
}

/// Several take the plural hint, carrying the count.
#[test]
fn several_unmigrated_sidecars_take_the_plural_hint() {
    let (row, hint) = library_info_unmigrated(3);
    assert_eq!(row, "Unmigrated sidecars: 3");
    assert_eq!(
        hint.as_deref(),
        Some("Run `capsule library migrate` to admit the 3 unsigned sidecars.")
    );
}
