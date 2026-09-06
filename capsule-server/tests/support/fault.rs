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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use capsule_server::blob::ContentAddress;
use capsule_server::index::{
    AssetIndex, AssetRow, BlobOutcome, BlobRecord, BlobReference, FeedEntry, HoldOutcome,
    IndexFuture, LifecycleOp, OpOutcome, PendingAsset, Reservation, ServingHold,
};
use capsule_server::store::{AlbumId, AssetId, OwnerId, StoreError};
use jiff::Timestamp;

/// An [`AssetIndex`] that loses one `record_blob` transaction, then behaves.
///
/// Every other operation delegates unconditionally: the crash this models is a process that died
/// with one transaction open, not a database that stopped answering.
#[derive(Debug)]
pub(crate) struct CrashBeforeCommit {
    inner: Arc<dyn AssetIndex>,
    armed: AtomicBool,
    fired: AtomicUsize,
}

impl CrashBeforeCommit {
    /// A disarmed decorator over `inner`.
    pub(crate) fn new(inner: Arc<dyn AssetIndex>) -> Self {
        Self {
            inner,
            armed: AtomicBool::new(false),
            fired: AtomicUsize::new(0),
        }
    }

    /// Lose the next `record_blob`.
    pub(crate) fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }

    /// How many times the fault has fired.
    ///
    /// Asserted by the case rather than assumed: a fault that never fired would leave every
    /// assertion after it describing an ordinary successful upload.
    pub(crate) fn fired(&self) -> usize {
        self.fired.load(Ordering::SeqCst)
    }

    /// Whether this call is the one that is lost.
    fn takes_the_fault(&self) -> bool {
        if self.armed.swap(false, Ordering::SeqCst) {
            self.fired.fetch_add(1, Ordering::SeqCst);
            return true;
        }
        false
    }
}

impl AssetIndex for CrashBeforeCommit {
    fn record_blob<'a>(
        &'a self,
        asset: &'a AssetId,
        blob: BlobRecord,
    ) -> IndexFuture<'a, BlobOutcome> {
        if self.takes_the_fault() {
            // The transaction never commits. `Unavailable` is the honest shape: the index did
            // not answer, so the caller cannot know whether the row was written — which is
            // exactly the state a crashed process leaves behind.
            return Box::pin(async {
                Err(StoreError::Unavailable {
                    store: "asset-index",
                    detail: "the server died before the index transaction committed".to_owned(),
                })
            });
        }
        self.inner.record_blob(asset, blob)
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

    fn find_reference<'a>(
        &'a self,
        address: &'a ContentAddress,
    ) -> IndexFuture<'a, Option<BlobReference>> {
        self.inner.find_reference(address)
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

    fn purge<'a>(&'a self, asset: &'a AssetId) -> IndexFuture<'a, Option<AssetRow>> {
        self.inner.purge(asset)
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
}
