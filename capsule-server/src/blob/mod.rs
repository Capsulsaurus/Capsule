//! The blob store port: one narrow, Capsule-owned contract over an arbitrary backend.
//!
//! # What this is, and what it deliberately is not
//!
//! The server holds ciphertext it cannot read. What it needs from storage is small and fixed:
//! stage an upload's bytes, place them at their content address atomically, look one up, read a
//! window of one, walk all of them, drop one, and hold a bad one for an operator. That list is
//! this trait. It is **not** a generic object store: `object_store` and generic CAS/transfer
//! crates are refused by AGENTS.md's Rust Architecture Decisions and by `xtask
//! architecture-check`'s retired list, because the security contract here — content addressing,
//! immutability, atomic finalization, and enumeration cost — is the thing that would be
//! abstracted away.
//!
//! It follows the shape `S-C29` fixed for the two state ports (see [`crate::store`]): typed
//! operations with no arbitrary serializable payload, properties that belong to the store rather
//! than to each call, boxed futures so every port stays dyn-compatible, and **one shared
//! conformance suite in `src/`** that every adapter must pass.
//!
//! # Enumeration is a first-class operation, and that is the whole design
//!
//! `blobs/{hash[0:2]}/{hash[2:4]}/{hash}.bin` was settled (design/filesystem/server.md) on a
//! sizing case that is *not* lookup: a lookup is a `stat` at a known address and costs the same
//! flat or sharded. The cost lands on the three full-store walks — the integrity scrub's
//! blob→row pass (`S-C14`), the refcount GC's orphan sweep (`S-C11`), and the index rebuild
//! (`S-C1`'s recovery counterpart) — each a complete `readdir` + `stat`, each an integrity or
//! recovery path that must stay affordable exactly when a deployment is already in trouble.
//!
//! So [`BlobStore::enumerate`] is on the port. A caller that walked the tree itself would fork
//! the layout, and the first fork is the one that stops finding blobs after the shard changes.
//! It is paged, because "return every address" is not an operation a multi-million-entry store
//! can answer, and its order is the address's own — which a sharded walk and a flat walk agree
//! on precisely because the shard is an address *prefix*. That is what makes the cursor
//! resumable and what makes the shard invisible to every consumer.
//!
//! # Four things the layout decision did not settle
//!
//! ## 1. Temp-file placement
//!
//! There are two write paths, and neither writes `blobs/{hash}.tmp`.
//!
//! - **The upload path has no temp file at all.** `incoming/{upload_id}.bin` *is* the staging
//!   file — that is what the append-only upload protocol already builds — and finalization
//!   renames it straight to its content address. The whole tree is on one filesystem
//!   (design/filesystem/server.md; the requirement is recorded in `self-hosting.md`), so that
//!   rename is atomic.
//! - **[`BlobStore::put`], for bytes already in hand, stages inside the target shard**:
//!   `blobs/{aa}/{bb}/.{hash}.{nonce}.tmp`, renamed in place. Same directory is a stronger
//!   guarantee than same filesystem and needs no configuration to stay true. It is deliberately
//!   *not* `incoming/` — that namespace is owned by upload sessions and swept at startup — and
//!   deliberately not `blobs/{hash}.tmp`, which under sharding would be the one file living
//!   outside every shard, a permanent exception the enumeration walk would have to carry. A temp
//!   left by a crash is debris inside a shard the scrub already walks, and
//!   [`BlobStore::enumerate`] reports it as such.
//!
//! ## 2. Shard-directory durability
//!
//! POSIX does not make a rename durable until the *containing directory* is fsynced, and does
//! not make a newly created directory durable until *its parent* is. A crash in that window
//! loses the shard directory entry and with it the blob — while the caller's Postgres row may
//! already have committed. That is the asymmetric failure: the store would hold a **dangling
//! reference**, which design/filesystem/server.md says is a loud integrity error and never
//! auto-deleted, rather than the benign orphan the GC sweep reclaims.
//!
//! So the filesystem adapter fsyncs directories, and does it in the only order that helps:
//! create the shard directories, fsync the parent of each one it actually created (top down), do
//! the rename, then fsync the directory the rename landed in — and only then may the caller
//! commit its index row. The cost is up to three extra directory fsyncs the first time a shard
//! is touched and exactly one per commit after that, which is the honest price of the atomicity
//! the design already claims. It is not a tuning knob; there is one code path.
//!
//! ## 3. What else shards
//!
//! **Nothing. `incoming/` and `quarantine/` stay flat**, and for the same reason `blobs/` does
//! not: the sizing case is enumeration over a multi-million-entry directory.
//!
//! - `incoming/` is bounded by *concurrent upload sessions*, capped at a 24-hour lifetime
//!   (design/filesystem/server.md, "Required Services"). Its enumeration — the startup orphan
//!   scrub, [`BlobStore::staged`] — is over that live set, not over the store's history.
//! - `quarantine/` is bounded by *failures*. A healthy store's is empty, and the directory exists
//!   to be read by a human; sharding it would hide the one inventory an operator wants to `ls`.
//!
//! Neither is content-addressed either, so neither has a hex prefix to shard *on* without
//! inventing one. If `quarantine/` ever grows to the size that motivated the `blobs/` shard, the
//! store has a problem no directory layout answers.
//!
//! ## 4. Tying the layout to the address invariant
//!
//! The shard is a slice of the address's own hex, so it is correct only while the address is 64
//! lowercase-hex characters. [`ContentAddress`] is the single gate: one constructor, no
//! `From<String>`, no public field, so no `&str` reaches a path. A digest change fails at the
//! const assertions in [`address`] rather than silently filing new blobs under a truncated
//! shard. See that module's docs.
//!
//! # What this port does not do
//!
//! - **It does not hash.** The caller supplies a verified [`ContentAddress`]; the store places
//!   bytes at it. Re-hashing on every commit would be a second full read of every upload on the
//!   hot path, and the upload protocol already hashes each chunk on arrival. The store's own
//!   invariant is *immutability* — a committed address is never overwritten — which is what
//!   makes a wrong address a caller bug rather than silent corruption of a good blob.
//! - **It does not own `.server/`.** The schema version and the operator's configuration are
//!   plaintext server metadata, not blobs; nothing in this slice reads them.
//! - **It does not count references.** Reference counting is a query over committed Postgres
//!   rows (design/filesystem/server.md, "Deletion and Garbage Collection"), and a store that
//!   kept its own counter is the drift that design forbids.

