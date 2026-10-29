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
//!   `checksum_mismatch` → re-send the chunk; `rate_limited`, `unavailable` and
//!   `storage_full` → wait out `Retry-After`, then resume from HEAD. The client
//!   switches on the stable `error.*` code (from `capsule_i18n::error_codes`),
//!   never on the bare status.
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
    ConnectionClass, RetryClass, RetryDecision, RetryEngine, Sleeper, TokioSleeper, negotiation,
};

pub mod queue;

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
/// Window size for throughput measurements
const THROUGHPUT_WINDOW_SECS: f64 = 30.0;
/// Minimum bytes before scaling chunk size
const MIN_BYTES_BEFORE_SCALING: u64 = 8 * 1024 * 1024; // 8MB
/// Minimum chunks before scaling chunk size
const MIN_CHUNKS_BEFORE_SCALING: u32 = 5;
/// Maximum number of recent chunks to keep in history
const MAX_RECENT_CHUNKS: usize = 1000;

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

/// Chunk size strategy for adaptive upload.
///
/// Normative for `capsule-sdk` (transfer upload doc, §Adaptive Chunk Sizing):
/// warm-up before adjusting, double above 5 MB/s, halve below 1 MB/s, clamp to
/// the file-size tier's range — and every tier range sits inside the protocol
/// bounds `[256 KiB, 16 MiB]` for non-final chunks.
#[derive(Debug, Clone)]
pub struct AdaptiveChunkSizeStrategy {
    /// Current chunk size
    pub current_size: u64,
    /// Minimum allowed chunk size based on file size tier
    pub min_size: u64,
    /// Maximum allowed chunk size based on file size tier
    pub max_size: u64,
    /// Number of successful chunks at current size
    pub successful_chunks: u32,
    /// Total bytes uploaded
    pub bytes_uploaded: u64,
    /// Total time spent uploading
    pub total_upload_time: Duration,
    /// Recent chunk records for windowed throughput (bytes, duration, timestamp)
    /// Using VecDeque for efficient sliding window
    recent_chunks: VecDeque<(u64, Duration, std::time::Instant)>,
    /// Pinned to the tier floor under `adverse` (the connection class's chunk-size floor):
    /// while set, the size stays at `min_size` and adaptive scaling is suppressed,
    /// so each request is small enough to usually complete between mid-transfer
    /// resets (networking doc, "Bounded transfer windows under `adverse`").
    pinned_floor: bool,
}

impl AdaptiveChunkSizeStrategy {
    /// Create a new chunk size strategy based on total file size
    pub fn for_file_size(total_size: u64) -> Self {
        let (min_size, max_size, current_size) = if total_size < 10 * 1024 * KB {
            // < 10MB: 256KB - 1MB chunks
            (CHUNK_SIZE_256KB, CHUNK_SIZE_1MB, CHUNK_SIZE_256KB)
        } else if total_size < 100 * 1024 * KB {
            // < 100MB: 1MB - 4MB chunks
            (CHUNK_SIZE_1MB, CHUNK_SIZE_4MB, CHUNK_SIZE_1MB)
        } else {
            // >= 100MB: 4MB - 16MB chunks
            (CHUNK_SIZE_4MB, CHUNK_SIZE_16MB, CHUNK_SIZE_4MB)
        };

        Self {
            current_size,
            min_size,
            max_size,
            successful_chunks: 0,
            bytes_uploaded: 0,
            total_upload_time: Duration::ZERO,
            recent_chunks: VecDeque::with_capacity(32),
            pinned_floor: false,
        }
    }

    /// Couple the chunk size to a detected [`ConnectionClass`]. On
    /// [`ConnectionClass::Adverse`] the strategy pins to the tier floor
    /// (`min_size`) and suppresses adaptive growth for the rest of the transfer;
    /// any other class releases the pin and lets the normative adaptive algorithm
    /// run. The floor is still 4 KiB-aligned and within the protocol bounds by
    /// construction (it is the tier minimum).
    #[must_use]
    pub fn with_connection_class(mut self, class: crate::net::ConnectionClass) -> Self {
        self.apply_connection_class(class);
        self
    }

    /// Apply the connection-class chunk-floor coupling in place (see
    /// [`with_connection_class`](Self::with_connection_class)).
    pub fn apply_connection_class(&mut self, class: crate::net::ConnectionClass) {
        if class == crate::net::ConnectionClass::Adverse {
            self.pinned_floor = true;
            self.current_size = self.min_size;
        } else {
            self.pinned_floor = false;
        }
    }

