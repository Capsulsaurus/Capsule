//! The client-side **staged upload scheduler** (slice `S-B4`; SSoT:
//! [Download & Sync — Upload Tiering (Staged Uploads)]).
//!
//! Uploads used to be all-or-nothing; staged uploads add the upload-direction
//! ladder for low-data situations. The policy is **client-side session ordering
//! only** — the server has zero mode branches (the canonical [`UploadPolicy`] /
//! [`UploadTier`] contract lives in `capsule_core::import::upload`). This module is
//! that ordering's network half:
//!
//! - **Ladder ordering + gating.** [`StagedScheduler::plan_sessions`] returns the
//!   blobs to open sessions for *now*, in strict ladder order (T0 index → T1 preview
//!   → T2 original). Under [`UploadPolicy::Full`] every session opens eagerly; under
//!   [`UploadPolicy::Staged`] a tier opens only when the detected
//!   [`ConnectionClass`] permits it ([`ConnectionClass::permits_tier`]) — T0 escapes
//!   on any usable link, T1/T2 wait for a non-metered link (or an explicit
//!   force-sync). Because the tier gate is monotone, the permitted set is always a
//!   ladder *prefix*: T2 never opens before T1, which never opens before T0.
//! - **Resume from server truth.** Resume needs no durable client state:
//!   [`remaining_tiers`] re-derives the outstanding work from what the server holds
//!   ([`held_from_feed`] reads the held blob roles + `original_held` off a sync feed
//!   entry). Kill the client mid-ladder and the queue rebuilds from the feed; only
//!   missing tiers are re-uploaded, and any tier with an in-flight session resumes
//!   through the [`UploadClient`]'s own create-dedup / `HEAD` offset rather than
//!   restarting.
//! - **`awaiting-original`.** T2 is exactly what staged defers, so a staged asset is
//!   in the derived [`awaiting-original`](crate::sync::OriginalAvailability) state on
//!   other devices until its original lands (`original_held` flips). The badge and
//!   the transient `error.blob.pending_upload` fetch state are the sync/fetch halves
//!   ([`crate::sync`], [`crate::fetch`]); this scheduler is what makes T2 land last.
//!
//! Verify-before-destroy is untouched: a staged asset pins its local original until
//! T2 is durable, by the same [`ReleaseGate`](capsule_core::library::ReleaseGate)
//! that always governed release. Staged and streaming import are mutually exclusive
//! per import — enforced in `capsule_core::import` (planner confirmation + the
//! streaming executor), so it can never reach this scheduler.
//!
//! [Download & Sync — Upload Tiering (Staged Uploads)]: https://docs/design/import/download-sync/#upload-tiering-staged-uploads

use std::collections::HashSet;

use capsule_core::import::{UploadPolicy, UploadTier};
use tracing::instrument;

use crate::net::ConnectionClass;
use crate::sync::FeedEntry;
use crate::upload::{CreateUploadRequest, UploadClient, UploadError, UploadOutcome};

// ─── Bundle shape ─────────────────────────────────────────────────────────────

/// One upload unit within an asset bundle: a single blob, tagged with the
/// [`UploadTier`] it belongs to and its ciphertext content address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TierBlob {
    /// Which tier of the ladder this blob belongs to.
    pub tier: UploadTier,
    /// The blob's ciphertext content address (lowercase hex) — the resume key.
    pub hash: String,
    /// Ciphertext size in bytes.
    pub size: u64,
}

impl TierBlob {
    /// A tier blob from its tier, content address, and size.
    #[must_use]
    pub fn new(tier: UploadTier, hash: impl Into<String>, size: u64) -> Self {
        Self {
            tier,
            hash: hash.into(),
            size,
        }
    }
}

/// An asset's staged upload bundle: its id and the blobs to upload. The blobs may
/// be supplied in any order — the scheduler sorts them into ladder order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedAsset {
    /// The asset id (UUIDv7, as a string).
    pub asset_id: String,
    /// The bundle's blobs, in any order.
    pub blobs: Vec<TierBlob>,
}

impl StagedAsset {
    /// A bundle for `asset_id` over `blobs`.
    #[must_use]
    pub fn new(asset_id: impl Into<String>, blobs: Vec<TierBlob>) -> Self {
        Self {
            asset_id: asset_id.into(),
            blobs,
        }
    }

    /// The bundle's blobs in strict ladder order (T0 → T1 → T2), stable within a
    /// tier so a caller's intra-tier order (e.g. manifest before metadata) is kept.
    #[must_use]
    pub fn ladder_ordered(&self) -> Vec<TierBlob> {
        let mut blobs = self.blobs.clone();
        blobs.sort_by_key(|b| b.tier); // stable: preserves within-tier order
        blobs
    }
}

