//! [`UploadPolicy`] — the deployment-tunable half of the upload contract.
//!
//! # What belongs here and what does not
//!
//! The [Upload Protocol](../../../capsule-docs/src/content/docs/design/transfer/upload.md)
//! draws one line down the middle of this surface, and this module is that line made into a
//! type:
//!
//! - **Protocol surface** — the 4 KiB alignment, the `[4 KiB, 16 MiB]` chunk range, the
//!   offset semantics — is *not* here. It is fixed for a protocol version, so it lives as
//!   constants in [`super::chunk`] where no deployment can move it.
//! - **Server-tunable** — the accepted protocol window and the client-build cutoff it
//!   advertises beside it, the per-file ceiling, the closed `content_type` enum, the
//!   timestamp-drift bound, the chunk-body read pace, and the suggested chunk-size tiers —
//!   is here, because a self-hosted
//!   deployment legitimately sets them differently.
//!
//! The protocol window is read by more than the upload surface: [`crate::negotiation`]
//! advertises it on every response and gates every covered operation against it, from this one
//! value, so the window a client is told and the window it is held to cannot be two numbers.
//!
//! Every value carries the Salvo deployment's default, so the rebuild starts from the
//! behaviour clients already see rather than from a fresh set of numbers.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use jiff::Timestamp;

/// The closed `content_type` enum for the current protocol version (invariant 5).
///
/// Frozen for a given `protocol_version` and server-tunable across versions. Metadata,
/// provenance, derivative-manifest and backup blobs are opaque CBOR or ciphertext and declare
/// `application/octet-stream`.
///
/// It carries two disjoint things: the **originals** a client imports, and the **derivatives**
/// its ladder generates (every
/// [`DerivativeFormat::STILL_DELIVERY_ORDER`](capsule_core::derivative_format::DerivativeFormat::STILL_DELIVERY_ORDER)
/// value mapped through `mime()`, plus `video/mp4` for the H.264 baseline video preview, which
/// the stills-only derivative set does not model). The still set is not restated here: the
/// [Thumbnails and Previews](../../../capsule-docs/src/content/docs/design/thumbnails.md) tier
/// table is the closed set, `capsule_core::derivative_format` is that table as a type, and this
/// server is a receiver that must accept every value it admits — the tests below fail the moment
/// this list and that set disagree. The `original` sentinel is deliberately **absent**: a tier
/// that references the original carries no bytes, opens no upload session, and never presents a
/// `content_type`. `image/jxl` is here for
/// the second reason only — nothing imports a JXL original today — and its absence is #470:
/// every still larger than the 256 px thumbnail cap failed its T1 upload with
/// `400 error.upload.unsupported_content_type`, because the ladder encodes that tier as JXL and
/// the server had never been told the format existed.
pub const DEFAULT_CONTENT_TYPES: &[&str] = &[
    "image/jpeg",
    "image/png",
    "image/heic",
    "image/heif",
    "image/webp",
    "image/avif",
    "image/jxl",
    "image/gif",
    "image/tiff",
    "video/mp4",
    "video/quicktime",
    "video/webm",
    "application/octet-stream",
];

/// Lowest protocol date this server accepts (`X-Capsule-Protocol-Min`).
pub const DEFAULT_PROTOCOL_MIN: &str = "2026-01-01";

/// Highest protocol date this server accepts (`X-Capsule-Protocol-Max`).
pub const DEFAULT_PROTOCOL_MAX: &str = "2026-12-31";

/// The semver client build below which this server stops answering
/// (`X-Capsule-Min-Client-Build`).
///
/// `0.0.0` is "no cutoff announced": every build satisfies it. The header is advisory until a
/// path is hard-deprecated (threat-model/validation.md), and no path is, so nothing refuses on
/// it — but it is sent on every response so a client that reads it today reads a real value.
pub const DEFAULT_MIN_CLIENT_BUILD: &str = "0.0.0";

/// Gross-drift sanity bound for the envelope timestamp, in days (invariant 8).
pub const DEFAULT_DRIFT_DAYS: i64 = 30;

