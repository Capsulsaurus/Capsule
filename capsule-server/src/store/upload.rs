//! [`UploadSessionStore`] — the volatile transfer state of one upload, and the two views over
//! it.
//!
//! # What changed from the Salvo `UploadSessionManager`
//!
//! That type was already upload-specific, so the grab-bag problem is not the one here. Two
//! others are:
//!
//! - **The record, the uploader index and the progress index were three writes.** `create`
//!   issued an `HSET`, an `SADD` + `EXPIRE` on `upload:uploader_sessions:{user}`, and a `ZADD`
//!   on `upload:progress_index`; `delete` had to `HGET upload_user_id` *before* deleting the
//!   record so it could still find the index to clean — a read-then-write with the record's
//!   own deletion in between. As in [`super::auth`], no operation here names an index: both
//!   are adapter-internal derivatives of the record set, so they cannot drift from it and
//!   cannot outlive it.
//! - **Accepting a chunk was three calls.** `increment_received_bytes`, `touch_progress` and
//!   `record_chunk` were issued separately from the append path, so a crash between them left
//!   a counter, a progress score and a replay entry disagreeing about the same chunk. Here
//!   accepting a chunk is [`UploadSessionStore::record_progress`], one operation taking one
//!   [`AcceptedChunk`].
//!
//! The 24-hour lifetime cap is a property of the store, matching
//! design/filesystem/server.md; the ≥1-hour survival floor and pressure-discard policy are
//! the caller's, expressed through [`UploadSessionStore::least_recently_progressed`].

use jiff::{SignedDuration, Timestamp};

use super::{AlbumId, AssetId, OwnerId, StoreFuture, UploadId, UserId};

/// A blob's role within its asset bundle, declared at session creation.
///
/// Closed: the visibility gate and staged uploads reason over it (upload-protocol design doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BlobRole {
    /// The encrypted original.
    Original,
    /// An encrypted derivative — thumbnail, preview.
    Derivative,
    /// The encrypted CBOR metadata blob.
    Metadata,
    /// The signed manifest envelope object.
    Provenance,
    /// A backup copy.
    Backup,
}

impl BlobRole {
    /// The role's stable wire token.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Derivative => "derivative",
            Self::Metadata => "metadata",
            Self::Provenance => "provenance",
            Self::Backup => "backup",
        }
    }
}

/// Where an upload session is in its state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UploadSessionStatus {
    /// Active, no chunk accepted yet.
    Pending,
    /// Active, at least one chunk accepted.
    Uploading,
    /// Finalization has been claimed and is running.
    WaitingForProcessing,
    /// Finalized successfully.
    Completed,
    /// Finalization failed.
    FailedProcessing,
}

impl UploadSessionStatus {
    /// Whether the session can still accept chunks or be claimed for finalization.
    pub fn is_active(self) -> bool {
        !self.is_terminal()
    }

    /// Whether the session has reached an end state.
    ///
    /// A terminal session leaves the progress view: its bytes are already gone, so it is
    /// exempt from pressure eviction while its receipt is retained to the lifetime cap.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::FailedProcessing)
    }

    /// Whether pressure eviction may still pick the session — `Pending` or `Uploading`.
    ///
    /// Narrower than [`Self::is_active`], and the one predicate every adapter's
    /// [`UploadSessionStore::least_recently_progressed`] applies: a `WaitingForProcessing`
    /// session is in flight for every other purpose, but the finalize claim that moved it there
    /// is the promise that it will not be evicted out from under the finalizer (upload-protocol
    /// design doc, the finalization claim).
    pub fn is_evictable(self) -> bool {
        matches!(self, Self::Pending | Self::Uploading)
    }

    /// The token the `X-Capsule-Upload-Status` response header carries.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Uploading => "uploading",
            Self::WaitingForProcessing => "waiting_for_processing",
            Self::Completed => "completed",
            Self::FailedProcessing => "failed_processing",
        }
    }
}