// ─── Server truth (resume) ────────────────────────────────────────────────────

/// The set of blob content addresses the server **durably holds** for an asset,
/// read from a sync feed entry — the server truth a resume re-derives from.
///
/// A derivative/metadata blob ref present on the entry is held (its tier finalized;
/// the entry's very existence means T0 index finalized). The original is included
/// **iff** `original_held` — the `awaiting-original` fact — so T2 is treated as
/// outstanding until the feed flips it, never as a dangling reference.
#[must_use]
pub fn held_from_feed(entry: &FeedEntry) -> HashSet<String> {
    let mut held = HashSet::new();
    for derivative in &entry.blobs.derivatives {
        held.insert(derivative.ciphertext_hash.clone());
    }
    if entry.original_held
        && let Some(original) = &entry.blobs.original
    {
        held.insert(original.ciphertext_hash.clone());
    }
    held
}

/// Re-derive the outstanding staged work from server truth: keep only the blobs the
/// server does **not** already hold, in ladder order.
///
/// This is the whole resume story (download-sync doc: "Resume needs no new client
/// state"). No durable local queue is trusted — `library.sqlite`'s work queue is a
/// rebuildable cache. Kill the client mid-ladder, pull the feed, pass `held` from
/// [`held_from_feed`], and this rebuilds exactly the missing tiers. A tier whose
/// blob is already held is skipped; a tier still in flight is re-driven and resumes
/// through the [`UploadClient`]'s create-dedup / `HEAD` offset (no bytes re-sent).
#[must_use]
pub fn remaining_tiers<H: std::hash::BuildHasher>(
    asset: &StagedAsset,
    held: &HashSet<String, H>,
) -> StagedAsset {
    StagedAsset {
        asset_id: asset.asset_id.clone(),
        blobs: asset
            .ladder_ordered()
            .into_iter()
            .filter(|blob| !held.contains(&blob.hash))
            .collect(),
    }
}

// ─── The scheduler ────────────────────────────────────────────────────────────

/// The staged upload scheduler: given a [`UploadPolicy`], the detected
/// [`ConnectionClass`], and an optional force-sync consent, it decides which of an
/// asset's tier sessions open now and in what order.
#[derive(Debug, Clone, Copy)]
pub struct StagedScheduler {
    policy: UploadPolicy,
    class: ConnectionClass,
    force_sync: bool,
}

impl StagedScheduler {
    /// A scheduler for `policy` on the detected connection `class`, without
    /// force-sync (the metered/Wi-Fi criteria apply).
    #[must_use]
    pub fn new(policy: UploadPolicy, class: ConnectionClass) -> Self {
        Self {
            policy,
            class,
            force_sync: false,
        }
    }

    /// Set the user-consented **force-sync** flag (the two-week-staleness "sync
    /// now"): it overrides the metered/Wi-Fi criteria for above-index tiers, but
    /// never resurrects an offline path.
    #[must_use]
    pub fn with_force_sync(mut self, force_sync: bool) -> Self {
        self.force_sync = force_sync;
        self
    }

    /// Whether a given tier's session opens right now under this scheduler.
    ///
    /// `Full` opens every tier eagerly; `Staged` defers to the connection-class tier
    /// gate ([`ConnectionClass::permits_tier`]).
    #[must_use]
    pub fn tier_opens_now(&self, tier: UploadTier) -> bool {
        match self.policy {
            UploadPolicy::Full => true,
            UploadPolicy::Staged => self.class.permits_tier(tier, self.force_sync),
        }
    }

    /// The blobs to open sessions for **now**, in strict ladder order.
    ///
    /// Under [`UploadPolicy::Full`] this is the whole bundle (still ladder-ordered
    /// for a deterministic session sequence). Under [`UploadPolicy::Staged`] it is
    /// the ladder *prefix* the connection currently permits — T0 alone on a metered
    /// link, the whole bundle on unmetered Wi-Fi (or under force-sync). The deferred
    /// tiers carry no client state: the next window re-derives them from server
    /// truth ([`remaining_tiers`]).
    #[must_use]
    pub fn plan_sessions(&self, asset: &StagedAsset) -> Vec<TierBlob> {
        asset
            .ladder_ordered()
            .into_iter()
            .filter(|blob| self.tier_opens_now(blob.tier))
            .collect()
    }

