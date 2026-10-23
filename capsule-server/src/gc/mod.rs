//! Reference-counted garbage collection and the retention purge.
//!
//! Two workers, and a rule they both obey: **a bug here must bias toward keeping bytes.**
//! Every asymmetry below follows from that, and none of them is a matter of taste.
//!
//! # Reference counting is a query, never a counter
//!
//! A blob is collectable when *no asset row references it*, and that is asked of
//! [`AssetIndex::reference_count`] each time rather
//! than tracked in a number somebody has to remember to decrement. A counter is a second copy
//! of a derivable fact, and the failure mode of a counter that drifts low is deleting a live
//! blob — which is the one outcome this module exists to prevent.
//!
//! A **live share link** is the other kind of reference (#653). Its sealed metadata record —
//! and any blob its issuer prepared for the boundary crossing — has no asset row, so the index
//! answers zero for it, and [`ShareStore::names_live`] is asked instead. A link that is revoked
//! or past its expiry holds nothing, so its blobs fall back to the ordinary mark-then-sweep.
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
//! # The marks are durable, because a sweep is a later pass
//!
//! A mark has to outlive the process that wrote it: the sweep happens on a pass after the grace
//! window, and a collector run once per process over a volatile store could only ever mark.
//! [`postgres::PostgresCollection`] is the durable adapter and [`memory::InMemoryCollection`]
//! the deterministic one; [`conformance`] is the suite both pass.
//!
//! # Dry run is the default posture, not a debugging aid
//!
//! Both workers take a [`Mode`], and a [`Mode::DryRun`] pass reports precisely what a real one
//! would do without touching anything. A sweep that looks wrong should be inspectable before it
//! runs, and an operator who cannot preview a destructive batch will eventually run one blind.
//!
//! # The pass also sweeps the upload-progress view (#519)
//!
//! The Valkey upload-session adapter keeps one global sorted set of evictable sessions by last
//! progress, and that set has no lifetime of its own: a session whose record expired leaves its
//! member behind until something walks over it, and the only walk is the pressure sweep,
//! [`UploadSessionStore::least_recently_progressed`].
//! Nothing on the request path runs it, so the set grew with every abandoned upload. A collection
//! pass is the server's periodic housekeeping already, so an **applied** pass ends by running
//! that sweep over the whole view — see [`ProgressSweep`]. A dry run skips it, because it
//! removes entries, and a dry run removes nothing.
//!
//! # A purge is ledgered before it is applied (#767)
//!
//! The owner ledger is never rewritten, so a retention purge appends a purge entry to the
//! asset owner's ledger **before** the index drops the references: a `lifecycle` entry with
//! action [`PURGE_ACTION`], the asset id, and no manifest hash, because the purge is
//! server-initiated under the `retention_until` the user signed into the delete. An asset whose
//! entry cannot be appended is not purged on this pass and is reported as
//! [`PurgeReport::unrecorded`]: a purge the ledger does not hold is a deletion a rebuild from
//! the blob root would never learn of. So an applied purge needs the attestation key that signs
//! the ledger ([`CollectionContext::with_attestation`]); a collector given none refuses to apply
//! one, and a dry run, which writes nothing, needs none.
//!
//! Once the index answers that the purge committed, the worker appends a second `lifecycle`
//! entry, action [`PURGED_ACTION`], for the same asset, naming the purge entry's ledger position
//! (#811). Rebuild empties a tombstone only for a purge so confirmed. An entry whose index call answers that the row is no longer
//! purgeable (restored, re-deleted under a later floor, or gone) is never confirmed, so a
//! rebuild keeps the references the live row kept even when the restore reached the ledger
//! *after* the purge entry but the index *before* the purge. The confirmation is best-effort:
//! a lost one keeps references to blobs this collector may since have swept, which a rebuild
//! reports as dangling or purged rather than losing, the bias the module asks for.

use std::sync::Arc;

use jiff::{SignedDuration, Timestamp};

