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
//!   committing a range is [`UploadSessionStore::record_range`], one operation taking one
//!   [`CommittedRange`] and merging it into the session's [`CommittedSet`].
//!
//! # The committed set
//!
//! A session's bytes sit on a 4 KiB grid and arrive as ranges in any order (transfer upload
//! design doc, Chunk Rules and Strictness), so what the server has made durable is a set of
//! byte ranges, not an offset. The set is the commit point: a range is recorded only after its
//! bytes are written and fsynced, and staged bytes outside it are garbage whatever the staging
//! file holds. The set is merged inside the store in one step, so two ranges recorded at once
//! never lose each other. The ranges still in flight are not here: they are claims held by the
//! process writing them ([`crate::upload::chunk`]), and they do not survive it.
//!
//! The 24-hour lifetime cap is a property of the store, matching
//! design/filesystem/server.md; the ≥1-hour survival floor and pressure-discard policy are
//! the caller's, expressed through [`UploadSessionStore::least_recently_progressed`].

use jiff::{SignedDuration, Timestamp};

use super::{AlbumId, AssetId, OwnerId, StoreFuture, UploadId, UserId};

/// A blob's role within its asset bundle, declared at session creation.
///
/// Closed: the visibility gate and staged uploads reason over it (transfer upload design doc).
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
    /// The signed `DerivativeManifest` of one derivative tier, stored verbatim — canonical
    /// CBOR, server-visible like the asset's own manifest, and the material a receiver opens
    /// that tier's derivative with (#556).
    ///
    /// Plural, like [`Self::Derivative`]: an asset carries one per derivative tier, and the
    /// `original`-sentinel tier carries one with no derivative blob beside it.
    DerivativeManifest,
}

impl BlobRole {
    /// Every role, in declaration order — the one list every stored-token reader parses
    /// against, so a role added here is a role every adapter reads back.
    pub const ALL: [Self; 6] = [
        Self::Original,
        Self::Derivative,
        Self::Metadata,
        Self::Provenance,
        Self::Backup,
        Self::DerivativeManifest,
    ];

    /// The role a stored token names, or `None` for a token no role spells.
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|role| role.as_str() == token)
    }

    /// The role's stable wire token.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Derivative => "derivative",
            Self::Metadata => "metadata",
            Self::Provenance => "provenance",
            Self::Backup => "backup",
            Self::DerivativeManifest => "derivative_manifest",
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
    /// is the promise that it will not be evicted out from under the finalizer (transfer
    /// upload design doc, the finalization claim).
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
/// with no further client input (transfer upload design doc, §Endpoints).
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
    /// The byte ranges durably written and recorded so far — the commit point. Merged by
    /// [`UploadSessionStore::record_range`]; never inferred from the staging file.
    pub committed: CommittedSet,
    /// How many ranges of this session may be in flight at once, fixed at creation and
    /// advertised as the advice's `ranges` member. At most
    /// [`crate::upload::chunk::MAX_IN_FLIGHT_RANGES`].
    pub in_flight_cap: u8,
    /// The declared total.
    pub total_size: u64,
    /// Where the session is in its state machine.
    pub status: UploadSessionStatus,
    /// The `error.*` code finalization failed with, set only with `FailedProcessing` by
    /// [`UploadSessionStore::fail_processing`].
    ///
    /// The one channel through which a client learns why its upload was refused: no request
    /// waits on finalization, so `HEAD` and the session list carry this code instead.
    pub failure_code: Option<String>,
    /// When the session was created.
    pub created_at: Timestamp,
    /// When the last chunk was accepted, or creation time if none. Anchors the survival floor
    /// and orders [`UploadSessionStore::least_recently_progressed`].
    pub last_progress_at: Timestamp,
}

impl UploadSessionRecord {
    /// Whether this session is a guest drop's rather than an album upload's.
    ///
    /// A drop carries no manifest (invariant 30), so its envelope is empty, while every album
    /// session holds the serialized envelope its create passed the gate with. The two finish
    /// differently — a drop is filed in its owner's inbox, an album upload is finalized into
    /// the asset index — so anything that drives a session it did not open, boot recovery
    /// first, asks this before choosing which.
    pub fn is_guest_drop(&self) -> bool {
        self.manifest_envelope.is_empty()
    }

