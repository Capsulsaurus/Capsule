//! Giving a session's quota reservation back when it ends without its bytes.
//!
//! # One rule for every terminal path
//!
//! Quota is reserved for the declared size at session creation ([`crate::quota::reserve_upload`])
//! and released when the session is deleted, discarded, expired or ends in `FailedProcessing`
//! (transfer upload design doc, Quota and Permissions). Cancellation, `FailedProcessing` and a
//! pressure discard ([`discard_under_pressure`]) hold the ended session's record and call
//! [`release`], so the question "may this attribution go?" has one answer however the session
//! ended. Expiry holds no record — the session store drops it at the lifetime cap and nothing
//! observes that — so it is the reservation sweep's ([`ReservationSweep`]), below.
//!
//! # Settled at `Completed`, swept past its deadline
//!
//! The reservation a create writes carries a deadline one lifetime cap away
//! ([`crate::quota::reservation_deadline`]). A finalization that commits the bytes settles it
//! ([`settle`]), and a settled attribution is never swept. A reservation still unsettled past
//! its deadline belongs to a session that expired, unless something still holds it, and the
//! sweep asks the same holders [`release`] asks, in an order that keeps a committed blob charged
//! however the steps interleave:
//!
//! 1. **A committed blob at the address** settles the reservation instead: the bytes arrived,
//!    and the finalizer that committed them died before settling.
//! 2. **A live session declaring the address**, of any account, extends the deadline: it was
//!    admitted against this reservation and may still store the bytes.
//! 3. Otherwise the reservation is released, **conditionally** on still being an expired
//!    reservation ([`crate::quota::QuotaStore::release_expired`]): a settle or an extension
//!    that lands first keeps it, and a second sweep releases nothing.
//! 4. After the release both holders are asked again, and either one puts the reservation back
//!    on the same account — settled for a blob, reserved for a session.
//!
//! The finalizer's side writes its own fact first and reads the other's after: it commits the
//! blob, then settles, and a settle that finds the address unattributed charges the bytes
//! itself. So a sweep that released between a finalizer's commit and its settle is answered by
//! that settle, and one that released before the commit is answered by step 4 or by the settle.
//!
//! # Why a release can be refused
//!
//! Attribution is keyed by content address, globally, and a create whose address is already
//! attributed debits nothing ([`crate::quota::UploadCharge::AlreadyAttributed`]). A session
//! ending therefore does not mean the attribution it found is its own to give back. Two holders
//! are checked before the ledger is touched, and either keeps the attribution:
//!
//! - **The bytes are stored.** A committed blob at the address is what the attribution pays
//!   for, whichever session put it there; the collector releases it when the blob goes
//!   ([`crate::quota::QuotaStore::release_attribution`]). Releasing here would hand the
//!   uploader free storage for every blob they ever started a second session for.
//! - **Another live session of the same uploader declares it.** Two sessions for one hash —
//!   the same bytes filed into two albums — share one reservation, and the one still
//!   transferring needs it.
//!
//! The ledger's own [`crate::quota::QuotaStore::release`] then refuses an attribution that is
//! another account's, so a session never gives back somebody else's charge.
//!
//! # Another account's session for the same bytes
//!
//! A live session of a *different* account does not keep the attribution: it was admitted
//! `AlreadyAttributed` against this uploader's charge, and keeping the uploader charged for it
//! would bill them for bytes they abandoned. The attribution goes, and once it has, [`release`]
//! asks for a live session of **any** uploader declaring the address and charges the first one
//! it finds in its place
//! ([`crate::store::UploadSessionStore::live_for_address`]). Without that step such a session,
//! whose own post-open re-charge passed long ago, would store its bytes charged to nobody.
//!
//! That charge is an upload's charge ([`crate::quota::reserve_upload`]), with its hard-limit and
//! suspension checks, so a dedup never pushes an account over its limit. A session whose
//! uploader is refused is discarded with its stage, the way a refused post-open re-charge
//! undoes a session, and the next live session for the address is tried. Its client's next
//! request finds no session, and re-creating it is refused with the same code. Discarding
//! leaves the session state machine as it is: a session is claimed for finalization only once
//! every byte has arrived. The one exception is a session whose finalization is already
//! claimed: every byte is on the server and ending it would race its finalizer, so it is
//! charged past the check, as [`crate::quota`] records.
//!
//! # Racing a create for the same bytes
//!
//! The live-session check is not atomic with the ledger's release, so a create that charged
//! `AlreadyAttributed` could open its session after the check and before the release, and
//! store its bytes against an attribution that is gone. Both sides write their own fact first
//! and read the other's after: session creation opens the session and then charges the
//! address again, and [`release`] releases and then asks for a live session of any uploader
//! again, putting the reservation back for one it finds. Whichever order the four steps run
//! in, one of the two second reads sees the other's write, so a session that stores bytes is
//! charged for them.