use crate::attestation::AttestationContext;
use crate::blob::{BlobStore, ContentAddress};
use crate::index::{AssetIndex, AssetRow, PurgeOutcome, purge_refusal};
use crate::ledger::LedgerEvent;
pub use crate::ledger::rebuild::{PURGE as PURGE_ACTION, PURGED as PURGED_ACTION};
use crate::share::ShareStore;
use crate::store::{AssetId, StoreError, StoreFuture, UploadSessionStore};

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
    /// Bytes credited back by the sweep, per account.
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
    /// Listed tombstones the purge re-check found were no longer tombstones — restored since
    /// the listing — or no longer existed. Nothing was dropped for them.
    ///
    /// Reported rather than only logged, so the report accounts for every row the pass listed:
    /// a listed asset that is in none of the four lists would be one the pass lost track of.
    pub skipped: Vec<AssetId>,
    /// Purgeable assets whose purge entry the owner ledger could not take. Nothing was dropped
    /// for them; the next pass tries again.
    ///
    /// Reported rather than only logged, because a pass that purged nothing for this reason is
    /// a ledger fault an operator has to act on, not a quiet one.
    pub unrecorded: Vec<AssetId>,
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

/// The upload-progress view's sweep, as an applied collection pass runs it.
///
/// A port of one method rather than the whole [`UploadSessionStore`], because one method is all
/// the collector asks of it — and so a test can assert *that* the pass swept, and when, without
/// standing up an upload-session store to infer it from.
pub trait ProgressSweep: std::fmt::Debug + Send + Sync {
    /// Walk the whole view as of `now`, dropping every member whose session is gone or no
    /// longer evictable, and answer how many evictable sessions remain in it.
    fn sweep_progress(&self, now: Timestamp) -> StoreFuture<'_, usize>;
}

/// Every upload-session store sweeps through its own pressure listing.
///
/// [`UploadSessionStore::least_recently_progressed`] heals each member it walks over, and it
/// walks until it has found `limit` live candidates — so an unbounded limit at a horizon of `now`
/// visits every member that has progressed at all, which is the whole view. The candidates it
/// answers are counted and dropped: this is housekeeping, not eviction.
impl ProgressSweep for Arc<dyn UploadSessionStore> {
    fn sweep_progress(&self, now: Timestamp) -> StoreFuture<'_, usize> {
        Box::pin(async move { Ok(self.least_recently_progressed(now, usize::MAX).await?.len()) })
    }
}

/// The collector's collaborators.
#[derive(Debug, Clone)]
pub struct CollectionContext {
    index: Arc<dyn AssetIndex>,
    blobs: Arc<dyn BlobStore>,
    marks: Arc<dyn CollectionStore>,
    quotas: Arc<dyn crate::quota::QuotaStore>,
    /// The share links, whose live serve sets hold blobs no asset row names (#653).
    shares: Arc<dyn ShareStore>,
    clock: Arc<dyn crate::store::Clock>,
    grace_window: SignedDuration,
    /// Set by [`Self::with_progress_sweep`]; `None` runs no sweep.
    progress: Option<Arc<dyn ProgressSweep>>,
    /// Set by [`Self::with_attestation`]; `None` refuses an applied purge.
    attestation: Option<AttestationContext>,
}

impl CollectionContext {
    /// Assembles the collector.
    pub fn new(
        index: Arc<dyn AssetIndex>,
        blobs: Arc<dyn BlobStore>,
        marks: Arc<dyn CollectionStore>,
        quotas: Arc<dyn crate::quota::QuotaStore>,
        shares: Arc<dyn ShareStore>,
        clock: Arc<dyn crate::store::Clock>,
        grace_window: SignedDuration,
    ) -> Self {
        Self {
            index,
            blobs,
            marks,
            quotas,
            shares,
            clock,
            grace_window,
            progress: None,
            attestation: None,
        }
    }

    /// The same collector, ledgering every purge it applies through `attestation` (#767).
    ///
    /// A builder rather than a constructor argument, because only an applied purge signs
    /// anything: `gc` and a dry-run purge run on a host that holds no attestation key, and
    /// requiring one for them would be a reason to keep the key on a maintenance host.
    #[must_use]
    pub fn with_attestation(mut self, attestation: AttestationContext) -> Self {
        self.attestation = Some(attestation);
        self
    }

