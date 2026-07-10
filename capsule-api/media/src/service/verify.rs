//! The storage-verification engine (slice `S-C3`).
//!
//! Computes the key-free per-asset durability verdict — `stored ∧ indexed ∧ retrievable`
//! for every declared blob — from the content-addressed blob store plus the Postgres index,
//! and serves the opt-in `deep` re-hash that catches silent bit-rot. Deep scans are a
//! server-priced operation: **rate-limited per user** and **coalesced** (concurrent deep
//! requests for the same blob share one re-hash, and a repeat within a window reuses the
//! cached result) so a client cannot turn them into an I/O-amplification attack.
//!
//! The request is a pure read: it writes no blob, index, or verdict state. The
//! verify-before-destroy soundness against a racing GC pass comes from the standing grace
//! window in [`service::gc`], not a per-request lease — see that module's contract.
//!
//! Determinism: the wall clock ([`Clock`]) and the blob re-hasher ([`BlobHasher`]) are
//! seams, so the rate-limit and coalescing behavior is proven without sleeps (a `MockClock`
//! and a gated hasher in tests), mirroring the `S-D7` `Clock` pattern.
//!
//! SSoT: [Storage Verification](../../../../../capsule-docs/src/content/docs/design/import/storage-verification.md).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use jiff::{SignedDuration, Timestamp};
use salvo::async_trait;
use sea_orm::ConnectionTrait;
use service::gc;
use tokio::sync::Mutex;
use tracing::{debug, instrument, trace, warn};

/// Default deep-scan budget per user, per [`VerifyLimits::deep_window`].
pub(crate) const DEFAULT_DEEP_MAX_PER_WINDOW: u32 = 32;
/// Default per-user deep-scan rate window.
pub(crate) const DEFAULT_DEEP_WINDOW: SignedDuration = SignedDuration::from_secs(60);
/// Default coalesce-cache TTL: a deep result for a blob is reused for this long.
pub(crate) const DEFAULT_COALESCE_WINDOW: SignedDuration = SignedDuration::from_secs(60);

// ─── Clock seam ──────────────────────────────────────────────────────────────

/// Wall-clock seam so rate-limit windows and `checked_at` are deterministically testable.
pub(crate) trait Clock: Send + Sync {
    /// The current trusted-server instant.
    fn now(&self) -> Timestamp;
}

/// The production [`Clock`], backed by the system wall clock.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

// ─── Blob re-hash seam ───────────────────────────────────────────────────────

/// Re-reads and re-hashes a committed blob, for the `deep` integrity scan. A seam so tests
/// can gate the (otherwise instantaneous) hash to prove coalescing without sleeping.
#[async_trait]
pub(crate) trait BlobHasher: Send + Sync {
    /// The blob file's content address (lowercase hex), or `None` if the file is absent.
    async fn content_address(&self, path: &Path) -> std::io::Result<Option<String>>;
}

/// The production [`BlobHasher`]: read the whole ciphertext blob and hash it.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct FsBlobHasher;