use std::sync::Arc;

use capsule_i18n::error_codes;
use jiff::Timestamp;

use crate::blob::{BlobError, BlobStore, ContentAddress};
use crate::drop::DropStore;
use crate::quota::{ExpiredReservation, QuotaLimits, QuotaStore, reservation_deadline};
use crate::store::{
    StoreError, StoreFuture, UnclaimedDiscard, UploadId, UploadSessionRecord, UploadSessionStatus,
    UploadSessionStore,
};
use crate::upload::UploadContext;

/// What giving a session's reservation back did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Released {
    /// The uploader's attribution for the address went back.
    Released,
    /// Nothing was released: the address was not attributed to the uploader.
    NotHeld,
    /// Nothing was released: a committed blob holds the address.
    BytesStored,
    /// Nothing was released: another live session of the uploader declares the address.
    StillReserved,
    /// The attribution went back, and a live session for the address — of this uploader or of
    /// another account, admitted against the attribution that went — was charged in its place.
    Restored,
    /// Nothing was released: the session's declared hash is not a content address.
    ///
    /// Unreachable for a session creation admitted, which parsed the hash before charging it.
    Unaddressable,
}

/// Why a release could not be decided.
#[derive(Debug, thiserror::Error)]
pub enum ReleaseError {
    /// The session store or the quota ledger could not answer.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The blob store could not answer whether the bytes are stored.
    #[error(transparent)]
    Blob(#[from] BlobError),
}

/// Give `record`'s reservation back, unless something still holds it.
///
/// Called after the session has left the live set — discarded, or terminal — so the session
/// itself never counts as the "other live session" that keeps the reservation.
///
/// # Errors
///
/// [`ReleaseError`] when a collaborator could not answer; nothing was released. Every caller
/// treats it as best-effort, because the session has already ended and failing its response
/// would tell a client the end did not happen. A release that fails leaves the uploader
/// charged for bytes they do not hold — an over-charge an operator can see in the log line,
/// never free storage.
#[tracing::instrument(level = "debug", skip_all, fields(upload_id = %record.upload_id))]
pub async fn release(
    context: &UploadContext,
    record: &UploadSessionRecord,
) -> Result<Released, ReleaseError> {
    let Ok(address) = ContentAddress::parse(&record.expected_hash) else {
        tracing::warn!("a session's declared hash is not a content address; nothing to release");
        return Ok(Released::Unaddressable);
    };
    if context.blobs().stat(&address).await?.is_some() {
        tracing::debug!("the bytes are stored, so the reservation stays with them");
        return Ok(Released::BytesStored);
    }
    if let Some(other) = other_live_session(context, record).await? {
        tracing::debug!(%other, "another live session of the uploader still holds the reservation");
        return Ok(Released::StillReserved);
    }
    let released = context
        .quotas()
        .release(&record.upload_user_id, &address)
        .await?;
    if !released {
        tracing::debug!("the address was not attributed to the uploader");
        return Ok(Released::NotHeld);
    }
    tracing::info!(
        uploader = %record.upload_user_id,
        size = record.total_size,
        status = record.status.as_str(),
        "released an ended session's quota reservation"
    );
    // A session of any account open by this point was admitted against the attribution just
    // released — charged `AlreadyAttributed`, whether it opened long before this session ended
    // or between the check above and the release — and its own post-open re-charge may already
    // have passed, so nothing else will charge it for the bytes it stores. The reservation is
    // put back on its uploader's behalf. A session opening after this point re-charges for
    // itself and finds nothing attributed.
    let live = context
        .sessions()
        .live_for_address(&record.expected_hash)
        .await?;
    for other in live.into_iter().filter(|other| *other != record.upload_id) {
        match context.sessions().read(&other).await? {
            Some(holder) if holder.status.is_active() => {
                if recharge(context, &address, &holder).await? {
                    return Ok(Released::Restored);
                }
            }
            _ => tracing::debug!(%other, "a live session for the address has since ended"),
        }
    }
    Ok(Released::Released)
}

/// Charge `holder`, a live session left owing for the released address, in the released
/// attribution's place. `true` when the address is charged again — to `holder`'s uploader, or
/// already to somebody else — so no other session needs charging.
///
/// The charge is the upload's own ([`crate::quota::reserve_upload`]), so the hard limit and a
/// suspension refuse it exactly as they would have refused the session at creation. A session
/// whose uploader is refused may not go on to store bytes it is not charged for, and is undone
/// the way a refused post-open re-charge undoes it: the session discarded and its stage
/// dropped. Its client's next request is the uniform `404`, and the re-create it answers with
/// is refused with the code this charge was. The discard is a compare-and-set
/// ([`crate::store::UploadSessionStore::discard_unclaimed`]), so a session whose finalization
/// is already under way is never discarded out from under its finalizer; see
/// [`charge_finalizing`]. The next live session for the address is then tried, and `false`
/// says so.
async fn recharge(
    context: &UploadContext,
    address: &ContentAddress,
    holder: &UploadSessionRecord,
) -> Result<bool, StoreError> {
    let other = &holder.upload_id;
    let code = match crate::quota::reserve_upload(
        context.quota(),
        &holder.upload_user_id,
        address,
        holder.total_size,
    )
    .await?
    {
        crate::quota::UploadCharge::Admitted | crate::quota::UploadCharge::AlreadyAttributed => {
            tracing::info!(
                %other,
                uploader = %holder.upload_user_id,
                size = holder.total_size,
                "a live session declares the released address; its reservation was put back"
            );
            return Ok(true);
        }
        crate::quota::UploadCharge::Refused => error_codes::QUOTA_EXCEEDED,
        crate::quota::UploadCharge::Suspended => error_codes::QUOTA_SUSPENDED,
    };
    match context.sessions().discard_unclaimed(other).await? {
        UnclaimedDiscard::Discarded(_) => {
            // The record first, so a finalizer can no longer claim the session; a stage left by
            // a failed drop is an orphan the startup scrub deletes.
            if let Err(error) = context.blobs().abandon(other).await {
                tracing::error!(%error, %other, "a refused session's stage could not be dropped");
            }
            tracing::info!(
                %other,
                uploader = %holder.upload_user_id,
                size = holder.total_size,
                refusal = code,
                "a live session for the released address could not be charged; discarded it"
            );
            Ok(false)
        }
        UnclaimedDiscard::Claimed => match context.sessions().read(other).await? {
            Some(current)
                if matches!(
                    current.status,
                    UploadSessionStatus::WaitingForProcessing | UploadSessionStatus::Completed
                ) =>
            {
                charge_finalizing(context, address, &current, code).await
            }
            _ => {
                tracing::debug!(%other, "a refused session for the address has since failed");
                Ok(false)
            }
        },
        UnclaimedDiscard::NotFound => {
            tracing::debug!(%other, "a refused session for the address has since gone");
            Ok(false)
        }
    }
}

/// Charge `holder`, whose finalization is claimed or has completed, past the check that
/// refused it.
///
/// Every byte is on the server and a finalizer is committing them (or has), so refusing now
/// costs storage rather than saving it — the reason session creation is quota's one
/// enforcement point ([`crate::quota`], "One hard enforcement point") — and ending the session
/// would race the finalizer's own terminal write. The bytes are charged to their uploader
/// through the ledger directly, which may carry the account over its hard limit: the
/// documented exception in [`crate::quota`], "How an account reaches `HardExceeded`". Should
/// the finalization then fail, its own [`release`] gives the charge back.
async fn charge_finalizing(
    context: &UploadContext,
    address: &ContentAddress,
    holder: &UploadSessionRecord,
    refusal: &str,
) -> Result<bool, StoreError> {
    let outcome = context
        .quotas()
        .charge(
            &holder.upload_user_id,
            address,
            holder.total_size,
            context.clock().now(),
            context.quota_limits(),
        )
        .await?;
    tracing::warn!(
        other = %holder.upload_id,
        uploader = %holder.upload_user_id,
        size = holder.total_size,
        refusal,
        charged = matches!(outcome, crate::quota::ChargeOutcome::Charged { .. }),
        "a finalizing session for the released address was charged past its quota check"
    );
    Ok(true)
}

/// Another live session of `record`'s uploader that declares the same bytes, if one exists.
async fn other_live_session(
    context: &UploadContext,
    record: &UploadSessionRecord,
) -> Result<Option<crate::store::UploadId>, StoreError> {
    Ok(context
        .sessions()
        .pending_for_address(&record.upload_user_id, &record.expected_hash)
        .await?
        .filter(|other| *other != record.upload_id))
}

/// [`release`], logging rather than returning a failure — the form every terminal path uses.
pub async fn release_logged(context: &UploadContext, record: &UploadSessionRecord) {
    if let Err(error) = release(context, record).await {
        tracing::error!(
            %error,
            upload_id = %record.upload_id,
            "an ended session's quota reservation was not released"
        );
    }
}

/// Give a discarded guest drop's link back the reservation its admission spent, logging rather
/// than returning a failure.
///
/// A drop's session holds two reservations: the link owner's quota, which [`release`] gives
/// back, and the link's caps, which the drop store charged at admission and holds as a
/// [`PendingDeposit`](crate::drop::PendingDeposit) until the bytes are filed. A session
/// discarded without its bytes must give back both, as a refused deposit does: otherwise the
/// link's caps stay spent on a drop that never arrived, and the held declaration is never
/// taken ([`DropStore::refund`]).
///
/// Nothing is asked of the drop store for an album session. A drop session whose declaration
/// is already gone refunds nothing, so a second discard of the same session cannot refund the
/// link twice.
pub async fn refund_drop_logged(drops: &dyn DropStore, record: &UploadSessionRecord) {
    if !record.is_guest_drop() {
        return;
    }
    let upload = &record.upload_id;
    match drops.take_reservation(upload).await {
        Ok(Some(pending)) => match drops.refund(&pending.opaque_id, pending.size).await {
            Ok(()) => tracing::info!(
                upload_id = %upload,
                size = pending.size,
                "gave a discarded drop's link its reservation back"
            ),
            Err(error) => tracing::error!(
                %error,
                upload_id = %upload,
                "a discarded drop's link reservation could not be refunded"
            ),
        },
        Ok(None) => {
            tracing::debug!(upload_id = %upload, "a discarded drop held no declaration to give back");
        }
        Err(error) => tracing::error!(
            %error,
            upload_id = %upload,
            "a discarded drop's declaration could not be released"
        ),
    }
}

/// Settle `record`'s reservation: its finalization committed the bytes.
///
/// Called after the blob commit and before the session is marked `Completed`. An address the
/// ledger no longer attributes was released by the reservation sweep between this
/// finalization's commit and now, so the committed bytes are charged to the uploader here,
/// settled, through the ledger directly: they are on the server, and refusing them now would
/// cost storage rather than save it, the reason a finalizing session is charged past the check
/// when [`release`] restores a reservation.
///
/// A settle the ledger cannot answer is logged, and the session completes: the reservation
/// it could not settle is still an attribution the sweep lists, and the sweep finds the
/// committed blob and settles it then ([`ReservationSweep`]). Idempotent: a settled
/// attribution settles again as `true`, and a charge of an attributed address debits nothing.
///
/// # Errors
///
/// [`StoreError`] only when the settle found the reservation already swept and the ledger
/// could not charge the committed bytes. Nothing attributes the address then, and no sweep
/// lists it again, so the bytes would be charged to nobody: the failure is the finalizer's,
/// not a log line. It leaves the session claimed rather than `Completed`, and the claim is
/// re-driven through this settle until the charge lands ([`super::finalize::recover`]).
pub async fn settle(
    context: &UploadContext,
    record: &UploadSessionRecord,
) -> Result<(), StoreError> {
    let upload_id = &record.upload_id;
    let Ok(address) = ContentAddress::parse(&record.expected_hash) else {
        tracing::warn!(%upload_id, "a completed session's hash is not a content address; nothing to settle");
        return Ok(());
    };
    match context.quotas().settle(&address).await {
        Ok(true) => {
            tracing::debug!(%upload_id, "settled a completed session's reservation");
            return Ok(());
        }
        Ok(false) => {}
        Err(error) => {
            tracing::error!(
                %error,
                %upload_id,
                "a completed session's reservation was not settled; the sweep settles it on \
                 finding the blob"
            );
            return Ok(());
        }
    }
    let outcome = context
        .quotas()
        .charge(
            &record.upload_user_id,
            &address,
            record.total_size,
            context.clock().now(),
            context.quota_limits(),
        )
        .await?;
    tracing::warn!(
        %upload_id,
        uploader = %record.upload_user_id,
        size = record.total_size,
        charged = matches!(outcome, crate::quota::ChargeOutcome::Charged { .. }),
        "a completed session's reservation had been swept; charged its committed bytes"
    );
    Ok(())
}

/// Discard `upload` under storage pressure, and give its reservation back.
///
/// The hook a pressure-discard worker calls for each candidate
/// [`UploadSessionStore::least_recently_progressed`] answers: the discard is the
/// compare-and-set [`UploadSessionStore::discard_unclaimed`], so a session whose finalization
/// was claimed in the meantime is left to its finalizer, and the release is [`release`]'s, so
/// the reservation stays where the bytes are stored or another live session of the uploader
/// still declares them. The stage is dropped after the record, as a refused session's is. A
/// guest drop's link gets its caps back through `drops` ([`refund_drop_logged`]).
///
/// `None` when nothing was discarded — the session is gone, claimed or terminal.
///
/// # Errors
///
/// [`ReleaseError`] when the session store could not discard, or a collaborator could not
/// decide the release. A session discarded before the release failed stays discarded, and a
/// drop's link is refunded before the release is attempted.
#[tracing::instrument(level = "debug", skip(context, drops))]
pub async fn discard_under_pressure(
    context: &UploadContext,
    drops: &dyn DropStore,
    upload: &UploadId,
) -> Result<Option<Released>, ReleaseError> {
    let record = match context.sessions().discard_unclaimed(upload).await? {
        UnclaimedDiscard::Discarded(record) => record,
        UnclaimedDiscard::Claimed => {
            tracing::debug!("a pressure candidate was claimed for finalization; left it");
            return Ok(None);
        }
        UnclaimedDiscard::NotFound => {
            tracing::debug!("a pressure candidate has already gone");
            return Ok(None);
        }
    };
    if let Err(error) = context.blobs().abandon(upload).await {
        tracing::error!(%error, "a pressure-discarded session's stage could not be dropped");
    }
    refund_drop_logged(drops, &record).await;
    let released = release(context, &record).await?;
    tracing::info!(
        uploader = %record.upload_user_id,
        size = record.total_size,
        received = record.committed.covered(),
        released = ?released,
        "discarded an upload session under storage pressure"
    );
    Ok(Some(released))
}

/// How many expired reservations one listing reads.
pub const SWEEP_BATCH: usize = 256;

/// What the reservation sweep did with one expired reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Swept {
    /// Released: no committed blob and no live session held it.
    Released,
    /// Settled: a committed blob holds it.
    Settled,
    /// Extended: a live session still declares the address.
    Extended,
    /// Released, then put back on the same account: a blob or a session appeared between the
    /// checks and the release.
    Restored,
    /// Left: a settle, an extension or another release reached the row first.
    Raced,
}