    /// The same collector, ending every applied pass with `sweep` over the upload-progress view.
    ///
    /// A builder rather than a constructor argument, because the sweep is housekeeping the pass
    /// carries for another store, not a collaborator the collection itself depends on.
    #[must_use]
    pub fn with_progress_sweep(mut self, sweep: Arc<dyn ProgressSweep>) -> Self {
        self.progress = Some(sweep);
        self
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
/// ever removed a blob that was confirmed unreferenced twice. The upload-progress sweep runs
/// after every blob is decided, so a session store that cannot answer fails the pass without
/// having stopped the collection short.
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

    sweep_progress(context, now, mode).await?;

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

/// Run the upload-progress sweep, if this collector carries one and the pass applies.
async fn sweep_progress(
    context: &CollectionContext,
    now: Timestamp,
    mode: Mode,
) -> Result<(), StoreError> {
    let Some(progress) = &context.progress else {
        return Ok(());
    };
    if !mode.applies() {
        tracing::debug!("a dry run leaves the upload-progress view unswept");
        return Ok(());
    }
    let evictable = progress.sweep_progress(now).await?;
    tracing::info!(evictable, "swept the upload-progress view");
    Ok(())
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
    // A live share link is a reference too (#653): its sealed metadata record has no asset row,
    // and a blob a link serves must outlive the link's ability to serve it. Asked only when no
    // asset holds the blob, which is the rare case, so the ordinary blob costs one query.
    let shared = references == 0 && context.shares.names_live(address, now).await?;
    let marked_since = context.marks.marked_since(address).await?;

    if references > 0 || shared {
        // Live. If it was marked, a reference reappeared during the window and the mark goes.
        if marked_since.is_some() {
            tracing::info!(
                %address,
                references,
                shared,
                "a mark was cancelled: a reference reappeared"
            );
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
        // The credit belongs **here**, to the sweep, and after the removal.
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
/// Each purge is appended to the owner's ledger before the index applies it; see the module
/// docs.
///
/// # Errors
///
/// Propagates the first store failure, and refuses an applied pass on a collector given no
/// [attestation context](CollectionContext::with_attestation) before it lists anything. A
/// ledger append that fails is not an error: that asset is skipped and reported as
/// [`PurgeReport::unrecorded`].
#[tracing::instrument(skip(context), fields(mode = ?mode))]
pub async fn purge_expired(
    context: &CollectionContext,
    mode: Mode,
    limit: usize,
) -> Result<PurgeReport, StoreError> {
    let attestation = match (&context.attestation, mode.applies()) {
        (Some(attestation), _) => Some(attestation),
        (None, false) => None,
        (None, true) => {
            tracing::error!(
                "an applied retention purge has no attestation key to ledger its purges with"
            );
            return Err(StoreError::Unavailable {
                store: "ledger",
                detail: "an applied retention purge is signed into the owner ledger, and this \
                         host holds no attestation key (ATTESTATION_KEY_SEED)"
                    .to_owned(),
            });
        }
    };
    let now = context.clock.now();
    let mut report = PurgeReport::default();

    for row in context.index.tombstoned(limit).await? {
        // The same rule the index re-checks under the row lock. A tombstone with no signed
        // retention is never purged: absent is not "immediately", and reading it that way would
        // purge exactly the assets whose delete manifest the server failed to project a field
        // out of.
        if let Some(refusal) = purge_refusal(&row, now) {
            tracing::debug!(
                asset = %row.asset_id,
                retention_until = ?row.retention_until,
                ?refusal,
                "a listed row is not purgeable yet"
            );
            if refusal == PurgeOutcome::Retained {
                report.retained.push(row.asset_id);
            } else {
                report.skipped.push(row.asset_id);
            }
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
        if !mode.applies() {
            tracing::info!(
                asset = %row.asset_id,
                retention_until = ?row.retention_until,
                blobs = row.blobs.len(),
                "a dry run would purge a tombstoned asset's blob references"
            );
            report.purged.push(row.asset_id);
            continue;
        }
        // `None` is refused above for an applied pass; were it reached, it reads as an append
        // that failed, which keeps the bytes.
        let recorded = match attestation {
            Some(attestation) => ledger_purge(attestation, &row, None, now).await,
            None => None,
        };
        let Some(purge_seq) = recorded else {
            report.unrecorded.push(row.asset_id);
            continue;
        };
        // The listing is a snapshot; the index re-checks state and retention under the row
        // lock, because a restore that committed since the listing must keep its bytes. So the
        // purge is logged once the re-check has answered, not before: a line saying "purging"
        // for an asset the re-check then kept would be a false record of a deletion.
        match context.index.purge(&row.asset_id, now).await? {
            PurgeOutcome::Purged(_) => {
                tracing::info!(
                    asset = %row.asset_id,
                    retention_until = ?row.retention_until,
                    blobs = row.blobs.len(),
                    "purged a tombstoned asset's blob references"
                );
                // The confirmation rebuild applies the purge on (#811). Losing it is logged
                // and not counted against the pass: the purge stands, and a rebuild that never
                // sees it keeps references rather than dropping them. It names the purge entry
                // it confirms, so a concurrent pass's refused purge of the same asset is never
                // the one confirmed.
                if let Some(attestation) = attestation {
                    ledger_purge(attestation, &row, Some(purge_seq), now).await;
                }
                report.purged.push(row.asset_id);
            }
            PurgeOutcome::Retained => {
                tracing::info!(asset = %row.asset_id, "a tombstone was re-deleted under a later retention floor since it was listed");
                report.retained.push(row.asset_id);
            }
            PurgeOutcome::NotTombstoned => {
                tracing::info!(asset = %row.asset_id, "an asset was restored after it was listed for purge; its bytes are kept");
                report.skipped.push(row.asset_id);
            }
            PurgeOutcome::Unknown => {
                tracing::warn!(asset = %row.asset_id, "a listed tombstone has no row by the time it is purged");
                report.skipped.push(row.asset_id);
            }
        }
    }

    tracing::info!(
        purged = report.purged.len(),
        retained = report.retained.len(),
        skipped = report.skipped.len(),
        unrecorded = report.unrecorded.len(),
        "a retention purge pass finished"
    );
    Ok(report)
}

/// Append one of `row`'s purge entries to its owner's ledger and return its ledger position, or
/// `None` when the ledger could not take it. With `confirms: None` it is the [`PURGE_ACTION`]
/// entry, appended before the index is touched; a failure leaves the asset unpurged for this
/// pass. With `confirms: Some(seq)` it is the [`PURGED_ACTION`] entry confirming the purge
/// entry at `seq`, appended once the index answered that the purge committed.
///
/// Neither carries a manifest hash: the purge is authorised by the retention floor the delete
/// manifest already signed, and it mints no sequence number. The confirmation carries the
/// purge entry's ledger position in `sync_seq` (see [`crate::ledger::rebuild::PURGED`]). If the
/// index answers that the row is no longer purgeable, no confirmation follows, and rebuild
/// keeps the asset's references.
async fn ledger_purge(
    attestation: &AttestationContext,
    row: &AssetRow,
    confirms: Option<u64>,
    now: Timestamp,
) -> Option<u64> {
    let action = if confirms.is_some() {
        PURGED_ACTION
    } else {
        PURGE_ACTION
    };
    let event = LedgerEvent::Lifecycle {
        asset_id: row.asset_id.as_str().to_owned(),
        action: action.to_owned(),
        manifest_hash: None,
        sync_seq: confirms,
    };
    match attestation
        .append_to_ledger(&row.owner_id, event, now)
        .await
    {
        Ok(entry) => {
            tracing::debug!(
                asset = %row.asset_id,
                owner = %row.owner_id,
                action,
                ledger_seq = entry.core.seq,
                confirms,
                "ledgered a retention purge entry"
            );
            Some(entry.core.seq)
        }
        Err(error) if confirms.is_none() => {
            tracing::error!(
                %error,
                asset = %row.asset_id,
                owner = %row.owner_id,
                "the owner ledger could not record a retention purge; the asset keeps its \
                 references until a later pass"
            );
            None
        }
        Err(error) => {
            tracing::error!(
                %error,
                asset = %row.asset_id,
                owner = %row.owner_id,
                confirms,
                "the owner ledger could not confirm an applied retention purge; a rebuild will \
                 keep the asset's references and report any blob GC sweeps as dangling"
            );
            None
        }
    }
}

pub mod conformance;
pub mod memory;
pub mod postgres;

pub use self::postgres::PostgresCollection;

#[cfg(test)]
mod tests;
