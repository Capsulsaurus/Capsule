//! The key-free **sync consumer** (slice `S-D2`; SSoT: [Download & Sync]).
//!
//! [`SyncConsumer`] drives the gRPC `capsule.sync.v1.SyncService` — the same
//! unary, paged `Sync(cursor, page_size)` the server crate serves over the salvo
//! bridge (slice `S-C2`) — injecting the `x-capsule-protocol` handshake and the
//! bearer token as call metadata, and retrying once through the `S-D7` session on
//! an `Unauthenticated` status. The opaque, server-MAC'd cursor is round-tripped
//! verbatim: the client stores `next_cursor` and hands it back untouched.
//!
//! [`SyncState`] is the **client-side anti-rewind + forward-version layer**, split
//! out as a pure, network-free state machine so every rejection rule is a
//! deterministic unit test (download-sync doc, "Sync feed validation"):
//!
//! - **Forward-version rejection.** A feed entry whose `protocol_version` is above
//!   the client's max known is rejected *without partial application* — the whole
//!   page is validated before any high-water mark advances (the tightened Postel's
//!   Law).
//! - **Rewind rejection.** The client holds a per-album `sync_seq` high-water mark;
//!   a page whose `sync_seq` regresses against it is surfaced, not applied. This is
//!   the independent anti-rewind layer that the server's cursor MAC cannot provide:
//!   a malicious server can always hand back one of its own *older*, validly-MAC'd
//!   cursors, and only the client-held high-water mark defeats that
//!   ([Download & Sync — cursor authenticity]).
//!
//! Blob bytes never ride this surface; on-demand ranged blob fetch is [`crate::fetch`].
//!
//! [Download & Sync]: https://docs/design/import/download-sync/
//! [Download & Sync — cursor authenticity]: https://docs/design/import/download-sync/#discovering-what-changed

use std::collections::HashMap;

use capsule_i18n::error_codes;
use secrecy::ExposeSecret;
use tonic::transport::Channel;
use tonic::{Code, Request, Response, Status};
use tracing::instrument;

use crate::auth::{AuthError, Session};
use crate::net::{RetryClass, RetryDecision, RetryEngine, Sleeper, TokioSleeper};
use crate::proto::capsule::sync::v1::sync_service_client::SyncServiceClient;
use crate::proto::capsule::sync::v1::{
    BlobManifest as ProtoBlobManifest, BlobRef as ProtoBlobRef, ChangeKind as ProtoChangeKind,
    SyncEntry as ProtoEntry, SyncRequest, SyncResponse,
};

/// Universal request/response metadata keys (lowercased REST headers; the
/// api-surfaces mapping mirrored by the server crate's `feed.rs`).
const MD_PROTOCOL: &str = "x-capsule-protocol";
const MD_ERROR_CODE: &str = "x-capsule-error-code";
const MD_AUTHORIZATION: &str = "authorization";

// ─── Cursor ──────────────────────────────────────────────────────────────────

/// The opaque, server-MAC'd sync cursor. The client never interprets it — it is
/// round-tripped verbatim: empty on first sync, then whatever the server last
/// returned as `next_cursor`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncCursor(Vec<u8>);

impl SyncCursor {
    /// The first-sync sentinel (empty cursor).
    #[must_use]
    pub fn start() -> Self {
        Self(Vec::new())
    }

    /// Wrap raw cursor bytes handed back by the server.
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// The opaque bytes, for persistence across restarts.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Whether this is the first-sync sentinel.
    #[must_use]
    pub fn is_start(&self) -> bool {
        self.0.is_empty()
    }
}

// ─── Decoded feed entry ──────────────────────────────────────────────────────

/// What changed, mirroring `capsule.sync.v1.ChangeKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// A new asset became visible in the album.
    Created,
    /// An existing asset's metadata (or `original_held`) advanced.
    MetadataUpdated,
    /// The asset was tombstoned (the manifest itself is the delete).
    Deleted,
}

