//! Upload client module for Capsule SDK.
//!
//! The hand-written, chunked, resumable, adaptive upload client. The protocol is too
//! stateful for codegen — the spargen-generated REST client covers the plain request/response
//! surfaces instead — so this module owns the wire directly:
//!
//! - `create`/`PATCH`/`HEAD`/`DELETE`/`list` with `application/octet-stream`
//!   chunk bodies and the required per-chunk `X-Capsule-Checksum` (lowercase-hex
//!   SHA-256), `X-Capsule-Offset`, and the `X-Capsule-Protocol` handshake header;
//! - the **normative** adaptive chunk-size algorithm ([`AdaptiveChunkSizeStrategy`]),
//!   clamped to `[PROTOCOL_MIN_CHUNK, PROTOCOL_MAX_CHUNK]` with 4 KiB alignment
//!   guaranteed *by construction*;
//! - the code-driven recovery matrix ([`UploadClient::upload`]): `offset_mismatch`
//!   → HEAD re-align; `session_not_found` → re-create; `duplicate_blob` → merge
//!   (success carrying the existing asset ref); `426` → abort-with-upgrade;
//!   `checksum_mismatch` → re-send the chunk; `chunk_timeout` → re-send at the
//!   same offset, shorter; `rate_limited`, `unavailable` and
//!   `storage_full` → wait out `Retry-After`, then resume from HEAD. The client
//!   switches on the stable `error.*` code (from `capsule_i18n::error_codes`),
//!   never on the bare status;
//! - per-request stall bounds ([`RequestBounds`]): a chunk body is cut when no bytes move for
//!   the stall bound, the wait for a response once the body is sent runs under a separate,
//!   longer bound, and nothing bounds a request's total duration — a slow but live link is
//!   never cut. A client-cut chunk is re-sent at the same offset, shorter, like a `408`.
//!
//! Tokens stay SDK-owned: an upload composes with the SDK session — build the
//! transport with [`UploadTransport::with_session`] and every request runs through
//! [`crate::auth::Session::execute`] (pre-flight refresh, single-flight, one `401`
//! replay), so callers never juggle raw access tokens. [`StaticToken`] +
//! [`UploadTransport::with_static_token`] plug in a fixed bearer for tests and
//! callers that already hold a live token. No filename or plaintext metadata ever
//! rides the wire — it travels inside the encrypted metadata blob (transfer
//! upload doc, §Chunk Rules).

use std::borrow::Cow;
use std::collections::VecDeque;
use std::time::Duration;

use capsule_i18n::error_codes;
use serde::{Deserialize, Serialize};

use crate::events::{RetryReason, TransferEvent, TransferEvents, millis};
use crate::net::{
    ConnectionClass, ExchangeProgress, PacedBody, RequestBounds, RequestCut, RetryClass,
    RetryDecision, RetryEngine, Sleeper, TokioSleeper, negotiation, within_bounds,
};

pub mod advice;
pub mod queue;

use self::advice::{ServerAdvice, SessionExpiry};

// Constants for chunk sizes (4KB aligned)
const KB: u64 = 1024;
const CHUNK_SIZE_256KB: u64 = 256 * KB;
const CHUNK_SIZE_1MB: u64 = 1024 * KB;
const CHUNK_SIZE_4MB: u64 = 4 * 1024 * KB;
const CHUNK_SIZE_16MB: u64 = 16 * 1024 * KB;

/// All chunks MUST be multiples of 4KB (4096 bytes)
const ALIGNMENT: u64 = 4096;
/// Protocol-surface chunk bounds (transfer upload doc, §Chunk Rules and
/// Strictness). Every adaptive tier range sits inside these; the server rejects
/// outside them (400 / 413).
pub const PROTOCOL_MIN_CHUNK: u64 = 4096;
pub const PROTOCOL_MAX_CHUNK: u64 = CHUNK_SIZE_16MB;

/// Upper bound on recovery actions (re-align / re-create / re-send) for a single
/// blob transfer. A live server drives progress well inside this; hitting it
/// means a stuck peer, so the transfer fails loudly ([`UploadError::RetriesExhausted`])
/// rather than hot-looping (transfer upload doc, §Session Lifetime and Discard:
/// "retrying with backoff and halting after a bounded number of attempts").
const MAX_RECOVERIES: u32 = 64;

/// The longest single wait a server's `Retry-After` is honoured for.
///
/// A server names its own wait (transfer upload doc, §Hostile Clients), and a
/// client that waited however long it was told would hand a misconfigured or
/// hostile server the power to park a transfer indefinitely. Five minutes is the
/// server's own disk-full hint; anything longer is waited in five-minute steps,
/// each one a HEAD that re-learns the offset and charges the recovery budget.
const MAX_SERVER_BACKOFF: Duration = Duration::from_mins(5);

/// How long one chunk request should take at the measured goodput, on an ordinary link: eight
/// seconds.
///
/// Long enough that a round trip and the server's write are a small share of each request; short
/// enough that a request lost to a reset or a stall costs seconds, not minutes.
pub const TARGET_REQUEST_DURATION: Duration = Duration::from_secs(8);

/// How long one chunk request should take under an [`ConnectionClass::Adverse`] link: two
/// seconds, so a request usually completes between the resets that made the link adverse.
pub const ADVERSE_TARGET_REQUEST_DURATION: Duration = Duration::from_secs(2);

/// The sliding window goodput is measured over.
const GOODPUT_WINDOW: Duration = Duration::from_secs(30);
/// Maximum number of recent chunks to keep in history.
const MAX_RECENT_CHUNKS: usize = 1000;
/// Chunks measured at one size before the strategy grows it. Shrinking needs one: a chunk that
/// took far longer than the target is evidence enough.
const WARMUP_CHUNKS: u32 = 5;
/// Successful chunks after a stall before the size may grow again: the link has to show it
/// recovered first.
const GROW_BACK_AFTER_SUCCESSES: u32 = 4;

/// Chunk size strategy for adaptive upload.
///
/// Normative for `capsule-sdk` (transfer upload doc, §Adaptive Chunk Sizing): the size is the
/// one that completes in a [target request duration](TARGET_REQUEST_DURATION) at the goodput
/// measured over a sliding window — shorter under an adverse link. Because the server cuts a
/// chunk only when it stalls, never for its total duration, a chunk's size never has to fit a
/// deadline: a slow link gets chunks that take the target time at its own rate, a fast one
/// chunks up to the file-size tier's ceiling.
///
/// It starts from the server's advised size, clamped to `[PROTOCOL_MIN_CHUNK, tier ceiling]`.
/// After each chunk it moves towards the target size: down to it at once when the target is
/// half the current size or less, up by doubling after a warm-up when it is twice or more.
/// Every candidate is aligned down to 4 KiB and clamped, so the size is aligned and within the
/// protocol bounds by construction.
///
/// A stall — the server's `408 error.upload.chunk_timeout`, or the client's own stall cut —
/// re-sends the same offset at half the stalled length ([`shrink_after_stall`]), and the size
/// does not grow again until a run of successes shows the link recovered. A chunk that got no
/// answer within the response wait is shrunk the same way: its tail may have been too slow to
/// drain out of the send buffers in time.
///
/// [`shrink_after_stall`]: Self::shrink_after_stall
#[derive(Debug, Clone)]
pub struct AdaptiveChunkSizeStrategy {
    /// Current chunk size.
    pub current_size: u64,
    /// The file-size tier's ceiling: no chunk is larger.
    pub max_size: u64,
    /// Successful chunks measured since the size last changed.
    pub successful_chunks: u32,
    /// Total bytes uploaded.
    pub bytes_uploaded: u64,
    /// Total time spent uploading.
    pub total_upload_time: Duration,
    /// Recent chunk records for windowed goodput: (bytes, duration, when recorded).
    recent_chunks: VecDeque<(u64, Duration, std::time::Instant)>,
    /// How long one chunk request should take at the measured goodput.
    target: Duration,
    /// Successes still owed after a stall before the size may grow.
    hold_growth: u32,
}

impl AdaptiveChunkSizeStrategy {
    /// Create a new chunk size strategy based on total file size: the tier names the starting
    /// size and the ceiling.
    pub fn for_file_size(total_size: u64) -> Self {
        let (current_size, max_size) = if total_size < 10 * 1024 * KB {
            // < 10 MiB: start at 256 KiB, at most 1 MiB.
            (CHUNK_SIZE_256KB, CHUNK_SIZE_1MB)
        } else if total_size < 100 * 1024 * KB {
            // < 100 MiB: start at 1 MiB, at most 4 MiB.
            (CHUNK_SIZE_1MB, CHUNK_SIZE_4MB)
        } else {
            // >= 100 MiB: start at 4 MiB, at most 16 MiB.
            (CHUNK_SIZE_4MB, CHUNK_SIZE_16MB)
        };

        Self {
            current_size,
            max_size,
            successful_chunks: 0,
            bytes_uploaded: 0,
            total_upload_time: Duration::ZERO,
            recent_chunks: VecDeque::with_capacity(32),
            target: TARGET_REQUEST_DURATION,
            hold_growth: 0,
        }
    }

    /// `size` aligned down to 4 KiB and clamped into `[PROTOCOL_MIN_CHUNK, max_size]`.
    fn bounded(&self, size: u64) -> u64 {
        ((size / ALIGNMENT) * ALIGNMENT).clamp(PROTOCOL_MIN_CHUNK, self.max_size)
    }