/// What one reservation sweep pass did, address by address.
///
/// Lists rather than counts, as [`crate::gc::CollectionReport`] keeps them, so the log names
/// the reservations an operator would look at.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Reservations released.
    pub released: Vec<ContentAddress>,
    /// Reservations settled for a committed blob.
    pub settled: Vec<ContentAddress>,
    /// Reservations extended for a live session.
    pub extended: Vec<ContentAddress>,
    /// Reservations released and put back.
    pub restored: Vec<ContentAddress>,
    /// Reservations another writer reached first.
    pub raced: Vec<ContentAddress>,
    /// Reservations a collaborator could not decide; left for a later pass.
    pub failed: Vec<ContentAddress>,
}

impl SweepReport {
    fn record(&mut self, address: ContentAddress, swept: Swept) {
        match swept {
            Swept::Released => self.released.push(address),
            Swept::Settled => self.settled.push(address),
            Swept::Extended => self.extended.push(address),
            Swept::Restored => self.restored.push(address),
            Swept::Raced => self.raced.push(address),
        }
    }
}

/// The bounded sweep that releases the reservations of sessions that expired.
///
/// Reads only reservations past their deadline ([`QuotaStore::expired_reservations`]), so a
/// pass costs the number of expired reservations rather than the number of stored blobs. Each
/// one it handles leaves the expired set — released, settled, or extended one lifetime cap — so
/// the pass reads batch after batch until a batch comes back short, and stops early when a
/// collaborator fails rather than reading the same reservation again.
#[derive(Debug, Clone)]
pub struct ReservationSweep {
    quotas: Arc<dyn QuotaStore>,
    blobs: Arc<dyn BlobStore>,
    sessions: Arc<dyn UploadSessionStore>,
    limits: QuotaLimits,
    batch: usize,
}

