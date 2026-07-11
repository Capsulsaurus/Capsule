//! Staged-upload contract types — the **one canonical** `UploadPolicy`/`UploadTier`
//! set (slice `S-B4` in the repo-root `SLICES.md`; SSoT:
//! [Download & Sync — Upload Tiering (Staged Uploads)](https://docs/design/import/download-sync/#upload-tiering-staged-uploads)).
//!
//! Uploads used to be all-or-nothing; **staged uploads** add the upload-direction
//! ladder for low-data situations (a metered plan, weeks from Wi-Fi) where what
//! matters most is that the *index* of what exists escapes the device. This module
//! owns the two closed contract enums and the one pure invariant they carry — the
//! **staged × streaming exclusion**. Everything network-facing (the scheduler that
//! opens sessions in tier order, gated by the connection class) lives in the SDK
//! half (`capsule_sdk::staged`); `capsule-core` stays network-free.
//!
//! The policy is **client-side session ordering only** — the server has zero mode
//! branches (download-sync doc). Under [`UploadPolicy::Staged`] the scheduler opens
//! each asset's sessions in [`UploadTier`] order, gating each tier on the sync
//! connection criteria; under [`UploadPolicy::Full`] all sessions open eagerly.

/// The per-device upload policy: a closed enum with exactly two members.
///
/// The choice is *ordering*, not a distinct code path — the same `POST /upload`
/// sessions, bundle mechanics, and finalization run under both; `staged` simply
/// hasn't opened the higher-tier sessions yet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum UploadPolicy {
    /// Every session of an asset's bundle opens eagerly, in any order (today's
    /// behavior, and the default).
    #[default]
    Full,
    /// Sessions open in tier order (index → preview → original) per asset, each
    /// tier gated by the connection class. Mutually exclusive with streaming
    /// import ([`ensure_streaming_compatible`]).
    Staged,
}

impl UploadPolicy {
    /// Whether this policy stages sessions in tier order (vs. opening eagerly).
    #[must_use]
    pub fn is_staged(self) -> bool {
        matches!(self, Self::Staged)
    }
}

/// The upload tier ladder, mirroring the download ladder. Tiers map directly onto
/// existing blob roles — **no new blob kind exists for staging**. The derived
/// `Ord` is the ladder order (`Index < Preview < Original`), so a scheduler emits
/// sessions strictly T0 → T1 → T2 by sorting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UploadTier {
    /// T0: signed manifest + metadata blob (embedded LQIP) — the index that makes
    /// the asset visible (`awaiting-original`) on other devices. A few KB per
    /// asset; escapes on any usable connection.
    Index,
    /// T1: thumbnail + preview derivative blobs. Needs a non-metered link
    /// (small-reconciliation rule).
    Preview,
    /// T2: the original blob; its finalization flips `original_held` on the sync
    /// feed and unlocks every release path (verify-before-destroy). Needs unmetered
    /// Wi-Fi (large-reconciliation rule) or explicit force-sync.
    Original,
}

impl UploadTier {
    /// The tiers in strict ladder order (T0 → T1 → T2) — the ordering input a
    /// staged scheduler iterates.
    pub const LADDER: [UploadTier; 3] = [Self::Index, Self::Preview, Self::Original];
}

/// Raised when a staged upload policy is combined with a storage-constrained
/// streaming import for the same run — the combination the planner rejects
/// outright (download-sync doc: "Staged and streaming import are mutually
/// exclusive per import").
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "staged upload policy is mutually exclusive with streaming import: streaming releases local \
     bytes quickly, staged defers exactly the upload release depends on"
)]
pub struct StagedStreamingConflict;

/// The pure staged × streaming exclusion invariant.
///
/// Streaming exists to release local bytes as quickly as possible; staged defers
/// exactly the T2 (original) upload that release depends on. Combining them is a
/// contradiction, so it is refused at confirmation and — belt and braces — the
/// streaming executor refuses a staged policy by construction. Returns
/// [`StagedStreamingConflict`] iff `policy` is [`UploadPolicy::Staged`] and
/// `streaming` is set.
pub fn ensure_streaming_compatible(
    policy: UploadPolicy,
    streaming: bool,
) -> Result<(), StagedStreamingConflict> {
    if policy.is_staged() && streaming {
        Err(StagedStreamingConflict)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
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
}
