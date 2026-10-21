//! The crash-injection seam E2E case 11 needs.
//!
//! # Why a decorator and not a hook in the server
//!
//! Finalization's order is the contract (`upload/finalize.rs`): the blob is committed onto its
//! content address — a rename and an fsync, irreversible — and only then is it recorded against
//! its asset. The window between those two steps is what case 11 is about, and the seam that
//! reaches into it is the [`AssetIndex`] **port itself**. No production code gains a test hook;
//! this wraps the index the fixture already builds and fails one call.
//!
//! # Why it fails exactly once
//!
//! A decorator that failed forever would test an index that is permanently down, which the suite
//! already covers through `SwitchableIndex`. What case 11 is about is a crash — a single
//! transaction that never commits, followed by a server that comes back. So the fault arms,
//! fires once, and disarms itself, which lets the same case drive the retry that recovers.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use capsule_server::blob::ContentAddress;
use capsule_server::index::{
    AssetIndex, AssetRow, BlobOutcome, BlobRecord, BlobReference, Custody, FeedEntry, HoldOutcome,
    IndexFuture, LifecycleOp, OpOutcome, PendingAsset, PurgeOutcome, Receipted, Reservation,
    ServingHold,
};
use capsule_server::store::{AlbumId, AssetId, OwnerId, StoreError};
use jiff::Timestamp;

/// Disarmed.
const DISARMED: u8 = 0;
/// The next finalization write never commits.
const BEFORE_COMMIT: u8 = 1;
/// The next finalization write commits, and its answer is lost.
const AFTER_COMMIT: u8 = 2;

/// An [`AssetIndex`] that loses one finalization write, then behaves.
///
/// The finalization writes are `record_blob` and the two custody writes that carry a receipt
/// (#693). Every other operation delegates unconditionally: the crash this models is a process
/// that died around one transaction, not a database that stopped answering.
#[derive(Debug)]
pub(crate) struct CrashBeforeCommit {
    inner: Arc<dyn AssetIndex>,
    armed: AtomicU8,
    fired: AtomicUsize,
}

impl CrashBeforeCommit {
    /// A disarmed decorator over `inner`.
    pub(crate) fn new(inner: Arc<dyn AssetIndex>) -> Self {
        Self {
            inner,
            armed: AtomicU8::new(DISARMED),
            fired: AtomicUsize::new(0),
        }
    }

    /// Lose the next finalization write before it commits.
    pub(crate) fn arm(&self) {
        self.armed.store(BEFORE_COMMIT, Ordering::SeqCst);
    }

    /// Let the next finalization write commit, then lose its answer — the process died between
    /// the database's commit and the caller hearing of it.
    pub(crate) fn arm_after_commit(&self) {
        self.armed.store(AFTER_COMMIT, Ordering::SeqCst);
    }

    /// How many times the fault has fired.
    ///
    /// Asserted by the case rather than assumed: a fault that never fired would leave every
    /// assertion after it describing an ordinary successful upload.
    pub(crate) fn fired(&self) -> usize {
        self.fired.load(Ordering::SeqCst)
    }

    /// Which fault this call takes, if any, disarming it.
    fn take_the_fault(&self) -> u8 {
        let fault = self.armed.swap(DISARMED, Ordering::SeqCst);
        if fault != DISARMED {
            self.fired.fetch_add(1, Ordering::SeqCst);
        }
        fault
    }

    /// Run `write` under whatever fault is armed.
    fn faulted<'a, T: Send + 'a>(&'a self, write: IndexFuture<'a, T>) -> IndexFuture<'a, T> {
        match self.take_the_fault() {
            // The transaction never commits. `Unavailable` is the honest shape: the index did
            // not answer, so the caller cannot know whether the row was written — which is
            // exactly the state a crashed process leaves behind. The write is dropped unpolled.
            BEFORE_COMMIT => Box::pin(async {
                Err(lost(
                    "the server died before the index transaction committed",
                ))
            }),
            AFTER_COMMIT => Box::pin(async move {
                write.await?;
                Err(lost(
                    "the server died after the index transaction committed",
                ))
            }),
            _ => write,
        }
    }
}

