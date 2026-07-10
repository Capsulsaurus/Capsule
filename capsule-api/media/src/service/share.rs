//! The public share-link serve engine (slice `S-C4`).
//!
//! Wraps the authoritative share store ([`service::share::Query`]) with the four serve-path
//! security policies the [Share Links design doc]'s Security Contract fixes, none of which is
//! optional or per-share:
//!
//! - **Indistinguishable `404`.** A not-found / revoked / expired link all resolve to
//!   [`ServeOutcome::NotFound`], which the route renders as one byte-identical bodyless `404`
//!   — never `410`, so a probe reveals nothing.
//! - **Per-IP + per-`{opaque-id}` rate limits.** Two independent fixed-window limiters charged
//!   on every serve (both, never short-circuited) throttle enumeration; the window is driven by
//!   an injected [`Clock`] so the limit is deterministically testable.
//! - **Fail-closed revocation cache.** Revocation/liveness is consulted through a short-TTL
//!   cache (default 60 s) rather than an authoritative read per request. Within the TTL a cached
//!   result is trusted; past it the [`LinkResolver`] is re-read, and if revocation state is
//!   **unreachable** the serve **refuses** rather than serving on stale-allowed state.
//! - **Mandatory privacy strip.** [`stripped_metadata`](ShareServeService::stripped_metadata)
//!   always applies [`strip_for_export`] with [`ExportOptions::default`] (strip everything) —
//!   there is no code path that passes a non-default option, so a share can never leak a
//!   fingerprinting field.
//!
//! Home-server-only serving is enforced by the resolver ([`ShareResolution::Foreign`]); the route
//! returns the `{ home_server }` pointer, never content.
//!
//! Determinism mirrors the S-C3 [`crate::service::verify`] pattern: the wall clock ([`Clock`])
//! and the authoritative read ([`LinkResolver`]) are seams, so the rate-limit windows, the cache
//! TTL, and the fail-closed refusal are all provable without sleeps or a torn-down database.
//!
//! [Share Links design doc]: ../../../../../capsule-docs/src/content/docs/design/share-links.md

use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_core::metadata::export_policy::{ExportOptions, strip_for_export};
use capsule_core::sidecar::sidecar_v1::{SIDECAR_SCHEMA_V1, SidecarV1};
use jiff::{SignedDuration, Timestamp};
use salvo::async_trait;
use sea_orm::DatabaseConnection;
use service::share::{Query as ShareQuery, ServeAsset, ServeRecord, ShareResolution};
use tokio::sync::Mutex;
use tracing::{debug, instrument, trace, warn};

/// Default revocation/liveness cache TTL (Security Contract — Revocation cache, 60 s).
pub(crate) const DEFAULT_REVOCATION_TTL: SignedDuration = SignedDuration::from_secs(60);
/// Default serve-path requests allowed per window, per key (per-IP and per-`{opaque-id}`).
pub(crate) const DEFAULT_SERVE_MAX_PER_WINDOW: u32 = 60;
/// Default serve-path rate window.
pub(crate) const DEFAULT_SERVE_WINDOW: SignedDuration = SignedDuration::from_secs(60);

// ─── Clock seam ──────────────────────────────────────────────────────────────

/// Wall-clock seam so the rate windows and the cache TTL are deterministically testable.
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

// ─── Authoritative resolver seam ───────────────────────────────────────────────

/// The revocation state could not be confirmed (a database read failed). The serve path treats
/// this as fail-closed once its cached liveness is past the TTL.
#[derive(Debug)]
pub(crate) struct ResolveError;

/// The authoritative serve-path read the revocation cache backs. A seam so the fail-closed
/// posture is testable with an injected unreachable resolver (no torn-down DB required).
#[async_trait]
pub(crate) trait LinkResolver: Send + Sync {
    /// Resolve an opaque id at `now`, or [`ResolveError`] if revocation state is unreachable.
    async fn resolve(
        &self,
        opaque_id: &str,
        now: Timestamp,
    ) -> Result<ShareResolution, ResolveError>;
}

/// The production resolver: the authoritative `public_shares` read on this home server.
pub(crate) struct DbLinkResolver {
    conn: DatabaseConnection,
    server_id: String,
}

#[async_trait]
impl LinkResolver for DbLinkResolver {
    async fn resolve(
        &self,
        opaque_id: &str,
        now: Timestamp,
    ) -> Result<ShareResolution, ResolveError> {
        ShareQuery::resolve_by_opaque(&self.conn, opaque_id, &self.server_id, now)
            .await
            .map_err(|e| {
                warn!("share revocation read failed: {e}");
                ResolveError
            })
    }
}