/// Volatile transfer state for one upload session.
///
/// Carries everything finalization needs — sizes, hash, crypto and protocol pins, blob role,
/// the manifest envelope and the parties — so a session is finalizable from its own record
/// with no further client input (upload-protocol design doc, §Endpoints).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadSessionRecord {
    /// The session's own identifier, and the name of its `incoming/{id}.bin` file.
    pub upload_id: UploadId,
    /// The pending asset row this session reserved at creation.
    pub asset_id: AssetId,
    /// The billing and namespace entity the blob is filed under.
    pub owner_id: OwnerId,
    /// The uploading party — the one quota is accounted to, and the one that can resume.
    pub upload_user_id: UserId,
    /// The album the upload is filed into, when it named one.
    pub album_id: Option<AlbumId>,
    /// The declared content type.
    pub content_type: Option<String>,
    /// The lowercase-hex SHA-256 finalization verifies against.
    pub expected_hash: String,
    /// The crypto suite the blob was sealed under.
    pub crypto_suite_id: u16,
    /// The pinned protocol date (`YYYY-MM-DD`) from session creation.
    pub protocol_version: String,
    /// This blob's role in its bundle.
    pub blob_role: BlobRole,
    /// The album-upgrade intent, when the write is part of an upgrade ceremony.
    pub intent_id: Option<String>,
    /// The server-visible manifest envelope, held verbatim. Structural validation is the
    /// envelope gate's job, not the store's.
    pub manifest_envelope: String,
    /// Bytes durably appended so far. The on-disk file length is the truth this caches; see
    /// [`UploadSessionStore::reconcile_received_bytes`].
    pub received_bytes: u64,
    /// The declared total.
    pub total_size: u64,
    /// Where the session is in its state machine.
    pub status: UploadSessionStatus,
    /// When the session was created.
    pub created_at: Timestamp,
    /// When the last chunk was accepted, or creation time if none. Anchors the survival floor
    /// and orders [`UploadSessionStore::least_recently_progressed`].
    pub last_progress_at: Timestamp,
}

/// One chunk the server durably appended.
///
/// The replay half of the `(upload_id, offset, chunk_hash)` idempotency tuple: a client that
/// re-sends a chunk it already got an acknowledgement for is answered from this rather than
/// appending twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedChunk {
    /// The offset the chunk was written at.
    pub offset: u64,
    /// The chunk's lowercase-hex SHA-256, as the `X-Capsule-Checksum` header carried it.
    pub chunk_hash: String,
    /// The session's received-byte count after this chunk.
    pub next_offset: u64,
    /// When the append was acknowledged. Becomes the session's `last_progress_at`.
    pub accepted_at: Timestamp,
}

/// The outcome of racing for the right to finalize a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinalizeClaim {
    /// This caller won and the session is now `WaitingForProcessing`. The record is the
    /// session as it was claimed, so the winner finalizes without a second read.
    Won(Box<UploadSessionRecord>),
    /// Another caller already claimed it, or the session is no longer active.
    AlreadyClaimed,
    /// No such session.
    NotFound,
}

/// The lifetime cap an upload session is stored under.
///
/// 24 hours, from design/filesystem/server.md: Valkey's native TTL *is* the cap. The record
/// carries no `expires_at` of its own — a caller-written expiry field alongside a store-owned
/// TTL is two clocks for one fact, and they drift. A route that must publish an expiry renders
/// `record.created_at + store.ttl()`.
pub const LIFETIME_CAP: SignedDuration = SignedDuration::from_hours(24);

/// Upload transfer state.
pub trait UploadSessionStore: std::fmt::Debug + Send + Sync {
    /// How long a session lives from creation. A property of the store; see [`LIFETIME_CAP`].
    ///
    /// The at-least-one-hour survival floor and the pressure-discard semantics layered under
    /// this cap are the caller's policy, expressed through [`Self::least_recently_progressed`].
    fn ttl(&self) -> SignedDuration;

    /// Open `record`'s session, making it visible to [`Self::read`], to
    /// [`Self::sessions_for_uploader`] and to [`Self::least_recently_progressed`] in one step.
    fn open(&self, record: UploadSessionRecord) -> StoreFuture<'_, ()>;

    /// The live session `upload`, or `None`.
    fn read<'a>(&'a self, upload: &'a UploadId) -> StoreFuture<'a, Option<UploadSessionRecord>>;

