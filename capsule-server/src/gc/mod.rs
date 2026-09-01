//! Reference-counted garbage collection and the retention purge (`S-C11`).
//!
//! Two workers, and a rule they both obey: **a bug here must bias toward keeping bytes.**
//! Every asymmetry below follows from that, and none of them is a matter of taste.
//!
//! # Reference counting is a query, never a counter
//!
//! A blob is collectable when *no asset row references it*, and that is asked of
//! [`AssetIndex::reference_count`](crate::index::AssetIndex::reference_count) each time rather
//! than tracked in a number somebody has to remember to decrement. A counter is a second copy
//! of a derivable fact, and the failure mode of a counter that drifts low is deleting a live
//! blob — which is the one outcome this module exists to prevent.
//!
//! A **tombstoned** asset still holds its references. Deleting is not purging: the bytes stay
//! until the retention window the user signed into the delete manifest has passed, which is
//! what makes trash recoverable.
//!
//! # Two phases, and what the grace window is actually for
//!
//! Reaching zero references **marks** a blob; it is swept only after a grace window *and* only
//! after zero is re-confirmed. The window is not politeness — it is what makes the
//! finalization-crash orphan collectable without racing a legitimate late reference. A blob
//! renamed into the store whose index write never landed looks exactly like a blob whose last
//! reference just went away, and the difference only becomes visible by waiting.
//!
//! A reference reappearing during the window **cancels** the mark. That is the retry case: an
//! in-flight finalization or a concurrent merge re-references a blob mid-window, and the mark
//! has to go rather than merely be re-evaluated at sweep time.
//!
//! # A mismatch is never resolved by deletion
//!
//! The two directions are asymmetric because only one risks data loss:
//!
//! - a blob with **no referencing row** is an orphan, and the sweep above reclaims it;
//! - a committed row referencing a blob **missing from the store** is a loud integrity error —
//!   reported, logged, and left alone. Deleting the row would erase the only record that the
//!   asset should exist, which is the data-loss class the integrity principle forbids.
//!
//! # The retention floor is the client's, not the server's
//!
//! `retention_until` is signed into the `delete` manifest, so the purge worker reads it from
//! the asset row rather than from a local policy. A hostile server cannot accelerate a purge by
//! editing a config, and a buggy one cannot retain past the window the user chose. A tombstone
//! whose row carries **no** retention is never purged: absent is not "immediately", and reading
//! it that way would purge exactly the assets whose delete manifest the server failed to
//! project a field out of.
//!
//! # Dry run is the default posture, not a debugging aid
//!
//! Both workers take a [`Mode`], and a [`Mode::DryRun`] pass reports precisely what a real one
//! would do without touching anything. A sweep that looks wrong should be inspectable before it
//! runs, and an operator who cannot preview a destructive batch will eventually run one blind.

use std::sync::Arc;

use jiff::{SignedDuration, Timestamp};

use crate::blob::{BlobStore, ContentAddress};
use crate::index::AssetIndex;
use crate::store::{AssetId, StoreError};

/// How long a blob must sit at zero references before it is swept.
///
/// Long enough for an in-flight finalization retry to re-reference it, and short enough that an
/// orphan is not held forever. The design's range is 24–72 hours.
pub const DEFAULT_GRACE_WINDOW: SignedDuration = SignedDuration::from_hours(24);

/// Whether a pass may change anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Report what would happen. Nothing is marked, unmarked, swept or purged.
    DryRun,
    /// Carry it out.
    Apply,
}

impl Mode {
    /// Whether this pass writes.
    fn applies(self) -> bool {
        self == Self::Apply
    }
}

/// What one collection pass found.
///
/// Every field is a list rather than a count, because the traceability principle asks for a log
/// naming the blob, and a count tells an operator that something happened without telling them
/// what to look at.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CollectionReport {
    /// Blobs that reached zero references in this pass and were marked.
    pub marked: Vec<ContentAddress>,
    /// Blobs whose mark was cancelled because a reference reappeared.
    pub unmarked: Vec<ContentAddress>,
    /// Blobs swept: marked, past the window, and still at zero when re-checked.
    pub swept: Vec<ContentAddress>,
    /// Blobs marked and past the window whose reference count was **not** zero at sweep time.
    ///
    /// Reported separately from [`Self::unmarked`] because it is the case the re-confirmation
    /// exists to catch: a reference that appeared between the mark and the sweep.
    pub reprieved: Vec<ContentAddress>,
    /// Addresses an asset row references that the store does not hold.
    ///
    /// Never deleted, never marked, never counted as collectable. Surfaced for an operator.
    pub dangling: Vec<ContentAddress>,
    /// Bytes credited back by the sweep, per account (`S-C44`).
    ///
    /// Reported rather than merely done, because "the trash emptied and my usage did not move"
    /// is exactly the complaint this exists to answer, and an operator needs to be able to say
    /// what the pass gave back and to whom.
    pub credited: Vec<(crate::store::UserId, u64)>,
}