// ─── Serve outcome + response types ────────────────────────────────────────────

/// The result of a serve-path resolution — what the route renders.
pub(crate) enum ServeOutcome {
    /// A live, home-server-owned link — serve it.
    Serve(Box<ServeRecord>),
    /// A link this server does not host: render the `{ home_server }` pointer, never content.
    Foreign {
        /// The authoritative home server the client resolves.
        home_server: String,
    },
    /// Not found, revoked, expired, or fail-closed — one indistinguishable `404`.
    NotFound,
    /// A per-IP or per-`{opaque-id}` rate limit engaged (`429`).
    RateLimited,
}

/// One asset's served metadata after the mandatory export strip (the sidecar is base64 canonical
/// CBOR of the **stripped** [`SidecarV1`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StrippedAsset {
    pub(crate) asset_id: String,
    pub(crate) content_hash: String,
    pub(crate) content_type: String,
    pub(crate) size: u64,
    /// Base64 of the stripped sidecar's canonical CBOR (the served metadata blob).
    pub(crate) metadata_blob_b64: String,
}

// ─── Rate + cache state ────────────────────────────────────────────────────────

/// A fixed-window rate counter (mirrors S-C3's `RateBucket`).
#[derive(Debug, Clone, Copy)]
struct RateBucket {
    window_start: Timestamp,
    count: u32,
}

/// A cached authoritative resolution and the instant it was confirmed.
#[derive(Clone)]
struct CacheEntry {
    checked_at: Timestamp,
    resolution: ShareResolution,
}

#[derive(Default)]
struct ServeState {
    /// Per-key fixed-window buckets (keys are `opaque:{id}` and `ip:{ip}`).
    rate: HashMap<String, RateBucket>,
    /// Per-`{opaque-id}` revocation/liveness cache.
    cache: HashMap<String, CacheEntry>,
}

/// Serve-path pricing + cache tuning.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ServeLimits {
    /// Max serve requests per window, per key.
    pub(crate) max_per_window: u32,
    /// The per-key rate window.
    pub(crate) window: SignedDuration,
    /// Revocation-cache TTL — a cached liveness is trusted for this long before re-reading.
    pub(crate) revocation_ttl: SignedDuration,
}

impl Default for ServeLimits {
    fn default() -> Self {
        Self {
            max_per_window: DEFAULT_SERVE_MAX_PER_WINDOW,
            window: DEFAULT_SERVE_WINDOW,
            revocation_ttl: DEFAULT_REVOCATION_TTL,
        }
    }
}

// ─── The service ───────────────────────────────────────────────────────────────

/// The share-link serve engine, shared across requests (holds the rate buckets + revocation
/// cache).
#[derive(Clone)]
pub(crate) struct ShareServeService {
    inner: Arc<Inner>,
}

struct Inner {
    resolver: Arc<dyn LinkResolver>,
    clock: Arc<dyn Clock>,
    limits: ServeLimits,
    state: Mutex<ServeState>,
}

impl ShareServeService {
    /// The production service: the DB resolver over this home server, system clock, default limits.
    #[must_use]
    pub(crate) fn new(conn: DatabaseConnection, server_id: String) -> Self {
        Self::with_seams(
            Arc::new(DbLinkResolver { conn, server_id }),
            Arc::new(SystemClock),
            ServeLimits::default(),
        )
    }

