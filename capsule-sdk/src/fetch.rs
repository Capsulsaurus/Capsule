//! Tiered, on-demand **blob fetch** (slice `S-D2`; SSoT: [Download & Sync]).
//!
//! Where [`crate::sync`] discovers *what* changed, this module fetches *the
//! smallest representation that satisfies the user's current intent, and nothing
//! more*. It owns four things the download-sync doc specifies:
//!
//! - **Tiered fetch + cross-asset dedup.** [`plan_eager_fetches`] returns only the
//!   content addresses the configured [`FetchScope`] wants that are not already in
//!   the local cache (blobs are content-addressed, so a representation shared
//!   between assets — an identical thumbnail — is fetched at most once).
//! - **The degrade ladder.** When an above-tier representation cannot be fetched,
//!   the client distinguishes *permanent* (`410 Gone` / purged) from *authorization
//!   change* (`403`) from *transient*, and degrades gracefully to the best
//!   representation already in hand ([`best_available`]) — down to the always-present
//!   LQIP — never removing the asset's metadata or index entry
//!   ([`open_representation`]).
//! - **Resumable ranged fetch.** [`fetch_blob`] fetches a blob with HTTP `Range`
//!   windows; an interrupted transfer resumes from the last persisted byte instead
//!   of restarting, and re-fetches zero bytes it already holds.
//! - **Self-verification.** The client recomputes the ciphertext content hash
//!   against the requested content address; any mismatch discards the blob.
//!
//! The [`BlobSource`] seam abstracts the wire so the resume loop and degrade ladder
//! are driven by deterministic mocks; [`HttpBlobSource`] is the production
//! `reqwest`-over-`Range` implementation against the media server's `GET /blob/{hash}`.
//!
//! [Download & Sync]: https://docs/design/import/download-sync/

use std::collections::BTreeSet;

use capsule_i18n::error_codes;
use sha2::{Digest, Sha256};
use tracing::instrument;

use crate::net::{
    ConnectionClass, MonotonicClock, RetryClass, RetryDecision, RetryEngine, Sleeper, StallConfig,
    StallDetector, SystemClock, TokioSleeper, TransferSignal,
};

// ─── Representation ladder ───────────────────────────────────────────────────

/// The ladder of an asset's representations, cheapest first. `Ord` is the ladder
/// order, so [`best_available`] can degrade with a plain `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Representation {
    /// Embedded in the metadata blob; available the instant metadata syncs, at
    /// zero extra request. Always present.
    Lqip,
    /// Fetched when the asset scrolls into (or near) a grid.
    Thumbnail,
    /// A screen-resolution derivative, fetched when the asset is opened.
    Preview,
    /// Full fidelity, fetched only on explicit demand.
    Original,
}

/// The per-library synchronization-scope setting: the eager fetch ceiling. LQIP is
/// always present (embedded); Preview is *always* on-demand (fetched on open), so
/// it never appears in the eager set even at the highest scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchScope {
    /// Fetch nothing beyond the synced metadata (LQIP is embedded).
    MetadataOnly,
    /// Eagerly fetch thumbnails.
    MetadataThumbnails,
    /// Eagerly fetch thumbnails and originals.
    MetadataThumbnailsOriginal,
}

impl FetchScope {
    /// The representations fetched eagerly (on sync) under this scope.
    #[must_use]
    pub fn eager_representations(self) -> &'static [Representation] {
        match self {
            Self::MetadataOnly => &[],
            Self::MetadataThumbnails => &[Representation::Thumbnail],
            Self::MetadataThumbnailsOriginal => {
                &[Representation::Thumbnail, Representation::Original]
            }
        }
    }
}

/// A single representation's content address and ciphertext size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepBlob {
    /// Ciphertext content address (lowercase hex).
    pub hash: String,
    /// Ciphertext size in bytes.
    pub size: u64,
}

/// An asset's fetchable representations, resolved from a sync feed entry's blob
/// manifest. LQIP is not here — it rides the metadata blob.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssetBlobs {
    /// The thumbnail derivative, when present.
    pub thumbnail: Option<RepBlob>,
    /// The preview derivative, when present.
    pub preview: Option<RepBlob>,
    /// The original, when present (absent while `awaiting-original`).
    pub original: Option<RepBlob>,
}