/// A single blob's content address and role, carried on a feed entry (never blob
/// bytes). The ergonomic mirror of the proto `BlobRef`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobRef {
    /// Ciphertext content address (lowercase hex).
    pub ciphertext_hash: String,
    /// `original | metadata | derivative | provenance` (closed enum, server-set).
    pub role: String,
    /// MIME/format string, for derivatives.
    pub format: String,
    /// Ciphertext size in bytes.
    pub size: u64,
}

/// The asset's blobs by role.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlobManifest {
    /// The original ciphertext blob, when this entry carries one.
    pub original: Option<BlobRef>,
    /// Derivative / metadata / provenance blobs.
    pub derivatives: Vec<BlobRef>,
}

/// One decoded sync feed entry. Ids arrive as opaque bytes (UTF-8 nanoids / raw
/// UUIDs); the manifest and metadata blob pass through as opaque bytes.
#[derive(Debug, Clone)]
pub struct FeedEntry {
    /// The album this entry belongs to.
    pub album_id: Vec<u8>,
    /// Per-album, strictly-increasing sequence — the anti-rewind high-water mark.
    pub sync_seq: u64,
    /// The album protocol pin (`YYYY-MM-DD`) this entry conforms to.
    pub protocol_version: String,
    /// What changed.
    pub kind: ChangeKind,
    /// The asset id.
    pub asset_id: Vec<u8>,
    /// The signed `AssetManifest` as opaque canonical CBOR (verified by core).
    pub manifest_cbor: Vec<u8>,
    /// The encrypted metadata blob; empty for deletes.
    pub metadata_blob: Vec<u8>,
    /// Content addresses of the asset's blobs by role.
    pub blobs: BlobManifest,
    /// Whether the original blob is finalized on the server (`awaiting-original`
    /// derived state when `false`; staged uploads).
    pub original_held: bool,
}

/// A decoded page of the feed plus the cursor for the next page.
#[derive(Debug, Clone)]
pub struct SyncPage {
    /// The changes in this page, in strictly-increasing per-album `sync_seq` order.
    pub entries: Vec<FeedEntry>,
    /// The opaque cursor to pass to the next [`SyncConsumer::pull`].
    pub next_cursor: SyncCursor,
}

// ─── Errors ──────────────────────────────────────────────────────────────────

/// Everything the sync consumer / reconciliation can fail with. Callers switch on
/// the typed variant (never a bare gRPC status); [`SyncError::error_code`] yields
/// the stable `error.*` catalog code where one applies.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    /// A feed entry's `protocol_version` is above the client's max known — the
    /// forward-version rejection. The whole page is refused with no partial apply.
    #[error(
        "feed entry protocol {entry_version} exceeds the client's max known {max_known} (album {album})"
    )]
    ForwardVersion {
        /// The offending album (lossy UTF-8 of the id bytes).
        album: String,
        /// The entry's protocol version.
        entry_version: String,
        /// The client's max known protocol version.
        max_known: String,
    },
    /// A feed entry's `sync_seq` regresses against the locally-seen high-water mark
    /// for its album — a malicious/buggy server attempting to rewind the client.
    #[error(
        "feed entry sync_seq {entry_seq} regresses against high-water {high_water} (album {album})"
    )]
    Rewind {
        /// The offending album (lossy UTF-8 of the id bytes).
        album: String,
        /// The entry's (regressing) `sync_seq`.
        entry_seq: u64,
        /// The high-water mark the client already applied for the album.
        high_water: u64,
    },
    /// A feed entry carried a `protocol_version` that is not a `YYYY-MM-DD` date.
    #[error("malformed feed entry protocol version {0:?}")]
    MalformedProtocol(String),
    /// A feed entry carried an unknown `ChangeKind` discriminant.
    #[error("unknown change kind discriminant {0}")]
    UnknownChangeKind(i32),
    /// The server rejected the call (cursor invalid, forward-version, auth, …).
    /// Carries the stable `error.*` code when the server supplied one.
    #[error("sync rejected (code {code:?}): {message}")]
    Rejected {
        /// The stable `error.*` code from `x-capsule-error-code`, when present.
        code: Option<String>,
        /// The English detail message from the gRPC status.
        message: String,
    },
    /// Transport-level failure (connection, TLS, timeout, malformed metadata).
    #[error("sync transport error: {0}")]
    Transport(String),
    /// The session's auth layer failed (not authenticated, refresh rejected).
    #[error("authentication error: {0}")]
    Auth(#[from] AuthError),
}