    /// Every live session `uploader` can resume, oldest first, ties broken by upload id.
    ///
    /// Scoped to the *uploading* party rather than the owner, because resumption is what this
    /// listing is for.
    fn sessions_for_uploader<'a>(
        &'a self,
        uploader: &'a UserId,
    ) -> StoreFuture<'a, Vec<UploadSessionRecord>>;

    /// Accept `chunk`: record it for replay, advance `received_bytes` to its `next_offset`,
    /// and move the session's progress clock — one operation, because they describe one event.
    ///
    /// Returns the updated record, or `None` if there was no live session to advance.
    fn record_progress<'a>(
        &'a self,
        upload: &'a UploadId,
        chunk: AcceptedChunk,
    ) -> StoreFuture<'a, Option<UploadSessionRecord>>;

    /// The chunk previously accepted at `offset`, if any.
    fn chunk_at<'a>(
        &'a self,
        upload: &'a UploadId,
        offset: u64,
    ) -> StoreFuture<'a, Option<AcceptedChunk>>;

    /// Set `received_bytes` to an absolute value, for the startup scrub only.
    ///
    /// The file on disk is the truth and this counter is its cache, so a crash between the
    /// durable append and the counter update is reconciled *up* to the on-disk length. This is
    /// the one write that does not touch the progress clock: a scrub is not progress.
    fn reconcile_received_bytes<'a>(
        &'a self,
        upload: &'a UploadId,
        on_disk: u64,
    ) -> StoreFuture<'a, Option<UploadSessionRecord>>;

    /// Move a live session to `status`, returning the updated record.
    ///
    /// A status that is not [`UploadSessionStatus::is_evictable`] — a terminal one, or
    /// `WaitingForProcessing` — also drops the session from [`Self::least_recently_progressed`],
    /// so pressure eviction cannot pick a session whose bytes are already committed or being
    /// committed.
    fn set_status<'a>(
        &'a self,
        upload: &'a UploadId,
        status: UploadSessionStatus,
    ) -> StoreFuture<'a, Option<UploadSessionRecord>>;

    /// Claim the exclusive right to finalize `upload`.
    ///
    /// Compare-and-set into `WaitingForProcessing`: only a `Pending` or `Uploading` session
    /// transitions, so two racing finalizers cannot both win, and the winner leaves the
    /// progress view rather than being evicted out from under itself.
    fn claim_finalize<'a>(&'a self, upload: &'a UploadId) -> StoreFuture<'a, FinalizeClaim>;

    /// Discard a session: its record, its accepted-chunk replay entries, and its place in
    /// both views, together. Returns the record that was removed, or `None`.
    fn discard<'a>(&'a self, upload: &'a UploadId) -> StoreFuture<'a, Option<UploadSessionRecord>>;

    /// The active session `owner` currently has open for `expected_hash`, if any (`S-C40`).
    ///
    /// The question the blob serve path asks when nothing references an address: *are these
    /// exact bytes on their way?* An active upload session declaring that hash **is** the
    /// promise, which is why this is a lookup and not a new record — a session already carries a
    /// bounded lifetime ([`LIFETIME_CAP`]) and is already reconciled by the discard worker, so
    /// an abandoned upload's promise expires on its own. A separate "declared original" row in
    /// the asset index would have needed a second lifetime, a second reconciliation, and would
    /// have made every in-flight upload look like a dangling reference to the integrity scrub.
    ///
    /// Scoped to an owner deliberately. Unscoped, this answers "is somebody, anywhere, uploading
    /// these bytes right now" to any authenticated caller who can name the hash — a small but
    /// real cross-account signal for no gain, since the case the transient answer exists for is
    /// a *second device of the same account* fetching an original the first one is still
    /// sending.
    ///
    /// Terminal sessions never match: a completed session's bytes are committed and a failed
    /// one's are not coming. Adapters need a secondary index on `(owner_id, expected_hash)`;
    /// scanning is acceptable only in the in-memory double.
    fn pending_for_address<'a>(
        &'a self,
        owner: &'a OwnerId,
        expected_hash: &'a str,
    ) -> StoreFuture<'a, Option<UploadId>>;

    /// How many sessions are still in flight against `album` (`S-C24`).
    ///
    /// The drain count versioning.md step 3 asks the server to expose: *"the upgrade cannot
    /// proceed while any session for this album is in `Uploading` or `WaitingForProcessing`"*.
    /// Counted rather than listed, because the proposer needs to know **whether** to wait and has
    /// no business seeing other members' upload identifiers to find out.
    ///
    /// Includes `Pending` — a session that has been opened and has sent no bytes is exactly as
    /// much in flight as one that has, and the ceremony's whole purpose is that nothing is
    /// mid-write at the cutover.
    fn in_flight_for_album<'a>(&'a self, album: &'a AlbumId) -> StoreFuture<'a, u64>;

    /// Up to `limit` evictable sessions — `Pending` or `Uploading`, see
    /// [`UploadSessionStatus::is_evictable`] — that have not progressed since
    /// `not_progressed_since`, least recently progressed first.
    ///
    /// The eviction *policy* — the ≥1-hour survival floor, when pressure is high enough to
    /// discard at all — belongs to the caller; the store only orders candidates. Neither a
    /// terminal session nor a claimed one is ever returned: the first has nothing left to
    /// evict, the second is being finalized and [`Self::claim_finalize`] promised it would not
    /// be evicted out from under that.
    fn least_recently_progressed(
        &self,
        not_progressed_since: Timestamp,
        limit: usize,
    ) -> StoreFuture<'_, Vec<UploadId>>;
}