/// What one purge pass found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PurgeReport {
    /// Assets whose retention window has passed and whose blob references were dropped.
    pub purged: Vec<AssetId>,
    /// Tombstoned assets still inside their retention window.
    ///
    /// Reported so a dry run shows what is *waiting*, which is what an operator asking "why has
    /// this not gone yet" wants to see.
    pub retained: Vec<AssetId>,
}

/// Where the collectable marks live.
///
/// A port of its own rather than a field on the blob store, because the mark is the collector's
/// bookkeeping and nothing else writes it — and because putting it on the store would oblige
/// every backend, including a filesystem, to persist a fact that belongs beside the index.
pub trait CollectionStore: std::fmt::Debug + Send + Sync {
    /// Record that `address` reached zero references at `at`. `false` if it was already marked.
    fn mark(&self, address: &ContentAddress, at: Timestamp) -> crate::store::StoreFuture<'_, bool>;

    /// Cancel `address`'s mark. `false` if it was not marked.
    fn unmark<'a>(&'a self, address: &'a ContentAddress) -> crate::store::StoreFuture<'a, bool>;

    /// When `address` was marked, if it is.
    ///
    /// Read by the serving path and by storage verification, which is why it is a lookup rather
    /// than only a listing: a blob mid-collection is not retrievable, and both of those have to
    /// be able to say so about one address.
    fn marked_since<'a>(
        &'a self,
        address: &'a ContentAddress,
    ) -> crate::store::StoreFuture<'a, Option<Timestamp>>;

    /// Every marked address, with when it was marked, in address order.
    fn marks(&self) -> crate::store::StoreFuture<'_, Vec<(ContentAddress, Timestamp)>>;
}

/// The collector's collaborators.
#[derive(Debug, Clone)]
pub struct CollectionContext {
    index: Arc<dyn AssetIndex>,
    blobs: Arc<dyn BlobStore>,
    marks: Arc<dyn CollectionStore>,
    quotas: Arc<dyn crate::quota::QuotaStore>,
    clock: Arc<dyn crate::store::Clock>,
    grace_window: SignedDuration,
}

impl CollectionContext {
    /// Assembles the collector.
    pub fn new(
        index: Arc<dyn AssetIndex>,
        blobs: Arc<dyn BlobStore>,
        marks: Arc<dyn CollectionStore>,
        quotas: Arc<dyn crate::quota::QuotaStore>,
        clock: Arc<dyn crate::store::Clock>,
        grace_window: SignedDuration,
    ) -> Self {
        Self {
            index,
            blobs,
            marks,
            quotas,
            clock,
            grace_window,
        }
    }

    /// Where the collectable marks live.
    pub fn marks(&self) -> &dyn CollectionStore {
        self.marks.as_ref()
    }
}

/// Run one collection pass over every blob in the store.
///
/// # Errors
///
/// Propagates the first store failure. A pass that fails part-way has applied whatever it did
/// before the failure, which is safe in both directions: a mark is reversible and a sweep only
/// ever removed a blob that was confirmed unreferenced twice.
#[tracing::instrument(skip(context), fields(mode = ?mode))]
pub async fn collect(
    context: &CollectionContext,
    mode: Mode,
) -> Result<CollectionReport, StoreError> {
    let now = context.clock.now();
    let mut report = CollectionReport::default();

    // Every blob the store holds, page by page, so an incident-sized store does not have to fit
    // in memory. The address order is the port's contract, so a pass interrupted by a restart
    // resumes where the walk left off.
    let mut after = None;
    loop {
        let page = context
            .blobs
            .enumerate(after.as_ref(), 256)
            .await
            .map_err(|error| StoreError::Unavailable {
                store: "blobs",
                detail: error.to_string(),
            })?;
        for entry in &page.entries {
            visit(context, &entry.address, now, mode, &mut report).await?;
        }
        for debris in &page.debris {
            tracing::warn!(%debris, "the blob store holds something that is not a blob");
        }
        match page.next {
            Some(next) => after = Some(next),
            None => break,
        }
    }

    tracing::info!(
        marked = report.marked.len(),
        unmarked = report.unmarked.len(),
        swept = report.swept.len(),
        reprieved = report.reprieved.len(),
        dangling = report.dangling.len(),
        "a collection pass finished"
    );
    Ok(report)
}