impl SyncError {
    /// The stable `error.*` catalog code a client localizes, when one applies.
    #[must_use]
    pub fn error_code(&self) -> Option<&str> {
        match self {
            Self::ForwardVersion { .. } => Some(error_codes::PROTOCOL_VERSION_UNSUPPORTED),
            Self::Rejected { code, .. } => code.as_deref(),
            _ => None,
        }
    }
}

// ─── Reconciliation state (pure) ─────────────────────────────────────────────

/// The client-side reconciliation state: the per-album `sync_seq` high-water
/// marks, the max-known protocol version, and the opaque cursor. Pure and
/// network-free — [`SyncState::apply_page`] is the whole anti-rewind /
/// forward-version contract in one testable function.
#[derive(Debug, Clone)]
pub struct SyncState {
    max_known_protocol: String,
    high_water: HashMap<Vec<u8>, u64>,
    cursor: SyncCursor,
}

impl SyncState {
    /// A fresh state pinned to the client's max-known protocol version.
    #[must_use]
    pub fn new(max_known_protocol: impl Into<String>) -> Self {
        Self {
            max_known_protocol: max_known_protocol.into(),
            high_water: HashMap::new(),
            cursor: SyncCursor::start(),
        }
    }

    /// Rehydrate a state from a durably-persisted cursor and per-album high-water
    /// marks (the client's store loaded at startup), pinned to the client's
    /// current max-known protocol. The inverse of [`SyncState::cursor`] +
    /// [`SyncState::high_water_marks`]: a client persists those two after every
    /// [`apply_page`](SyncState::apply_page) and restores here on the next run so
    /// the anti-rewind high-water mark survives process restarts (the CLI's
    /// `capsule sync`, slice `S-D5`). `max_known_protocol` is intentionally *not*
    /// persisted — it is the running build's own ceiling, so a client that
    /// upgraded raises it here rather than restoring a stale one.
    #[must_use]
    pub fn restore(
        max_known_protocol: impl Into<String>,
        cursor: SyncCursor,
        high_water: impl IntoIterator<Item = (Vec<u8>, u64)>,
    ) -> Self {
        Self {
            max_known_protocol: max_known_protocol.into(),
            high_water: high_water.into_iter().collect(),
            cursor,
        }
    }

    /// Every applied per-album high-water mark, for persistence across restarts.
    /// Pair with [`SyncState::cursor`] and reload through [`SyncState::restore`].
    pub fn high_water_marks(&self) -> impl Iterator<Item = (&[u8], u64)> {
        self.high_water
            .iter()
            .map(|(album, seq)| (album.as_slice(), *seq))
    }

    /// The cursor to hand to the next pull (round-tripped verbatim).
    #[must_use]
    pub fn cursor(&self) -> &SyncCursor {
        &self.cursor
    }

    /// The applied per-album high-water mark, if the album has been seen.
    #[must_use]
    pub fn high_water(&self, album_id: &[u8]) -> Option<u64> {
        self.high_water.get(album_id).copied()
    }