    /// Whether this session holds every declared byte and nobody has claimed its finish.
    ///
    /// The state a process leaves when it dies between recording the completing chunk and
    /// claiming the finalization or deposit that chunk triggers. Nothing is driving such a
    /// session, so whoever meets it — a replayed chunk, a duplicate create, a `HEAD`, boot
    /// recovery — finishes it through the claim, which makes two of them racing harmless.
    pub fn is_full_and_unclaimed(&self) -> bool {
        self.status.is_evictable() && self.is_full()
    }

    /// Whether the committed set is exactly `[0, total_size)`.
    pub fn is_full(&self) -> bool {
        self.committed.is_full(self.total_size)
    }
}

/// One range the server durably wrote, to be merged into its session's [`CommittedSet`].
///
/// Half-open: `start..end`. Carries no hash, deliberately: a range wholly inside the committed
/// set is answered as a duplicate without comparing bytes, and the finalize re-hash — not a
/// per-write record — is what admits bytes to a content address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommittedRange {
    /// The range's first byte.
    pub start: u64,
    /// One past its last byte.
    pub end: u64,
    /// When its write was recorded. Becomes the session's `last_progress_at`.
    pub accepted_at: Timestamp,
}

/// The coalesced, ordered set of half-open byte ranges a session has committed.
///
/// Always normal: ranges are non-empty, sorted, and neither overlap nor touch, so two sets
/// holding the same bytes are equal and the fragment count is the number of gaps a client still
/// has to fill, plus one. Every operation is pure; the stores merge through
/// [`Self::with_range`]'s rule, the Valkey adapter in Lua.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommittedSet {
    ranges: Vec<(u64, u64)>,
}

impl CommittedSet {
    /// The set holding exactly `[0, prefix)`, or nothing when `prefix` is zero.
    ///
    /// The shape a session recorded by an append-only server reads back as: its counter was a
    /// contiguous prefix.
    pub fn prefix_of(prefix: u64) -> Self {
        Self::default().with_range(0, prefix)
    }

    /// The ranges, ascending, each `(start, end)` half-open.
    pub fn ranges(&self) -> &[(u64, u64)] {
        &self.ranges
    }

    /// The set with `start..end` merged in. An empty range changes nothing.
    #[must_use]
    pub fn with_range(&self, start: u64, end: u64) -> Self {
        if start >= end {
            return self.clone();
        }
        let (mut start, mut end) = (start, end);
        let mut merged = Vec::with_capacity(self.ranges.len() + 1);
        let mut placed = false;
        for &(a, b) in &self.ranges {
            if b < start {
                merged.push((a, b));
            } else if a > end {
                if !placed {
                    merged.push((start, end));
                    placed = true;
                }
                merged.push((a, b));
            } else {
                start = start.min(a);
                end = end.max(b);
            }
        }
        if !placed {
            merged.push((start, end));
        }
        Self { ranges: merged }
    }

    /// How many fragments the set would hold with `start..end` merged in.
    pub fn fragments_with(&self, start: u64, end: u64) -> usize {
        self.with_range(start, end).ranges.len()
    }

    /// How many fragments the set holds.
    pub fn fragments(&self) -> usize {
        self.ranges.len()
    }

    /// The contiguous committed prefix: the end of a range starting at zero, else zero.
    ///
    /// What `X-Capsule-Offset` reports, so a client that sends in order resumes from it.
    pub fn prefix(&self) -> u64 {
        match self.ranges.first() {
            Some(&(0, end)) => end,
            _ => 0,
        }
    }

    /// How many bytes the set covers.
    pub fn covered(&self) -> u64 {
        self.ranges.iter().map(|&(a, b)| b - a).sum()
    }

