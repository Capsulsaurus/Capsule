use super::library_info_unanchored;

/// A library with no debris still shows the row, with a zero, and no notice.
#[test]
fn a_library_without_debris_shows_zero_and_no_hint() {
    let (row, hint) = library_info_unanchored(0);
    assert_eq!(row, "Unanchored sidecars: 0");
    assert_eq!(hint, None);
}

/// One unanchored sidecar takes the singular notice; it names no verb, since nothing
/// rewrites such a file.
#[test]
fn one_unanchored_sidecar_takes_the_singular_notice() {
    let (row, hint) = library_info_unanchored(1);
    assert_eq!(row, "Unanchored sidecars: 1");
    assert_eq!(
        hint.as_deref(),
        Some("1 sidecar in this library has no provenance chain, so its asset is hidden.")
    );
}

/// Several take the plural notice, carrying the count.
#[test]
fn several_unanchored_sidecars_take_the_plural_notice() {
    let (row, hint) = library_info_unanchored(3);
    assert_eq!(row, "Unanchored sidecars: 3");
    assert_eq!(
        hint.as_deref(),
        Some("3 sidecars in this library have no provenance chain, so their assets are hidden.")
    );
}
