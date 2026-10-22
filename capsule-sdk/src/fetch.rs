//! Tiered, on-demand **blob fetch** (slice `S-D2`; SSoT: [Download & Sync]).
//!
//! Where [`crate::sync`] discovers *what* changed, this module fetches *the
//! smallest representation that satisfies the user's current intent, and nothing
//! more*. It owns four things the transfer download doc specifies:
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
//!   of restarting, and re-fetches zero bytes it already holds. [`fetch_blob_into`]
//!   streams the same loop into a writer in bounded windows, so an original of any size
//!   is fetched — and, through a `StreamDecryptor` sink, restored — in bounded memory.
//! - **Self-verification.** The client recomputes the ciphertext content hash
//!   against the requested content address; any mismatch discards the blob.
//! - **Opening a derivative.** Derivative bytes are ciphertext on the server
//!   ([Encryption]). [`open_representation`] decrypts a fetched thumbnail or preview with
//!   the material its signed `DerivativeManifest` records — the epoch, the source asset
//!   id and the nonce prefix — after running the closed still-format check a receiver
//!   owes a manifest it did not author. An AEAD failure is a corrupt blob: it is
//!   discarded and the open degrades so the caller regenerates from the original. A
//!   tier whose manifest carries the `format = "original"` sentinel is satisfied by the
//!   original and is never fetched.
//! - **Admitting derivative manifests.** [`verify_derivative_manifests`] decodes the exact
//!   bytes a feed entry carries and runs each through core's `verify_derivative` chokepoint,
//!   walking each `(asset, role)` chain from its start. Only what it accepts goes into
//!   [`AssetBlobs::derivative_manifests`]; what fails is quarantined and returned, never
//!   dropped.
//!
//! The [`BlobSource`] seam abstracts the wire so the resume loop and degrade ladder
//! are driven by deterministic mocks; [`HttpBlobSource`] is the production
//! `reqwest`-over-`Range` implementation against the media server's `GET /blob/{hash}`.
//!
//! [Download & Sync]: https://docs/design/transfer/download/
//! [Encryption]: https://docs/design/cryptography/encryption/

use std::collections::{BTreeMap, BTreeSet};

use capsule_core::cbor;
use capsule_core::crypto::authority::AlbumAuthority;
use capsule_core::crypto::encryption::{NONCE_PREFIX_LEN, decrypt_asset_vec};
use capsule_core::crypto::hash::Hash32;
use capsule_core::crypto::keys::{Amk, AmkVersion, DeviceDirectory};
use capsule_core::crypto::provenance::{DerivativeManifest, DerivativeRole};
use capsule_core::crypto::verify_asset::{RejectReason, VerifyOutcome};
use capsule_core::crypto::verify_derivative::verify_derivative;
use capsule_core::derivative_format::{DerivativeFormat, verify_still_format};
use capsule_i18n::error_codes;
use sha2::{Digest, Sha256};
use tracing::instrument;

use crate::events::{TransferEvent, TransferEvents, millis};
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
/// manifest, plus the signed derivative manifests that open them. LQIP is not here —
/// it rides the metadata blob.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssetBlobs {
    /// The thumbnail derivative, when present.
    pub thumbnail: Option<RepBlob>,
    /// The preview derivative, when present.
    pub preview: Option<RepBlob>,
    /// The original, when present (absent while `awaiting-original`).
    pub original: Option<RepBlob>,
    /// The `DerivativeManifest`s of each derivative role this asset carries, already verified:
    /// [`verify_derivative_manifests`]'s [`VerifiedDerivatives::accepted`], which ran each
    /// through core's `verify_derivative` chokepoint. The open path checks nothing a signature
    /// covers.
    ///
    /// A derivative's ciphertext is opened with `AMK(amk_version).derive_file_key(
    /// source_asset_id, nonce_prefix)`, and those three values live here and nowhere else: the
    /// blob manifest carries only content addresses. A thumbnail or preview with no manifest
    /// describing its blob is not opened ([`DegradeReason::NoManifest`]).
    ///
    /// A receiver gets them from the sync feed, as the exact signed bytes in
    /// [`FeedEntry::derivative_manifests`](crate::sync::FeedEntry::derivative_manifests); they
    /// are decoded and verified before they are put here.
    pub derivative_manifests: Vec<DerivativeManifest>,
}