    /// Whether the set is exactly `[0, size)`.
    pub fn is_full(&self, size: u64) -> bool {
        self.ranges.as_slice() == [(0, size)]
    }

    /// Whether `start..end` lies wholly inside one committed range.
    pub fn contains(&self, start: u64, end: u64) -> bool {
        self.ranges.iter().any(|&(a, b)| a <= start && end <= b)
    }

    /// Whether `start..end` shares at least one byte with the set.
    pub fn overlaps(&self, start: u64, end: u64) -> bool {
        self.ranges.iter().any(|&(a, b)| a < end && start < b)
    }

    /// The `X-Capsule-Committed` spelling: comma-separated **inclusive** byte ranges, such as
    /// `0-1048575,2097152-4194303`, or the empty string for an empty set.
    pub fn to_header(&self) -> String {
        self.ranges
            .iter()
            .map(|&(a, b)| format!("{a}-{}", b - 1))
            .collect::<Vec<_>>()
            .join(",")
    }

    /// The stored spelling: comma-separated **half-open** `start-end` pairs, the empty string
    /// for an empty set. The Valkey adapter's merge script reads and writes the same form.
    pub fn to_stored(&self) -> String {
        self.ranges
            .iter()
            .map(|&(a, b)| format!("{a}-{b}"))
            .collect::<Vec<_>>()
            .join(",")
    }

    /// The set a [`Self::to_stored`] string spells, or `None` when it is not one — an empty or
    /// reversed range, or ranges out of order, touching or overlapping.
    pub fn from_stored(text: &str) -> Option<Self> {
        let mut ranges: Vec<(u64, u64)> = Vec::new();
        for pair in text.split(',').filter(|pair| !pair.is_empty()) {
            let (a, b) = pair.split_once('-')?;
            let (a, b): (u64, u64) = (a.parse().ok()?, b.parse().ok()?);
            if a >= b || ranges.last().is_some_and(|&(_, end)| end >= a) {
                return None;
            }
            ranges.push((a, b));
        }
        Some(Self { ranges })
    }
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

/// The outcome of discarding a session only while nobody has claimed its finalization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnclaimedDiscard {
    /// The session was `Pending` or `Uploading` and is gone. The record is the session as it
    /// was discarded.
    Discarded(Box<UploadSessionRecord>),
    /// The session's finalization is claimed, or it is terminal: it was left as it is.
    Claimed,
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

    /// Commit `range`: merge it into the session's committed set, move a `Pending` session to
    /// `Uploading`, and move its progress clock — one atomic step, because they describe one
    /// event, and because two ranges recorded at once must both land.
    ///
    /// Returns the updated record, or `None` if there was no live, active session to record
    /// against — expired, discarded, cancelled or terminal. A range recorded against a removed
    /// session is lost, which is the contract: its bytes are garbage and the client sends them
    /// again.
    fn record_range<'a>(
        &'a self,
        upload: &'a UploadId,
        range: CommittedRange,
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

