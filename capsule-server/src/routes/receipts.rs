//! `GET /v1/upload/{id}/receipt` — the custody receipt for a finalized write (slice `S-C15`).
//!
//! The server's signed admission of what it accepted, served as the canonical CBOR the client
//! verifies and persists. [`crate::attestation`] owns the log, the chain and the reason
//! issuance happens where it does.
//!
//! # Why the bytes, and not a projection
//!
//! The signature covers the canonical encoding of the receipt's core, so anything that
//! re-serialises on the way out risks handing a client bytes that no longer verify — the same
//! discipline as `S-C30`'s manifest and `S-C9`'s directory. What the log holds is what the wire
//! carries.
//!
//! # `error.upload.receipt_not_available` is a state, not a failure
//!
//! A session that has not finalized has no receipt, and saying so is the honest answer rather
//! than a `404`: the upload exists, the caller may see it, and the receipt is coming. A client
//! waiting to release its local copy polls this and must be able to tell "not yet" from "never".
//! It is also what a crash between custody and issuance looks like — see
//! [`crate::attestation`]'s note on ordering — which is why it is retryable rather than final.
//!
//! # Who may read one
//!
//! The uploader or the owner, exactly as `HEAD /v1/upload/{id}` allows. A receipt names an
//! account, a device and a content address, so it is not something to hand to a caller who
//! merely knows a session id.

use capsule_i18n::error_codes;
use kynos::extract::body::binary::Binary;
use kynos::extract::media::MediaType;
use kynos::prelude::*;

use crate::attestation::AttestationContext;
use crate::auth::AccessToken;
use crate::routes::upload::{UploadPath, UploadTag};
use crate::store::{UploadId, UserId};
use crate::upload::UploadContext;

/// `application/cbor` — the encoding the receipt is *signed* in.
#[derive(Clone, Copy, Debug)]
pub struct Cbor;

impl MediaType for Cbor {
    const MEDIA_TYPE: &'static str = "application/cbor";
}

/// Why no receipt was returned.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum ReceiptRejection {
    /// No such session, or not one this caller may see.
    ///
    /// One answer for both, as the rest of the upload surface gives: a session id is opaque and
    /// a guess must not reveal whether it named something.
    #[error("no such upload session")]
    #[problem(status = 404, title = "Not found")]
    SessionNotFound {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The session exists and has not produced a receipt yet.
    #[error("this upload has no custody receipt yet")]
    #[problem(status = 409, title = "Receipt not available")]
    NotAvailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A collaborator could not answer.
    #[error("the custody receipt could not be read")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Fetch the custody receipt for a finalized upload.
#[kynos::get(
    "/v1/upload/{id}/receipt",
    operation_id = "get_upload_receipt",
    tag = UploadTag
)]
pub async fn get_upload_receipt(
    Inject(upload): Inject<UploadContext>,
    Inject(attestation): Inject<AttestationContext>,
    Auth(credential): Auth<AccessToken>,
    Path(path): Path<UploadPath>,
) -> Result<Binary<Cbor>, ReceiptRejection> {
    let id = UploadId::new(path.id);
    let record = upload
        .sessions()
        .read(&id)
        .await
        .map_err(|error| {
            tracing::error!(%error, upload_id = %id, "the session store could not answer");
            ReceiptRejection::Unavailable {
                code: error_codes::UPLOAD_UNAVAILABLE,
            }
        })?
        .ok_or_else(ReceiptRejection::session_not_found)?;

    // The uploader or the owner. A receipt names an account, a device and a content address.
    let caller = UserId::new(credential.user.as_str());
    if record.upload_user_id != caller && record.owner_id.as_str() != caller.as_str() {
        tracing::info!(upload_id = %id, "a receipt was refused: not this caller's session");
        return Err(ReceiptRejection::session_not_found());
    }

    let receipt = attestation
        .receipts()
        .for_upload(&id)
        .await
        .map_err(|error| {
            tracing::error!(%error, upload_id = %id, "the receipt log could not answer");
            ReceiptRejection::Unavailable {
                code: error_codes::UPLOAD_UNAVAILABLE,
            }
        })?
        .ok_or(ReceiptRejection::NotAvailable {
            code: error_codes::UPLOAD_RECEIPT_NOT_AVAILABLE,
        })?;

    // Verbatim: the signature covers these exact bytes.
    Ok(Binary::new(receipt.to_canonical_cbor()))
}

impl ReceiptRejection {
    /// No such session, or not this caller's.
    fn session_not_found() -> Self {
        Self::SessionNotFound {
            code: error_codes::UPLOAD_SESSION_NOT_FOUND,
        }
    }
}