    /// Shrink after a chunk of `stalled` bytes stalled — the server answered
    /// `408 error.upload.chunk_timeout`, or the client cut it: the next chunk, sent at the same
    /// offset, is half that, 4 KiB-aligned down, and at least [`PROTOCOL_MIN_CHUNK`]. The size
    /// then holds until a run of chunks succeeds.
    ///
    /// Returns `false` when `stalled` is already at or below the protocol minimum, so nothing
    /// smaller can be sent; the size is then left as it is.
    pub fn shrink_after_stall(&mut self, stalled: u64) -> bool {
        self.hold_growth = GROW_BACK_AFTER_SUCCESSES;
        self.successful_chunks = 0;
        if stalled <= PROTOCOL_MIN_CHUNK {
            return false;
        }
        self.current_size = self.bounded(stalled / 2);
        debug_assert!(self.current_size.is_multiple_of(ALIGNMENT));
        true
    }

    /// Couple the chunk size to a detected [`ConnectionClass`]: under
    /// [`ConnectionClass::Adverse`] each request targets
    /// [`ADVERSE_TARGET_REQUEST_DURATION`], otherwise [`TARGET_REQUEST_DURATION`].
    #[must_use]
    pub fn with_connection_class(mut self, class: ConnectionClass) -> Self {
        self.apply_connection_class(class);
        self
    }

    /// Apply the connection-class coupling in place (see
    /// [`with_connection_class`](Self::with_connection_class)).
    pub fn apply_connection_class(&mut self, class: ConnectionClass) {
        self.target = if class == ConnectionClass::Adverse {
            ADVERSE_TARGET_REQUEST_DURATION
        } else {
            TARGET_REQUEST_DURATION
        };
    }

    /// How long one chunk request should take at the measured goodput.
    pub fn target_duration(&self) -> Duration {
        self.target
    }

    /// Seed the starting chunk size from the server's suggestion: the `chunk` member of
    /// `X-Capsule-Advice`, or the create body's `suggested_chunk_size` from a server that
    /// sends no advice.
    ///
    /// The suggestion is a *starting point only* (transfer upload doc, §Adaptive Chunk Sizing):
    /// 4 KiB-aligned down and clamped into `[PROTOCOL_MIN_CHUNK, tier ceiling]`, so the size
    /// is aligned and within the protocol bounds by construction.
    pub fn seeded_from_suggested(mut self, suggested: u64) -> Self {
        self.current_size = self.bounded(suggested);
        debug_assert!(self.current_size.is_multiple_of(ALIGNMENT));
        self
    }

    /// Goodput in bytes per second over the sliding window.
    pub fn throughput_bytes_per_second(&self) -> f64 {
        let now = std::time::Instant::now();
        let window_start = now.checked_sub(GOODPUT_WINDOW).unwrap_or(now);

        let (bytes, time) = self
            .recent_chunks
            .iter()
            .filter(|(_, _, timestamp)| *timestamp >= window_start)
            .fold((0u64, Duration::ZERO), |(acc_b, acc_t), (b, t, _)| {
                (acc_b + b, acc_t + *t)
            });

        if time.as_secs_f64() == 0.0 {
            return 0.0;
        }
        bytes as f64 / time.as_secs_f64()
    }

    /// The size that completes in the target duration at the measured goodput, aligned and
    /// clamped; `None` before anything was measured.
    pub fn target_size(&self) -> Option<u64> {
        let goodput = self.throughput_bytes_per_second();
        (goodput > 0.0).then(|| self.bounded((goodput * self.target.as_secs_f64()) as u64))
    }

    /// Record a successful chunk upload and move the size towards the target.
    pub fn record_chunk(&mut self, chunk_size: u64, upload_duration: Duration) {
        self.successful_chunks += 1;
        self.bytes_uploaded += chunk_size;
        self.total_upload_time += upload_duration;
        self.hold_growth = self.hold_growth.saturating_sub(1);

        let now = std::time::Instant::now();
        self.recent_chunks
            .push_back((chunk_size, upload_duration, now));
        if self.recent_chunks.len() > MAX_RECENT_CHUNKS {
            self.recent_chunks.pop_front();
        }
        if let Some(window_start) = now.checked_sub(GOODPUT_WINDOW) {
            // Sorted by time, so the stale records are all at the front.
            while self
                .recent_chunks
                .front()
                .is_some_and(|(_, _, timestamp)| *timestamp < window_start)
            {
                self.recent_chunks.pop_front();
            }
        }

        let Some(target) = self.target_size() else {
            return;
        };
        let current = self.current_size;
        let next = if target.saturating_mul(2) <= current {
            // Far above the target: down to it at once, since every chunk at this size costs
            // more than the target to lose.
            target
        } else if target >= current.saturating_mul(2)
            && self.successful_chunks >= WARMUP_CHUNKS
            && self.hold_growth == 0
        {
            self.bounded(current.saturating_mul(2))
        } else {
            current
        };
        if next != current {
            tracing::debug!(
                from = current,
                to = next,
                target_bytes = target,
                target_ms = self.target.as_millis(),
                "chunk size moved towards the target request duration"
            );
            self.current_size = next;
            self.successful_chunks = 0;
        }
    }

    /// Get the next chunk size to use.
    ///
    /// Alignment is a hard guarantee by construction: every candidate is aligned down and
    /// clamped by one function, so this can never return an unaligned size — the
    /// debug_assert is a tripwire for future edits, not a runtime dependency.
    pub fn next_chunk_size(&self) -> u64 {
        debug_assert!(
            self.current_size.is_multiple_of(ALIGNMENT),
            "Chunk size must be 4KB aligned"
        );
        debug_assert!(
            (PROTOCOL_MIN_CHUNK..=PROTOCOL_MAX_CHUNK).contains(&self.current_size),
            "Chunk size must sit within the protocol bounds"
        );
        self.current_size
    }
}

// ─── Transport seam ─────────────────────────────────────────────────────────

/// A fixed bearer token, for tests and callers that already hold a live token.
/// Production clients compose with the SDK session via
/// [`UploadTransport::with_session`] instead.
#[derive(Debug, Clone)]
pub struct StaticToken(pub String);

/// How upload requests are authorized. Closed on purpose: the SDK owns the
/// complete user flow, so an upload either rides the session's token store or a
/// caller-supplied fixed bearer — never a hand-rolled header.
#[derive(Clone)]
enum UploadAuth {
    /// Drive every request through [`crate::auth::Session::execute`]: pre-flight
    /// expiry refresh, single-flight coalescing, and one `401` refresh-and-replay.
    Session(crate::auth::Session),
    /// A fixed bearer token over a plain client.
    Static {
        http: reqwest::Client,
        token: String,
    },
}

/// The authorized HTTP transport for one upload endpoint: base URL, the pinned
/// protocol date sent as `X-Capsule-Protocol`, the authorization seam, and the stall bounds
/// every request runs under.
#[derive(Clone)]
pub struct UploadTransport {
    base_url: String,
    protocol_version: String,
    auth: UploadAuth,
    /// The per-request stall and response-wait bounds ([`crate::net::within_bounds`]).
    bounds: RequestBounds,
}

impl UploadTransport {
    /// Build a transport that authorizes through an authenticated
    /// [`crate::auth::Session`] — the sanctioned production path: the session
    /// injects (and auto-refreshes) the bearer token, so the upload client never
    /// sees raw token material.
    ///
    /// `base_url` points at the upload endpoint root (no trailing slash) —
    /// `create` posts to it, chunk/HEAD/DELETE hang under `{base_url}/{id}`, and
    /// listing is `{base_url}/sessions`.
    pub fn with_session(
        session: crate::auth::Session,
        base_url: impl Into<String>,
        protocol_version: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            protocol_version: protocol_version.into(),
            auth: UploadAuth::Session(session),
            bounds: RequestBounds::default(),
        }
    }

    /// Build a transport over a fixed bearer token (tests; callers that already
    /// hold a live token). Same URL layout as [`Self::with_session`].
    ///
    /// `http` **must** come from [`crate::net::http_builder`] or [`crate::net::http_client`]: a
    /// client built any other way sends no protocol handshake, and every gated route refuses it.
    pub fn with_static_token(
        http: reqwest::Client,
        base_url: impl Into<String>,
        protocol_version: impl Into<String>,
        token: StaticToken,
    ) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            protocol_version: protocol_version.into(),
            auth: UploadAuth::Static {
                http,
                token: token.0,
            },
            bounds: RequestBounds::default(),
        }
    }

    /// Run every request under `bounds` instead of [`RequestBounds::default`].
    #[must_use]
    pub fn with_request_bounds(mut self, bounds: RequestBounds) -> Self {
        self.bounds = bounds;
        self
    }

    /// Send an authorized request whose body is small enough to be sent at once: the
    /// response-wait bound runs from the start, then the stall bound over the response body.
    async fn send<F>(&self, build: F) -> Result<reqwest::Response, UploadError>
    where
        F: Fn(&reqwest::Client) -> reqwest::RequestBuilder,
    {
        self.send_tracked(&ExchangeProgress::waiting(), build)
            .await?
            .map_err(|cut| UploadError::Transport(cut.to_string()))
    }

    /// Send an authorized request under the transport's [`RequestBounds`], reading its whole
    /// response body before handing it back, so no read after this one can hang.
    ///
    /// `progress` is the exchange's: a request that streams a large body hands it to its
    /// [`PacedBody`] in `build`, so the stall bound runs while the body is sent and the
    /// response-wait bound once it is all sent. The inner `Err` is the [`RequestCut`] that ended
    /// it, kept apart so a chunk can tell a stalled link from any other transport failure.
    async fn send_tracked<F>(
        &self,
        progress: &ExchangeProgress,
        build: F,
    ) -> Result<Result<reqwest::Response, RequestCut>, UploadError>
    where
        F: Fn(&reqwest::Client) -> reqwest::RequestBuilder,
    {
        // Boxed: the send is the largest future in the upload state machine, and every caller
        // up to the CLI's dispatch would otherwise carry it inline.
        let send = Box::pin(self.send_unbounded(build));
        let response = match within_bounds(self.bounds, progress, send).await {
            Ok(response) => response?,
            Err(cut) => return Ok(Err(self.cut(cut, "request"))),
        };
        progress.on_received();
        let status = response.status();
        let version = response.version();
        let headers = response.headers().clone();
        let mut response = response;
        let mut body = Vec::new();
        loop {
            let piece = match within_bounds(self.bounds, progress, response.chunk()).await {
                Ok(piece) => piece?,
                Err(cut) => return Ok(Err(self.cut(cut, "response body"))),
            };
            let Some(piece) = piece else { break };
            progress.on_received();
            body.extend_from_slice(&piece);
        }
        let mut rebuilt = http::Response::new(body);
        *rebuilt.status_mut() = status;
        *rebuilt.version_mut() = version;
        *rebuilt.headers_mut() = headers;
        Ok(Ok(reqwest::Response::from(rebuilt)))
    }

    /// Log a cut request, and hand the cut back.
    fn cut(&self, cut: RequestCut, during: &str) -> RequestCut {
        tracing::info!(
            ?cut,
            during,
            stall_ms = millis(self.bounds.stall),
            response_wait_ms = millis(self.bounds.response_wait),
            "an upload request was cut"
        );
        cut
    }

    /// Send an authorized request with no bound of its own. `build` constructs the
    /// un-authenticated request (method, URL, headers, body) from the transport's HTTP client;
    /// the seam adds the bearer. `build` may be invoked twice on the session path (the `401`
    /// refresh-and-replay), so it must be reconstructible.
    async fn send_unbounded<F>(&self, build: F) -> Result<reqwest::Response, UploadError>
    where
        F: Fn(&reqwest::Client) -> reqwest::RequestBuilder,
    {
        // Both paths go through the protocol window (issue #674): the session's `execute`
        // screens its own sends, and the static path screens here, so a refusal from either is
        // the same `UpgradeRequired`.
        match &self.auth {
            UploadAuth::Session(session) => match session.execute(build).await {
                Ok(response) => Ok(response),
                Err(crate::auth::AuthError::UpgradeRequired(refusal)) => Err((*refusal).into()),
                Err(other) => Err(other.into()),
            },
            UploadAuth::Static { http, token } => {
                match negotiation::send_negotiated(build(http).bearer_auth(token)).await {
                    Ok(response) => Ok(response),
                    Err(negotiation::SendError::Upgrade(refusal)) => Err(refusal.into()),
                    Err(negotiation::SendError::Transport(error)) => Err(error.into()),
                }
            }
        }
    }

    /// The upgrade refusal an error response carrying `error.protocol.version_unsupported`
    /// answers, with the window its headers advertise.
    fn upgrade_required(
        &self,
        headers: &reqwest::header::HeaderMap,
        message: String,
    ) -> UploadError {
        negotiation::UpgradeRequired::refused(headers, &self.protocol_version, message).into()
    }

    fn session_url(&self, id: &str) -> String {
        let base = &self.base_url;
        format!("{base}/{id}")
    }
}

