//! The key-free **sync consumer** (slice `S-D2`; SSoT: [Download & Sync]).
//!
//! [`SyncConsumer`] drives `GET /v1/sync` through the **generated REST client** — the same
//! paged `(cursor, page_size)` feed the server serves (slice `S-C2`) — authorizing through the
//! `S-D7` session and retrying once on a `401`. The opaque, server-MAC'd cursor is round-tripped
//! verbatim: the client stores `next_cursor` and hands it back untouched.
//!
//! **There was a second transport here and there is not any more** (`S-D28`). The feed rode
//! `capsule.sync.v1.SyncService` over tonic, which meant the SDK described one wire contract in
//! a `.proto` and every other in an OpenAPI document. Nothing below the transport moved:
//! [`SyncState`] and its two rules were never about gRPC.
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
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_i18n::error_codes;
use tracing::instrument;

use crate::auth::{AuthError, Session};
use crate::net::{RetryClass, RetryDecision, RetryEngine, Sleeper, TokioSleeper};
use crate::rest;

/// The security-scheme key the Kynos document declares for the bearer JWT.
const BEARER_SCHEME: &str = "bearer";

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

    /// The cursor as the wire spells it, or `None` if it is not text.
    ///
    /// The REST feed carries the cursor as a string; it is stored as bytes because that is what
    /// a client persists and because the *client* is not entitled to an opinion about the
    /// encoding — it round-trips whatever it was handed. A non-UTF-8 cursor is one this client
    /// never received, so it is answered as absent and the next pull starts from the beginning
    /// rather than sending something the server will refuse.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        std::str::from_utf8(&self.0).ok()
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
    Updated,
    /// The asset was tombstoned (the manifest itself is the delete).
    Deleted,
}

/// A single blob's content address and role, carried on a feed entry (never blob bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobRef {
    /// Ciphertext content address (lowercase hex).
    pub ciphertext_hash: String,
    /// `original | metadata | derivative | provenance` (closed enum, server-set).
    pub role: String,
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
    /// The encrypted metadata blob's **content address**, as its UTF-8 bytes; empty when the
    /// entry carries none (a tombstone).
    ///
    /// An address, not the bytes: blob bytes never ride the feed. The field name predates the
    /// REST port and the type is kept so a caller's storage does not move.
    pub metadata_blob: Vec<u8>,
    /// Content addresses of the asset's blobs by role.
    pub blobs: BlobManifest,
    /// Whether the original blob is finalized on the server (`awaiting-original`
    /// derived state when `false`; staged uploads).
    pub original_held: bool,
    /// When the change happened, RFC 3339, on the server's clock.
    pub changed_at: String,
}

/// The derived per-asset **original availability** — the badge a client's timeline
/// reads (staged uploads, slice `S-B4`; download-sync doc, "The `awaiting-original`
/// state"). It is **always derived** from the feed's `original_held` fact, never
/// stored as a second source of truth: an asset whose original has not yet landed
/// on the server shows in the timeline immediately (LQIP, then T1 tiers as they
/// arrive) with an "original still on device" badge, and its full-resolution fetch
/// returns the transient `error.blob.pending_upload` rather than a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginalAvailability {
    /// The original is finalized on the server; full resolution is fetchable.
    Held,
    /// The original has not landed yet — the `awaiting-original` state. Show the
    /// badge, keep the asset listed, and re-fetch when the feed flips `original_held`.
    AwaitingOriginal,
}

impl FeedEntry {
    /// The derived [`OriginalAvailability`] badge state for this asset — a pure
    /// projection of [`original_held`](Self::original_held), so the "awaiting-original"
    /// badge is never a stored second source of truth (staged uploads).
    #[must_use]
    pub fn original_availability(&self) -> OriginalAvailability {
        if self.original_held {
            OriginalAvailability::Held
        } else {
            OriginalAvailability::AwaitingOriginal
        }
    }

    /// Whether this asset is in the derived **awaiting-original** state (its original
    /// is still on the source device). The UI shows the "original still on *device*"
    /// badge while this holds and never removes the asset's metadata or index entry.
    #[must_use]
    pub fn is_awaiting_original(&self) -> bool {
        !self.original_held
    }
}