/// The per-blob ceiling a deployment defaults to, in bytes.
///
/// 4 GiB, the Salvo deployment's `max_file_size`. It bounds one blob; the total in-flight
/// bytes are bounded by the discard window instead, which is the pressure-eviction half
/// of the upload lifecycle and is not this type's business.
pub const DEFAULT_MAX_FILE_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// How long a chunk body may deliver no bytes before the server cuts it: thirty seconds.
///
/// The body-read half of transfer/upload.md's per-connection bound (Hostile Clients). Without
/// it a client that opens a chunk and then sends nothing holds a connection, a handler task and
/// up to the 32 MiB transport backstop of buffer for as long as it likes, and nothing above the
/// read loop ever ends it (#717).
///
/// A chunk is never cut for its total duration (#1041): a slow but live link finishes any chunk
/// it keeps sending. The bound sits below the idle timeout of every common reverse proxy (nginx
/// and AWS ALB at sixty seconds, Cloudflare at a hundred), so a stalled chunk is answered by
/// this server's `408 error.upload.chunk_timeout` rather than cut by the proxy with a bare `504`.
/// The offset never moves, because nothing of a chunk that did not arrive whole is written.
pub const DEFAULT_CHUNK_STALL_TIMEOUT: Duration = Duration::from_secs(30);

/// The slowest sustained rate a chunk body may arrive at, in bytes per second: 1 KiB/s.
///
/// The second half of the cut. The stall bound alone lets a client send one byte every
/// twenty-nine seconds and hold a connection for days; the floor ends that without touching an
/// honest slow link, which a 4 KiB chunk at 1 KiB/s already is. Averaged over the whole read
/// after a grace of one stall bound (see [`ChunkPace`]), so a burst after a pause is not cut
/// for the pause alone.
pub const DEFAULT_CHUNK_MIN_RATE: u64 = 1024;

/// When a chunk body is cut: no bytes for [`stall`](Self::stall), or a sustained rate below
/// [`min_bytes_per_second`](Self::min_bytes_per_second). Never a total-duration deadline.
///
/// The rate is owed only after a grace of one stall bound: by `t` seconds into the read, a body
/// must have delivered at least `min_bytes_per_second × (t − stall)` bytes. A zero rate turns
/// the floor off and leaves the stall bound alone; a zero stall refuses every chunk before its
/// first byte is read, which configuration refuses and only the conformance walk sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkPace {
    /// The longest a body may go without delivering a byte.
    pub stall: Duration,
    /// The slowest sustained rate owed after the grace, in bytes per second.
    pub min_bytes_per_second: u64,
}

impl ChunkPace {
    /// A pace of `stall` and `min_bytes_per_second`.
    #[must_use]
    pub const fn new(stall: Duration, min_bytes_per_second: u64) -> Self {
        Self {
            stall,
            min_bytes_per_second,
        }
    }
}

impl Default for ChunkPace {
    fn default() -> Self {
        Self::new(DEFAULT_CHUNK_STALL_TIMEOUT, DEFAULT_CHUNK_MIN_RATE)
    }
}

/// When a chunk body is cut.
///
/// # Why a port and not a value
///
/// For the reason [`FederationSetting`](crate::federation::FederationSetting) is one. Production
/// reads the pace once at boot and never changes it: `boot::assemble` hands in a plain
/// [`ChunkPace`], which implements this trait. It is a trait only because the conformance walk
/// (`tests/conformance.rs`) proves every response the document declares through **one** server,
/// and the `408` a stalled chunk earns has to come from the same server that answers every
/// chunk's `204`. An in-process request's body is whole before the handler sees it, so the
/// test double is a switch to a zero stall bound, which cuts before the first byte.
pub trait ChunkReadPace: fmt::Debug + Send + Sync {
    /// The pace a chunk body is read under, read when its read starts.
    fn chunk_pace(&self) -> ChunkPace;
}

/// The pace as boot reads it: fixed for the life of the process.
impl ChunkReadPace for ChunkPace {
    fn chunk_pace(&self) -> ChunkPace {
        *self
    }
}

