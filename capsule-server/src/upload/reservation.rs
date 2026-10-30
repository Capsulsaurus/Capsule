//! Giving a session's quota reservation back when it ends without its bytes.
//!
//! # One rule for every terminal path
//!
//! Quota is reserved for the declared size at session creation ([`crate::quota::charge_upload`])
//! and released when the session is deleted, discarded, expired or ends in `FailedProcessing`
//! (transfer upload design doc, Quota and Permissions). Each of those paths calls
//! [`release`], so the question "may this attribution go?" has one answer however the session
//! ended.
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
    if let Some(other) = context
        .sessions()
        .pending_for_address(&record.upload_user_id, &record.expected_hash)
        .await?
        && other != record.upload_id
    {
        tracing::debug!(%other, "another live session of the uploader still holds the reservation");
        return Ok(Released::StillReserved);
    }
    let released = context
        .quotas()
        .release(&record.upload_user_id, &address)
        .await?;
    if released {
        tracing::info!(
            uploader = %record.upload_user_id,
            size = record.total_size,
            status = record.status.as_str(),
            "released an ended session's quota reservation"
        );
        Ok(Released::Released)
    } else {
        tracing::debug!("the address was not attributed to the uploader");
        Ok(Released::NotHeld)
    }
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
