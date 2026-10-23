use super::*;

/// The tier ladder is strictly ordered T0 < T1 < T2, so sorting yields ladder
/// order — the property a staged scheduler relies on to open sessions in order.
#[test]
fn tier_ladder_is_strictly_ordered() {
    assert!(UploadTier::Index < UploadTier::Preview);
    assert!(UploadTier::Preview < UploadTier::Original);
    let mut shuffled = [UploadTier::Original, UploadTier::Index, UploadTier::Preview];
    shuffled.sort_unstable();
    assert_eq!(shuffled, UploadTier::LADDER);
}

/// `Full` is the default policy (today's eager behavior).
#[test]
fn full_is_the_default_policy() {
    assert_eq!(UploadPolicy::default(), UploadPolicy::Full);
    assert!(!UploadPolicy::Full.is_staged());
    assert!(UploadPolicy::Staged.is_staged());
}

/// **Staged × streaming exclusion (pure).** Only the exact `staged + streaming`
/// combination is refused; every other pairing is compatible.
#[test]
fn only_staged_plus_streaming_is_refused() {
    assert_eq!(
        ensure_streaming_compatible(UploadPolicy::Staged, true),
        Err(StagedStreamingConflict)
    );
    assert!(ensure_streaming_compatible(UploadPolicy::Staged, false).is_ok());
    assert!(ensure_streaming_compatible(UploadPolicy::Full, true).is_ok());
    assert!(ensure_streaming_compatible(UploadPolicy::Full, false).is_ok());
}