/// The tunable half of the upload contract.
///
/// Cheap to clone (one `Vec` of short strings), because a Kynos provider hands the context out
/// per request.
#[derive(Debug, Clone)]
pub struct UploadPolicy {
    /// Lowest accepted protocol date (`YYYY-MM-DD`).
    protocol_min: String,
    /// Highest accepted protocol date (`YYYY-MM-DD`).
    protocol_max: String,
    /// The advisory semver deprecation cutoff advertised on every response.
    min_client_build: String,
    /// The closed `content_type` allow-list (invariant 5).
    content_types: Vec<String>,
    /// Gross-drift sanity bound in days for the envelope timestamp (invariant 8).
    drift_days: i64,
    /// The per-blob ceiling in bytes (invariant 4's upper half).
    max_file_bytes: u64,
    /// When a chunk body is cut.
    chunk_pace: Arc<dyn ChunkReadPace>,
}

impl Default for UploadPolicy {
    fn default() -> Self {
        Self {
            protocol_min: DEFAULT_PROTOCOL_MIN.to_owned(),
            protocol_max: DEFAULT_PROTOCOL_MAX.to_owned(),
            min_client_build: DEFAULT_MIN_CLIENT_BUILD.to_owned(),
            content_types: DEFAULT_CONTENT_TYPES
                .iter()
                .map(|kind| (*kind).to_owned())
                .collect(),
            drift_days: DEFAULT_DRIFT_DAYS,
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            chunk_pace: Arc::new(ChunkPace::default()),
        }
    }
}

impl UploadPolicy {
    /// The lowest protocol date this server accepts.
    pub fn protocol_min(&self) -> &str {
        &self.protocol_min
    }

    /// The highest protocol date this server accepts.
    pub fn protocol_max(&self) -> &str {
        &self.protocol_max
    }

    /// The semver client build below which this server stops answering.
    pub fn min_client_build(&self) -> &str {
        &self.min_client_build
    }

    /// The closed `content_type` allow-list, as the shared predicate wants it.
    pub fn content_types(&self) -> Vec<&str> {
        self.content_types.iter().map(String::as_str).collect()
    }

    /// The gross-drift bound in days.
    pub fn drift_days(&self) -> i64 {
        self.drift_days
    }

    /// The per-blob ceiling in bytes.
    pub fn max_file_bytes(&self) -> u64 {
        self.max_file_bytes
    }

    /// When a chunk body is cut, read now.
    pub fn chunk_pace(&self) -> ChunkPace {
        self.chunk_pace.chunk_pace()
    }

    /// Replace the chunk-body pace — a [`ChunkPace`] at boot, a switch in the test suite.
    #[must_use]
    pub fn with_chunk_pace(mut self, pace: Arc<dyn ChunkReadPace>) -> Self {
        self.chunk_pace = pace;
        self
    }

    /// Narrow the accepted protocol window.
    #[must_use]
    pub fn with_protocol_window(mut self, min: impl Into<String>, max: impl Into<String>) -> Self {
        self.protocol_min = min.into();
        self.protocol_max = max.into();
        self
    }

    /// Announce a client-build cutoff.
    #[must_use]
    pub fn with_min_client_build(mut self, build: impl Into<String>) -> Self {
        self.min_client_build = build.into();
        self
    }

    /// Replace the closed `content_type` enum.
    #[must_use]
    pub fn with_content_types<I, S>(mut self, kinds: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.content_types = kinds.into_iter().map(Into::into).collect();
        self
    }

    /// Replace the per-blob ceiling.
    #[must_use]
    pub fn with_max_file_bytes(mut self, bytes: u64) -> Self {
        self.max_file_bytes = bytes;
        self
    }

    /// Replace the gross-drift bound.
    #[must_use]
    pub fn with_drift_days(mut self, days: i64) -> Self {
        self.drift_days = days;
        self
    }
}

/// The server's clock as the RFC3339 string the shared validation predicates read.
///
/// `capsule_core::validation` takes timestamps as text because the values it compares arrive
/// as text on the wire; this is the one place the server's own `jiff` reading is rendered into
/// that form, so no call site formats a clock by hand.
pub fn as_rfc3339(at: Timestamp) -> String {
    at.to_string()
}

#[cfg(test)]
mod tests;