/// Decide one blob.
async fn visit(
    context: &CollectionContext,
    address: &ContentAddress,
    now: Timestamp,
    mode: Mode,
    report: &mut CollectionReport,
) -> Result<(), StoreError> {
    let references = context.index.reference_count(address).await?;
    let marked_since = context.marks.marked_since(address).await?;

    if references > 0 {
        // Live. If it was marked, a reference reappeared during the window and the mark goes.
        if marked_since.is_some() {
            tracing::info!(%address, references, "a mark was cancelled: a reference reappeared");
            if mode.applies() {
                context.marks.unmark(address).await?;
            }
            report.unmarked.push(address.clone());
        }
        return Ok(());
    }

    let Some(since) = marked_since else {
        tracing::info!(%address, "a blob reached zero references and was marked");
        if mode.applies() {
            context.marks.mark(address, now).await?;
        }
        report.marked.push(address.clone());
        return Ok(());
    };

    if now.duration_since(since) <= context.grace_window {
        // Marked and still inside the window. Nothing to do and nothing to report: a blob
        // waiting out its grace is the normal case, not an event.
        return Ok(());
    }

    // Past the window at zero references. The count was just re-read, which is the
    // re-confirmation the contract asks for — the arm above is what fires when it disagrees.
    tracing::info!(%address, marked_since = %since, "sweeping an unreferenced blob");
    if mode.applies() {
        context
            .blobs
            .remove(address)
            .await
            .map_err(|error| StoreError::Unavailable {
                store: "blobs",
                detail: error.to_string(),
            })?;
        context.marks.unmark(address).await?;
        // The credit belongs **here**, to the sweep, and after the removal (`S-C44`).
        //
        // After, because crediting bytes that are still on disk would let a failed sweep refund
        // storage the server is still paying for. And to the sweep rather than to the purge,
        // because a purge drops one asset's references while the blob may still have others —
        // two assets sharing a thumbnail, one deleted — so a refund there would give back bytes
        // held for the surviving holder.
        //
        // The ledger names the account, because the collector cannot: attribution is global by
        // content address, so this blob may be charged to somebody with no remaining connection
        // to the asset whose deletion exposed it.
        if let Some((user, size)) = context.quotas.release_attribution(address).await? {
            report.credited.push((user, size));
        }
    }
    report.swept.push(address.clone());
    Ok(())
}

/// Report every address an asset references that the store does not hold.
///
/// Separate from [`collect`] because it walks the *index* rather than the store, and because it
/// never changes anything: a dangling reference is an integrity error for an operator, and the
/// one thing that must not happen to it is a repair that deletes the row.
///
/// # Errors
///
/// Propagates the first store failure.
#[tracing::instrument(skip(context))]
pub async fn dangling(
    context: &CollectionContext,
    addresses: &[ContentAddress],
) -> Result<Vec<ContentAddress>, StoreError> {
    let mut found = Vec::new();
    for address in addresses {
        let held = context
            .blobs
            .stat(address)
            .await
            .map_err(|error| StoreError::Unavailable {
                store: "blobs",
                detail: error.to_string(),
            })?
            .is_some();
        if held {
            continue;
        }
        if context.index.reference_count(address).await? > 0 {
            tracing::error!(
                %address,
                "a committed row references a blob the store does not hold: integrity error"
            );
            found.push(address.clone());
        }
    }
    Ok(found)
}

/// Run one retention-purge pass.
///
/// Drops the blob references of every tombstoned asset whose signed `retention_until` has
/// passed. The tombstone itself stays: a client that has not synced since the delete still has
/// to learn about it, and removing the row would make the deletion invisible rather than final.
///
/// # Errors
///
/// Propagates the first store failure.
#[tracing::instrument(skip(context), fields(mode = ?mode))]
pub async fn purge_expired(
    context: &CollectionContext,
    mode: Mode,
    limit: usize,
) -> Result<PurgeReport, StoreError> {
    let now = context.clock.now();
    let mut report = PurgeReport::default();

    for row in context.index.tombstoned(limit).await? {
        let Some(retention_until) = row.retention_until else {
            // A tombstone with no signed retention is never purged. Absent is not
            // "immediately", and reading it that way would purge exactly the assets whose
            // delete manifest the server failed to project a field out of.
            tracing::debug!(asset = %row.asset_id, "a tombstone carries no retention floor");
            report.retained.push(row.asset_id);
            continue;
        };
        if now < retention_until {
            report.retained.push(row.asset_id);
            continue;
        }
        // A moderation hold does **not** stop the purge, and that is a decision rather than an
        // omission — see `S-C47`. The retention floor is *signed by the user's own delete
        // manifest*, so honoring it is honoring a deletion the user asked for; a server that
        // kept the bytes anyway would be retaining data against an explicit request, which is
        // the promise this whole path exists to keep. The competing reading — that a legal hold
        // is a preservation obligation and must outlast a delete — is a legal question about
        // what a hold obliges an operator to do, not an engineering one, and guessing it in
        // either direction here would be worse than recording that it is open.
        tracing::info!(
            asset = %row.asset_id,
            %retention_until,
            blobs = row.blobs.len(),
            "purging a tombstoned asset's blob references"
        );
        if mode.applies() {
            context.index.purge(&row.asset_id).await?;
        }
        report.purged.push(row.asset_id);
    }

    tracing::info!(
        purged = report.purged.len(),
        retained = report.retained.len(),
        "a retention purge pass finished"
    );
    Ok(report)
}

pub mod memory;

#[cfg(test)]
mod tests;