    /// Validate and apply a page: check every entry (forward-version, then
    /// per-album `sync_seq` monotonicity against the high-water mark) **before**
    /// mutating any state, then advance the high-water marks and the cursor.
    ///
    /// On any rejection nothing is applied — no high-water mark moves and the
    /// cursor does not advance — so a poisoned page can never partially land.
    #[instrument(skip(self, page), fields(entries = page.entries.len()))]
    pub fn apply_page(&mut self, page: &SyncPage) -> Result<(), SyncError> {
        self.validate(page)?;
        for entry in &page.entries {
            let slot = self.high_water.entry(entry.album_id.clone()).or_insert(0);
            *slot = (*slot).max(entry.sync_seq);
        }
        self.cursor = page.next_cursor.clone();
        tracing::debug!(
            entries = page.entries.len(),
            "sync page applied; high-water marks advanced"
        );
        Ok(())
    }

    /// The full validation pass, sharing nothing with the apply pass so a failure
    /// leaves `self` untouched.
    fn validate(&self, page: &SyncPage) -> Result<(), SyncError> {
        // Per-album working floor: the max of the persisted high-water mark and the
        // highest seq seen earlier in this page (enforces strictly-increasing
        // within the page too).
        let mut seen: HashMap<&[u8], u64> = HashMap::new();
        for entry in &page.entries {
            if !is_protocol_date(&entry.protocol_version) {
                return Err(SyncError::MalformedProtocol(entry.protocol_version.clone()));
            }
            // Forward-version: refuse any entry beyond the client's max known.
            if entry.protocol_version.as_str() > self.max_known_protocol.as_str() {
                return Err(SyncError::ForwardVersion {
                    album: lossy(&entry.album_id),
                    entry_version: entry.protocol_version.clone(),
                    max_known: self.max_known_protocol.clone(),
                });
            }
            // Rewind: the sequence must strictly exceed the album's floor.
            let floor = seen
                .get(entry.album_id.as_slice())
                .copied()
                .or_else(|| self.high_water.get(&entry.album_id).copied());
            if let Some(hw) = floor
                && entry.sync_seq <= hw
            {
                return Err(SyncError::Rewind {
                    album: lossy(&entry.album_id),
                    entry_seq: entry.sync_seq,
                    high_water: hw,
                });
            }
            seen.insert(entry.album_id.as_slice(), entry.sync_seq);
        }
        Ok(())
    }
}

// ─── gRPC consumer ───────────────────────────────────────────────────────────

/// How the sync feed's `authorization` metadata is populated. Closed on purpose,
/// mirroring the upload client: either the `S-D7` session (pre-flight refresh +
/// one refresh-and-retry on `Unauthenticated`) or a fixed bearer for tests /
/// callers that already hold a live token.
#[derive(Clone)]
enum SyncAuth {
    Session(Session),
    Static(String),
}

/// The gRPC sync consumer. Wraps the generated `SyncServiceClient` with the
/// handshake + auth metadata and the reconciliation-friendly [`SyncPage`] shape.
#[derive(Clone)]
pub struct SyncConsumer {
    client: SyncServiceClient<Channel>,
    auth: SyncAuth,
    protocol_version: String,
}

impl SyncConsumer {
    /// Build a consumer over an established channel, authorizing through an
    /// `S-D7` [`Session`] — the sanctioned production path.
    #[must_use]
    pub fn with_session(
        channel: Channel,
        session: Session,
        protocol_version: impl Into<String>,
    ) -> Self {
        Self {
            client: SyncServiceClient::new(channel),
            auth: SyncAuth::Session(session),
            protocol_version: protocol_version.into(),
        }
    }

    /// Build a consumer over a fixed bearer token (tests; callers holding a live
    /// token).
    #[must_use]
    pub fn with_static_token(
        channel: Channel,
        token: impl Into<String>,
        protocol_version: impl Into<String>,
    ) -> Self {
        Self {
            client: SyncServiceClient::new(channel),
            auth: SyncAuth::Static(token.into()),
            protocol_version: protocol_version.into(),
        }
    }