impl AssetBlobs {
    /// The blob backing a representation (`None` for [`Representation::Lqip`], which
    /// is embedded, and for any representation this asset does not carry).
    #[must_use]
    pub fn blob_for(&self, representation: Representation) -> Option<&RepBlob> {
        match representation {
            Representation::Lqip => None,
            Representation::Thumbnail => self.thumbnail.as_ref(),
            Representation::Preview => self.preview.as_ref(),
            Representation::Original => self.original.as_ref(),
        }
    }
}

/// A read-only view of the local content-addressed blob cache, for dedup.
pub trait BlobCache {
    /// Whether a blob with this content address is already cached.
    fn contains(&self, hash: &str) -> bool;
}

/// A cache that holds nothing — every planned representation is fetched.
#[derive(Debug, Clone, Copy, Default)]
pub struct EmptyCache;

impl BlobCache for EmptyCache {
    fn contains(&self, _hash: &str) -> bool {
        false
    }
}

/// Plan the eager fetches for one asset under `scope`: the content addresses the
/// scope wants that are not already cached, de-duplicated within the plan. A blob
/// already in `cache` (looked up by content address) is skipped — the cross-asset
/// dedup guarantee.
#[must_use]
pub fn plan_eager_fetches(
    scope: FetchScope,
    asset: &AssetBlobs,
    cache: &impl BlobCache,
) -> Vec<String> {
    let mut planned: Vec<String> = Vec::new();
    for &representation in scope.eager_representations() {
        if let Some(blob) = asset.blob_for(representation)
            && !cache.contains(&blob.hash)
            && !planned.contains(&blob.hash)
        {
            planned.push(blob.hash.clone());
        }
    }
    planned
}

/// The best locally-held representation no higher than `desired` — the degrade
/// target. LQIP is always present, so this is `Some` whenever `held` contains it.
#[must_use]
pub fn best_available(
    desired: Representation,
    held: &BTreeSet<Representation>,
) -> Option<Representation> {
    held.iter()
        .copied()
        .filter(|representation| *representation <= desired)
        .max()
}

// ─── Blob source seam ────────────────────────────────────────────────────────

/// The outcome of one ranged request to a [`BlobSource`].
#[derive(Debug, Clone)]
pub enum RangeOutcome {
    /// The remaining bytes from the requested `start` through end-of-blob arrived.
    Complete {
        /// The bytes from `start` to the end of the blob.
        bytes: Vec<u8>,
    },
    /// A prefix of the remaining bytes arrived, then the stream dropped
    /// (transient) — resume from the new offset.
    Partial {
        /// The bytes received before the drop.
        bytes: Vec<u8>,
    },
    /// The server answered a status with no usable body (`403`/`410`/`5xx`/pending).
    Status {
        /// The HTTP status (`0` for a transport error with no response).
        status: u16,
        /// The stable `error.*` code from the body, when present.
        code: Option<String>,
    },
}

/// The ranged blob transport. Abstracted so the resume loop and degrade ladder are
/// exercised by deterministic mocks; [`HttpBlobSource`] is the production impl.
pub trait BlobSource {
    /// Fetch the blob `hash` from byte offset `start` (an HTTP `Range` request).
    ///
    /// `max_len` caps how many bytes this one request asks for: `Some(w)` requests
    /// the bounded window `bytes={start}-{start+w-1}` (the adverse-network posture's
    /// bounded transfer windows), and `None` requests the open-ended remainder
    /// `bytes={start}-`. A source may return fewer bytes than requested — the loop
    /// resumes from wherever it actually reached.
    fn get_range(
        &self,
        hash: &str,
        start: u64,
        max_len: Option<u64>,
    ) -> impl std::future::Future<Output = RangeOutcome> + Send;
}

// ─── Errors ──────────────────────────────────────────────────────────────────