    /// Move a live session to `FailedProcessing`, recording the `error.*` code it failed with,
    /// and return the updated record.
    ///
    /// [`Self::set_status`] with `FailedProcessing`, plus [`UploadSessionRecord::failure_code`]:
    /// the code is what `HEAD` and the session list report, because the request that completed
    /// the session never waits to hear it.
    fn fail_processing<'a>(
        &'a self,
        upload: &'a UploadId,
        failure_code: &'a str,
    ) -> StoreFuture<'a, Option<UploadSessionRecord>>;

    /// Claim the exclusive right to finalize `upload`.
    ///
    /// Compare-and-set into `WaitingForProcessing`: only a `Pending` or `Uploading` session
    /// transitions, so two racing finalizers cannot both win, and the winner leaves the
    /// progress view rather than being evicted out from under itself.
    fn claim_finalize<'a>(&'a self, upload: &'a UploadId) -> StoreFuture<'a, FinalizeClaim>;

    /// Every live session whose finalization is claimed — `WaitingForProcessing` — oldest first,
    /// ties broken by upload id.
    ///
    /// The listing boot recovery reads (transfer upload design doc, the finalization claim): a
    /// claim is held by a finalizer *in a process*, and the claim outlives the process in the
    /// store, so a process that died between claiming and a terminal status leaves a session no
    /// live finalizer is driving. Read before the listener accepts a request, every claim it
    /// returns belongs to a process that is gone. A session reaching a terminal status, or being
    /// discarded, leaves this listing in the same step. Adapters need a secondary index; scanning
    /// is acceptable only in the in-memory double.
    fn claimed(&self) -> StoreFuture<'_, Vec<UploadSessionRecord>>;

    /// Discard a session: its record and its place in every view, together. Returns the record
    /// that was removed, or `None`.
    fn discard<'a>(&'a self, upload: &'a UploadId) -> StoreFuture<'a, Option<UploadSessionRecord>>;

    /// [`Self::discard`] `upload` only if it is `Pending` or `Uploading`.
    ///
    /// The compare and the discard are one step, as in [`Self::claim_finalize`], so a session
    /// whose finalization a racing request claims is never discarded out from under its
    /// finalizer: one of the two wins, and the other sees [`UnclaimedDiscard::Claimed`] or
    /// [`FinalizeClaim::NotFound`].
    fn discard_unclaimed<'a>(&'a self, upload: &'a UploadId) -> StoreFuture<'a, UnclaimedDiscard>;

    /// The active session `uploader` currently has open for `expected_hash`, if any.
    ///
    /// The question the blob serve path asks when nothing references an address: *are these
    /// exact bytes on their way?* An active upload session declaring that hash **is** the
    /// promise, which is why this is a lookup and not a new record — a session already carries a
    /// bounded lifetime ([`LIFETIME_CAP`]) and is already reconciled by the discard worker, so
    /// an abandoned upload's promise expires on its own. A separate "declared original" row in
    /// the asset index would have needed a second lifetime, a second reconciliation, and would
    /// have made every in-flight upload look like a dangling reference to the integrity scrub.
    ///
    /// Scoped to the uploader deliberately. Unscoped, this answers "is somebody, anywhere,
    /// uploading these bytes right now" to any authenticated caller who can name the hash — a
    /// small but real cross-account signal for no gain, since the case the transient answer
    /// exists for is a *second device of the same account* fetching an original the first one
    /// is still sending.
    ///
    /// The **uploader**, not the owner the session is filed under (#462). A writer member's
    /// session is filed under the album owner, so an owner-keyed lookup answered the owner `409`
    /// about the member's transfer and answered the member's own other device `404`.
    ///
    /// Terminal sessions never match: a completed session's bytes are committed and a failed
    /// one's are not coming. Adapters need a secondary index on
    /// `(upload_user_id, expected_hash)`; scanning is acceptable only in the in-memory double.
    fn pending_for_address<'a>(
        &'a self,
        uploader: &'a UserId,
        expected_hash: &'a str,
    ) -> StoreFuture<'a, Option<UploadId>>;

    /// Every active session declaring `expected_hash`, whoever uploads it, smallest id first.
    ///
    /// The question quota release asks after giving an attribution back
    /// ([`crate::upload::reservation`]): attribution is keyed by content address globally, so a
    /// session of **another** account may have been admitted against the attribution just
    /// released, and is now the one that has to be charged for the bytes it will store.
    ///
    /// Unscoped on purpose, and therefore server-internal only. It must never back an answer to
    /// a caller: that would tell any account that can name a hash whether somebody else is
    /// uploading it — the signal [`Self::pending_for_address`] is uploader-scoped to withhold.
    ///
    /// Terminal sessions never match, as for [`Self::pending_for_address`]. Adapters need a
    /// secondary index on `expected_hash`; scanning is acceptable only in the in-memory double.
    fn live_for_address<'a>(&'a self, expected_hash: &'a str) -> StoreFuture<'a, Vec<UploadId>>;

    /// How many sessions are still in flight against `album`.
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

#[cfg(test)]
mod tests;