    /// Drive each asset's planned tier sessions through `sink`, per asset, in ladder
    /// order — the session sequence a mock records to prove the ladder. Assets are
    /// processed in the order given; within an asset, strictly T0 → T1 → T2.
    #[instrument(skip_all, fields(policy = ?self.policy, class = ?self.class, assets = assets.len()))]
    pub async fn run<S: TierSink>(
        &self,
        assets: &[StagedAsset],
        sink: &S,
    ) -> Result<StagedReport, StagedError> {
        let mut report = StagedReport::default();
        for asset in assets {
            for blob in self.plan_sessions(asset) {
                tracing::debug!(asset = %asset.asset_id, tier = ?blob.tier, hash = %blob.hash, "opening staged tier session");
                let outcome = sink.open_session(&asset.asset_id, &blob).await?;
                report.opened.push(OpenedTier {
                    asset_id: asset.asset_id.clone(),
                    tier: blob.tier,
                    hash: blob.hash.clone(),
                    outcome,
                });
            }
        }
        tracing::info!(
            opened = report.opened.len(),
            "staged upload window complete"
        );
        Ok(report)
    }
}

// ─── Sink seam ────────────────────────────────────────────────────────────────

/// How one opened tier session resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TierSessionOutcome {
    /// The tier's blob transferred; the server has begun finalization.
    Uploaded {
        /// The session that was driven to completion.
        session_id: String,
    },
    /// The server already held the blob (dedup/merge) — nothing to transfer.
    AlreadyStored {
        /// The existing asset the reference merges onto.
        asset_ref: String,
    },
}

/// Errors a staged run can fail with.
#[derive(Debug, thiserror::Error)]
pub enum StagedError {
    /// A tier's underlying upload failed non-recoverably (surfaced from the
    /// [`UploadClient`]).
    #[error("staged tier upload failed: {0}")]
    Upload(#[from] UploadError),
}

/// The network seam the scheduler drives to open one tier's upload session. Kept a
/// trait so the ordering + gating are exercised by a deterministic recording mock;
/// [`UploadClientTierSink`] is the production impl over the hand-written upload
/// client.
pub trait TierSink {
    /// Open (and drive to completion) the upload session for `blob` of `asset_id`.
    fn open_session(
        &self,
        asset_id: &str,
        blob: &TierBlob,
    ) -> impl std::future::Future<Output = Result<TierSessionOutcome, StagedError>> + Send;
}

/// Builds the per-blob upload request + ciphertext the scheduler hands to the
/// upload client. The **app** implements this — it holds the sealed bytes and the
/// signed manifest envelope; the scheduler owns only the tier ordering and gating,
/// so the two policies stay on one code path.
pub trait TierRequestBuilder {
    /// The `POST /upload` request and ciphertext bytes for one tier blob.
    fn build(&self, asset_id: &str, blob: &TierBlob) -> (CreateUploadRequest, Vec<u8>);
}

/// The production [`TierSink`]: opens each planned tier session through the
/// hand-written, resumable [`UploadClient`], so staged session ordering rides the
/// real upload protocol. In-flight tiers resume through the client's create-dedup /
/// `HEAD` offset — the reason resume needs no extra scheduler state.
pub struct UploadClientTierSink<'a, R: TierRequestBuilder> {
    client: &'a UploadClient,
    builder: R,
}

impl<'a, R: TierRequestBuilder> UploadClientTierSink<'a, R> {
    /// A sink over `client`, building each tier's request with `builder`.
    pub fn new(client: &'a UploadClient, builder: R) -> Self {
        Self { client, builder }
    }
}

impl<R: TierRequestBuilder + Sync> TierSink for UploadClientTierSink<'_, R> {
    async fn open_session(
        &self,
        asset_id: &str,
        blob: &TierBlob,
    ) -> Result<TierSessionOutcome, StagedError> {
        let (request, bytes) = self.builder.build(asset_id, blob);
        match self.client.upload(&request, &bytes).await? {
            UploadOutcome::Completed { session_id } => {
                Ok(TierSessionOutcome::Uploaded { session_id })
            }
            UploadOutcome::AlreadyStored { asset_ref } => {
                Ok(TierSessionOutcome::AlreadyStored { asset_ref })
            }
        }
    }
}

// ─── Report ───────────────────────────────────────────────────────────────────

/// One opened tier session, in observed order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedTier {
    /// The asset the session was opened for.
    pub asset_id: String,
    /// Which tier opened.
    pub tier: UploadTier,
    /// The blob's content address.
    pub hash: String,
    /// How it resolved.
    pub outcome: TierSessionOutcome,
}

/// The outcome of one staged run: the tier sessions opened, in observed order.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StagedReport {
    /// The opened tier sessions, in the exact order the scheduler drove them.
    pub opened: Vec<OpenedTier>,
}

impl StagedReport {
    /// The observed tier sequence — the ladder-order proof (`[Index, Preview, …]`).
    #[must_use]
    pub fn tier_sequence(&self) -> Vec<UploadTier> {
        self.opened.iter().map(|o| o.tier).collect()
    }
}

#[cfg(test)]
mod tests;