pub mod address;
pub mod conformance;
pub mod fs;
pub mod memory;

use std::fmt;
use std::future::Future;
use std::pin::Pin;

use jiff::Timestamp;

pub use self::address::{ContentAddress, MalformedAddress};
pub use self::fs::FilesystemBlobStore;
pub use self::memory::InMemoryBlobStore;
use crate::store::UploadId;

/// The future every blob-store operation returns.
///
/// Boxed rather than `async fn` in trait position for the same reason [`crate::store`] boxes
/// its own: application state holds an `Arc<dyn BlobStore>`, so the whole server does not become
/// generic over its storage.
pub type BlobFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, BlobError>> + Send + 'a>>;

/// What can go wrong reaching the blob store.
///
/// Operator diagnostics, not a user-facing surface: mapping a failed store operation onto an
/// `error.*` code belongs to the route that could not complete, which knows what the caller was
/// trying to do.
///
/// `#[non_exhaustive]` because an adapter this slice has not written yet may need a variant.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BlobError {
    /// The backend could not carry out `operation`.
    #[error("the blob store could not {operation}: {detail}")]
    Backend {
        /// What was being attempted, for the log line.
        operation: &'static str,
        /// The backend's own description of the failure.
        detail: String,
    },

    /// There is no staged file for this upload — it was never begun, or it has already been
    /// committed or abandoned.
    #[error("upload {upload} has no staged bytes")]
    NotStaged {
        /// The upload that was addressed.
        upload: UploadId,
    },

    /// An append arrived somewhere other than the end of the staged file.
    ///
    /// A value, not a surprise: the upload protocol answers a resuming client with the offset it
    /// should send from, and this carries it. Nothing was written.
    #[error("upload {upload} is {actual} bytes long, so an append at {offset} was refused")]
    OffsetMismatch {
        /// The upload that was addressed.
        upload: UploadId,
        /// Where the caller tried to write.
        offset: u64,
        /// Where the staged file actually ends — the offset to resume from.
        actual: u64,
    },

    /// An upload identifier that cannot name a file.
    ///
    /// Every adapter refuses one, not only the filesystem: an identifier that is a path segment
    /// on one backend and a key prefix on another must mean the same thing on both, and a
    /// traversal sequence must mean nothing anywhere.
    #[error("`{upload}` is not an identifier the blob store can name a staged file with")]
    MalformedUpload {
        /// The identifier that was refused.
        upload: UploadId,
    },
}