impl AssetBlobs {
    /// The manifest that describes `role`'s tier: the one committing to the blob the feed
    /// names, else a `format = "original"` sentinel, else whichever manifest of that role
    /// comes first (which the caller's [`open_representation`] then rejects as not describing
    /// the blob).
    fn manifest_for(
        &self,
        role: DerivativeRole,
        blob: Option<&RepBlob>,
    ) -> Option<&DerivativeManifest> {
        let of_role = || {
            self.derivative_manifests
                .iter()
                .filter(move |manifest| manifest.core.role == role)
        };
        blob.and_then(|blob| {
            of_role().find(|manifest| manifest.core.ciphertext_hash.to_hex() == blob.hash)
        })
        .or_else(|| {
            of_role().find(|manifest| manifest.core.format == DerivativeFormat::Original.mime())
        })
        .or_else(|| of_role().next())
    }

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

// ─── Admitting derivative manifests ──────────────────────────────────────────

/// Why a received derivative manifest was quarantined rather than admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DerivativeQuarantine {
    /// The bytes are not a `DerivativeManifest` in CBOR.
    Undecodable,
    /// Core's `verify_derivative` refused it. [`RejectReason::ForgedChain`] also covers a
    /// manifest no chain from its role's start reaches, and every manifest at a fork — two
    /// that name the same predecessor, which a receiver cannot choose between.
    Rejected(RejectReason),
}

/// One quarantined derivative manifest: surfaced with its reason, never silently dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantinedDerivative {
    /// The manifest blob's content address (lowercase hex of the digest of its exact bytes) —
    /// the same address [`crate::staged::held_from_feed`] names it by.
    pub address: String,
    /// Why it was quarantined.
    pub reason: DerivativeQuarantine,
}

/// The outcome of [`verify_derivative_manifests`] over one feed entry's manifests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VerifiedDerivatives {
    /// Accepted manifests, in chain order per role — what [`AssetBlobs::derivative_manifests`]
    /// takes.
    pub accepted: Vec<DerivativeManifest>,
    /// Manifests that verify but whose epoch AMK has not arrived locally: hold and retry once
    /// the key lands, never quarantine. They still advance their role's chain, so a later
    /// manifest of the same role is placed behind them.
    pub pending: Vec<DerivativeManifest>,
    /// Manifests that failed, each with its reason.
    pub quarantined: Vec<QuarantinedDerivative>,
}

/// The roles walked, in a fixed order so the output is deterministic.
const DERIVATIVE_ROLES: [DerivativeRole; 3] = [
    DerivativeRole::Thumbnail,
    DerivativeRole::Preview,
    DerivativeRole::Embedding,
];

/// Decode and verify a feed entry's derivative manifests
/// ([`FeedEntry::derivative_manifests`](crate::sync::FeedEntry::derivative_manifests)) for the
/// asset `asset_id`, so that only verified ones reach [`AssetBlobs::derivative_manifests`].
///
/// The feed serves **every** manifest the asset has accumulated, in content-address order —
/// not in chain order, and not only the heads — so each role's chain is rebuilt from its start
/// here rather than resumed from a stored head: beginning with the manifest whose
/// `prior_provenance_hash` is `None`, each next link is the one manifest naming the previous
/// one's [`link_hash`](DerivativeManifest::link_hash), and each link is checked by core's
/// `verify_derivative` against exactly that head.
///
/// The walk of a role stops at the first link that is refused, and at a fork. What it never
/// reached is quarantined with the reason `verify_derivative` gives it against the head the walk
/// stopped at. So a poisoned `derivative-replace` — one that does not chain to the role's head —
/// is quarantined and the head it tried to displace stays accepted: the existing derivative is
/// preserved.
///
/// `directory` is the published directory of the account whose devices generate this asset's
/// derivatives (as for `verify_asset`, the caller resolves whose it is); `authority` is the
/// album's MLS-attested authority.
#[instrument(
    level = "debug",
    skip(encoded, directory, authority),
    fields(%asset_id, manifests = encoded.len())
)]
pub fn verify_derivative_manifests(
    encoded: &[Vec<u8>],
    asset_id: uuid::Uuid,
    directory: &DeviceDirectory,
    authority: &dyn AlbumAuthority,
) -> VerifiedDerivatives {
    let mut out = VerifiedDerivatives::default();
    let mut decoded: Vec<(String, DerivativeManifest)> = Vec::with_capacity(encoded.len());
    for bytes in encoded {
        let address = hash_hex(bytes);
        match cbor::from_slice::<DerivativeManifest>(bytes) {
            Ok(manifest) => decoded.push((address, manifest)),
            Err(error) => {
                tracing::warn!(%address, %error, "derivative manifest is undecodable; quarantined");
                out.quarantined.push(QuarantinedDerivative {
                    address,
                    reason: DerivativeQuarantine::Undecodable,
                });
            }
        }
    }
    for role in DERIVATIVE_ROLES {
        let remaining: Vec<(String, DerivativeManifest)> = decoded
            .iter()
            .filter(|(_, manifest)| manifest.core.role == role)
            .cloned()
            .collect();
        walk_role_chain(remaining, asset_id, directory, authority, &mut out);
    }
    tracing::debug!(
        accepted = out.accepted.len(),
        pending = out.pending.len(),
        quarantined = out.quarantined.len(),
        "derivative manifests verified"
    );
    out
}