/// Why a representation could not be fetched. The variants drive the degrade
/// ladder: [`FetchError::Gone`] is permanent, [`FetchError::AuthorizationChanged`]
/// triggers a membership re-sync, [`FetchError::PendingUpload`] is the transient
/// `awaiting-original` state, and the rest are transient/structural.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// `410 Gone` / `404` / purged origin — permanently unavailable; degrade.
    #[error("blob permanently unavailable")]
    Gone,
    /// `403` — an authorization change, not a durability loss. Re-sync membership
    /// then retry; only then degrade (the asset may have been unshared).
    #[error("blob authorization changed")]
    AuthorizationChanged,
    /// `error.blob.pending_upload` — the original has not landed yet
    /// (`awaiting-original`); show the badge, never a failure, and re-fetch when
    /// the feed flips `original_held`. Explicitly distinct from `410 Gone`.
    #[error("blob pending upload (awaiting-original)")]
    PendingUpload,
    /// A transient failure (network drop, `5xx`): retry with backoff and resume.
    #[error("transient fetch failure: {0}")]
    Transient(String),
    /// A structural server rejection the client does not model.
    #[error("blob fetch rejected (status {status}, code {code:?})")]
    Rejected {
        /// HTTP status.
        status: u16,
        /// Stable `error.*` code, when present.
        code: Option<String>,
    },
    /// The reassembled ciphertext did not hash to the requested content address —
    /// the blob is discarded.
    #[error("integrity check failed: ciphertext hash mismatch for {expected}")]
    IntegrityFailed {
        /// The requested content address.
        expected: String,
    },
    /// The bounded resume budget was spent without completing the transfer.
    #[error("fetch gave up after exhausting the resume budget: {0}")]
    Exhausted(String),
}

impl FetchError {
    /// The stable `error.*` catalog code, when one applies.
    #[must_use]
    pub fn error_code(&self) -> Option<&str> {
        match self {
            Self::PendingUpload => Some(error_codes::BLOB_PENDING_UPLOAD),
            Self::Rejected { code, .. } => code.as_deref(),
            _ => None,
        }
    }

    /// Whether the failure is permanent (degrade) rather than retryable.
    #[must_use]
    pub fn is_permanent(&self) -> bool {
        matches!(self, Self::Gone)
    }
}

/// Map a status/code with no usable body onto a typed [`FetchError`]. The stable
/// `error.*` code is authoritative; the bare status is the fallback.
fn classify_status(status: u16, code: Option<String>) -> FetchError {
    if code.as_deref() == Some(error_codes::BLOB_PENDING_UPLOAD) {
        return FetchError::PendingUpload;
    }
    match status {
        403 => FetchError::AuthorizationChanged,
        404 | 410 => FetchError::Gone,
        0 => FetchError::Transient("transport error".to_string()),
        s if (500..600).contains(&s) => FetchError::Transient(format!("server status {s}")),
        s => FetchError::Rejected { status: s, code },
    }
}

// ─── Resumable ranged fetch (adverse-hardened) ───────────────────────────────

/// The resumable ranged-fetch engine (slice `S-D10`): stall-cut-resume, bounded
/// transfer windows under `adverse`, and the shared [`RetryEngine`] backoff — the
/// `bulk-transfer` retry class the download-resume ladder instantiates.
///
/// Behavior per the networking doc's adverse-network posture:
/// - **Bounded windows.** Under [`ConnectionClass::Adverse`] each request asks for
///   a bounded [`Range`](crate::net::ADVERSE_RANGE_WINDOW) window so it usually
///   completes between mid-transfer resets; other classes fetch the remainder.
/// - **Stall detection over total timeouts.** A [`StallDetector`] cuts on
///   *no-bytes-for-T* (never a total-duration timeout); the cut abandons the
///   in-flight request and resumes from the persisted offset — re-fetching **zero**
///   bytes already held. A stall emits a [`TransferSignal::Stall`] the caller folds
///   into `adverse` promotion.
/// - **Bounded give-up.** No-progress reads consume the `bulk-transfer` retry
///   budget; a persistently black-holing source gives up
///   ([`FetchError::Exhausted`]) rather than hot-looping.
///
/// The [`MonotonicClock`] and [`Sleeper`] are injected so the whole loop — the
/// stall cut and the backoff — is a deterministic, sleep-free test.
#[derive(Debug, Clone)]
pub struct RangedFetcher<S = TokioSleeper, C = SystemClock> {
    class: ConnectionClass,
    stall: StallConfig,
    window_override: Option<u64>,
    sleeper: S,
    clock: C,
}

impl RangedFetcher {
    /// A production fetcher for the given connection class (real clock + real
    /// tokio sleeper). The window is derived from the class:
    /// [`ConnectionClass::Adverse`] bounds it, others fetch the remainder.
    #[must_use]
    pub fn new(class: ConnectionClass) -> Self {
        Self {
            class,
            stall: StallConfig::default(),
            window_override: None,
            sleeper: TokioSleeper,
            clock: SystemClock::new(),
        }
    }
}