    /// Seed the starting chunk size from the server's `X-Capsule-Suggested-Chunk-Size`.
    ///
    /// The suggestion is a *starting point only* (transfer upload doc, §Adaptive
    /// Chunk Sizing): 4 KiB-aligned down and clamped into this tier's range, so
    /// the invariant "current size is aligned and within the tier" (hence within
    /// the protocol bounds) still holds by construction.
    pub fn seeded_from_suggested(mut self, suggested: u64) -> Self {
        let aligned_down = (suggested / ALIGNMENT) * ALIGNMENT;
        self.current_size = aligned_down.clamp(self.min_size, self.max_size);
        // Clamp can only land on aligned tier bounds or an aligned-down value;
        // never below the tier minimum.
        debug_assert!(self.current_size.is_multiple_of(ALIGNMENT));
        self
    }

    /// Calculate current throughput in bytes per second (windowed)
    pub fn throughput_bytes_per_second(&self) -> f64 {
        let now = std::time::Instant::now();
        let window_start = now
            .checked_sub(Duration::from_secs_f64(THROUGHPUT_WINDOW_SECS))
            .unwrap_or(now);

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

    /// Record a successful chunk upload and potentially adjust chunk size
    pub fn record_chunk(&mut self, chunk_size: u64, upload_duration: Duration) {
        self.successful_chunks += 1;
        self.bytes_uploaded += chunk_size;
        self.total_upload_time += upload_duration;

        // Record for windowed throughput
        let now = std::time::Instant::now();
        self.recent_chunks
            .push_back((chunk_size, upload_duration, now));

        // Enforce max size (circular buffer behavior)
        if self.recent_chunks.len() > MAX_RECENT_CHUNKS {
            self.recent_chunks.pop_front();
        }

        // Cleanup old chunks (older than window)
        // Since it's sorted by time, we can just pop from front until we see a new enough one
        if let Some(window_start) = now.checked_sub(Duration::from_secs_f64(THROUGHPUT_WINDOW_SECS))
        {
            while let Some((_, _, timestamp)) = self.recent_chunks.front() {
                if *timestamp < window_start {
                    self.recent_chunks.pop_front();
                } else {
                    break;
                }
            }
        }

        // Under the adverse floor pin, scaling is suppressed: the size stays at the
        // tier minimum so each request stays small across a hostile path.
        if self.pinned_floor {
            self.current_size = self.min_size;
            return;
        }

        // Adaptive scaling based on throughput
        if self.bytes_uploaded >= MIN_BYTES_BEFORE_SCALING
            || self.successful_chunks >= MIN_CHUNKS_BEFORE_SCALING
        {
            let throughput = self.throughput_bytes_per_second();

            // If throughput is high (> 5 MB/s) and we're not at max, double chunk size
            if throughput > 5.0 * 1024.0 * 1024.0 && self.current_size < self.max_size {
                self.current_size = (self.current_size * 2).min(self.max_size);
                self.successful_chunks = 0; // Reset counter after scaling
            }
            // If throughput is low (< 1 MB/s) and we're not at min, halve chunk size
            else if throughput < 1.0 * 1024.0 * 1024.0 && self.current_size > self.min_size {
                self.current_size = (self.current_size / 2).max(self.min_size);
                self.successful_chunks = 0;
            }
        }
    }

    /// Get the next chunk size to use.
    ///
    /// Alignment is a hard guarantee by construction: every candidate size is a
    /// doubling/halving of a 4 KiB-aligned tier bound, so this can never return
    /// an unaligned size — the debug_assert is a tripwire for future edits, not
    /// a runtime dependency. The size is likewise always within the protocol
    /// bounds because every tier range is.
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
/// protocol date sent as `X-Capsule-Protocol`, and the authorization seam.
#[derive(Clone)]
pub struct UploadTransport {
    base_url: String,
    protocol_version: String,
    auth: UploadAuth,
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
        }
    }

    /// Send an authorized request. `build` constructs the un-authenticated
    /// request (method, URL, headers, body) from the transport's HTTP client;
    /// the seam adds the bearer. `build` may be invoked twice on the session
    /// path (the `401` refresh-and-replay), so it must be reconstructible.
    async fn send<F>(&self, build: F) -> Result<reqwest::Response, UploadError>
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
    /// The server's starting chunk-size suggestion.
    pub suggested_chunk_size: u64,
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
}