/// Walk one role's chain from its start, sorting each manifest into `out`.
fn walk_role_chain(
    mut remaining: Vec<(String, DerivativeManifest)>,
    asset_id: uuid::Uuid,
    directory: &DeviceDirectory,
    authority: &dyn AlbumAuthority,
    out: &mut VerifiedDerivatives,
) {
    let quarantine = |out: &mut VerifiedDerivatives, address: String, reason: RejectReason| {
        tracing::warn!(%address, ?reason, "derivative manifest rejected; quarantined");
        out.quarantined.push(QuarantinedDerivative {
            address,
            reason: DerivativeQuarantine::Rejected(reason),
        });
    };
    let mut head: Option<Hash32> = None;
    loop {
        let (mut next, rest): (Vec<_>, Vec<_>) = remaining
            .into_iter()
            .partition(|(_, manifest)| manifest.core.prior_provenance_hash == head);
        remaining = rest;
        let (address, manifest) = match (next.pop(), next.is_empty()) {
            (Some(link), true) => link,
            // No successor (the chain ends), or a fork: links naming one predecessor are all
            // refused, since nothing a receiver holds can say which is the real one.
            (popped, _) => {
                for (address, _) in popped.into_iter().chain(next) {
                    quarantine(out, address, RejectReason::ForgedChain);
                }
                break;
            }
        };
        match verify_derivative(&manifest, asset_id, directory, authority, head) {
            VerifyOutcome::Accept => {
                head = Some(manifest.link_hash());
                out.accepted.push(manifest);
            }
            VerifyOutcome::Pending(_) => {
                head = Some(manifest.link_hash());
                out.pending.push(manifest);
            }
            VerifyOutcome::TerminalReject(reason) => {
                quarantine(out, address, reason);
                break;
            }
        }
    }
    // Whatever the walk never reached: report why, against the head it stopped at.
    for (address, manifest) in remaining {
        let reason = match verify_derivative(&manifest, asset_id, directory, authority, head) {
            VerifyOutcome::TerminalReject(reason) => reason,
            VerifyOutcome::Accept | VerifyOutcome::Pending(_) => RejectReason::ForgedChain,
        };
        quarantine(out, address, reason);
    }
}

/// The album keys a reader holds, by epoch — what opens a derivative.
///
/// A seam rather than a concrete key store, so the SDK never owns AMKs: the caller's
/// workspace answers from its album store, and tests answer from a map.
pub trait AmkSource {
    /// The AMK for `version`, or `None` when this reader does not hold that epoch.
    fn amk(&self, version: AmkVersion) -> Option<Amk>;
}