impl ReservationSweep {
    /// A sweep over the ledger, the blob store and the session store, re-charging under
    /// `limits` when it puts a reservation back.
    pub fn new(
        quotas: Arc<dyn QuotaStore>,
        blobs: Arc<dyn BlobStore>,
        sessions: Arc<dyn UploadSessionStore>,
        limits: QuotaLimits,
    ) -> Self {
        Self {
            quotas,
            blobs,
            sessions,
            limits,
            batch: SWEEP_BATCH,
        }
    }

    /// The same sweep, reading `batch` reservations per listing (at least one).
    pub fn with_batch(mut self, batch: usize) -> Self {
        self.batch = batch.max(1);
        self
    }

    /// Sweep every reservation whose deadline is at or before `now`.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the ledger cannot list expired reservations. A reservation a
    /// collaborator cannot decide is reported in [`SweepReport::failed`] and left as it is.
    #[tracing::instrument(level = "debug", skip(self))]
    pub async fn sweep(&self, now: Timestamp) -> Result<SweepReport, StoreError> {
        let mut report = SweepReport::default();
        loop {
            let expired = self.quotas.expired_reservations(now, self.batch).await?;
            let full = expired.len() == self.batch;
            let mut stalled = false;
            for reservation in expired {
                match self.sweep_one(&reservation, now).await {
                    Ok(swept) => {
                        tracing::debug!(
                            user = %reservation.user,
                            address = %reservation.address,
                            size = reservation.size,
                            swept = ?swept,
                            "swept an expired reservation"
                        );
                        report.record(reservation.address, swept);
                    }
                    Err(error) => {
                        tracing::error!(
                            %error,
                            user = %reservation.user,
                            address = %reservation.address,
                            "an expired reservation could not be swept; left for a later pass"
                        );
                        stalled = true;
                        report.failed.push(reservation.address);
                    }
                }
            }
            if !full || stalled {
                break;
            }
        }
        tracing::info!(
            released = report.released.len(),
            settled = report.settled.len(),
            extended = report.extended.len(),
            restored = report.restored.len(),
            raced = report.raced.len(),
            failed = report.failed.len(),
            "swept the expired upload reservations"
        );
        Ok(report)
    }