/// A decoded page of the feed plus the cursor for the next page.
#[derive(Debug, Clone)]
pub struct SyncPage {
    /// The changes in this page, in strictly-increasing per-album `sync_seq` order.
    pub entries: Vec<FeedEntry>,
    /// The opaque cursor to pass to the next [`SyncConsumer::pull`].
    pub next_cursor: SyncCursor,
    /// Whether the server holds changes beyond this page.
    ///
    /// Answered from the owner's high-water mark rather than by fetching one more entry, so a
    /// caught-up client learns it is caught up without paying for an empty page.
    pub has_more: bool,
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
    /// A feed entry's `manifest_cbor` is not base64.
    ///
    /// The feed is JSON and a manifest is CBOR, so the signed bytes ride base64. A body that
    /// fails to decode is a server that cannot be talked to, not an asset to quarantine.
    #[error("a feed entry's manifest is not base64: {0}")]
    MalformedManifest(String),
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

// ─── REST consumer ───────────────────────────────────────────────────────────

/// How the feed's `Authorization` header is populated. Closed on purpose, mirroring the upload
/// client: either the `S-D7` session (pre-flight refresh, single-flight coalescing, and one
/// refresh-and-retry on a `401`) or a fixed bearer for tests and callers that already hold a
/// live token.
#[derive(Clone)]
enum SyncAuth {
    Session(Session),
    /// A fixed token, held by the client itself. The variant carries nothing because there is
    /// nothing left to do with it: the credential is already registered on the generated client,
    /// and what this arm decides is that a `401` is **final** — a static token cannot be
    /// refreshed, so retrying would only ask the same question twice.
    Static,
}

/// The sync consumer, over the **generated REST client** (`S-D28`).
///
/// It used to drive `capsule.sync.v1.SyncService` over tonic. The feed is `GET /v1/sync` now and
/// is a generated operation like every other, which removes the SDK's second transport and, with
/// it, the second place a wire contract could be described. What did **not** change is
/// everything below the transport: [`SyncState`], the anti-rewind high-water marks and the
/// forward-version rule are the same pure state machine, because none of them was ever about
/// gRPC.
#[derive(Clone)]
pub struct SyncConsumer {
    client: rest::Client,
    auth: SyncAuth,
}

impl SyncConsumer {
    /// A consumer for the API at `base_url`, authorizing through an `S-D7` [`Session`] — the
    /// sanctioned production path.
    ///
    /// # Errors
    ///
    /// [`SyncError::Transport`] when `base_url` is not a URL the client can hang paths off.
    pub fn with_session(base_url: &str, session: Session) -> Result<Self, SyncError> {
        Ok(Self {
            client: build_client(base_url, bearer_provider(session.clone()))?,
            auth: SyncAuth::Session(session),
        })
    }

    /// A consumer over a fixed bearer token (tests; callers holding a live token).
    ///
    /// # Errors
    ///
    /// [`SyncError::Transport`] when `base_url` is not a URL the client can hang paths off.
    pub fn with_static_token(base_url: &str, token: impl Into<String>) -> Result<Self, SyncError> {
        let token = token.into();
        Ok(Self {
            client: build_client(base_url, rest::Credential::Bearer(token.into()))?,
            auth: SyncAuth::Static,
        })
    }

    /// Pull one page after `cursor`.
    ///
    /// On a `401` under a session, refreshes once and retries — the reactive half of the session
    /// contract, which the pre-flight refresh cannot cover when a token is revoked mid-flight
    /// (`S-C48` made that a real answer rather than a theoretical one). A `503` or a transport
    /// failure rides the shared retry engine's `interactive` class: short timeout, at most two
    /// retries, then a visible failure — no configuration hot-loops.
    #[instrument(skip(self, cursor), fields(page_size, entries))]
    pub async fn pull(&self, cursor: &SyncCursor, page_size: u32) -> Result<SyncPage, SyncError> {
        let mut engine: RetryEngine = RetryClass::Interactive.engine();
        let response = loop {
            match self.call(cursor, page_size).await {
                Ok(page) => break page,
                Err(error) if is_unauthenticated(&error) => match &self.auth {
                    SyncAuth::Session(session) => {
                        tracing::info!("the feed answered 401; refreshing once and retrying");
                        session.refresh().await?;
                        break self.call(cursor, page_size).await.map_err(map_error)?;
                    }
                    SyncAuth::Static => return Err(map_error(error)),
                },
                Err(error) if is_transient(&error) => match engine.next_backoff(None) {
                    RetryDecision::GiveUp => {
                        tracing::info!("the feed is unavailable; retry budget spent");
                        return Err(map_error(error));
                    }
                    RetryDecision::Retry { after } => {
                        tracing::debug!(?after, "the feed is unavailable; backing off");
                        TokioSleeper.sleep(after).await;
                    }
                },
                Err(error) => return Err(map_error(error)),
            }
        };
        let page = decode_page(response)?;
        tracing::Span::current().record("entries", page.entries.len());
        Ok(page)
    }