/// Whether a write placed bytes, or found the address already occupied.
///
/// Deduplication is the point (design/filesystem/server.md, "Content-Addressing and
/// Deduplication"): a blob already present is never stored twice, and a finalized blob is
/// immutable — so an occupied address is a success that wrote nothing, never an overwrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// The bytes were written and the address is now occupied.
    Stored,
    /// The address was already occupied; the existing bytes are untouched.
    AlreadyPresent,
}

/// What the store knows about one blob without reading it.
///
/// Size and nothing else. A modification time is not a fact this contract owns — a rename does
/// not change one, no consumer in the design reads one, and `collectable_since` (the fact GC
/// actually reasons over) lives in Postgres.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobStat {
    /// The blob's content address.
    pub address: ContentAddress,
    /// Its length in bytes.
    pub size: u64,
}

/// One page of a full-store enumeration.
///
/// See [`BlobStore::enumerate`] for the ordering and cursor contract.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BlobPage {
    /// The blobs in this page, in ascending content-address order.
    pub entries: Vec<BlobStat>,

    /// Entries encountered while producing this page that are not blobs, named relative to the
    /// finalized store's root — a crashed temp file, a stray directory, a file whose name is a
    /// valid address but whose path is a shard it does not derive.
    ///
    /// Reported rather than skipped, because the integrity scrub's debris inventory is one of
    /// this operation's three consumers and "the walk quietly ignored it" is how debris becomes
    /// permanent. A complete enumeration reports every debris entry at least once; which page it
    /// arrives on is an adapter's business.
    pub debris: Vec<String>,

    /// The cursor to pass as `after` for the next page, or `None` when the walk is complete.
    pub next: Option<ContentAddress>,
}

/// Why a blob is being held for an operator.
///
/// Deliberately derives no `serde` traits, exactly as the state ports' records do not: the
/// persistence encoding belongs to the *adapter* (the filesystem one writes the `.reason.json`
/// design/filesystem/server.md names), not to the record, so a record cannot be smuggled through
/// a store that was not built for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantineReason {
    /// The stable `error.*` code naming the structural check that failed.
    pub code: String,
    /// The operator-facing detail, in English.
    pub detail: String,
    /// The server's own trusted clock reading when the blob was pulled.
    pub at: Timestamp,
}

/// A blob under forensic hold, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantinedBlob {
    /// The address it was stored under before the hold.
    pub address: ContentAddress,
    /// Why it was pulled.
    pub reason: QuarantineReason,
}

/// Ciphertext storage: staging, content-addressed commit, lookup, enumeration, removal, hold.
///
/// Every method is total over its inputs — absence is `None` or `false`, never an error — so the
/// recovery paths that walk a damaged store keep walking.
pub trait BlobStore: fmt::Debug + Send + Sync {
    // ── Staging ───────────────────────────────────────────────────────────────────────────

    /// Open an append-only staging file for `upload`, replacing any bytes already staged under
    /// that identifier.
    ///
    /// Replacing rather than refusing is what makes a re-created session start from zero: the
    /// upload store mints a fresh identifier per session, so a collision is a restart.
    fn begin<'a>(&'a self, upload: &'a UploadId) -> BlobFuture<'a, ()>;