// ─── Wire DTOs (mirror the server's transport JSON) ─────────────────────────

/// The blob's role within its asset bundle (closed enum; snake_case on the wire
/// to match the server).
#[derive(Debug, Copy, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlobRole {
    /// The source asset.
    Original,
    /// A client-generated thumbnail/preview.
    Derivative,
    /// The CBOR metadata document.
    Metadata,
    /// Append-only provenance.
    Provenance,
    /// A library backup artifact.
    Backup,
    /// A derivative tier's signed `DerivativeManifest`, canonical CBOR — what a receiver opens
    /// that tier's derivative with.
    DerivativeManifest,
}

/// The server-visible mirror of the signed manifest's envelope fields, declared
/// at `POST /upload` (owned by the provenance design doc). Opaque to this client
/// — the import pipeline builds and signs it; the SDK only carries it on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestEnvelope {
    /// Crypto suite the blob is sealed under.
    pub crypto_suite_id: u16,
    /// Protocol date (`YYYY-MM-DD`).
    pub protocol_version: String,
    /// Optional album this blob belongs to.
    pub album_id: Option<String>,
    /// The asset id this blob belongs to (UUIDv7).
    pub file_id: String,
    /// AMK epoch the blob is sealed under.
    pub amk_version: u32,
    /// Ciphertext content hash, lowercase hex — equals the top-level `hash`.
    pub ciphertext_hash: String,
    /// Plaintext size in bytes.
    pub plaintext_size: u64,
    /// STREAM plaintext chunk size.
    pub chunk_size: u32,
    /// `derived | wrapped`.
    pub key_mode: String,
    /// Hash of the bundle's metadata blob, when known.
    pub metadata_blob_hash: Option<String>,
    /// A derivative blob's role (`thumbnail | preview | embedding`), from its signed
    /// `DerivativeManifest`. Set on a `derivative` blob and on nothing else: the server
    /// requires it there, refuses it elsewhere, and checks the pair against the closed
    /// still-derivative format set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derivative_role: Option<String>,
    /// A derivative blob's format (`DerivativeManifest.format`, e.g. `image/jxl`) — the same
    /// value as the request's `content_type`. Set exactly when [`Self::derivative_role`] is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derivative_format: Option<String>,
    /// The authoring user.
    pub created_by_user: String,
    /// The authoring device.
    pub created_by_device: String,
    /// The exact client build.
    pub client_version: String,
    /// RFC 3339 authoring timestamp.
    pub timestamp: String,
    /// Lifecycle action (`create` for a fresh bundle).
    pub action: String,
    /// Prior provenance hash, for append-only chains.
    pub prior_provenance_hash: Option<String>,
    /// Optional retention floor.
    pub retention_until: Option<String>,
}

/// The `POST /upload` request body. Plaintext metadata (filename, capture date)
/// is deliberately absent — it rides the encrypted metadata blob.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateUploadRequest {
    /// Ciphertext size in bytes (declared once, immutable thereafter).
    pub size: u64,
    /// Ciphertext content hash, lowercase hex.
    pub hash: String,
    /// MIME type (closed enum per protocol version).
    pub content_type: String,
    /// Crypto suite the blob is sealed under.
    pub crypto_suite_id: u16,
    /// Protocol date (`YYYY-MM-DD`) this client speaks — also sent as the
    /// `X-Capsule-Protocol` header.
    pub protocol_version: String,
    /// This blob's role in its bundle.
    pub blob_role: BlobRole,
    /// The unencrypted manifest fields the server validates.
    pub manifest_envelope: ManifestEnvelope,
    /// Optional album to add the asset to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album_id: Option<String>,
    /// Optional owner id (defaults to the authenticated uploader).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_id: Option<String>,
    /// Album-upgrade intent id (album upgrade ceremony only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent_id: Option<String>,
}

/// The `POST /upload` success body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSessionResponse {
    /// Upload session ID.
    pub id: String,
    /// Server-reported upload URL (advisory; the client builds its own paths).
    pub upload_url: String,
    /// The server's starting chunk-size suggestion. The fallback for a server that sends no
    /// `X-Capsule-Advice`; where both are present the advice's `chunk` member is used.
    pub suggested_chunk_size: u64,
}

impl CreateSessionResponse {
    /// The starting chunk size: the advice's `chunk` member while it holds, otherwise the body's
    /// suggestion. Read at receipt, so the advice is never stale here.
    fn starting_chunk(&self, advice: &ServerAdvice) -> u64 {
        advice
            .chunk(Duration::ZERO)
            .unwrap_or(self.suggested_chunk_size)
    }
}

/// A `HEAD /upload/{id}` result: the resumption primitive.
#[derive(Debug, Clone)]
pub struct HeadInfo {
    /// Next expected byte (authoritative received-byte count).
    pub offset: u64,
    /// Declared total size, when known.
    pub total_size: Option<u64>,
    /// Session state (`X-Capsule-Upload-Status`).
    pub status: String,
    /// The server's advice (`X-Capsule-Advice`), already clamped; absent from an older server.
    pub advice: ServerAdvice,
    /// The session's remaining lifetime, relative to this response.
    pub expiry: SessionExpiry,
    /// For a `failed_processing` session, the `error.*` code finalization failed with
    /// (`X-Capsule-Upload-Error`). Absent for every other state, and from an older server.
    pub error_code: Option<String>,
}