#[async_trait]
impl BlobHasher for FsBlobHasher {
    async fn content_address(&self, path: &Path) -> std::io::Result<Option<String>> {
        match tokio::fs::read(path).await {
            Ok(bytes) => Ok(Some(
                capsule_core::crypto::hash::hash_bytes(&bytes).to_hex(),
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
}

// ─── Config + public verdict types ───────────────────────────────────────────

/// Deep-scan pricing.
#[derive(Debug, Clone, Copy)]
pub(crate) struct VerifyLimits {
    /// Max blob re-hashes (real I/O) a user may trigger per [`Self::deep_window`].
    pub(crate) deep_max_per_window: u32,
    /// The per-user rate window.
    pub(crate) deep_window: SignedDuration,
    /// Coalesce-cache TTL — a repeat deep request for the same blob within this window
    /// returns the cached result (no re-hash, no rate charge).
    pub(crate) coalesce_window: SignedDuration,
}

impl Default for VerifyLimits {
    fn default() -> Self {
        Self {
            deep_max_per_window: DEFAULT_DEEP_MAX_PER_WINDOW,
            deep_window: DEFAULT_DEEP_WINDOW,
            coalesce_window: DEFAULT_COALESCE_WINDOW,
        }
    }
}

/// A blob's role on an asset, as the verdict reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlobRole {
    /// The original ciphertext blob.
    Original,
    /// The encrypted metadata blob.
    Metadata,
    /// A derivative (thumbnail / preview / embedding / backup) blob.
    Derivative,
    /// The provenance chain.
    Provenance,
    /// A hash the server does not associate with the asset (surfaced, never omitted).
    Unknown,
}

impl BlobRole {
    fn from_index(role: &str) -> Self {
        match role {
            "original" => Self::Original,
            "metadata" => Self::Metadata,
            "derivative" | "backup" => Self::Derivative,
            "provenance" => Self::Provenance,
            _ => Self::Unknown,
        }
    }

    /// The wire string for the closed verdict role enum.
    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Metadata => "metadata",
            Self::Derivative => "derivative",
            Self::Provenance => "provenance",
            Self::Unknown => "unknown",
        }
    }
}

/// One asset to verify: the exact blob hashes the client relies on.
#[derive(Debug, Clone)]
pub(crate) struct AssetQuery {
    /// The asset id.
    pub(crate) asset_id: String,
    /// Declared content addresses (lowercase hex).
    pub(crate) blob_hashes: Vec<String>,
}

/// One blob's verdict.
#[derive(Debug, Clone)]
pub(crate) struct BlobVerdict {
    /// The declared content address (hex).
    pub(crate) hash: String,
    /// The blob's role on the asset.
    pub(crate) role: BlobRole,
    /// Present in the blob store at its content address (deep: **and** its bytes re-hash).
    pub(crate) stored: bool,
    /// Referenced by a committed, `uploaded = true` index row.
    pub(crate) indexed: bool,
    /// Refcount > 0, not mid-GC (`collectable_since`), not quarantined.
    pub(crate) retrievable: bool,
}

impl BlobVerdict {
    /// All three independent facts hold — this blob's contribution to durability.
    fn safely_stored(&self) -> bool {
        self.stored && self.indexed && self.retrievable
    }
}

/// One asset's verdict.
#[derive(Debug, Clone)]
pub(crate) struct AssetVerdict {
    /// The asset id.
    pub(crate) asset_id: String,
    /// Every required blob is stored ∧ indexed ∧ retrievable.
    pub(crate) durable: bool,
    /// Per-blob detail, one entry per declared hash.
    pub(crate) blobs: Vec<BlobVerdict>,
    /// The server's trusted clock at verification.
    pub(crate) checked_at: Timestamp,
}

/// Verification failures the route surfaces.
#[derive(Debug, thiserror::Error)]
pub(crate) enum VerifyError {
    /// The caller exceeded its per-user deep-scan budget.
    #[error("deep-scan rate limit exceeded")]
    DeepRateLimited,
    /// The caller exceeded its per-user signed-attestation budget (priced like `deep`).
    #[error("signed-attestation rate limit exceeded")]
    SignRateLimited,
    /// A database read failed.
    #[error(transparent)]
    Db(#[from] sea_orm::DbErr),
    /// A blob re-hash failed with an I/O error other than "absent".
    #[error("blob re-hash failed: {0}")]
    Hash(#[from] std::io::Error),
}

// ─── Coalescing / rate-limit state ───────────────────────────────────────────

/// A fixed-window per-user rate counter.
#[derive(Debug, Clone, Copy)]
struct RateBucket {
    window_start: Timestamp,
    count: u32,
}

/// A cached deep-scan result for one blob.
#[derive(Debug, Clone, Copy)]
struct DeepCacheEntry {
    computed_at: Timestamp,
    /// The blob's bytes re-hash to its declared content address.
    intact: bool,
}

#[derive(Default)]
struct DeepState {
    rate: HashMap<String, RateBucket>,
    cache: HashMap<String, DeepCacheEntry>,
    gates: HashMap<String, Arc<Mutex<()>>>,
    /// The per-user signed-attestation budget (S-C15), priced identically to `deep` so a
    /// client cannot turn signature generation into a server-CPU-amplification attack.
    sign_rate: HashMap<String, RateBucket>,
}

// ─── The service ─────────────────────────────────────────────────────────────

/// The storage-verification engine, shared across requests (holds the deep-scan coalesce
/// cache + per-user rate state).
#[derive(Clone)]
pub(crate) struct VerificationService {
    inner: Arc<Inner>,
}

struct Inner {
    upload_dir: PathBuf,
    limits: VerifyLimits,
    clock: Arc<dyn Clock>,
    hasher: Arc<dyn BlobHasher>,
    deep: Mutex<DeepState>,
}

impl VerificationService {
    /// The production service: system clock, filesystem re-hasher, default limits.
    #[must_use]
    pub(crate) fn new(upload_dir: PathBuf) -> Self {
        Self::with_seams(
            upload_dir,
            VerifyLimits::default(),
            Arc::new(SystemClock),
            Arc::new(FsBlobHasher),
        )
    }

    /// Construct with injected seams — the test entry point (custom clock/hasher/limits).
    #[must_use]
    pub(crate) fn with_seams(
        upload_dir: PathBuf,
        limits: VerifyLimits,
        clock: Arc<dyn Clock>,
        hasher: Arc<dyn BlobHasher>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                upload_dir,
                limits,
                clock,
                hasher,
                deep: Mutex::new(DeepState::default()),
            }),
        }
    }