    /// Construct with injected seams — the test entry point (mock resolver/clock/limits).
    #[must_use]
    pub(crate) fn with_seams(
        resolver: Arc<dyn LinkResolver>,
        clock: Arc<dyn Clock>,
        limits: ServeLimits,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                resolver,
                clock,
                limits,
                state: Mutex::new(ServeState::default()),
            }),
        }
    }

    /// Resolve a serve request, applying the two rate limiters, the fail-closed revocation cache,
    /// and the home-server gate. The one entry point every serve endpoint (metadata, blob,
    /// wrapped-secret) funnels through, so all three share one uniform security posture.
    #[instrument(skip(self), fields(opaque_id = %opaque_id))]
    pub(crate) async fn resolve_serve(&self, opaque_id: &str, source_ip: &str) -> ServeOutcome {
        let now = self.inner.clock.now();

        // Two independent limiters (per source IP and per `{opaque-id}`). Charge BOTH — never
        // short-circuit — so one key's exhaustion cannot starve the other's budget.
        let opaque_ok = self.charge_rate(&format!("opaque:{opaque_id}"), now).await;
        let ip_ok = self.charge_rate(&format!("ip:{source_ip}"), now).await;
        if !opaque_ok || !ip_ok {
            warn!(%opaque_id, "share serve rate limit engaged");
            return ServeOutcome::RateLimited;
        }

        // A fresh cached result short-circuits the authoritative read (intra-server staleness is
        // bounded by the TTL). Past the TTL, re-read authoritatively.
        if let Some(res) = self.fresh_cache(opaque_id, now).await {
            trace!(%opaque_id, "share resolution served from revocation cache");
            return Self::to_outcome(res);
        }

        if let Ok(res) = self.inner.resolver.resolve(opaque_id, now).await {
            self.store_cache(opaque_id, now, res.clone()).await;
            Self::to_outcome(res)
        } else {
            // Fail-closed: past the TTL and unable to confirm liveness → refuse, never serve on
            // stale-allowed state (Security Contract — Revocation cache).
            warn!(%opaque_id, "revocation state unreachable past TTL; failing closed");
            ServeOutcome::NotFound
        }
    }

    /// Apply the **mandatory** export strip to a live record's served metadata. Always uses
    /// [`ExportOptions::default`] (strip every fingerprinting field) — there is no parameter or
    /// code path that retains one, so a public share can never leak a boundary-crossing field.
    #[must_use]
    pub(crate) fn stripped_metadata(&self, record: &ServeRecord) -> Vec<StrippedAsset> {
        record.assets.iter().map(strip_asset).collect()
    }

    /// Find the servable asset for a requested blob hash (the blob endpoint serves only hashes a
    /// share actually covers — never an arbitrary blob oracle).
    #[must_use]
    pub(crate) fn asset_for_hash<'a>(
        &self,
        record: &'a ServeRecord,
        hash: &str,
    ) -> Option<&'a ServeAsset> {
        record.assets.iter().find(|a| a.content_hash == hash)
    }

    fn to_outcome(res: ShareResolution) -> ServeOutcome {
        match res {
            ShareResolution::Serve(record) => ServeOutcome::Serve(Box::new(record)),
            ShareResolution::Foreign { home_server } => ServeOutcome::Foreign { home_server },
            ShareResolution::Gone => ServeOutcome::NotFound,
        }
    }

    /// A cached resolution still inside the revocation-cache TTL, if any.
    async fn fresh_cache(&self, opaque_id: &str, now: Timestamp) -> Option<ShareResolution> {
        let state = self.inner.state.lock().await;
        state.cache.get(opaque_id).and_then(|entry| {
            (now.duration_since(entry.checked_at) < self.inner.limits.revocation_ttl)
                .then(|| entry.resolution.clone())
        })
    }

    async fn store_cache(&self, opaque_id: &str, now: Timestamp, resolution: ShareResolution) {
        let mut state = self.inner.state.lock().await;
        state.cache.insert(
            opaque_id.to_string(),
            CacheEntry {
                checked_at: now,
                resolution,
            },
        );
    }

    /// Charge one hit against a key's fixed-window budget; `true` if within budget.
    async fn charge_rate(&self, key: &str, now: Timestamp) -> bool {
        let mut state = self.inner.state.lock().await;
        let bucket = state.rate.entry(key.to_string()).or_insert(RateBucket {
            window_start: now,
            count: 0,
        });
        if now.duration_since(bucket.window_start) >= self.inner.limits.window {
            bucket.window_start = now;
            bucket.count = 0;
        }
        if bucket.count >= self.inner.limits.max_per_window {
            return false;
        }
        bucket.count += 1;
        debug!(key, count = bucket.count, "share serve rate charged");
        true
    }
}

/// Decode → strip → re-encode one served asset's sidecar (the export strip is mandatory and
/// always default; a sidecar that fails to decode yields an empty metadata blob, never an
/// un-stripped one).
fn strip_asset(asset: &ServeAsset) -> StrippedAsset {
    let metadata_blob_b64 = if let Ok(sidecar) =
        SidecarV1::from_canonical_slice(&asset.sidecar_cbor, SIDECAR_SCHEMA_V1)
    {
        // No opt-out: the boundary-crossing strip always runs with the default (strip all).
        let stripped = strip_for_export(&sidecar, &ExportOptions::default());
        BASE64.encode(stripped.to_canonical_vec())
    } else {
        warn!("share sidecar decode failed; serving empty metadata");
        String::new()
    };
    StrippedAsset {
        asset_id: asset.asset_id.clone(),
        content_hash: asset.content_hash.clone(),
        content_type: asset.content_type.clone(),
        size: asset.size,
        metadata_blob_b64,
    }
}