/// A row from `GET /upload/sessions` — the subset the resuming client needs.
/// Unknown server fields are tolerated (serde ignores them).
#[derive(Debug, Clone, Deserialize)]
pub struct SessionSummary {
    /// Upload session ID.
    pub id: String,
    /// The asset the session's blob belongs to, so a client can find every session of one
    /// asset (the redaction a delete makes). Empty when the server named none.
    #[serde(default)]
    pub asset_id: String,
    /// Bytes the server has received so far.
    pub received_bytes: u64,
    /// Declared total size.
    pub total_size: u64,
    /// Session state (serialized enum variant name).
    pub status: String,
    /// For a `failed_processing` session, the `error.*` code finalization failed with — the
    /// code `HEAD` carries in `X-Capsule-Upload-Error`. Absent otherwise, and from an older
    /// server.
    #[serde(default)]
    pub error_code: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ListSessionsBody {
    sessions: Vec<SessionSummary>,
}

/// The server's `ApiError` body: an English detail plus, when present, the stable
/// `error.*` catalog code the client switches on.
#[derive(Debug, Deserialize)]
struct ApiErrorBody {
    #[serde(default)]
    error: String,
    #[serde(default)]
    code: Option<String>,
}

// ─── Outcomes ───────────────────────────────────────────────────────────────

/// How `POST /upload` resolved.
#[derive(Debug, Clone)]
pub enum CreateOutcome {
    /// A session to transfer into. `resume_offset` is the authoritative
    /// received-byte count: `0` on a fresh create (`201`); on an idempotent
    /// re-create the server returns the **existing active session** (`200`) with
    /// its current `X-Capsule-Offset`, so the client resumes without a `HEAD`.
    Created {
        /// The session body (`id`, advisory URL, suggested chunk size).
        response: CreateSessionResponse,
        /// Where the transfer continues from (bytes the server already holds).
        resume_offset: u64,
        /// The server's advice (`X-Capsule-Advice`), already clamped; absent from an older
        /// server.
        advice: ServerAdvice,
        /// The session's remaining lifetime, relative to this response.
        expiry: SessionExpiry,
        /// The resumed session's state (`X-Capsule-Upload-Status`) on a `200`:
        /// `waiting_for_processing` once a duplicate create has started the finalization of a
        /// full session. Absent on a fresh session, and from an older server.
        status: Option<String>,
    },
    /// The server already holds this exact ciphertext (`duplicate_blob`): resolve
    /// as a [merge](https://docs/design/transfer/upload/#deduplication-and-merge),
    /// not a transfer. Carries the existing asset reference.
    AlreadyStored {
        /// The server-named existing asset the new reference should merge onto.
        asset_ref: String,
    },
}

/// How a full [`UploadClient::upload`] resolved.
#[derive(Debug, Clone)]
pub enum UploadOutcome {
    /// Every declared byte was transferred and the server finalized the session: `HEAD` reported
    /// it `completed`. From an older server, which finalized before it answered the completing
    /// chunk and so never answered `waiting_for_processing`, the accepted final chunk is the
    /// outcome.
    ///
    /// A session finalization refused is not this: it is [`UploadError::Rejected`] carrying the
    /// `error.*` code `HEAD` reported in `X-Capsule-Upload-Error`.
    Completed {
        /// The session that was driven to completion.
        session_id: String,
    },
    /// The blob was already stored server-side — treated as success carrying the
    /// existing asset ref (the `duplicate_blob` → merge path).
    AlreadyStored {
        /// The existing asset the caller merges onto.
        asset_ref: String,
    },
}

/// Upload client errors.
#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    /// IO error over the local byte source.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// Network / transport failure (connection, timeout, body read).
    #[error("transport error: {0}")]
    Transport(String),
    /// A typed server rejection carrying its stable `error.*` code and status.
    /// Non-recoverable at this layer — surfaced to the caller.
    ///
    /// A finalization the server refused is reported the same way, with the code `HEAD` carried
    /// in `X-Capsule-Upload-Error` (`content_hash_mismatch`, `envelope_rejected`,
    /// `replace_incomplete`, `stale_revival`, …) and the `200` of the `HEAD` that read it: no
    /// request waits on finalization, so its refusal is read, never answered. A caller switches
    /// on the code, as it always has.
    #[error("upload rejected (HTTP {status}, code {code:?}): {message}")]
    Rejected {
        /// HTTP status.
        status: u16,
        /// The stable `error.*` code, when the server supplied one.
        code: Option<String>,
        /// The English detail message.
        message: String,
    },
    /// The server will not take this client's uploads: it answered `426 Upgrade Required`, or
    /// the window it advertised already refuses this build, so the request never left. The
    /// SDK's one [`UpgradeRequired`](crate::net::negotiation::UpgradeRequired), flattened:
    /// carries the advertised window so the UI can show an actionable "update Capsule to keep
    /// uploading" message. Abort-with-upgrade.
    #[error(
        "protocol upgrade required ({reason:?}; server accepts {min:?}..={max:?}, client build \
         >= {min_client_build:?}): {message}"
    )]
    UpgradeRequired {
        /// Which rule refused.
        reason: crate::net::negotiation::UpgradeReason,
        /// `X-Capsule-Protocol-Min`.
        min: Option<String>,
        /// `X-Capsule-Protocol-Max`.
        max: Option<String>,
        /// `X-Capsule-Min-Client-Build`.
        min_client_build: Option<String>,
        /// The English detail message.
        message: String,
    },
    /// A recovery action (re-align / re-create / re-send) exceeded its bounded
    /// budget — the peer is stuck; fail loudly rather than hot-loop.
    #[error("upload gave up after exhausting the recovery budget: {0}")]
    RetriesExhausted(String),
    /// A server response was missing a required header or otherwise unparsable.
    #[error("malformed server response: {0}")]
    MalformedResponse(String),
    /// The session's auth layer failed (not authenticated, refresh rejected,
    /// session expired). Carries the typed session error so callers can trigger
    /// interactive re-authentication.
    #[error("authentication error: {0}")]
    Auth(#[from] crate::auth::AuthError),
}

/// The bytes of one blob, read a chunk at a time.
///
/// The client asks for exactly the window it is about to send — again after a re-align, a
/// re-send or a resume — so a source never has to hold the blob. A byte slice is one; an
/// original, whose ciphertext is re-derived from its plaintext per window, is the other that
/// matters (`capsule_library::lifecycle::OriginalCiphertext`).
pub trait UploadSource: Sync {
    /// The bytes `[offset, offset + len)`. Fewer than `len` bytes is a source error: the
    /// declared size is the blob's, and a short read means the source changed under the upload.
    fn read_at(&self, offset: u64, len: usize) -> std::io::Result<Cow<'_, [u8]>>;
}

impl UploadSource for [u8] {
    fn read_at(&self, offset: u64, len: usize) -> std::io::Result<Cow<'_, [u8]>> {
        usize::try_from(offset)
            .ok()
            .and_then(|start| self.get(start..start.checked_add(len)?))
            .map(Cow::Borrowed)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "upload window past the end of the bytes",
                )
            })
    }
}

impl UploadSource for Vec<u8> {
    fn read_at(&self, offset: u64, len: usize) -> std::io::Result<Cow<'_, [u8]>> {
        self.as_slice().read_at(offset, len)
    }
}

impl<const N: usize> UploadSource for [u8; N] {
    fn read_at(&self, offset: u64, len: usize) -> std::io::Result<Cow<'_, [u8]>> {
        self.as_slice().read_at(offset, len)
    }
}

impl<T: UploadSource + ?Sized> UploadSource for &T {
    fn read_at(&self, offset: u64, len: usize) -> std::io::Result<Cow<'_, [u8]>> {
        (**self).read_at(offset, len)
    }
}

impl From<reqwest::Error> for UploadError {
    fn from(err: reqwest::Error) -> Self {
        UploadError::Transport(err.to_string())
    }
}

impl From<crate::net::negotiation::UpgradeRequired> for UploadError {
    fn from(refusal: crate::net::negotiation::UpgradeRequired) -> Self {
        let crate::net::negotiation::UpgradeRequired {
            reason,
            window,
            message,
            ..
        } = refusal;
        UploadError::UpgradeRequired {
            reason,
            min: window.protocol_min,
            max: window.protocol_max,
            min_client_build: window.min_client_build,
            message,
        }
    }
}

/// Compute the required `X-Capsule-Checksum`: the SHA-256 of the chunk bytes as
/// bare lowercase hex (byte-identical to the server's `hash_bytes`).
fn chunk_checksum(chunk: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(chunk);
    hex::encode(hasher.finalize())
}

/// Best-effort extraction of the existing asset id from a `duplicate_blob` detail
/// message (`"This content is already stored as asset {id}"`).
///
/// The current server exposes the reference only in the English detail, not as a
/// structured field/header; this parses it
/// out while falling back to the whole message so nothing is lost.
fn duplicate_asset_ref(message: &str) -> String {
    message
        .rsplit_once("asset ")
        .map(|(_, id)| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| message.to_string())
}

/// The outcome of a single `PATCH` attempt, folded into the recovery matrix.
#[derive(Debug)]
enum ChunkAck {
    /// Accepted: the new authoritative offset, and the session's state as the server reported it
    /// (`X-Capsule-Upload-Status`; absent from an older server).
    Accepted {
        /// The authoritative offset after the chunk.
        offset: u64,
        /// `uploading`, or `waiting_for_processing` once the chunk completed the session.
        status: Option<String>,
    },
    /// `offset_mismatch` — re-align to the authoritative offset (from the 409's
    /// `X-Capsule-Offset` when present, else a follow-up HEAD).
    Realign(Option<u64>),
    /// `session_not_found` — re-create the session and resume from zero.
    Recreate,
    /// `checksum_mismatch` — nothing was persisted; re-send the same chunk.
    Resend,
    /// `chunk_timeout` — the server cut the body for stalling, and nothing of it was
    /// written; re-send at the same offset, shorter.
    TimedOut,
    /// The client cut the body for stalling: no bytes moved for the stall bound. Nothing of a
    /// chunk that did not arrive whole is written, so the same answer as [`Self::TimedOut`].
    Stalled,
    /// The client handed the whole body off and no response head came within the response
    /// wait. The tail may still have been in the HTTP stack's or the kernel's send buffer on a
    /// link too slow to drain it, or the server may have kept the chunk and never answered:
    /// re-send at the same offset, shorter — so a slow link converges on a chunk it drains in
    /// time, and a `409 offset_mismatch` re-aligns where the server did keep it. A cut that
    /// shrinks is charged as a stall re-send; only one at the protocol minimum backs off as for
    /// a reset.
    Unanswered,
    /// `rate_limited`, `unavailable` or `storage_full` — the server asked the
    /// client to wait (for the `Retry-After` it sent, when it sent one) and then
    /// resume from HEAD. Nothing of the refused chunk was kept.
    Backoff(Option<Duration>),
}

/// Whether `code` is one of the three answers that mean "wait, then resume".
fn asks_to_wait(code: &str) -> bool {
    code == error_codes::UPLOAD_RATE_LIMITED
        || code == error_codes::UPLOAD_UNAVAILABLE
        || code == error_codes::UPLOAD_STORAGE_FULL
}