impl<S: Sleeper, C: MonotonicClock> RangedFetcher<S, C> {
    /// Assemble a fetcher from explicit parts — the seam the deterministic
    /// stall-cut-resume test drives (a mock clock + a no-op recording sleeper).
    #[must_use]
    pub fn from_parts(
        class: ConnectionClass,
        stall: StallConfig,
        window_override: Option<u64>,
        sleeper: S,
        clock: C,
    ) -> Self {
        Self {
            class,
            stall,
            window_override,
            sleeper,
            clock,
        }
    }

    /// The bounded `Range` window this fetch uses, or `None` for the open-ended
    /// remainder. An explicit override wins; otherwise the connection class decides.
    #[must_use]
    fn window(&self) -> Option<u64> {
        self.window_override.or_else(|| self.class.range_window())
    }

    /// Fetch a whole blob, resumable and stall-cut, verifying the reassembled
    /// ciphertext against its content address.
    pub async fn fetch<B: BlobSource>(
        &self,
        source: &B,
        hash: &str,
        expected_len: u64,
    ) -> Result<Vec<u8>, FetchError> {
        Ok(self.run(source, hash, expected_len).await?.0)
    }

    /// As [`fetch`](Self::fetch), also returning the [`TransferSignal`] trail (clean
    /// windows and stall cuts) the caller folds into `adverse` promotion.
    pub async fn fetch_with_signals<B: BlobSource>(
        &self,
        source: &B,
        hash: &str,
        expected_len: u64,
    ) -> Result<(Vec<u8>, Vec<TransferSignal>), FetchError> {
        self.run(source, hash, expected_len).await
    }

    #[instrument(skip(self, source), fields(hash, expected_len, class = ?self.class))]
    async fn run<B: BlobSource>(
        &self,
        source: &B,
        hash: &str,
        expected_len: u64,
    ) -> Result<(Vec<u8>, Vec<TransferSignal>), FetchError> {
        let mut buffer: Vec<u8> = Vec::with_capacity(usize::try_from(expected_len).unwrap_or(0));
        let mut signals: Vec<TransferSignal> = Vec::new();
        // The shared retry engine, `bulk-transfer` class: resume-first, patient,
        // backoff between attempts, bounded give-up.
        let mut engine: RetryEngine = RetryClass::BulkTransfer.engine();
        let mut stall = StallDetector::new(self.stall, self.clock.now_millis());
        let window = self.window();

        while (buffer.len() as u64) < expected_len {
            let start = buffer.len() as u64;
            let remaining = expected_len - start;
            let max_len = window.map(|w| w.min(remaining));
            let outcome = source.get_range(hash, start, max_len).await;
            let now = self.clock.now_millis();

            match outcome {
                // Progress: bytes landed. Rearm the stall timer, refresh the retry
                // budget (each resumed window earns a fresh one), and continue —
                // every request resumes from `buffer.len()`, so zero held bytes are
                // ever re-requested.
                RangeOutcome::Complete { bytes } | RangeOutcome::Partial { bytes }
                    if !bytes.is_empty() =>
                {
                    stall.on_progress(now);
                    engine.reset();
                    signals.push(TransferSignal::Clean);
                    tracing::trace!(
                        resumed_from = start,
                        now = start + bytes.len() as u64,
                        "range window landed; resuming from persisted offset"
                    );
                    buffer.extend_from_slice(&bytes);
                }
                // A no-byte read: the request stalled or was black-holed. Cut on
                // no-bytes-for-T (a stall counts toward `adverse`), back off through
                // the shared engine, and resume from the SAME offset — the buffer is
                // untouched, so the resume re-fetches zero duplicate bytes.
                RangeOutcome::Complete { .. } | RangeOutcome::Partial { .. } => {
                    if stall.is_stalled(now) {
                        tracing::debug!(
                            offset = start,
                            stall_after = ?stall.stall_after(),
                            "no-bytes-for-T stall cut; resuming from persisted offset"
                        );
                        signals.push(TransferSignal::Stall);
                    }
                    match engine.next_backoff(None) {
                        RetryDecision::GiveUp => {
                            return Err(FetchError::Exhausted(format!(
                                "no progress fetching {hash} after {} retries",
                                engine.policy().max_retries
                            )));
                        }
                        RetryDecision::Retry { after } => self.sleeper.sleep(after).await,
                    }
                }
                RangeOutcome::Status { status, code } => {
                    return Err(classify_status(status, code));
                }
            }
        }

        // The server can only attest to ciphertext; the client verifies the content
        // address itself before trusting the bytes.
        if hash_hex(&buffer) != hash {
            tracing::warn!(hash, "ciphertext content-hash mismatch; discarding blob");
            return Err(FetchError::IntegrityFailed {
                expected: hash.to_string(),
            });
        }
        Ok((buffer, signals))
    }
}

