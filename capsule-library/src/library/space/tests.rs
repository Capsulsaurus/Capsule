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

#[test]
fn largest_asset_fits_thresholds() {
    // The largest asset plus headroom fits comfortably.
    assert!(largest_asset_fits(100, 1_000, 50));
    // Exactly at the boundary (largest + headroom == available) still fits.
    assert!(largest_asset_fits(950, 1_000, 50));
    // One byte over the boundary does not fit — the hard error case.
    assert!(!largest_asset_fits(951, 1_000, 50));
    // A single asset larger than the whole volume never fits, no matter the window.
    assert!(!largest_asset_fits(2_000, 1_000, 0));
    // Saturating: an absurd headroom never wraps into a false "fits".
    assert!(!largest_asset_fits(1, 1_000, u64::MAX));
}

/// The acceptance: the probe reports the real volume's free bytes on every supported
/// platform, and the executor's streaming drive mode keys off `streaming_recommended` fed
/// by it. We assert the probe returns a plausible non-zero figure for the temp volume and
/// that the two pure verdicts agree with it at both extremes (a trivially small plan never
/// recommends streaming and always fits; a plan the size of the whole volume does).
#[test]
fn available_bytes_reports_real_free_space() {
    let dir = tempfile::TempDir::new().unwrap();
    let free = available_bytes(dir.path()).expect("probe the temp volume");
    assert!(
        free > 0,
        "a writable temp volume must report some free space"
    );

    // A trivially small plan is nowhere near full and fits with room to spare.
    assert!(!streaming_recommended(1, free, 0));
    assert!(largest_asset_fits(1, free, 0));

    // A plan as large as the whole reported free space (no headroom) is at/over the
    // threshold, so streaming is recommended; and a single asset that size + any headroom
    // no longer fits — exactly the constrained regime the drive mode exists for.
    assert!(streaming_recommended(free, free, 0));
    assert!(!largest_asset_fits(free, free, 1));
}