/// Whether a non-success answer asks the client to wait.
///
/// The code decides whenever the server sent one. A `HEAD` answer has no body by
/// HTTP's rules, so it can carry no code: there the status names the three wait
/// answers (`429`, `503`, `507`), which the server sends only with those codes.
fn answer_asks_to_wait(status: u16, code: Option<&str>) -> bool {
    match code {
        Some(code) => asks_to_wait(code),
        None => matches!(status, 429 | 503 | 507),
    }
}

/// A single request the server answered with "wait, then ask again".
///
/// The single-shot public calls surface it as [`UploadError::Rejected`]; the
/// transfer loop waits it out under the recovery budget instead.
#[derive(Debug)]
struct WaitAsked {
    status: u16,
    code: Option<String>,
    message: String,
    /// The `Retry-After` the server sent, when it sent one.
    after: Option<Duration>,
}

impl From<WaitAsked> for UploadError {
    fn from(wait: WaitAsked) -> Self {
        UploadError::Rejected {
            status: wait.status,
            code: wait.code,
            message: wait.message,
        }
    }
}

/// The bounded recovery state one transfer shares across every request it makes:
/// the recovery budget, and the shared retry engine's backoff schedule.
struct Recovery {
    /// Recovery actions taken so far, bounded by [`MAX_RECOVERIES`].
    count: u32,
    /// The shared retry engine, `bulk-transfer` class: exponential backoff with
    /// full jitter, bounded give-up. It governs mid-transfer transport resets and
    /// server-asked waits that name no `Retry-After`.
    engine: RetryEngine,
}

impl Recovery {
    fn new() -> Self {
        Self {
            count: 0,
            engine: RetryClass::BulkTransfer.engine(),
        }
    }

    /// Charge one recovery action, failing loudly once the budget is spent.
    fn charge(&mut self, action: &str) -> Result<(), UploadError> {
        self.count += 1;
        guard_budget(self.count, action)
    }

    /// Charge a server-asked wait and resolve how long to wait: the server's
    /// `Retry-After` up to [`MAX_SERVER_BACKOFF`], else the engine's schedule.
    fn server_wait(
        &mut self,
        asked: Option<Duration>,
        action: &str,
    ) -> Result<Duration, UploadError> {
        self.charge(action)?;
        match asked {
            Some(asked) => Ok(asked.min(MAX_SERVER_BACKOFF)),
            None => match self.engine.next_backoff(None) {
                RetryDecision::Retry { after } => Ok(after),
                RetryDecision::GiveUp => Err(UploadError::RetriesExhausted(format!(
                    "{action}: the server kept asking for a wait without naming one"
                ))),
            },
        }
    }
}

/// The first wait between two `HEAD`s while the server finalizes a session.
const FINALIZE_POLL_FIRST: Duration = Duration::from_millis(100);

/// The longest wait between two `HEAD`s while the server finalizes a session. Each wait doubles
/// the one before it up to this.
const FINALIZE_POLL_MAX: Duration = Duration::from_secs(5);

/// How long, in waits summed, the client polls a finalizing session before it gives up.
///
/// The server drives every claim to a terminal state — across its own restarts — so the poll
/// ends; this bound only stops a client from polling a server that never says so. Half an hour
/// re-hashes a 4 GiB blob on a disk slower than any the server is deployed on.
const FINALIZE_POLL_DEADLINE: Duration = Duration::from_mins(30);

/// The session states the server reports while, and once, it finalizes.
const STATUS_WAITING: &str = "waiting_for_processing";
const STATUS_COMPLETED: &str = "completed";
const STATUS_FAILED: &str = "failed_processing";

/// What the finalization poll found.
enum Settled {
    /// The session is terminal: completed, or the transfer ends here.
    Done(UploadOutcome),
    /// The server holds fewer bytes than declared after all; the transfer resumes at this
    /// offset.
    Resume(u64),
}

/// The wait a `Retry-After` header names, in its delta-seconds form.
///
/// The server sends delta-seconds; the HTTP-date form is not parsed, and a value
/// that is neither leaves the client on its own backoff rather than guessing.
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    header_u64(headers, "Retry-After").map(Duration::from_secs)
}

/// The hand-written chunked, resumable, adaptive upload client.
#[derive(Clone)]
pub struct UploadClient {
    transport: UploadTransport,
    /// The detected connection class, coupling the chunk strategy to the network: under
    /// [`ConnectionClass::Adverse`] each chunk targets the shorter
    /// [`ADVERSE_TARGET_REQUEST_DURATION`]. Defaults to [`ConnectionClass::Unmetered`].
    connection: ConnectionClass,
    /// The backoff sleeper for the shared retry engine's transient-reset retries.
    /// Real in production; kept as a seam for determinism.
    sleeper: TokioSleeper,
    /// Where each transfer step is published ([`crate::events`]); disabled by default.
    events: TransferEvents,
}

impl UploadClient {
    /// Build a client over an authorized [`UploadTransport`], on an unmetered link.
    pub fn new(transport: UploadTransport) -> Self {
        Self {
            transport,
            connection: ConnectionClass::Unmetered,
            sleeper: TokioSleeper,
            events: TransferEvents::disabled(),
        }
    }

    /// Publish every transfer this client drives to `events` (session, chunk, retry,
    /// checksum-reject, finalize and merge steps). Without a subscriber it costs nothing.
    #[must_use]
    pub fn with_events(mut self, events: TransferEvents) -> Self {
        self.events = events;
        self
    }

    /// The event handle this client publishes to.
    #[must_use]
    pub fn events(&self) -> &TransferEvents {
        &self.events
    }

    /// Set the detected connection class (the target request duration follows it). On an
    /// [`ConnectionClass::Adverse`] link every transfer this client drives sizes its chunks to
    /// the shorter [`ADVERSE_TARGET_REQUEST_DURATION`].
    #[must_use]
    pub fn with_connection_class(mut self, class: ConnectionClass) -> Self {
        self.connection = class;
        self
    }

    /// Run every request this client sends under `bounds` instead of
    /// [`RequestBounds::default`].
    #[must_use]
    pub fn with_request_bounds(mut self, bounds: RequestBounds) -> Self {
        self.transport = self.transport.with_request_bounds(bounds);
        self
    }

    /// Create an upload session (`POST /upload`).
    ///
    /// Resolves to [`CreateOutcome::Created`], or [`CreateOutcome::AlreadyStored`]
    /// on `duplicate_blob` (the merge trigger). A `426` returns
    /// [`UploadError::UpgradeRequired`] — the session is never created.
    ///
    /// This is one request: `rate_limited`, `unavailable` and `storage_full` come
    /// back as [`UploadError::Rejected`]. [`UploadClient::upload`] waits those out.
    #[tracing::instrument(level = "debug", skip(self, request), fields(size = request.size, hash = %request.hash))]
    pub async fn create_session(
        &self,
        request: &CreateUploadRequest,
    ) -> Result<CreateOutcome, UploadError> {
        Ok(self.try_create(request).await??)
    }

    /// One `POST /upload`, with a server-asked wait kept apart from a rejection.
    async fn try_create(
        &self,
        request: &CreateUploadRequest,
    ) -> Result<Result<CreateOutcome, WaitAsked>, UploadError> {
        let resp = self
            .transport
            .send(|http| {
                http.post(self.transport.base_url.as_str())
                    .header("X-Capsule-Protocol", &self.transport.protocol_version)
                    .json(request)
            })
            .await?;

        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            // 201: fresh session (offset 0). 200: the idempotent re-create — the
            // existing active session, whose X-Capsule-Offset says where to resume.
            let resume_offset = header_u64(resp.headers(), "X-Capsule-Offset").unwrap_or(0);
            let (advice, expiry) = advice_of(resp.headers());
            let session_status = header_str(resp.headers(), "X-Capsule-Upload-Status");
            let body: CreateSessionResponse = resp.json().await?;
            tracing::debug!(session_id = %body.id, resume_offset, ?session_status, ?advice, ?expiry, "upload session created");
            return Ok(Ok(CreateOutcome::Created {
                response: body,
                resume_offset,
                advice,
                expiry,
                status: session_status,
            }));
        }