/// Fetch a whole blob with resumable `Range` windows, verifying the reassembled
/// ciphertext against its content address.
///
/// The unmetered convenience over [`RangedFetcher`]: each request resumes from the
/// current buffer length, so an interruption re-fetches **zero** bytes already
/// held, and on completion the SHA-256 of the assembled ciphertext must equal
/// `hash` or the blob is discarded ([`FetchError::IntegrityFailed`]). A caller on a
/// detected [`ConnectionClass::Adverse`] link builds `RangedFetcher::new(Adverse)`
/// instead to get bounded transfer windows.
pub async fn fetch_blob<S: BlobSource>(
    source: &S,
    hash: &str,
    expected_len: u64,
) -> Result<Vec<u8>, FetchError> {
    RangedFetcher::new(ConnectionClass::Unmetered)
        .fetch(source, hash, expected_len)
        .await
}

// ─── On-demand open with degrade ladder ──────────────────────────────────────

/// Why an asset is shown at a lower representation than requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DegradeReason {
    /// The representation is permanently gone (`410` / purged).
    PermanentlyGone,
    /// Authorization changed (`403`) and, after a membership re-sync, the fetch
    /// still failed — the asset was likely unshared.
    AuthorizationChanged,
    /// A transient failure; the asset stays listed and re-fetches automatically.
    TemporarilyUnavailable,
}

/// The resolution of an on-demand open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchResolution {
    /// The desired representation was fetched.
    Fetched {
        /// Which representation landed.
        representation: Representation,
        /// The ciphertext bytes (integrity-verified).
        bytes: Vec<u8>,
    },
    /// The desired representation is unavailable; the asset is shown at the best
    /// locally-held representation instead. The asset's metadata and index entry
    /// are untouched — nothing is removed.
    Degraded {
        /// The representation shown instead (down to the always-present LQIP).
        shown: Representation,
        /// Why.
        reason: DegradeReason,
    },
    /// The original has not landed yet (`awaiting-original`): show the badge at the
    /// best-held representation, never a failure.
    Pending {
        /// The representation shown while awaiting the original.
        shown: Representation,
    },
}

/// Fetch `desired` for an asset, applying the degrade ladder on failure.
///
/// - Success → [`FetchResolution::Fetched`].
/// - `403` → invoke `on_authorization_change` (re-sync membership/capability),
///   retry once; if it still fails, [`FetchResolution::Degraded`] with
///   [`DegradeReason::AuthorizationChanged`] (a revocation surfaced as such, not
///   masked as a missing file).
/// - `410`/purged → [`FetchResolution::Degraded`] / [`DegradeReason::PermanentlyGone`].
/// - `pending_upload` → [`FetchResolution::Pending`].
/// - transient → [`FetchResolution::Degraded`] / [`DegradeReason::TemporarilyUnavailable`]
///   (the asset stays listed and re-fetches automatically later).
///
/// The degrade target is [`best_available`] over `locally_held`, so it steps down
/// preview → thumbnail → LQIP. The asset's metadata / index entry is never removed
/// — degrade only changes what is *shown*.
#[instrument(skip(source, asset, locally_held, on_authorization_change), fields(?desired))]
pub async fn open_representation<S, H>(
    source: &S,
    asset: &AssetBlobs,
    desired: Representation,
    locally_held: &BTreeSet<Representation>,
    mut on_authorization_change: H,
) -> FetchResolution
where
    S: BlobSource,
    H: AsyncFnMut(),
{
    match fetch_representation(source, asset, desired).await {
        Ok(bytes) => FetchResolution::Fetched {
            representation: desired,
            bytes,
        },
        Err(FetchError::AuthorizationChanged) => {
            tracing::info!("403 on fetch — re-syncing album membership before retrying");
            on_authorization_change().await;
            match fetch_representation(source, asset, desired).await {
                Ok(bytes) => FetchResolution::Fetched {
                    representation: desired,
                    bytes,
                },
                Err(_) => degrade(desired, locally_held, DegradeReason::AuthorizationChanged),
            }
        }
        Err(FetchError::Gone) => degrade(desired, locally_held, DegradeReason::PermanentlyGone),
        Err(FetchError::PendingUpload) => FetchResolution::Pending {
            shown: best_available(desired, locally_held).unwrap_or(Representation::Lqip),
        },
        Err(_) => degrade(desired, locally_held, DegradeReason::TemporarilyUnavailable),
    }
}