    /// Compute a verdict for each requested asset. `deep = true` re-hashes present blob bytes
    /// (rate-limited + coalesced); `deep = false` trusts `stat` + index.
    #[instrument(skip_all, fields(user = %user_id, assets = assets.len(), deep))]
    pub(crate) async fn verify<C: ConnectionTrait>(
        &self,
        conn: &C,
        user_id: &str,
        assets: &[AssetQuery],
        deep: bool,
    ) -> Result<Vec<AssetVerdict>, VerifyError> {
        let checked_at = self.inner.clock.now();
        let mut verdicts = Vec::with_capacity(assets.len());
        for asset in assets {
            verdicts.push(
                self.verify_asset(conn, user_id, asset, deep, checked_at)
                    .await?,
            );
        }
        Ok(verdicts)
    }

    async fn verify_asset<C: ConnectionTrait>(
        &self,
        conn: &C,
        user_id: &str,
        asset: &AssetQuery,
        deep: bool,
        checked_at: Timestamp,
    ) -> Result<AssetVerdict, VerifyError> {
        // The `indexed` source of truth: every finalized blob of the asset, keyed by content
        // address → role (minted in the finalization transaction, so a reference here means a
        // committed `uploaded = true` row).
        let index = service::sync::Query::asset_blob_index(conn, &asset.asset_id).await?;
        let gc_states = gc::Query::blob_states(conn, &asset.blob_hashes).await?;

        let mut blobs = Vec::with_capacity(asset.blob_hashes.len());
        for hash in &asset.blob_hashes {
            let indexed = index.contains_key(hash);
            let role = index
                .get(hash)
                .map_or(BlobRole::Unknown, |r| BlobRole::from_index(r));

            let path = service::blob_store::blob_path(&self.inner.upload_dir, hash);
            let present = tokio::fs::try_exists(&path).await.unwrap_or(false);
            let stored = if deep && present {
                self.deep_intact(user_id, hash, &path, checked_at).await?
            } else {
                present
            };

            let gc_state = gc_states.get(hash).copied().unwrap_or_default();
            let retrievable = stored && indexed && gc_state.is_retrievable();

            trace!(%hash, ?role, stored, indexed, retrievable, "blob verdict");
            blobs.push(BlobVerdict {
                hash: hash.clone(),
                role,
                stored,
                indexed,
                retrievable,
            });
        }

        // A `durable` verdict requires every required blob to hold — and an empty declaration
        // confirms nothing.
        let durable = !blobs.is_empty() && blobs.iter().all(BlobVerdict::safely_stored);
        debug!(asset = %asset.asset_id, durable, blobs = blobs.len(), "asset verdict");
        Ok(AssetVerdict {
            asset_id: asset.asset_id.clone(),
            durable,
            blobs,
            checked_at,
        })
    }