    /// Durably append `bytes` at `offset`, returning the staged file's new length.
    ///
    /// `offset` must be the current length: the file is append-only, and a mismatch is
    /// [`BlobError::OffsetMismatch`] carrying the offset to resume from, with nothing written.
    /// The append is durable before this returns, which is what makes [`Self::staged_len`] the
    /// truth the session's cached counter is reconciled *up* to
    /// ([`UploadSessionStore::reconcile_received_bytes`](crate::store::UploadSessionStore::reconcile_received_bytes)).
    fn append<'a>(
        &'a self,
        upload: &'a UploadId,
        offset: u64,
        bytes: &'a [u8],
    ) -> BlobFuture<'a, u64>;

    /// The staged file's length, or `None` when nothing is staged.
    fn staged_len<'a>(&'a self, upload: &'a UploadId) -> BlobFuture<'a, Option<u64>>;

    /// Discard `upload`'s staged bytes. `true` if there were any.
    fn abandon<'a>(&'a self, upload: &'a UploadId) -> BlobFuture<'a, bool>;

    /// Every upload with bytes currently staged, in ascending identifier order.
    ///
    /// The startup scrub's input: a staged file whose session no longer exists is an orphan to
    /// abandon, and one whose length diverges from its session's counter is a reconcile.
    fn staged(&self) -> BlobFuture<'_, Vec<UploadId>>;

    /// Atomically place `upload`'s staged bytes at `address`, clearing the stage either way.
    ///
    /// The caller has already verified that the staged bytes hash to `address`; see the module
    /// docs on why the store does not re-hash. If `address` is already occupied the staged bytes
    /// are discarded and [`Placement::AlreadyPresent`] is returned — deduplication, and the
    /// immutability of a finalized blob, in one rule.
    ///
    /// On return the blob is durable, including the directory entries that reach it, so a caller
    /// may commit its index row next.
    fn commit<'a>(
        &'a self,
        upload: &'a UploadId,
        address: &'a ContentAddress,
    ) -> BlobFuture<'a, Placement>;

    /// Atomically place `bytes` at `address`.
    ///
    /// For a blob the server already holds whole — a signed manifest envelope object, a
    /// federation pull that has been buffered, a test fixture. The streaming path is
    /// [`Self::begin`] / [`Self::append`] / [`Self::commit`]; this one materialises its argument
    /// by definition, so it is not the path a multi-gigabyte original takes.
    fn put<'a>(&'a self, address: &'a ContentAddress, bytes: &'a [u8])
    -> BlobFuture<'a, Placement>;

    // ── Reading ───────────────────────────────────────────────────────────────────────────

    /// What the store knows about `address` without reading it, or `None` if absent.
    ///
    /// The `stat` behind storage verification's *stored* fact (design/filesystem/server.md,
    /// "Storage Verification"), and the lookup the shard costs nothing extra for.
    fn stat<'a>(&'a self, address: &'a ContentAddress) -> BlobFuture<'a, Option<BlobStat>>;

    /// Read up to `len` bytes of `address` starting at `offset`.
    ///
    /// Clamped, not refused: a window past the end yields what exists, which is what a `Range`
    /// response and a deep re-hash both want. `None` only when the blob is absent.
    fn read_at<'a>(
        &'a self,
        address: &'a ContentAddress,
        offset: u64,
        len: usize,
    ) -> BlobFuture<'a, Option<Vec<u8>>>;

    // ── Enumeration ───────────────────────────────────────────────────────────────────────

    /// One page of every blob in the store, in ascending content-address order.
    ///
    /// `after` is exclusive: pass `None` to start, then the previous page's
    /// [`BlobPage::next`] until it is `None`. `limit` bounds [`BlobPage::entries`] only —
    /// [`BlobPage::debris`] rides along with whatever page encountered it.
    ///
    /// The order is the contract, not an artefact. It is the address's own, so a sharded walk
    /// and a flat walk emit the same sequence; that is what lets a scrub, a GC sweep or a
    /// rebuild resume mid-store after a restart, and what keeps the shard invisible to all
    /// three. A store whose tree is partially populated — most shards absent, some holding one
    /// blob — enumerates without error, because the absent directories are not entries.
    fn enumerate<'a>(
        &'a self,
        after: Option<&'a ContentAddress>,
        limit: usize,
    ) -> BlobFuture<'a, BlobPage>;

    // ── Removal and hold ──────────────────────────────────────────────────────────────────

    /// Remove `address`. `true` if it was there.
    ///
    /// Only ever called for a blob the caller has proved unreferenced inside its deleting
    /// transaction; the store enforces no policy of its own, and holds no reference count to
    /// enforce one with.
    fn remove<'a>(&'a self, address: &'a ContentAddress) -> BlobFuture<'a, bool>;

    /// Move `address` out of the store and hold it for an operator, recording `reason`.
    ///
    /// `true` if there was a blob to pull. The bytes are preserved rather than dropped: an
    /// unrecoverable byte sequence must survive a rebuild for forensic inspection
    /// (design/filesystem/server.md, "Recovering the Index from Blobs Alone").
    fn quarantine<'a>(
        &'a self,
        address: &'a ContentAddress,
        reason: QuarantineReason,
    ) -> BlobFuture<'a, bool>;

    /// Everything currently held, in ascending content-address order.
    ///
    /// Unpaged, unlike [`Self::enumerate`]: a healthy store's hold is empty, and one large
    /// enough to need paging is an incident, not a walk to optimise.
    fn quarantined(&self) -> BlobFuture<'_, Vec<QuarantinedBlob>>;
}