    /// Dial a lazy channel to the gRPC endpoint (`http[s]://host:port`). TLS, when
    /// the endpoint is `https`, is rustls (`tls-ring`) — never native-tls.
    pub async fn connect(endpoint: impl Into<String>) -> Result<Channel, SyncError> {
        let endpoint = tonic::transport::Endpoint::from_shared(endpoint.into())
            .map_err(|e| SyncError::Transport(e.to_string()))?;
        endpoint
            .connect()
            .await
            .map_err(|e| SyncError::Transport(e.to_string()))
    }

    /// Pull one page after `cursor`. Injects the handshake + bearer metadata and,
    /// on an `Unauthenticated` status under a session, refreshes once and retries.
    #[instrument(skip(self, cursor), fields(page_size, entries))]
    pub async fn pull(
        &mut self,
        cursor: &SyncCursor,
        page_size: u32,
    ) -> Result<SyncPage, SyncError> {
        let bearer = self.bearer().await?;
        // The shared retry engine, `interactive` class (short timeout, ≤ 2 retries,
        // then a visible failure). Instantiated here as upload/fetch do, so all
        // three paths share one engine. It governs transient `Unavailable`
        // responses — the mid-transfer black-holing an adverse path produces — with
        // exponential backoff + full jitter and a bounded give-up.
        let mut engine: RetryEngine = RetryClass::Interactive.engine();
        let response = loop {
            match self.call(cursor, page_size, &bearer).await {
                Ok(response) => break response,
                Err(status) if status.code() == Code::Unauthenticated => match &self.auth {
                    SyncAuth::Session(session) => {
                        tracing::info!(
                            "sync returned Unauthenticated; refreshing once and retrying"
                        );
                        session.refresh().await?;
                        let bearer = session.bearer().await?.expose_secret().to_string();
                        break self
                            .call(cursor, page_size, &bearer)
                            .await
                            .map_err(map_status)?;
                    }
                    SyncAuth::Static(_) => return Err(map_status(status)),
                },
                // A transient server signal (`Unavailable`, incl. the `503` the
                // salvo bridge maps): back off through the engine and retry, honoring
                // a `retry-after` metadata hint as a floor. A bounded give-up surfaces
                // the visible failure state; no configuration hot-loops.
                Err(status) if status.code() == Code::Unavailable => {
                    match engine.next_backoff(retry_after(&status)) {
                        RetryDecision::GiveUp => {
                            tracing::info!(
                                "sync unavailable; retry budget spent — surfacing failure"
                            );
                            return Err(map_status(status));
                        }
                        RetryDecision::Retry { after } => {
                            tracing::debug!(?after, "sync unavailable; backing off and retrying");
                            TokioSleeper.sleep(after).await;
                        }
                    }
                }
                Err(status) => return Err(map_status(status)),
            }
        };
        let page = decode_response(response)?;
        tracing::Span::current().record("entries", page.entries.len());
        Ok(page)
    }

    /// Pull the next page for `state` (using its stored cursor), validate + apply
    /// it (advancing the high-water marks and cursor), and return it. The single
    /// call that ties the opaque-cursor round-trip to the anti-rewind layer.
    #[instrument(skip(self, state), fields(page_size))]
    pub async fn pull_into(
        &mut self,
        state: &mut SyncState,
        page_size: u32,
    ) -> Result<SyncPage, SyncError> {
        let cursor = state.cursor().clone();
        let page = self.pull(&cursor, page_size).await?;
        state.apply_page(&page)?;
        Ok(page)
    }

    async fn bearer(&self) -> Result<String, SyncError> {
        match &self.auth {
            SyncAuth::Session(session) => Ok(session.bearer().await?.expose_secret().to_string()),
            SyncAuth::Static(token) => Ok(token.clone()),
        }
    }