    /// Steps 1–4 of the module docs, for one reservation.
    async fn sweep_one(
        &self,
        reservation: &ExpiredReservation,
        now: Timestamp,
    ) -> Result<Swept, ReleaseError> {
        let address = &reservation.address;
        if self.blobs.stat(address).await?.is_some() {
            self.quotas.settle(address).await?;
            return Ok(Swept::Settled);
        }
        if self.declared_live(address).await? {
            let extended = self
                .quotas
                .extend_reservation(address, reservation_deadline(now))
                .await?;
            return Ok(if extended {
                Swept::Extended
            } else {
                Swept::Raced
            });
        }
        if !self
            .quotas
            .release_expired(&reservation.user, address, now)
            .await?
        {
            return Ok(Swept::Raced);
        }
        // Released. A blob committed or a session opened since the checks above was admitted
        // against the reservation just released, so it goes back on the same account.
        if self.blobs.stat(address).await?.is_some() {
            self.quotas
                .charge(
                    &reservation.user,
                    address,
                    reservation.size,
                    now,
                    self.limits,
                )
                .await?;
            tracing::info!(user = %reservation.user, %address, "a blob landed as its reservation was swept; charged it back");
            return Ok(Swept::Restored);
        }
        if self.declared_live(address).await? {
            self.quotas
                .reserve(
                    &reservation.user,
                    address,
                    reservation.size,
                    now,
                    reservation_deadline(now),
                    self.limits,
                )
                .await?;
            tracing::info!(user = %reservation.user, %address, "a session opened as its reservation was swept; reserved it back");
            return Ok(Swept::Restored);
        }
        tracing::info!(
            user = %reservation.user,
            %address,
            size = reservation.size,
            "released an expired session's quota reservation"
        );
        Ok(Swept::Released)
    }