        let after = retry_after(resp.headers());
        let (code, message, headers) = read_error(resp).await?;
        if is_upgrade_required(status, code.as_deref()) {
            return Err(self.transport.upgrade_required(&headers, message));
        }
        if code.as_deref() == Some(error_codes::UPLOAD_DUPLICATE_BLOB) {
            let asset_ref = duplicate_asset_ref(&message);
            tracing::info!(%asset_ref, "duplicate_blob — resolving as a merge");
            return Ok(Ok(CreateOutcome::AlreadyStored { asset_ref }));
        }
        if answer_asks_to_wait(status, code.as_deref()) {
            tracing::debug!(status, ?code, ?after, "create: the server asked for a wait");
            return Ok(Err(WaitAsked {
                status,
                code,
                message,
                after,
            }));
        }
        Err(UploadError::Rejected {
            status,
            code,
            message,
        })
    }

    /// Query progress (`HEAD /upload/{id}`). `Ok(None)` means the session is gone
    /// (`404`) — a discarded/expired/never-existent session is uniform.
    ///
    /// This is one request: a `429`, `503` or `507` comes back as
    /// [`UploadError::Rejected`]. The transfer loop waits those out.
    #[tracing::instrument(level = "debug", skip(self))]
    pub async fn head(&self, session_id: &str) -> Result<Option<HeadInfo>, UploadError> {
        Ok(self.try_head(session_id).await??)
    }

    /// One `HEAD /upload/{id}`, with a server-asked wait kept apart from a rejection.
    async fn try_head(
        &self,
        session_id: &str,
    ) -> Result<Result<Option<HeadInfo>, WaitAsked>, UploadError> {
        let resp = self
            .transport
            .send(|http| {
                http.head(self.transport.session_url(session_id))
                    .header("X-Capsule-Protocol", &self.transport.protocol_version)
            })
            .await?;

        let status = resp.status().as_u16();
        if status == 404 {
            return Ok(Ok(None));
        }
        if !(200..300).contains(&status) {
            let after = retry_after(resp.headers());
            let (code, message, headers) = read_error(resp).await?;
            // A HEAD answer carries no body, so the bare `426` is all there is to switch on;
            // it is the same refusal a create or a chunk gets, and surfaces as the same error.
            if is_upgrade_required(status, code.as_deref()) {
                return Err(self.transport.upgrade_required(&headers, message));
            }
            if answer_asks_to_wait(status, code.as_deref()) {
                tracing::debug!(status, ?code, ?after, "HEAD: the server asked for a wait");
                return Ok(Err(WaitAsked {
                    status,
                    code,
                    message,
                    after,
                }));
            }
            return Err(UploadError::Rejected {
                status,
                code,
                message,
            });
        }

        let headers = resp.headers();
        let offset = header_u64(headers, "X-Capsule-Offset").ok_or_else(|| {
            UploadError::MalformedResponse("HEAD response missing X-Capsule-Offset".into())
        })?;
        let total_size = header_u64(headers, "X-Capsule-Content-Length");
        let statusv = header_str(headers, "X-Capsule-Upload-Status").unwrap_or_default();
        let error_code = header_str(headers, "X-Capsule-Upload-Error");
        let (advice, expiry) = advice_of(headers);
        Ok(Ok(Some(HeadInfo {
            offset,
            total_size,
            status: statusv,
            advice,
            expiry,
            error_code,
        })))
    }

    /// `POST /upload`, waiting out every `rate_limited`, `unavailable` and
    /// `storage_full` answer under the transfer's recovery budget.
    async fn create_waiting(
        &self,
        request: &CreateUploadRequest,
        recovery: &mut Recovery,
    ) -> Result<CreateOutcome, UploadError> {
        loop {
            match self.try_create(request).await? {
                Ok(outcome) => return Ok(outcome),
                Err(asked) => {
                    let wait = recovery.server_wait(asked.after, "session create")?;
                    tracing::debug!(?wait, "waiting as the server asked, then creating again");
                    self.sleeper.sleep(wait).await;
                }
            }
        }
    }

    /// `HEAD /upload/{id}`, waiting out every `429`, `503` and `507` answer under
    /// the transfer's recovery budget.
    async fn head_waiting(
        &self,
        session_id: &str,
        recovery: &mut Recovery,
    ) -> Result<Option<HeadInfo>, UploadError> {
        loop {
            match self.try_head(session_id).await? {
                Ok(info) => return Ok(info),
                Err(asked) => {
                    let wait = recovery.server_wait(asked.after, "progress query")?;
                    tracing::debug!(?wait, "waiting as the server asked, then asking HEAD again");
                    self.sleeper.sleep(wait).await;
                }
            }
        }
    }

    /// Cancel a session (`DELETE /upload/{id}`). Idempotent for the caller: a
    /// `404` (already gone) resolves as `Ok(())`; a `409` (finalization running,
    /// not interruptible, or the session already finished) surfaces as
    /// [`UploadError::Rejected`] with `error.upload.session_not_active`.
    /// [`push::redact_asset`](crate::push::redact_asset) reads that answer as a
    /// session that finalized during the abort.
    #[tracing::instrument(level = "debug", skip(self))]
    pub async fn delete(&self, session_id: &str) -> Result<(), UploadError> {
        let resp = self
            .transport
            .send(|http| {
                http.delete(self.transport.session_url(session_id))
                    .header("X-Capsule-Protocol", &self.transport.protocol_version)
            })
            .await?;

        let status = resp.status().as_u16();
        if (200..300).contains(&status) || status == 404 {
            return Ok(());
        }
        let (code, message, _) = read_error(resp).await?;
        Err(UploadError::Rejected {
            status,
            code,
            message,
        })
    }

    /// List the uploader's active sessions (`GET /upload/sessions`), so a client
    /// can resume across app restarts.
    #[tracing::instrument(level = "debug", skip(self))]
    pub async fn list_sessions(&self) -> Result<Vec<SessionSummary>, UploadError> {
        let base = &self.transport.base_url;
        let resp = self
            .transport
            .send(|http| {
                http.get(format!("{base}/sessions"))
                    .header("X-Capsule-Protocol", &self.transport.protocol_version)
            })
            .await?;

        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let (code, message, _) = read_error(resp).await?;
            return Err(UploadError::Rejected {
                status,
                code,
                message,
            });
        }
        let body: ListSessionsBody = resp.json().await?;
        Ok(body.sessions)
    }

    /// Append one chunk at `offset` (`PATCH /upload/{id}`), sending the required
    /// `X-Capsule-Checksum` and `X-Capsule-Offset`. Maps the server's response to
    /// a [`ChunkAck`]; `426` and other non-recoverable rejections are `Err`.
    #[tracing::instrument(level = "trace", skip(self, chunk), fields(session_id = %session_id, offset, len = chunk.len()))]
    async fn send_patch(
        &self,
        session_id: &str,
        chunk: &[u8],
        offset: u64,
    ) -> Result<ChunkAck, UploadError> {
        let checksum = chunk_checksum(chunk);
        let bytes = bytes::Bytes::copy_from_slice(chunk);
        // The stall bound runs while the body is sent, the response-wait bound once it is all
        // sent: a slow link is never cut, a stalled one is.
        let progress = ExchangeProgress::waiting();
        let resp = self
            .transport
            .send_tracked(&progress, |http| {
                http.patch(self.transport.session_url(session_id))
                    .header("X-Capsule-Protocol", &self.transport.protocol_version)
                    .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                    .header("X-Capsule-Offset", offset.to_string())
                    .header("X-Capsule-Checksum", &checksum)
                    // A fresh body per invocation, over the same bytes: the session path may
                    // rebuild the request for its one 401 refresh-and-replay.
                    .body(PacedBody::new(bytes.clone(), progress.clone()))
            })
            .await?;
        let resp = match resp {
            Ok(resp) => resp,
            // The link stopped moving: nothing of the chunk can have been kept, so it is the
            // same answer as the server's own stall cut.
            Err(RequestCut::Stalled) => return Ok(ChunkAck::Stalled),
            // The body was handed off whole and no answer came: either the server never
            // answered, or the link is too slow to drain the tail the stack still buffers.
            Err(RequestCut::NoResponse) => return Ok(ChunkAck::Unanswered),
        };

        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            let new_offset = header_u64(resp.headers(), "X-Capsule-Offset")
                .unwrap_or(offset + chunk.len() as u64);
            let session_status = header_str(resp.headers(), "X-Capsule-Upload-Status");
            return Ok(ChunkAck::Accepted {
                offset: new_offset,
                status: session_status,
            });
        }

        // The 409 offset_mismatch carries the authoritative offset in the header;
        // capture it before consuming the body.
        let authoritative = header_u64(resp.headers(), "X-Capsule-Offset");
        let wait = retry_after(resp.headers());
        let (code, message, headers) = read_error(resp).await?;

        // Recovery matrix — switch on the stable error.* code, never the status.
        match code.as_deref() {
            Some(c) if c == error_codes::UPLOAD_OFFSET_MISMATCH => {
                tracing::debug!(?authoritative, "offset_mismatch — re-aligning");
                Ok(ChunkAck::Realign(authoritative))
            }
            Some(c) if c == error_codes::UPLOAD_SESSION_NOT_FOUND => {
                tracing::debug!("session_not_found — re-creating");
                Ok(ChunkAck::Recreate)
            }
            Some(c) if c == error_codes::UPLOAD_CHECKSUM_MISMATCH => {
                tracing::debug!("checksum_mismatch — re-sending chunk");
                Ok(ChunkAck::Resend)
            }
            Some(c) if c == error_codes::UPLOAD_CHUNK_TIMEOUT => {
                tracing::debug!("chunk_timeout — re-sending smaller");
                Ok(ChunkAck::TimedOut)
            }
            Some(c) if asks_to_wait(c) => {
                tracing::debug!(code = c, ?wait, "the server asked for a wait — backing off");
                Ok(ChunkAck::Backoff(wait))
            }
            _ if is_upgrade_required(status, code.as_deref()) => {
                Err(self.transport.upgrade_required(&headers, message))
            }
            _ => Err(UploadError::Rejected {
                status,
                code,
                message,
            }),
        }
    }

    /// Transfer a full ciphertext blob with the adaptive algorithm and the
    /// code-driven recovery matrix, starting from a fresh session.
    ///
    /// `data` is the ciphertext's source (opaque bytes); the client reads it one
    /// chunk window at a time — never whole — and checksums each chunk. Every byte is sent when
    /// the received-byte count reaches the declared size, at which point the server finalizes.
    /// The server answers the completing chunk `waiting_for_processing` without waiting for
    /// finalization, so the client then polls `HEAD` with backoff until the session is
    /// `completed` — the outcome — or `failed_processing`, which is an
    /// [`UploadError::Rejected`] carrying the `X-Capsule-Upload-Error` code.
    ///
    /// A `rate_limited`, `unavailable` or `storage_full` answer to any request it
    /// makes — the create, a chunk, or a `HEAD` — is waited out, each wait charging
    /// the one recovery budget the transfer shares.
    #[tracing::instrument(level = "debug", skip(self, data), fields(size = request.size, blob_role = ?request.blob_role))]
    pub async fn upload<S: UploadSource + ?Sized>(
        &self,
        request: &CreateUploadRequest,
        data: &S,
    ) -> Result<UploadOutcome, UploadError> {
        self.upload_with(request, data, &mut Recovery::new()).await
    }

    /// [`UploadClient::upload`] over a recovery budget the caller may already have
    /// charged.
    async fn upload_with<S: UploadSource + ?Sized>(
        &self,
        request: &CreateUploadRequest,
        data: &S,
        recovery: &mut Recovery,
    ) -> Result<UploadOutcome, UploadError> {
        let (session, suggested, start_offset, start_status) =
            match self.create_waiting(request, recovery).await? {
                CreateOutcome::Created {
                    response,
                    resume_offset,
                    advice,
                    status,
                    ..
                } => {
                    let chunk = response.starting_chunk(&advice);
                    (response.id, chunk, resume_offset, status)
                }
                CreateOutcome::AlreadyStored { asset_ref } => {
                    self.emit_merged(request, &asset_ref);
                    return Ok(UploadOutcome::AlreadyStored { asset_ref });
                }
            };
        self.emit_created(request, &session, start_offset);

        let mut strategy = AdaptiveChunkSizeStrategy::for_file_size(request.size)
            .seeded_from_suggested(suggested)
            .with_connection_class(self.connection);
        self.drive(
            session,
            request,
            data,
            &mut strategy,
            (start_offset, start_status),
            recovery,
        )
        .await
    }

    /// Resume an existing session: HEAD for the authoritative offset, then
    /// continue from there — no bytes the server already holds are re-sent
    /// (transfer upload doc, §Idempotency and Resumption). If the session is gone
    /// (`404`), it is re-created and driven from zero.
    #[tracing::instrument(level = "debug", skip(self, data), fields(size = request.size))]
    pub async fn upload_resuming<S: UploadSource + ?Sized>(
        &self,
        session_id: &str,
        request: &CreateUploadRequest,
        data: &S,
    ) -> Result<UploadOutcome, UploadError> {
        let mut recovery = Recovery::new();
        if let Some(info) = self.head_waiting(session_id, &mut recovery).await? {
            let mut strategy = AdaptiveChunkSizeStrategy::for_file_size(request.size);
            if let Some(chunk) = info.advice.chunk(Duration::ZERO) {
                strategy = strategy.seeded_from_suggested(chunk);
            }
            let mut strategy = strategy.with_connection_class(self.connection);
            tracing::debug!(offset = info.offset, "resuming from authoritative offset");
            self.events.emit(|| TransferEvent::SessionResumed {
                blob: request.hash.clone(),
                session_id: session_id.to_string(),
                size: request.size,
                offset: info.offset,
            });
            self.drive(
                session_id.to_string(),
                request,
                data,
                &mut strategy,
                (info.offset, Some(info.status)),
                &mut recovery,
            )
            .await
        } else {
            tracing::debug!("session gone on resume — re-creating");
            self.upload_with(request, data, &mut recovery).await
        }
    }

    /// The chunk loop with the recovery matrix, then the finalization poll. `start` is the
    /// starting authoritative offset (0 for a fresh session, the HEAD offset on resume) and the
    /// session's state as last reported, when one was.
    async fn drive<S: UploadSource + ?Sized>(
        &self,
        mut session_id: String,
        request: &CreateUploadRequest,
        data: &S,
        strategy: &mut AdaptiveChunkSizeStrategy,
        start: (u64, Option<String>),
        recovery: &mut Recovery,
    ) -> Result<UploadOutcome, UploadError> {
        let size = request.size;
        let (mut offset, mut state) = start;
        // The recovery's shared retry engine governs mid-transfer *transport
        // resets* (the adverse-network steady state); the code-driven recovery
        // matrix below stays switched on `error.*` codes.

        while offset < size {
            let remaining = size - offset;
            // Non-final chunks are exactly the adaptive size (4 KiB-aligned by
            // construction); the final chunk is the remainder (alignment-exempt).
            let want = strategy.next_chunk_size().min(remaining);
            // Read exactly this window — a re-send or a re-align reads it again — so the
            // client holds one chunk of the blob, never the blob.
            let chunk = data.read_at(offset, want as usize)?;
            if chunk.len() as u64 != want {
                return Err(UploadError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    format!(
                        "upload source returned {} of {want} bytes at offset {offset}",
                        chunk.len()
                    ),
                )));
            }

            let attempt_start = std::time::Instant::now();
            self.events.emit(|| TransferEvent::ChunkSent {
                blob: request.hash.clone(),
                session_id: session_id.clone(),
                offset,
                len: want,
            });
            let ack = match self.send_patch(&session_id, &chunk, offset).await {
                Ok(ack) => ack,
                // A mid-transfer connection reset / silent black-hole: back off
                // through the shared engine and resume from the SAME offset (the
                // server holds nothing new, so no bytes are re-sent beyond this
                // chunk). Bounded give-up prevents a hot loop on a dead path.
                Err(UploadError::Transport(reason)) => {
                    self.back_off_reset(recovery, request, &session_id, (offset, want), &reason)
                        .await?;
                    continue;
                }
                Err(other) => return Err(other),
            };
            match ack {
                ChunkAck::Accepted {
                    offset: new_offset,
                    status,
                } => {
                    state = status;
                    strategy.record_chunk(want, attempt_start.elapsed());
                    // Real progress resets the transient-reset backoff budget.
                    recovery.engine.reset();
                    self.events.emit(|| TransferEvent::ChunkAcked {
                        blob: request.hash.clone(),
                        session_id: session_id.clone(),
                        offset,
                        len: want,
                        acked_offset: new_offset,
                    });
                    offset = new_offset;
                }
                ChunkAck::Realign(authoritative) => {
                    recovery.charge("offset re-align")?;
                    let realigned = match authoritative {
                        Some(o) => Some(o),
                        None => self
                            .head_waiting(&session_id, recovery)
                            .await?
                            .map(|info| info.offset),
                    };
                    state = None;
                    if let Some(to) = realigned {
                        self.emit_retried(
                            request,
                            &session_id,
                            offset,
                            want,
                            RetryReason::OffsetRealign { to },
                        );
                        offset = to;
                    } else {
                        // Vanished mid-realign → recreate from zero.
                        self.emit_retried(
                            request,
                            &session_id,
                            offset,
                            want,
                            RetryReason::SessionLost,
                        );
                        session_id = self.recreate(request, recovery).await?;
                        *strategy = AdaptiveChunkSizeStrategy::for_file_size(size);
                        offset = 0;
                    }
                }
                ChunkAck::Recreate => {
                    recovery.charge("session re-create")?;
                    self.emit_retried(request, &session_id, offset, want, RetryReason::SessionLost);
                    match self.create_waiting(request, recovery).await? {
                        CreateOutcome::Created {
                            response,
                            resume_offset,
                            advice,
                            status,
                            ..
                        } => {
                            let chunk = response.starting_chunk(&advice);
                            state = status;
                            session_id = response.id;
                            *strategy = AdaptiveChunkSizeStrategy::for_file_size(size)
                                .seeded_from_suggested(chunk);
                            offset = resume_offset;
                            self.emit_created(request, &session_id, offset);
                        }
                        // A blob that finalized between our attempts → merge.
                        CreateOutcome::AlreadyStored { asset_ref } => {
                            self.emit_merged(request, &asset_ref);
                            return Ok(UploadOutcome::AlreadyStored { asset_ref });
                        }
                    }
                }
                ChunkAck::Resend => {
                    recovery.charge("chunk re-send")?;
                    self.events.emit(|| TransferEvent::ChecksumRejected {
                        blob: request.hash.clone(),
                        session_id: session_id.clone(),
                        offset,
                        len: want,
                    });
                    // Offset unchanged; the loop re-sends the same chunk.
                }
                ack @ (ChunkAck::TimedOut | ChunkAck::Stalled) => {
                    recovery.charge("stalled chunk re-send")?;
                    // The offset stays: the server writes nothing of a chunk that did
                    // not arrive whole. No wait either, since the link just spent a
                    // whole stall bound on this chunk.
                    let shrunk = strategy.shrink_after_stall(want);
                    tracing::info!(
                        %session_id,
                        offset,
                        stalled_len = want,
                        next_len = strategy.next_chunk_size().min(remaining),
                        shrunk,
                        cut_by = if matches!(ack, ChunkAck::Stalled) { "client" } else { "server" },
                        "a chunk stalled — re-sending at the same offset, shorter"
                    );
                }
                ChunkAck::Unanswered => {
                    // Shrink first: a link too slow to drain the buffered tail inside the
                    // response wait would otherwise be cut at the same size until the retries
                    // ran out, though the server accepts every byte it receives.
                    let shrunk = strategy.shrink_after_stall(want);
                    tracing::info!(
                        %session_id,
                        offset,
                        unanswered_len = want,
                        next_len = strategy.next_chunk_size().min(remaining),
                        shrunk,
                        "a chunk got no answer — re-sending at the same offset, shorter where it can be"
                    );
                    if shrunk {
                        // A cut that shrank is a stall re-send, charged like one: halving is
                        // bounded by the protocol minimum, so it cannot hot-loop, and spending
                        // the reset retries on it would give up on a 16 MiB chunk at 64 KiB.
                        // No wait either, since the link just spent a whole response wait.
                        recovery.charge("unanswered chunk re-send")?;
                        continue;
                    }
                    // Nothing smaller can be sent: an unanswered minimum chunk is a dead path
                    // until shown otherwise, and backs off under the reset budget.
                    self.back_off_reset(
                        recovery,
                        request,
                        &session_id,
                        (offset, want),
                        &RequestCut::NoResponse.to_string(),
                    )
                    .await?;
                }
                ChunkAck::Backoff(asked) => {
                    let wait = recovery.server_wait(asked, "server-requested wait")?;
                    tracing::debug!(
                        offset,
                        ?wait,
                        "waiting as the server asked, then resuming from HEAD"
                    );
                    self.emit_retried(
                        request,
                        &session_id,
                        offset,
                        want,
                        RetryReason::ServerWait {
                            wait_ms: millis(wait),
                        },
                    );
                    self.sleeper.sleep(wait).await;
                    // The refused chunk left nothing behind, but anything may have moved
                    // while we waited: the authoritative offset is HEAD's. A HEAD that
                    // is itself asked to wait (an outage outlasting one wait) waits
                    // again under the same budget rather than failing the transfer.
                    offset = if let Some(info) = self.head_waiting(&session_id, recovery).await? {
                        state = Some(info.status);
                        info.offset
                    } else {
                        state = None;
                        self.emit_retried(
                            request,
                            &session_id,
                            offset,
                            want,
                            RetryReason::SessionLost,
                        );
                        session_id = self.recreate(request, recovery).await?;
                        *strategy = AdaptiveChunkSizeStrategy::for_file_size(size);
                        0
                    };
                }
            }
        }

        tracing::info!(%session_id, size, "every byte sent — server finalizing");
        self.events.emit(|| TransferEvent::Finalize {
            blob: request.hash.clone(),
            session_id: session_id.clone(),
            size,
        });
        match self
            .settle(&session_id, size, state.as_deref(), recovery)
            .await?
        {
            Settled::Done(outcome) => Ok(outcome),
            Settled::Resume(at) => {
                tracing::info!(%session_id, offset = at, size, "the server holds fewer bytes than declared; resuming");
                Box::pin(self.drive(session_id, request, data, strategy, (at, None), recovery))
                    .await
            }
        }
    }

    /// Wait for the server to finish finalizing `session_id`, polling `HEAD` with backoff.
    ///
    /// `state` is the session's state as the last answer reported it. Only a session reported
    /// `waiting_for_processing` or `failed_processing` is polled: an older server finalized
    /// before it answered the completing chunk and sends no state, so its accepted final chunk
    /// is the outcome. A `HEAD` the server asks to wait is waited out under the transfer's
    /// recovery budget; the poll itself charges nothing and is bounded by
    /// [`FINALIZE_POLL_DEADLINE`].
    async fn settle(
        &self,
        session_id: &str,
        size: u64,
        state: Option<&str>,
        recovery: &mut Recovery,
    ) -> Result<Settled, UploadError> {
        if !matches!(state, Some(STATUS_WAITING | STATUS_FAILED)) {
            tracing::debug!(%session_id, ?state, "no finalization to wait for");
            return Ok(Settled::Done(UploadOutcome::Completed {
                session_id: session_id.to_owned(),
            }));
        }
        let mut wait = FINALIZE_POLL_FIRST;
        let mut waited = Duration::ZERO;
        let mut polls: u32 = 0;
        loop {
            polls += 1;
            let Some(info) = self.head_waiting(session_id, recovery).await? else {
                tracing::warn!(%session_id, polls, "the session went away while it finalized");
                return Err(UploadError::Rejected {
                    status: 404,
                    code: Some(error_codes::UPLOAD_SESSION_NOT_FOUND.to_owned()),
                    message: format!("session {session_id} went away while it finalized"),
                });
            };
            match info.status.as_str() {
                STATUS_COMPLETED => {
                    tracing::info!(%session_id, polls, waited_ms = millis(waited), "the server finalized the upload");
                    return Ok(Settled::Done(UploadOutcome::Completed {
                        session_id: session_id.to_owned(),
                    }));
                }
                STATUS_FAILED => {
                    tracing::warn!(%session_id, polls, code = ?info.error_code, "the server refused the upload at finalization");
                    let message = format!(
                        "the server refused session {session_id} at finalization ({})",
                        info.error_code.as_deref().unwrap_or("no code given")
                    );
                    return Err(UploadError::Rejected {
                        status: 200,
                        code: info.error_code,
                        message,
                    });
                }
                STATUS_WAITING => {}
                _ if info.offset < size => return Ok(Settled::Resume(info.offset)),
                // Full and not yet claimed: this `HEAD` started its finalization, and the next
                // one reads how it went.
                _ => {}
            }
            if waited >= FINALIZE_POLL_DEADLINE {
                return Err(UploadError::RetriesExhausted(format!(
                    "session {session_id} was still finalizing after {polls} polls over {}s",
                    waited.as_secs()
                )));
            }
            tracing::debug!(%session_id, polls, ?wait, status = %info.status, "finalizing; polling HEAD again");
            self.sleeper.sleep(wait).await;
            waited += wait;
            wait = (wait * 2).min(FINALIZE_POLL_MAX);
        }
    }

    fn emit_created(&self, request: &CreateUploadRequest, session_id: &str, resume_offset: u64) {
        self.events.emit(|| TransferEvent::SessionCreated {
            blob: request.hash.clone(),
            session_id: session_id.to_string(),
            size: request.size,
            resume_offset,
        });
    }

    fn emit_merged(&self, request: &CreateUploadRequest, asset_ref: &str) {
        self.events.emit(|| TransferEvent::Merged {
            blob: request.hash.clone(),
            asset_ref: asset_ref.to_string(),
        });
    }

    /// Back off after a mid-transfer transport reset — or a chunk at the protocol minimum that
    /// got no answer — through
    /// the shared retry engine, so the caller resumes from the same offset; the server holds
    /// nothing new, so no bytes are re-sent beyond this chunk. The `(offset, len)` pair names
    /// the chunk that was cut.
    ///
    /// # Errors
    ///
    /// [`UploadError::RetriesExhausted`] once the engine gives up: a bounded give-up keeps a
    /// dead path from becoming a hot loop.
    async fn back_off_reset(
        &self,
        recovery: &mut Recovery,
        request: &CreateUploadRequest,
        session_id: &str,
        (offset, len): (u64, u64),
        reason: &str,
    ) -> Result<(), UploadError> {
        match recovery.engine.next_backoff(None) {
            RetryDecision::GiveUp => Err(UploadError::RetriesExhausted(format!(
                "transport reset unrecovered after {} retries: {reason}",
                recovery.engine.policy().max_retries
            ))),
            RetryDecision::Retry { after } => {
                tracing::debug!(offset, ?after, %reason, "mid-transfer reset — backing off and resuming");
                self.emit_retried(
                    request,
                    session_id,
                    offset,
                    len,
                    RetryReason::TransportReset {
                        wait_ms: millis(after),
                    },
                );
                self.sleeper.sleep(after).await;
                Ok(())
            }
        }
    }

    fn emit_retried(
        &self,
        request: &CreateUploadRequest,
        session_id: &str,
        offset: u64,
        len: u64,
        reason: RetryReason,
    ) {
        self.events.emit(|| TransferEvent::ChunkRetried {
            blob: request.hash.clone(),
            session_id: session_id.to_string(),
            offset,
            len,
            reason,
        });
    }

    /// Re-create a session and return its id, mapping a racing `duplicate_blob`
    /// into a transport error the caller resolves as a merge one level up. A create
    /// the server asks to wait is waited out under the transfer's recovery budget.
    async fn recreate(
        &self,
        request: &CreateUploadRequest,
        recovery: &mut Recovery,
    ) -> Result<String, UploadError> {
        match self.create_waiting(request, recovery).await? {
            // The caller drives a re-created session from zero, so that is where it starts.
            CreateOutcome::Created { response, .. } => {
                self.emit_created(request, &response.id, 0);
                Ok(response.id)
            }
            CreateOutcome::AlreadyStored { asset_ref } => Err(UploadError::Rejected {
                status: 409,
                code: Some(error_codes::UPLOAD_DUPLICATE_BLOB.to_string()),
                message: format!("blob already stored as {asset_ref}"),
            }),
        }
    }
}