/// How long an upload identifier may be before no adapter will name a staged file with it.
///
/// A UUID's 36 characters with room to spare, and short enough that no filesystem's name limit
/// is the thing that decides.
pub const MAX_UPLOAD_ID_LEN: usize = 64;

/// Check that `upload` can name a staged file on any adapter.
///
/// Shared by every adapter rather than left to the filesystem one, because the answer must not
/// depend on the backend: `../../etc/passwd` is a path traversal on a filesystem and an
/// unremarkable key elsewhere, and a port whose invariants hold only on one adapter is a port
/// whose conformance suite proves nothing.
///
/// # Errors
///
/// [`BlobError::MalformedUpload`] unless `upload` is 1..=[`MAX_UPLOAD_ID_LEN`] characters of
/// ASCII alphanumerics, `-` or `_`.
pub fn check_upload_id(upload: &UploadId) -> Result<(), BlobError> {
    let text = upload.as_str();
    let acceptable = !text.is_empty()
        && text.len() <= MAX_UPLOAD_ID_LEN
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');

    if acceptable {
        Ok(())
    } else {
        Err(BlobError::MalformedUpload {
            upload: upload.clone(),
        })
    }
}

/// Clamp a `[offset, offset + len)` window onto a blob of `size` bytes.
///
/// Shared so every adapter clamps identically: a window starting past the end is empty, and one
/// running past it stops there.
pub(crate) fn window(size: u64, offset: u64, len: usize) -> (usize, usize) {
    if offset >= size {
        return (0, 0);
    }
    let available = size - offset;
    let taken = available.min(len as u64);
    let start = usize::try_from(offset).unwrap_or(usize::MAX);
    let taken = usize::try_from(taken).unwrap_or(usize::MAX);
    (start, taken)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_upload_id_must_be_a_name_every_adapter_can_carry() {
        assert!(check_upload_id(&UploadId::new("019286c1-3f7a-7c11-9d2e-6f5a1b2c3d4e")).is_ok());
        assert!(check_upload_id(&UploadId::new("a_b-C9")).is_ok());
        assert!(check_upload_id(&UploadId::new("")).is_err());
        assert!(check_upload_id(&UploadId::new("..")).is_err());
        assert!(check_upload_id(&UploadId::new("../../etc/passwd")).is_err());
        assert!(check_upload_id(&UploadId::new("a/b")).is_err());
        assert!(check_upload_id(&UploadId::new("a.bin")).is_err());
        assert!(check_upload_id(&UploadId::new("a".repeat(MAX_UPLOAD_ID_LEN))).is_ok());
        assert!(check_upload_id(&UploadId::new("a".repeat(MAX_UPLOAD_ID_LEN + 1))).is_err());
    }

    #[test]
    fn a_read_window_is_clamped_to_the_blob_it_reads() {
        assert_eq!(window(10, 0, 4), (0, 4));
        assert_eq!(
            window(10, 8, 4),
            (8, 2),
            "a window past the end stops there"
        );
        assert_eq!(
            window(10, 10, 4),
            (0, 0),
            "a window starting at the end is empty"
        );
        assert_eq!(window(10, 99, 4), (0, 0));
        assert_eq!(window(10, 0, 0), (0, 0));
    }
}