fn degrade(
    desired: Representation,
    locally_held: &BTreeSet<Representation>,
    reason: DegradeReason,
) -> FetchResolution {
    let shown = best_available(desired, locally_held).unwrap_or(Representation::Lqip);
    tracing::info!(
        ?shown,
        ?reason,
        "degrading to best locally-held representation"
    );
    FetchResolution::Degraded { shown, reason }
}

/// Fetch exactly one representation's blob (no degrade). A representation the asset
/// does not carry is treated as permanently gone (degrade upstream).
async fn fetch_representation<S: BlobSource>(
    source: &S,
    asset: &AssetBlobs,
    representation: Representation,
) -> Result<Vec<u8>, FetchError> {
    let blob = asset.blob_for(representation).ok_or(FetchError::Gone)?;
    fetch_blob(source, &blob.hash, blob.size).await
}

// ─── Production HTTP source ──────────────────────────────────────────────────

/// The production ranged blob source: `reqwest` `GET /blob/{hash}` with a `Range`
/// header, authorized through the `S-D7` session. rustls only.
///
/// The mid-stream-drop resume path ([`RangeOutcome::Partial`]) is exercised by the
/// module's mocks; this impl reads each `Range` window whole (a clean drop surfaces
/// as a transport error and the loop resumes from the persisted offset), and the
/// streaming-partial wiring lands with the media-serving slice.
#[derive(Clone)]
pub struct HttpBlobSource {
    session: crate::auth::Session,
    base_url: String,
}

impl HttpBlobSource {
    /// Build a source against the media server's blob endpoint root (no trailing
    /// slash); a blob is fetched from `{base_url}/blob/{hash}`.
    pub fn new(session: crate::auth::Session, base_url: impl Into<String>) -> Self {
        Self {
            session,
            base_url: base_url.into().trim_end_matches('/').to_string(),
        }
    }
}

impl BlobSource for HttpBlobSource {
    async fn get_range(&self, hash: &str, start: u64, max_len: Option<u64>) -> RangeOutcome {
        let url = format!("{}/blob/{hash}", self.base_url);
        // Under `adverse` the fetcher passes a bounded window; otherwise the
        // open-ended remainder. A zero-length window would be malformed — treat it
        // as open-ended (the loop never asks for zero when bytes remain).
        let range = match max_len {
            Some(len) if len > 0 => format!("bytes={start}-{}", start + len - 1),
            _ => format!("bytes={start}-"),
        };
        let response = match self
            .session
            .execute(|http| http.get(&url).header(reqwest::header::RANGE, &range))
            .await
        {
            Ok(response) => response,
            Err(error) => {
                tracing::debug!(%error, "blob range request transport error");
                return RangeOutcome::Status {
                    status: 0,
                    code: None,
                };
            }
        };

        let status = response.status().as_u16();
        if (200..300).contains(&status) {
            match response.bytes().await {
                Ok(bytes) => RangeOutcome::Complete {
                    bytes: bytes.to_vec(),
                },
                // A mid-stream drop reading the body: resume from the persisted
                // offset (no bytes are known to have landed here).
                Err(_) => RangeOutcome::Partial { bytes: Vec::new() },
            }
        } else {
            // The stable `error.*` code (e.g. `error.blob.pending_upload`) rides
            // the `x-capsule-error-code` header, mirroring the sync/upload surfaces.
            let code = response
                .headers()
                .get("x-capsule-error-code")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            RangeOutcome::Status { status, code }
        }
    }
}

/// SHA-256 of the ciphertext as bare lowercase hex — byte-identical to the
/// server's content address (`capsule_core::crypto::hash::hash_bytes`).
fn hash_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests;