    /// Whether any live session, of any account, declares `address`.
    async fn declared_live(&self, address: &ContentAddress) -> Result<bool, StoreError> {
        Ok(!self
            .sessions
            .live_for_address(address.as_str())
            .await?
            .is_empty())
    }
}

/// An applied collection pass's housekeeping: the expired-state sweep when there is one, the
/// upload-progress count, then the reservation sweep.
///
/// The collector's one periodic hook into the upload sessions
/// ([`crate::gc::CollectionContext::with_progress_sweep`]) runs all three, because expiry is
/// what they clean up after and an applied `gc` pass is the server's periodic housekeeping. A
/// dry run runs none. An expiry or reservation sweep that fails is logged and does not fail the
/// collection pass: what either leaves is dead rows or an over-charge the next pass reclaims,
/// never lost bytes.
#[derive(Debug, Clone)]
pub struct UploadHousekeeping {
    sessions: Arc<dyn UploadSessionStore>,
    reservations: ReservationSweep,
    expiry: Option<Arc<crate::store::sweep::ExpirySweeper>>,
}

impl UploadHousekeeping {
    /// Housekeeping over `sessions`, sweeping reservations with `reservations`.
    pub fn new(sessions: Arc<dyn UploadSessionStore>, reservations: ReservationSweep) -> Self {
        Self {
            sessions,
            reservations,
            expiry: None,
        }
    }

    /// The same housekeeping, deleting expired state rows with `expiry` first when given one.
    #[must_use]
    pub fn with_expiry_sweep(
        mut self,
        expiry: Option<Arc<crate::store::sweep::ExpirySweeper>>,
    ) -> Self {
        self.expiry = expiry;
        self
    }
}

impl crate::gc::ProgressSweep for UploadHousekeeping {
    fn sweep_progress(&self, now: Timestamp) -> StoreFuture<'_, usize> {
        Box::pin(async move {
            if let Some(expiry) = &self.expiry
                && let Err(error) = expiry.sweep().await
            {
                tracing::error!(%error, "the expired state rows could not be swept");
            }
            let evictable = crate::gc::ProgressSweep::sweep_progress(&self.sessions, now).await?;
            if let Err(error) = self.reservations.sweep(now).await {
                tracing::error!(%error, "the expired upload reservations could not be swept");
            }
            Ok(evictable)
        })
    }
}

#[cfg(test)]
mod tests;