/// The error a lost transaction surfaces as.
fn lost(detail: &str) -> StoreError {
    StoreError::Unavailable {
        store: "asset-index",
        detail: detail.to_owned(),
    }
}

impl AssetIndex for CrashBeforeCommit {
    fn record_blob<'a>(
        &'a self,
        asset: &'a AssetId,
        blob: BlobRecord,
    ) -> IndexFuture<'a, BlobOutcome> {
        self.faulted(self.inner.record_blob(asset, blob))
    }

    fn record_blob_with_receipt<'a>(
        &'a self,
        asset: &'a AssetId,
        blob: BlobRecord,
        custody: Custody<'a>,
    ) -> IndexFuture<'a, Receipted<BlobOutcome>> {
        self.faulted(self.inner.record_blob_with_receipt(asset, blob, custody))
    }

    fn apply_op_with_receipt<'a>(
        &'a self,
        op: LifecycleOp,
        custody: Custody<'a>,
    ) -> IndexFuture<'a, Receipted<OpOutcome>> {
        self.faulted(self.inner.apply_op_with_receipt(op, custody))
    }

    fn reserve(&self, asset: PendingAsset) -> IndexFuture<'_, Reservation> {
        self.inner.reserve(asset)
    }

    fn read<'a>(&'a self, asset: &'a AssetId) -> IndexFuture<'a, Option<AssetRow>> {
        self.inner.read(asset)
    }

    fn tombstone<'a>(
        &'a self,
        asset: &'a AssetId,
        at: Timestamp,
    ) -> IndexFuture<'a, Option<AssetRow>> {
        self.inner.tombstone(asset, at)
    }

    fn find_by_address<'a>(
        &'a self,
        owner: &'a OwnerId,
        album: &'a AlbumId,
        address: &'a ContentAddress,
    ) -> IndexFuture<'a, Option<AssetId>> {
        self.inner.find_by_address(owner, album, address)
    }

    fn find_references<'a>(
        &'a self,
        address: &'a ContentAddress,
    ) -> IndexFuture<'a, Vec<BlobReference>> {
        self.inner.find_references(address)
    }

    fn apply_op(&self, op: LifecycleOp) -> IndexFuture<'_, OpOutcome> {
        self.inner.apply_op(op)
    }

    fn set_hold<'a>(
        &'a self,
        asset: &'a AssetId,
        hold: Option<ServingHold>,
    ) -> IndexFuture<'a, HoldOutcome> {
        self.inner.set_hold(asset, hold)
    }

    fn reference_count<'a>(&'a self, address: &'a ContentAddress) -> IndexFuture<'a, u64> {
        self.inner.reference_count(address)
    }

    fn rows<'a>(
        &'a self,
        after: Option<&'a AssetId>,
        limit: usize,
    ) -> IndexFuture<'a, Vec<AssetRow>> {
        self.inner.rows(after, limit)
    }

    fn tombstoned(&self, limit: usize) -> IndexFuture<'_, Vec<AssetRow>> {
        self.inner.tombstoned(limit)
    }

    fn purge<'a>(&'a self, asset: &'a AssetId, now: Timestamp) -> IndexFuture<'a, PurgeOutcome> {
        self.inner.purge(asset, now)
    }

    fn feed_page<'a>(
        &'a self,
        owner: &'a OwnerId,
        after: u64,
        limit: usize,
    ) -> IndexFuture<'a, Vec<FeedEntry>> {
        self.inner.feed_page(owner, after, limit)
    }

    fn head_seq<'a>(&'a self, owner: &'a OwnerId) -> IndexFuture<'a, u64> {
        self.inner.head_seq(owner)
    }

    fn album_feed_page<'a>(
        &'a self,
        owner: &'a OwnerId,
        album: &'a AlbumId,
        after: u64,
        limit: usize,
    ) -> IndexFuture<'a, Vec<FeedEntry>> {
        self.inner.album_feed_page(owner, album, after, limit)
    }

    fn album_head_seq<'a>(
        &'a self,
        owner: &'a OwnerId,
        album: &'a AlbumId,
    ) -> IndexFuture<'a, u64> {
        self.inner.album_head_seq(owner, album)
    }
}