    /// Pull the next page for `state` (using its stored cursor), validate and apply it, and
    /// return it. The one call that ties the opaque-cursor round-trip to the anti-rewind layer.
    #[instrument(skip(self, state), fields(page_size))]
    pub async fn pull_into(
        &self,
        state: &mut SyncState,
        page_size: u32,
    ) -> Result<SyncPage, SyncError> {
        let cursor = state.cursor().clone();
        let page = self.pull(&cursor, page_size).await?;
        state.apply_page(&page)?;
        Ok(page)
    }

    async fn call(
        &self,
        cursor: &SyncCursor,
        page_size: u32,
    ) -> Result<rest::types::SyncPageResponse, rest::Error<rest::SyncFeedError>> {
        let params = rest::SyncFeedParams {
            // The cursor is round-tripped verbatim. Empty means "from the beginning", which the
            // server spells as an absent parameter rather than an empty one.
            cursor: cursor
                .as_text()
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
            page_size: Some(i64::from(page_size)),
            // The suite and the sidecar schema are validated when present and a feed pull has
            // no use for either; the suite already rides the transport's default headers.
            ..rest::SyncFeedParams::default()
        };
        // The protocol date is a required parameter of every gated operation in the document,
        // so the generated signature asks for it; the value is the build's own, the same one the
        // transport's default header carries.
        Ok(self
            .client
            .sync_feed(capsule_core::crypto::primitives::PROTOCOL_VERSION, params)
            .await?
            .into_inner())
    }
}

/// A generated client for `base_url` carrying `credential` under the bearer scheme.
fn build_client(base_url: &str, credential: rest::Credential) -> Result<rest::Client, SyncError> {
    // The SDK's one HTTP client, so the feed pull carries the protocol handshake.
    let http =
        crate::net::http_client().map_err(|error| SyncError::Transport(error.to_string()))?;
    let client = rest::Client::with_client(http, base_url)
        .map_err(|error| SyncError::Transport(error.to_string()))?
        .with_credential(BEARER_SCHEME, credential);
    Ok(client)
}

/// A credential that pulls a fresh access token from `session` on every request.
fn bearer_provider(session: Session) -> rest::Credential {
    let provider: rest::TokenProvider = Arc::new(move || {
        let session = session.clone();
        Box::pin(async move {
            session
                .bearer()
                .await
                .map_err(|error| rest::AuthError::new(error.to_string()))
        })
    });
    rest::Credential::Provider(provider)
}

/// Whether the feed refused the credential.
fn is_unauthenticated(error: &rest::Error<rest::SyncFeedError>) -> bool {
    matches!(
        error,
        rest::Error::Api(response) if matches!(response.inner(), rest::SyncFeedError::Status401(_))
    )
}

/// Whether the failure is one worth backing off and retrying.
///
/// A `500` is included deliberately: the feed's only `500` is *"a collaborator could not
/// answer"*, which is exactly the transient the retry engine exists for. A `400` (bad cursor) or
/// a `403` (wrong credential kind) is not — retrying either changes nothing.
fn is_transient(error: &rest::Error<rest::SyncFeedError>) -> bool {
    match error {
        rest::Error::Transport(_) | rest::Error::Timeout(_) | rest::Error::Protocol(_) => true,
        rest::Error::Api(response) => {
            matches!(response.inner(), rest::SyncFeedError::Status500(_))
        }
        _ => false,
    }
}

// ─── Wire decoding ───────────────────────────────────────────────────────────

/// Map a generated client error to a typed [`SyncError`], keeping the stable `error.*` code the
/// problem body carries (`S-C36`/`S-C38` are why it is always there and always described).
fn map_error(error: rest::Error<rest::SyncFeedError>) -> SyncError {
    match error {
        rest::Error::Api(response) => {
            let (code, message) = match response.into_inner() {
                // The 400 includes the protocol gate's malformed-handshake answer (issue #404).
                // There is no 426 to map: the feed is a read, and a read is admitted at any
                // grammatical protocol date — the window rides the response headers instead.
                rest::SyncFeedError::Status400(problem)
                | rest::SyncFeedError::Status401(problem)
                | rest::SyncFeedError::Status403(problem)
                | rest::SyncFeedError::Status500(problem) => (
                    Some(problem.code.clone()),
                    problem.detail.clone().unwrap_or_default(),
                ),
                // The body-size backstop, which carries no body at all.
                rest::SyncFeedError::Status413 => (None, "the request was too large".into()),
            };
            SyncError::Rejected { code, message }
        }
        other => SyncError::Transport(other.to_string()),
    }
}

fn decode_page(page: rest::types::SyncPageResponse) -> Result<SyncPage, SyncError> {
    let entries = page
        .entries
        .into_iter()
        .map(decode_entry)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SyncPage {
        entries,
        next_cursor: SyncCursor::from_bytes(page.next_cursor.into_bytes()),
        has_more: page.has_more,
    })
}