impl AmkSource for BTreeMap<AmkVersion, Amk> {
    fn amk(&self, version: AmkVersion) -> Option<Amk> {
        self.get(&version).cloned()
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
    /// The session could not authorize the request: its refresh was refused or it holds no
    /// tokens. Retrying cannot help; the user has to sign in again.
    Unauthenticated {
        /// The stable `error.*` code, when the refusal carried one.
        code: Option<String>,
        /// English detail (developer/log message).
        message: String,
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
    ///
    /// Carries the stable code, because the `403`s on this route no longer say one thing: an
    /// account's `error.blob.access_revoked` means re-sync membership, while a federated
    /// peer's `error.federation.scope_insufficient` means the grant never covered this blob and
    /// re-syncing anything will not change that. A caller that could not tell them apart would
    /// retry the second forever.
    #[error("blob authorization changed")]
    AuthorizationChanged {
        /// Stable `error.*` code, when the refusal carried one.
        code: Option<String>,
    },
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
    /// The session could not authorize the fetch — a refused refresh, no tokens, or a `401`
    /// that survived the session's one refresh-and-retry. Not transient: the caller signs in
    /// again rather than retrying.
    #[error("blob fetch unauthenticated ({code:?}): {message}")]
    Unauthenticated {
        /// Stable `error.*` code, when the refusal carried one.
        code: Option<String>,
        /// English detail (developer/log message).
        message: String,
    },
    /// The bounded resume budget was spent without completing the transfer.
    #[error("fetch gave up after exhausting the resume budget: {0}")]
    Exhausted(String),
    /// A blob fetched without a known size ([`RangedFetcher::fetch_unsized`]) kept going past
    /// the caller's limit; the bytes held are discarded rather than grown without bound.
    #[error("blob exceeds the {limit}-byte limit for a blob of unlisted size")]
    TooLarge {
        /// The limit the caller set, in bytes.
        limit: u64,
    },
    /// The destination of a streamed fetch ([`RangedFetcher::fetch_into`]) refused bytes: a
    /// local write failed, or a decrypting sink rejected a chunk. Whatever the sink holds is
    /// unverified and must be discarded.
    #[error("writing the fetched blob failed: {0}")]
    Sink(#[source] std::io::Error),
    /// A derivative's ciphertext hashed correctly but failed AEAD authentication under the
    /// key its manifest selects — a corrupt blob, discarded.
    #[error("derivative {hash} failed authentication; discarded")]
    Corrupt {
        /// The content address that would not open.
        hash: String,
    },
}

impl FetchError {
    /// The stable `error.*` catalog code, when one applies.
    #[must_use]
    pub fn error_code(&self) -> Option<&str> {
        match self {
            Self::PendingUpload => Some(error_codes::BLOB_PENDING_UPLOAD),
            Self::AuthorizationChanged { code }
            | Self::Rejected { code, .. }
            | Self::Unauthenticated { code, .. } => code.as_deref(),
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
        // The session already refreshed once and retried; a second `401` is the credential.
        401 => FetchError::Unauthenticated {
            code,
            message: "the server refused the refreshed credential".to_string(),
        },
        403 => FetchError::AuthorizationChanged { code },
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
    events: TransferEvents,
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
            events: TransferEvents::disabled(),
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
            events: TransferEvents::disabled(),
        }
    }

    /// Publish every fetch this fetcher drives to `events` (range requested and received,
    /// retry, verified, integrity-rejected). Without a subscriber it costs nothing.
    #[must_use]
    pub fn with_events(mut self, events: TransferEvents) -> Self {
        self.events = events;
        self
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

    /// Fetch a whole blob into `sink` as it arrives, never holding more than one `Range`
    /// window of it — the path an original takes, at any size the server accepts.
    ///
    /// Each request asks for at most [`SINK_WINDOW`] bytes (or the class's smaller window), so
    /// the memory a fetch holds is bounded by the window rather than the blob. The content
    /// address is computed incrementally and checked once the last byte has landed: until this
    /// returns `Ok`, what `sink` holds is **unverified** and a caller writing to a file must
    /// discard it on any error. Composing the sink with a
    /// [`StreamDecryptor`](capsule_core::crypto::encryption::StreamDecryptor) restores an
    /// original to plaintext in the same bounded memory, each STREAM chunk authenticated
    /// before it is written.
    pub async fn fetch_into<B: BlobSource, W: std::io::Write>(
        &self,
        source: &B,
        hash: &str,
        expected_len: u64,
        sink: &mut W,
    ) -> Result<Vec<TransferSignal>, FetchError> {
        let window = Some(self.window().map_or(SINK_WINDOW, |w| w.min(SINK_WINDOW)));
        self.run_into(source, hash, expected_len, window, sink)
            .await
    }

    /// Fetch a whole blob whose size nobody listed, e.g. a metadata blob the sync feed names by
    /// address only, verifying it against its content address.
    ///
    /// Each request asks for a bounded window ([`UNSIZED_WINDOW`], or the class's smaller one).
    /// The blob ends at the first window that comes back short, or at a `416` for a range that
    /// starts exactly at its end. No-progress reads back off through the same `bulk-transfer`
    /// retry engine and status classification as [`fetch`](Self::fetch). The blob may not grow
    /// past `limit` bytes: a source that keeps answering full windows gets
    /// [`FetchError::TooLarge`], never an unbounded buffer.
    #[instrument(skip(self, source), fields(class = ?self.class))]
    pub async fn fetch_unsized<B: BlobSource>(
        &self,
        source: &B,
        hash: &str,
        limit: u64,
    ) -> Result<Vec<u8>, FetchError> {
        let window = self
            .window()
            .map_or(UNSIZED_WINDOW, |w| w.min(UNSIZED_WINDOW))
            .max(1);
        let mut bytes: Vec<u8> = Vec::new();
        let mut engine: RetryEngine = RetryClass::BulkTransfer.engine();
        let mut stall = StallDetector::new(self.stall, self.clock.now_millis());
        loop {
            let start = bytes.len() as u64;
            self.emit_requested(hash, start, Some(window));
            let outcome = source.get_range(hash, start, Some(window)).await;
            let now = self.clock.now_millis();
            match outcome {
                RangeOutcome::Complete { bytes: landed } if !landed.is_empty() || start > 0 => {
                    if !landed.is_empty() {
                        self.emit_received(hash, start, landed.len() as u64);
                    }
                    let short = (landed.len() as u64) < window;
                    bytes.extend_from_slice(&landed);
                    if bytes.len() as u64 > limit {
                        return Err(FetchError::TooLarge { limit });
                    }
                    if short {
                        break;
                    }
                    stall.on_progress(now);
                    engine.reset();
                }
                RangeOutcome::Partial { bytes: landed } if !landed.is_empty() => {
                    self.emit_received(hash, start, landed.len() as u64);
                    bytes.extend_from_slice(&landed);
                    if bytes.len() as u64 > limit {
                        return Err(FetchError::TooLarge { limit });
                    }
                    stall.on_progress(now);
                    engine.reset();
                }
                RangeOutcome::Complete { .. } | RangeOutcome::Partial { .. } => {
                    let stalled = stall.is_stalled(now);
                    if stalled {
                        tracing::debug!(offset = start, "unsized fetch stalled; resuming");
                    }
                    match engine.next_backoff(None) {
                        RetryDecision::GiveUp => {
                            return Err(FetchError::Exhausted(format!(
                                "no progress fetching {hash} after {} retries",
                                engine.policy().max_retries
                            )));
                        }
                        RetryDecision::Retry { after } => {
                            self.emit_retried(hash, start, stalled, after);
                            self.sleeper.sleep(after).await;
                        }
                    }
                }
                RangeOutcome::Status { status: 416, .. } if start > 0 => break,
                RangeOutcome::Status { status, code } => {
                    return Err(classify_status(status, code));
                }
                RangeOutcome::Unauthenticated { code, message } => {
                    tracing::warn!(hash, ?code, %message, "blob fetch unauthenticated; not retrying");
                    return Err(FetchError::Unauthenticated { code, message });
                }
            }
        }
        if hash_hex(&bytes) != hash {
            tracing::warn!(hash, "unsized blob content-hash mismatch; discarding");
            self.emit_verdict(hash, bytes.len() as u64, false);
            return Err(FetchError::IntegrityFailed {
                expected: hash.to_string(),
            });
        }
        self.emit_verdict(hash, bytes.len() as u64, true);
        tracing::debug!(hash, bytes = bytes.len(), "unsized blob fetched");
        Ok(bytes)
    }

    async fn run<B: BlobSource>(
        &self,
        source: &B,
        hash: &str,
        expected_len: u64,
    ) -> Result<(Vec<u8>, Vec<TransferSignal>), FetchError> {
        let mut buffer: Vec<u8> = Vec::with_capacity(usize::try_from(expected_len).unwrap_or(0));
        let signals = self
            .run_into(source, hash, expected_len, self.window(), &mut buffer)
            .await?;
        Ok((buffer, signals))
    }

    #[instrument(skip(self, source, sink), fields(hash, expected_len, ?window, class = ?self.class))]
    async fn run_into<B: BlobSource, W: std::io::Write>(
        &self,
        source: &B,
        hash: &str,
        expected_len: u64,
        window: Option<u64>,
        sink: &mut W,
    ) -> Result<Vec<TransferSignal>, FetchError> {
        let mut received: u64 = 0;
        let mut hasher = Sha256::new();
        let mut signals: Vec<TransferSignal> = Vec::new();
        // The shared retry engine, `bulk-transfer` class: resume-first, patient,
        // backoff between attempts, bounded give-up.
        let mut engine: RetryEngine = RetryClass::BulkTransfer.engine();
        let mut stall = StallDetector::new(self.stall, self.clock.now_millis());

        while received < expected_len {
            let start = received;
            let remaining = expected_len - start;
            let max_len = window.map(|w| w.min(remaining));
            self.emit_requested(hash, start, max_len);
            let outcome = source.get_range(hash, start, max_len).await;
            let now = self.clock.now_millis();

            match outcome {
                // Progress: bytes landed. Rearm the stall timer, refresh the retry
                // budget (each resumed window earns a fresh one), and continue —
                // every request resumes from the bytes received, so zero held bytes are
                // ever re-requested.
                RangeOutcome::Complete { bytes } | RangeOutcome::Partial { bytes }
                    if !bytes.is_empty() =>
                {
                    stall.on_progress(now);
                    engine.reset();
                    self.emit_received(hash, start, bytes.len() as u64);
                    signals.push(TransferSignal::Clean);
                    tracing::trace!(
                        resumed_from = start,
                        now = start + bytes.len() as u64,
                        "range window landed; resuming from persisted offset"
                    );
                    hasher.update(&bytes);
                    sink.write_all(&bytes).map_err(FetchError::Sink)?;
                    received += bytes.len() as u64;
                }
                // A no-byte read: the request stalled or was black-holed. Cut on
                // no-bytes-for-T (a stall counts toward `adverse`), back off through
                // the shared engine, and resume from the SAME offset — the sink is
                // untouched, so the resume re-fetches zero duplicate bytes.
                RangeOutcome::Complete { .. } | RangeOutcome::Partial { .. } => {
                    let stalled = stall.is_stalled(now);
                    if stalled {
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
                        RetryDecision::Retry { after } => {
                            self.emit_retried(hash, start, stalled, after);
                            self.sleeper.sleep(after).await;
                        }
                    }
                }
                RangeOutcome::Status { status, code } => {
                    return Err(classify_status(status, code));
                }
                RangeOutcome::Unauthenticated { code, message } => {
                    tracing::warn!(hash, ?code, %message, "blob fetch unauthenticated; not retrying");
                    return Err(FetchError::Unauthenticated { code, message });
                }
            }
        }

        // The server can only attest to ciphertext; the client verifies the content
        // address itself before trusting the bytes.
        if hex::encode(hasher.finalize()) != hash {
            tracing::warn!(hash, "ciphertext content-hash mismatch; discarding blob");
            self.emit_verdict(hash, received, false);
            return Err(FetchError::IntegrityFailed {
                expected: hash.to_string(),
            });
        }
        sink.flush().map_err(FetchError::Sink)?;
        self.emit_verdict(hash, received, true);
        Ok(signals)
    }

    fn emit_requested(&self, hash: &str, offset: u64, max_len: Option<u64>) {
        self.events.emit(|| TransferEvent::RangeRequested {
            blob: hash.to_string(),
            offset,
            max_len,
        });
    }

    fn emit_received(&self, hash: &str, offset: u64, len: u64) {
        self.events.emit(|| TransferEvent::RangeReceived {
            blob: hash.to_string(),
            offset,
            len,
        });
    }

    fn emit_retried(&self, hash: &str, offset: u64, stalled: bool, wait: std::time::Duration) {
        self.events.emit(|| TransferEvent::FetchRetried {
            blob: hash.to_string(),
            offset,
            stalled,
            wait_ms: millis(wait),
        });
    }

    /// The content-address check's outcome over `size` fetched bytes.
    fn emit_verdict(&self, hash: &str, size: u64, verified: bool) {
        self.events.emit(|| {
            let blob = hash.to_string();
            if verified {
                TransferEvent::FetchVerified { blob, size }
            } else {
                TransferEvent::IntegrityRejected { blob, size }
            }
        });
    }
}

/// The `Range` window [`RangedFetcher::fetch_unsized`] asks for (64 KiB). A sealed sidecar is a
/// few KiB, so one window is the usual whole fetch.
pub const UNSIZED_WINDOW: u64 = 64 * 1024;

/// The largest `Range` window [`RangedFetcher::fetch_into`] asks for: the memory a streamed
/// fetch holds is one of these, whatever the blob's size. It is the upload protocol's largest
/// chunk ([`PROTOCOL_MAX_CHUNK`](crate::upload::PROTOCOL_MAX_CHUNK), 16 MiB), so a transfer
/// holds the same bound in either direction.
pub const SINK_WINDOW: u64 = crate::upload::PROTOCOL_MAX_CHUNK;

/// Fetch a whole blob into `sink` with bounded `Range` windows, verifying it against its
/// content address — the unmetered convenience over [`RangedFetcher::fetch_into`], and the
/// bounded-memory counterpart of [`fetch_blob`]. `sink` holds unverified bytes until this
/// returns `Ok`.
pub async fn fetch_blob_into<S: BlobSource, W: std::io::Write>(
    source: &S,
    hash: &str,
    expected_len: u64,
    sink: &mut W,
) -> Result<(), FetchError> {
    RangedFetcher::new(ConnectionClass::Unmetered)
        .fetch_into(source, hash, expected_len, sink)
        .await
        .map(drop)
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
    /// No signed derivative manifest describes the blob the feed names, so there is no key
    /// to open it with. Nothing was fetched. Recovery-first: the derivative is rebuildable
    /// from the original.
    NoManifest,
    /// The derivative's manifest names a still format outside the closed set, or records no
    /// AMK epoch — a structural rejection of a manifest this reader did not author. Nothing
    /// was fetched.
    ManifestRejected,
    /// This reader does not hold the AMK epoch the manifest names. Nothing was fetched.
    KeyUnavailable,
    /// The ciphertext matched its content address but failed authentication under the key its
    /// manifest selects; the copy was discarded. Recovery-first: regenerate from the original.
    Corrupt,
}

/// The resolution of an on-demand open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchResolution {
    /// A thumbnail or preview was fetched, integrity-verified, and **decrypted**.
    Opened {
        /// Which representation landed.
        representation: Representation,
        /// The derivative's plaintext (an encoded still in the manifest's format).
        plaintext: Vec<u8>,
    },
    /// The original was fetched. Its bytes stay **ciphertext**: the original opens through its
    /// signed `AssetManifest` (which may carry a wrapped file key), not through this module.
    Fetched {
        /// Which representation landed (always [`Representation::Original`]).
        representation: Representation,
        /// The ciphertext bytes (integrity-verified).
        bytes: Vec<u8>,
    },
    /// The tier's signed manifest carries the `format = "original"` sentinel: the tier
    /// *references* the original rather than holding bytes of its own, so it is satisfied by
    /// the original and nothing was fetched. Never a missing derivative.
    ReferencesOriginal {
        /// The representation that the original satisfies.
        representation: Representation,
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
/// Before any byte moves, a thumbnail or preview is resolved against its signed manifest
/// in [`AssetBlobs::derivative_manifests`]:
///
/// - `format = "original"` → [`FetchResolution::ReferencesOriginal`], no fetch.
/// - a format outside the closed still set, or no AMK epoch →
///   [`DegradeReason::ManifestRejected`], no fetch (the receiver half of the tier table's
///   "every receiver compares `DerivativeManifest.format` against this list").
/// - no manifest committing to the blob the feed names → [`DegradeReason::NoManifest`].
/// - an epoch `keys` does not hold → [`DegradeReason::KeyUnavailable`].
///
/// Then the blob is fetched:
///
/// - Success → [`FetchResolution::Opened`] (a derivative, decrypted) or
///   [`FetchResolution::Fetched`] (the original, still ciphertext).
/// - AEAD failure on a derivative → the copy is discarded, [`DegradeReason::Corrupt`].
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
#[instrument(
    skip(source, asset, locally_held, keys, on_authorization_change),
    fields(?desired)
)]
pub async fn open_representation<S, K, H>(
    source: &S,
    asset: &AssetBlobs,
    desired: Representation,
    locally_held: &BTreeSet<Representation>,
    keys: &K,
    mut on_authorization_change: H,
) -> FetchResolution
where
    S: BlobSource,
    K: AmkSource,
    H: AsyncFnMut(),
{
    let opening = match plan_opening(asset, desired, keys) {
        Ok(opening) => opening,
        Err(PreFetch::ReferencesOriginal) => {
            tracing::debug!("tier carries the `original` sentinel; satisfied without a fetch");
            return FetchResolution::ReferencesOriginal {
                representation: desired,
            };
        }
        Err(PreFetch::Degrade(reason)) => return degrade(desired, locally_held, reason),
    };
    let opening = opening.as_ref();
    match fetch_representation(source, asset, desired, opening).await {
        Ok(bytes) => landed(desired, opening, bytes),
        Err(FetchError::AuthorizationChanged { .. }) => {
            tracing::info!("403 on fetch — re-syncing album membership before retrying");
            on_authorization_change().await;
            match fetch_representation(source, asset, desired, opening).await {
                Ok(bytes) => landed(desired, opening, bytes),
                // The re-sync admitted the fetch; the bytes themselves would not open.
                Err(FetchError::Corrupt { .. }) => {
                    degrade(desired, locally_held, DegradeReason::Corrupt)
                }
                Err(_) => degrade(desired, locally_held, DegradeReason::AuthorizationChanged),
            }
        }
        Err(FetchError::Gone) => degrade(desired, locally_held, DegradeReason::PermanentlyGone),
        Err(FetchError::PendingUpload) => FetchResolution::Pending {
            shown: best_available(desired, locally_held).unwrap_or(Representation::Lqip),
        },
        Err(FetchError::Corrupt { .. }) => degrade(desired, locally_held, DegradeReason::Corrupt),
        Err(_) => degrade(desired, locally_held, DegradeReason::TemporarilyUnavailable),
    }
}

/// The key material that opens one derivative's ciphertext, resolved from its manifest.
struct Opening {
    file_key: [u8; 32],
    nonce_prefix: [u8; NONCE_PREFIX_LEN],
}

/// An open that resolves before any byte is fetched.
enum PreFetch {
    /// The tier's manifest is the `original` sentinel.
    ReferencesOriginal,
    /// The tier cannot be opened; degrade for this reason.
    Degrade(DegradeReason),
}

/// The derivative role a ladder rung is backed by, or `None` for a rung that is not a
/// derivative (LQIP rides the metadata blob; the original opens through its asset manifest).
fn derivative_role(representation: Representation) -> Option<DerivativeRole> {
    match representation {
        Representation::Thumbnail => Some(DerivativeRole::Thumbnail),
        Representation::Preview => Some(DerivativeRole::Preview),
        Representation::Lqip | Representation::Original => None,
    }
}

/// Resolve how `desired` will be opened, before fetching anything. `Ok(None)` is a rung that
/// is fetched as-is (the original; LQIP, which then degrades as it always has).
fn plan_opening<K: AmkSource>(
    asset: &AssetBlobs,
    desired: Representation,
    keys: &K,
) -> Result<Option<Opening>, PreFetch> {
    let Some(role) = derivative_role(desired) else {
        return Ok(None);
    };
    let blob = asset.blob_for(desired);
    let Some(manifest) = asset.manifest_for(role, blob) else {
        // No manifest and no blob is the absent derivative it always was: the fetch answers
        // `Gone` and the ladder degrades. A blob nobody describes cannot be opened.
        return if blob.is_none() {
            Ok(None)
        } else {
            tracing::warn!(
                ?role,
                "no derivative manifest describes this blob; not fetching"
            );
            Err(PreFetch::Degrade(DegradeReason::NoManifest))
        };
    };
    let core = &manifest.core;
    match verify_still_format(manifest) {
        Ok(Some(DerivativeFormat::Original)) => return Err(PreFetch::ReferencesOriginal),
        Ok(Some(_)) => {}
        // `None` is an embedding-role manifest, which `manifest_for` never returns for a
        // still rung; reject it rather than guess.
        Ok(None) | Err(_) => {
            tracing::warn!(?role, format = %core.format, "derivative format outside the closed set");
            return Err(PreFetch::Degrade(DegradeReason::ManifestRejected));
        }
    }
    let Some(blob) = blob else {
        // A real derivative the server does not list: permanently unavailable.
        return Ok(None);
    };
    if core.ciphertext_hash.to_hex() != blob.hash {
        tracing::warn!(
            ?role,
            "derivative manifest commits to different bytes than the feed names"
        );
        return Err(PreFetch::Degrade(DegradeReason::NoManifest));
    }
    let Some(version) = core.amk_version else {
        tracing::warn!(?role, "derivative manifest records no AMK epoch");
        return Err(PreFetch::Degrade(DegradeReason::ManifestRejected));
    };
    let Some(amk) = keys.amk(version) else {
        tracing::info!(
            ?role,
            epoch = version.0,
            "AMK epoch not held; cannot open derivative"
        );
        return Err(PreFetch::Degrade(DegradeReason::KeyUnavailable));
    };
    Ok(Some(Opening {
        file_key: amk.derive_file_key(&core.source_asset_id, &core.nonce_prefix),
        nonce_prefix: core.nonce_prefix,
    }))
}

/// The resolution for bytes that landed: plaintext when they were opened, ciphertext when
/// the rung is not a derivative.
fn landed(desired: Representation, opening: Option<&Opening>, bytes: Vec<u8>) -> FetchResolution {
    if opening.is_some() {
        FetchResolution::Opened {
            representation: desired,
            plaintext: bytes,
        }
    } else {
        FetchResolution::Fetched {
            representation: desired,
            bytes,
        }
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

/// Fetch exactly one representation's blob (no degrade), and open it when `opening` says
/// how. A representation the asset does not carry is treated as permanently gone (degrade
/// upstream).
///
/// An opened blob that fails authentication is discarded as [`FetchError::Corrupt`] and is
/// **not** re-fetched: [`fetch_blob`] has already verified the bytes against their content
/// address, so a second fetch can only return the same bytes and fail the same way. The
/// recovery is the thumbnails doc's — regenerate from the original — and it is the caller's,
/// signalled by [`DegradeReason::Corrupt`].
async fn fetch_representation<S: BlobSource>(
    source: &S,
    asset: &AssetBlobs,
    representation: Representation,
    opening: Option<&Opening>,
) -> Result<Vec<u8>, FetchError> {
    let blob = asset.blob_for(representation).ok_or(FetchError::Gone)?;
    let ciphertext = fetch_blob(source, &blob.hash, blob.size).await?;
    let Some(opening) = opening else {
        return Ok(ciphertext);
    };
    decrypt_asset_vec(&opening.file_key, &opening.nonce_prefix, &ciphertext).map_err(|error| {
        tracing::warn!(hash = %blob.hash, %error, "derivative failed authentication; discarding");
        FetchError::Corrupt {
            hash: blob.hash.clone(),
        }
    })
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
            Err(error) => return session_failure(&error),
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

/// How a session that could not send a range request reads to the fetch loop.
///
/// Only a session that is over — refresh refused or revoked, no tokens, or a `401`-class
/// answer from the refresh — is [`RangeOutcome::Unauthenticated`], which tells the app to sign
/// in again. Everything else (a transport drop, an unreachable or rate-limited refresh, a
/// refresh `5xx`, a malformed refresh body) is temporary: it reads as a transient status, so a
/// blip never costs the user their session.
fn session_failure(error: &crate::auth::AuthError) -> RangeOutcome {
    use crate::auth::AuthError;
    match error {
        AuthError::SessionExpired
        | AuthError::NotAuthenticated
        | AuthError::Unexpected { status: 401, .. } => {
            tracing::debug!(%error, "blob range request could not be authorized");
            RangeOutcome::Unauthenticated {
                code: error.error_code().map(str::to_string),
                message: error.to_string(),
            }
        }
        AuthError::Unexpected { status, code, .. } if (500..600).contains(status) => {
            tracing::debug!(%error, "blob range request: the session's refresh failed server-side");
            RangeOutcome::Status {
                status: *status,
                code: code.clone(),
            }
        }
        _ => {
            tracing::debug!(%error, "blob range request: transient session or transport failure");
            RangeOutcome::Status {
                status: 0,
                code: None,
            }
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