/// `426`, or the stable `error.protocol.version_unsupported` code. The code is
/// authoritative; the bare `426` is a defensive fallback.
fn is_upgrade_required(status: u16, code: Option<&str>) -> bool {
    code == Some(error_codes::PROTOCOL_VERSION_UNSUPPORTED) || status == 426
}

/// Fail loudly once the bounded recovery budget is spent.
fn guard_budget(recoveries: u32, action: &str) -> Result<(), UploadError> {
    if recoveries > MAX_RECOVERIES {
        Err(UploadError::RetriesExhausted(format!(
            "{action}: exceeded {MAX_RECOVERIES} recovery attempts"
        )))
    } else {
        Ok(())
    }
}

/// Read an error response into `(code, message, headers)` — the headers for the window an
/// upgrade refusal carries. Tolerates a non-JSON (plain-text) body — such responses simply
/// carry no code.
async fn read_error(
    resp: reqwest::Response,
) -> Result<(Option<String>, String, reqwest::header::HeaderMap), UploadError> {
    let headers = resp.headers().clone();
    let text = resp.text().await?;
    match serde_json::from_str::<ApiErrorBody>(&text) {
        Ok(body) => {
            let message = if body.error.is_empty() {
                text
            } else {
                body.error
            };
            Ok((body.code, message, headers))
        }
        Err(_) => Ok((None, text, headers)),
    }
}

fn header_str(headers: &reqwest::header::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

fn header_u64(headers: &reqwest::header::HeaderMap, name: &str) -> Option<u64> {
    header_str(headers, name).and_then(|s| s.parse().ok())
}

/// The advice and the relative expiry a session create or `HEAD` carries, read leniently: a
/// server that sends neither, or sends them malformed, leaves the client on its own defaults.
fn advice_of(headers: &reqwest::header::HeaderMap) -> (ServerAdvice, SessionExpiry) {
    let advice = ServerAdvice::parse(header_str(headers, advice::ADVICE_HEADER).as_deref());
    let expiry = SessionExpiry::parse(
        header_str(headers, advice::EXPIRES_IN_HEADER).as_deref(),
        header_str(headers, advice::IDLE_EXPIRES_IN_HEADER).as_deref(),
    );
    (advice, expiry)
}

#[cfg(test)]
mod tests;