/// A row from `GET /upload/sessions` — the subset the resuming client needs.
/// Unknown server fields are tolerated (serde ignores them).
#[derive(Debug, Clone, Deserialize)]
pub struct SessionSummary {
    /// Upload session ID.
    pub id: String,
    /// Bytes the server has received so far.
    pub received_bytes: u64,
    /// Declared total size.
    pub total_size: u64,
    /// Session state (serialized enum variant name).
    pub status: String,
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
    /// Every declared byte was transferred; the server has begun finalization.
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
    /// Accepted; the new authoritative offset.
    Accepted(u64),
    /// `offset_mismatch` — re-align to the authoritative offset (from the 409's
    /// `X-Capsule-Offset` when present, else a follow-up HEAD).
    Realign(Option<u64>),
    /// `session_not_found` — re-create the session and resume from zero.
    Recreate,
    /// `checksum_mismatch` — nothing was persisted; re-send the same chunk.
    Resend,
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
    /// [`ConnectionClass::Adverse`] the transfer pins to the tier
    /// chunk floor. Defaults to [`ConnectionClass::Unmetered`].
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

    /// Set the detected connection class (the chunk-size floor follows it). On an
    /// [`ConnectionClass::Adverse`] link the adaptive strategy pins to the tier
    /// chunk floor for every transfer this client drives.
    #[must_use]
    pub fn with_connection_class(mut self, class: ConnectionClass) -> Self {
        self.connection = class;
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
            let body: CreateSessionResponse = resp.json().await?;
            tracing::debug!(session_id = %body.id, resume_offset, "upload session created");
            return Ok(Ok(CreateOutcome::Created {
                response: body,
                resume_offset,
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
        Ok(Ok(Some(HeadInfo {
            offset,
            total_size,
            status: statusv,
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
    /// not interruptible) surfaces as [`UploadError::Rejected`].
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
        let resp = self
            .transport
            .send(|http| {
                http.patch(self.transport.session_url(session_id))
                    .header("X-Capsule-Protocol", &self.transport.protocol_version)
                    .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                    .header("X-Capsule-Offset", offset.to_string())
                    .header("X-Capsule-Checksum", &checksum)
                    // The body is cloned per invocation: the session path may
                    // rebuild the request for its one 401 refresh-and-replay.
                    .body(chunk.to_vec())
            })
            .await?;

        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            let new_offset = header_u64(resp.headers(), "X-Capsule-Offset")
                .unwrap_or(offset + chunk.len() as u64);
            return Ok(ChunkAck::Accepted(new_offset));
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
    /// chunk window at a time — never whole — and checksums each chunk. The transfer completes exactly when the received-byte count
    /// reaches the declared size, at which point the server finalizes.
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
        let (session, suggested, start_offset) =
            match self.create_waiting(request, recovery).await? {
                CreateOutcome::Created {
                    response,
                    resume_offset,
                } => (response.id, response.suggested_chunk_size, resume_offset),
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
            start_offset,
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
            let mut strategy = AdaptiveChunkSizeStrategy::for_file_size(request.size)
                .with_connection_class(self.connection);
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
                info.offset,
                &mut recovery,
            )
            .await
        } else {
            tracing::debug!("session gone on resume — re-creating");
            self.upload_with(request, data, &mut recovery).await
        }
    }

    /// The chunk loop with the recovery matrix. `offset` is the starting
    /// authoritative offset (0 for a fresh session, the HEAD offset on resume).
    async fn drive<S: UploadSource + ?Sized>(
        &self,
        mut session_id: String,
        request: &CreateUploadRequest,
        data: &S,
        strategy: &mut AdaptiveChunkSizeStrategy,
        mut offset: u64,
        recovery: &mut Recovery,
    ) -> Result<UploadOutcome, UploadError> {
        let size = request.size;
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
                Err(UploadError::Transport(reason)) => match recovery.engine.next_backoff(None) {
                    RetryDecision::GiveUp => {
                        return Err(UploadError::RetriesExhausted(format!(
                            "transport reset unrecovered after {} retries: {reason}",
                            recovery.engine.policy().max_retries
                        )));
                    }
                    RetryDecision::Retry { after } => {
                        tracing::debug!(offset, ?after, %reason, "mid-transfer reset — backing off and resuming");
                        self.emit_retried(
                            request,
                            &session_id,
                            offset,
                            want,
                            RetryReason::TransportReset {
                                wait_ms: millis(after),
                            },
                        );
                        self.sleeper.sleep(after).await;
                        continue;
                    }
                },
                Err(other) => return Err(other),
            };
            match ack {
                ChunkAck::Accepted(new_offset) => {
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
                        } => {
                            session_id = response.id;
                            *strategy = AdaptiveChunkSizeStrategy::for_file_size(size)
                                .seeded_from_suggested(response.suggested_chunk_size);
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
                        info.offset
                    } else {
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

        tracing::info!(%session_id, size, "upload complete — server finalizing");
        self.events.emit(|| TransferEvent::Finalize {
            blob: request.hash.clone(),
            session_id: session_id.clone(),
            size,
        });
        Ok(UploadOutcome::Completed { session_id })
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

#[cfg(test)]
mod tests;