fn decode_entry(entry: rest::types::SyncEntry) -> Result<FeedEntry, SyncError> {
    let kind = match entry.change {
        rest::types::WireChangeKind::Created => ChangeKind::Created,
        rest::types::WireChangeKind::Updated => ChangeKind::Updated,
        rest::types::WireChangeKind::Deleted => ChangeKind::Deleted,
    };
    // Base64, because the manifest is CBOR and the feed is JSON. Decoded here so a caller holds
    // the *exact* signed bytes `verify_asset` needs — re-encoding them anywhere would be the
    // thing `S-C30` exists to prevent.
    let manifest_cbor = match entry.manifest_cbor.as_deref() {
        Some(encoded) => BASE64
            .decode(encoded)
            .map_err(|error| SyncError::MalformedManifest(error.to_string()))?,
        None => Vec::new(),
    };
    Ok(FeedEntry {
        album_id: entry.album_id.into_bytes(),
        sync_seq: u64::try_from(entry.sync_seq).unwrap_or(u64::MAX),
        protocol_version: entry.protocol_version,
        kind,
        asset_id: entry.asset_id.into_bytes(),
        manifest_cbor,
        metadata_blob: entry.metadata_blob.unwrap_or_default().into_bytes(),
        blobs: decode_blobs(entry.blobs),
        original_held: entry.original_held,
        changed_at: entry.changed_at,
    })
}

/// Split the feed's flat blob list into the by-role shape a caller reads.
///
/// The wire carries one list because the server has no reason to group it; the SDK groups it
/// because every caller asks "is there an original" first.
fn decode_blobs(blobs: Vec<rest::types::SyncBlobRef>) -> BlobManifest {
    let mut manifest = BlobManifest::default();
    for blob in blobs {
        let reference = BlobRef {
            ciphertext_hash: blob.hash,
            role: role_str(&blob.role).to_owned(),
            size: u64::try_from(blob.size).unwrap_or(u64::MAX),
        };
        if matches!(blob.role, rest::types::WireBlobRole::Original) {
            manifest.original = Some(reference);
        } else {
            manifest.derivatives.push(reference);
        }
    }
    manifest
}

/// The wire token for a blob role.
fn role_str(role: &rest::types::WireBlobRole) -> &'static str {
    match role {
        rest::types::WireBlobRole::Original => "original",
        rest::types::WireBlobRole::Metadata => "metadata",
        rest::types::WireBlobRole::Derivative => "derivative",
        rest::types::WireBlobRole::Provenance => "provenance",
        rest::types::WireBlobRole::Backup => "backup",
    }
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