    /// Deep-scan one present blob: does it re-hash to its content address? Rate-limited per
    /// user and coalesced per hash — concurrent scans of the same blob share one re-hash, and
    /// a repeat within the coalesce window reuses the cached result (no I/O, no rate charge).
    async fn deep_intact(
        &self,
        user_id: &str,
        hash: &str,
        path: &Path,
        now: Timestamp,
    ) -> Result<bool, VerifyError> {
        // Fast path: a fresh cached result short-circuits before touching the gate or rate
        // budget — the "repeat within a window" relief.
        if let Some(intact) = self.fresh_cache(hash, now).await {
            trace!(%hash, "deep scan served from coalesce cache (fast path)");
            return Ok(intact);
        }

        // Serialize concurrent scans of the *same* hash so they share one re-hash.
        let gate = {
            let mut deep = self.inner.deep.lock().await;
            Arc::clone(deep.gates.entry(hash.to_string()).or_default())
        };
        let _held = gate.lock().await;

        // Re-check under the gate: a concurrent scan that just finished populated the cache —
        // this waiter coalesces onto its result rather than re-reading the bytes.
        if let Some(intact) = self.fresh_cache(hash, now).await {
            trace!(%hash, "deep scan coalesced onto concurrent re-hash");
            return Ok(intact);
        }

        // A genuine cache miss under the gate: this is the one real I/O op. Charge the rate
        // budget first — coalesced/cached reads above never reach here, so the budget bounds
        // I/O, not requests.
        self.charge_rate(user_id, now).await?;

        let intact = if let Some(addr) = self.inner.hasher.content_address(path).await? {
            addr == hash
        } else {
            // Raced with a delete between the stat and the read.
            warn!(%hash, "blob vanished between stat and deep re-hash");
            false
        };

        let mut deep = self.inner.deep.lock().await;
        deep.cache.insert(
            hash.to_string(),
            DeepCacheEntry {
                computed_at: now,
                intact,
            },
        );
        debug!(%hash, intact, "deep re-hash computed");
        Ok(intact)
    }

    /// A cached deep result still inside the coalesce window, if any.
    async fn fresh_cache(&self, hash: &str, now: Timestamp) -> Option<bool> {
        let deep = self.inner.deep.lock().await;
        deep.cache.get(hash).and_then(|entry| {
            (now.duration_since(entry.computed_at) < self.inner.limits.coalesce_window)
                .then_some(entry.intact)
        })
    }

    /// Charge one re-hash against the user's fixed-window budget, or fail closed.
    async fn charge_rate(&self, user_id: &str, now: Timestamp) -> Result<(), VerifyError> {
        let mut deep = self.inner.deep.lock().await;
        let bucket = deep.rate.entry(user_id.to_string()).or_insert(RateBucket {
            window_start: now,
            count: 0,
        });
        if now.duration_since(bucket.window_start) >= self.inner.limits.deep_window {
            bucket.window_start = now;
            bucket.count = 0;
        }
        if bucket.count >= self.inner.limits.deep_max_per_window {
            warn!(user = %user_id, "deep-scan rate limit exceeded");
            return Err(VerifyError::DeepRateLimited);
        }
        bucket.count += 1;
        Ok(())
    }

    /// Charge one signed-attestation request against the user's fixed-window budget (priced
    /// identically to `deep`), or fail closed. Uses the injected [`Clock`] so the window is
    /// deterministically testable.
    pub(crate) async fn charge_sign(&self, user_id: &str) -> Result<(), VerifyError> {
        let now = self.inner.clock.now();
        let mut deep = self.inner.deep.lock().await;
        let bucket = deep
            .sign_rate
            .entry(user_id.to_string())
            .or_insert(RateBucket {
                window_start: now,
                count: 0,
            });
        if now.duration_since(bucket.window_start) >= self.inner.limits.deep_window {
            bucket.window_start = now;
            bucket.count = 0;
        }
        if bucket.count >= self.inner.limits.deep_max_per_window {
            warn!(user = %user_id, "signed-attestation rate limit exceeded");
            return Err(VerifyError::SignRateLimited);
        }
        bucket.count += 1;
        Ok(())
    }
}