    async fn call(
        &mut self,
        cursor: &SyncCursor,
        page_size: u32,
        bearer: &str,
    ) -> Result<Response<SyncResponse>, Status> {
        let mut request = Request::new(SyncRequest {
            cursor: cursor.0.clone(),
            page_size,
        });
        let metadata = request.metadata_mut();
        metadata.insert(
            MD_AUTHORIZATION,
            format!("Bearer {bearer}")
                .parse()
                .map_err(|_| Status::internal("un-encodable bearer metadata"))?,
        );
        metadata.insert(
            MD_PROTOCOL,
            self.protocol_version
                .parse()
                .map_err(|_| Status::internal("un-encodable protocol metadata"))?,
        );
        self.client.sync(request).await
    }
}

// ─── Wire decoding ───────────────────────────────────────────────────────────

fn decode_response(response: Response<SyncResponse>) -> Result<SyncPage, SyncError> {
    let inner = response.into_inner();
    let entries = inner
        .entries
        .into_iter()
        .map(decode_entry)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SyncPage {
        entries,
        next_cursor: SyncCursor::from_bytes(inner.next_cursor),
    })
}

fn decode_entry(entry: ProtoEntry) -> Result<FeedEntry, SyncError> {
    let kind = match ProtoChangeKind::try_from(entry.kind) {
        Ok(ProtoChangeKind::Created) => ChangeKind::Created,
        Ok(ProtoChangeKind::MetadataUpdated) => ChangeKind::MetadataUpdated,
        Ok(ProtoChangeKind::Deleted) => ChangeKind::Deleted,
        // UNSPECIFIED and any out-of-range value are structural errors on receipt.
        Ok(ProtoChangeKind::Unspecified) | Err(_) => {
            return Err(SyncError::UnknownChangeKind(entry.kind));
        }
    };
    Ok(FeedEntry {
        album_id: entry.album_id,
        sync_seq: entry.sync_seq,
        protocol_version: entry.protocol_version,
        kind,
        asset_id: entry.asset_id,
        manifest_cbor: entry.manifest_cbor,
        metadata_blob: entry.metadata_blob,
        blobs: entry.blobs.map(decode_blobs).unwrap_or_default(),
        original_held: entry.original_held,
    })
}

fn decode_blobs(blobs: ProtoBlobManifest) -> BlobManifest {
    BlobManifest {
        original: blobs.original.map(decode_ref),
        derivatives: blobs.derivatives.into_iter().map(decode_ref).collect(),
    }
}

fn decode_ref(blob: ProtoBlobRef) -> BlobRef {
    BlobRef {
        ciphertext_hash: String::from_utf8_lossy(&blob.ciphertext_hash).into_owned(),
        role: blob.role,
        format: blob.format,
        size: blob.size,
    }
}

/// Map a gRPC `Status` to a typed [`SyncError::Rejected`], pulling the stable
/// `error.*` code out of the `x-capsule-error-code` metadata the server advertises.
fn map_status(status: Status) -> SyncError {
    let code = status
        .metadata()
        .get(MD_ERROR_CODE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    SyncError::Rejected {
        code,
        message: status.message().to_string(),
    }
}

/// Parse a `retry-after` metadata hint (integer seconds) into a backoff floor the
/// shared retry engine honors. Absent or unparsable ⇒ `None` (engine uses its own
/// jittered backoff).
fn retry_after(status: &Status) -> Option<std::time::Duration> {
    status
        .metadata()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(std::time::Duration::from_secs)
}

/// Lossy UTF-8 of an id byte string, for error display only.
fn lossy(id: &[u8]) -> String {
    String::from_utf8_lossy(id).into_owned()
}

/// True if `v` is a well-formed `YYYY-MM-DD` date (the only grammar
/// `protocol_version` accepts). Lexicographic comparison of valid values equals
/// chronological order — mirrors `capsule_core::validation::protocol`'s server-side
/// gate on the client side.
fn is_protocol_date(v: &str) -> bool {
    let b = v.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[8..].iter().all(u8::is_ascii_digit)
}

#[cfg(test)]
mod tests;
