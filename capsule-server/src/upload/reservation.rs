//! Giving a session's quota reservation back when it ends without its bytes.
//!
//! # One rule for every terminal path
//!
//! Quota is reserved for the declared size at session creation ([`crate::quota::charge_upload`])
//! and released when the session is deleted, discarded, expired or ends in `FailedProcessing`
//! (transfer upload design doc, Quota and Permissions). Today only cancellation and
//! `FailedProcessing` call [`release`]; the discard and expiry paths do not release yet. Every
//! path that does release goes through [`release`], so the question "may this attribution
//! go?" has one answer however the session ended.
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

use crate::blob::{BlobError, ContentAddress};
use crate::store::{StoreError, UploadSessionRecord};
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
    let Some((other, holder)) = live_session_for_address(context, record).await? else {
        return Ok(Released::Released);
    };
    let restored = context
        .quotas()
        .charge(
            &holder.upload_user_id,
            &address,
            holder.total_size,
            context.clock().now(),
            context.quota_limits(),
        )
        .await?;
    tracing::info!(
        %other,
        uploader = %holder.upload_user_id,
        size = holder.total_size,
        restored = matches!(restored, crate::quota::ChargeOutcome::Charged { .. }),
        "a live session declares the released address; its reservation was put back"
    );
    Ok(Released::Restored)
}

/// The first live session of **any** uploader, other than `record`'s own, that declares the
/// same bytes and can still be read — with its record, so its uploader can be charged.
///
/// Unscoped, unlike [`other_live_session`], because attribution is global: the session left
/// owing for the bytes may be another account's. One charge is enough, since every other
/// session for the address is then `AlreadyAttributed` against that one's charge.
async fn live_session_for_address(
    context: &UploadContext,
    record: &UploadSessionRecord,
) -> Result<Option<(crate::store::UploadId, UploadSessionRecord)>, StoreError> {
    let live = context
        .sessions()
        .live_for_address(&record.expected_hash)
        .await?;
    for other in live.into_iter().filter(|other| *other != record.upload_id) {
        match context.sessions().read(&other).await? {
            Some(holder) if holder.status.is_active() => return Ok(Some((other, holder))),
            _ => tracing::debug!(%other, "a live session for the address has since ended"),
        }
    }
    Ok(None)
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

#[cfg(test)]
mod tests;
