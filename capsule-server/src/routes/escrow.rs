//! The master-key escrow surface (slice `S-C12`).
//!
//! Two operations on one blob per account: store it, and fetch it back. There is no delete —
//! see below.
//!
//! [`crate::escrow`] owns the port, the single-active-escrow rule and the reason replacement is
//! one store operation. This module is the wire shape and its refusals.
//!
//! # Scoped to the caller, with no path parameter
//!
//! Unlike the device directory, an escrow is **not** public: the directory carries public keys
//! and exists to be read by strangers, while an escrow is a wrapped master key and the only
//! account entitled to it is its own. So there is no `{user_id}` segment to get wrong — the
//! account comes from the credential, which makes "fetching somebody else's escrow" not a
//! forbidden request but an unrepresentable one.
//!
//! # There is no delete, deliberately
//!
//! design/backup-recovery.md gives escrow three verbs — store, fetch, replace — and replace is
//! `PUT` over the same path. A standalone delete would be a way to remove an account's last
//! recovery path in one authenticated request, which is exactly the capability a stolen session
//! token should not have; rotation replaces, and account deletion takes the escrow with it.
//!
//! # `S-C28` audit
//!
//! | Status | Verdict |
//! | --- | --- |
//! | store `200` | the escrow is held; the body says whether it displaced one |
//! | store `400 error.escrow.malformed` | empty, or past the coarse ceiling — the only judgement the server is entitled to make |
//! | store `415` | the body is not `application/octet-stream`; [`crate::body`] replaces the framework's uncoded rejection |
//! | fetch `404 error.escrow.not_stored` | this account has escrowed nothing |
//! | `401` / `403` | the framework's, through `Auth` |
//! | `500 error.escrow.unavailable` | the store could not answer |
//!
//! No `409`. Storing an escrow over an existing one is the *documented* operation, not a
//! conflict, and answering `409` would make a client's guided re-wrap fail on the one path it
//! exists for.

use capsule_i18n::error_codes;
use kynos::extract::body::binary::Binary;
use kynos::extract::media::MediaType;
use kynos::prelude::*;
use serde::{Deserialize, Serialize};

use crate::auth::AccessToken;
use crate::body::{CodedMedia, OpaqueBody};
use crate::escrow::{EscrowContext, EscrowRecord, MalformedEscrow, Replaced, admissible};
use crate::store::UserId;

/// The recovery surface: the one blob a server holds that can reconstruct a library.
#[derive(Tag)]
#[tag(
    name = "escrow",
    description = "Storing and fetching the wrapped account master key. Opaque to the server."
)]
pub struct EscrowTag;

/// `application/octet-stream` — the escrow is ciphertext and has no schema the server knows.
///
/// Deliberately *not* a vendor type like the directory's `application/cbor`: that document has a
/// shape a generated client decodes, and this one does not. Claiming a structured media type
/// for bytes the server cannot parse would be a promise nobody can keep.
#[derive(Clone, Copy, Debug)]
pub struct OctetStream;

impl MediaType for OctetStream {
    const MEDIA_TYPE: &'static str = "application/octet-stream";
}

impl CodedMedia for OctetStream {
    const UNSUPPORTED_MEDIA_TYPE: &'static str = error_codes::ESCROW_MALFORMED;
    const UNREADABLE: &'static str = error_codes::ESCROW_MALFORMED;
}

/// The wrapped master key, as it arrives.
pub type EscrowBody = OpaqueBody<OctetStream>;

/// What storing an escrow did.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct StoreEscrowResponse {
    /// When the server accepted it, RFC 3339.
    ///
    /// Echoed so a client can tell whether a cached copy is current — the stale-cache rule,
    /// which exists because a rotation from another device would otherwise manufacture false
    /// verification failures on this one.
    pub stored_at: String,
    /// Whether this displaced an earlier escrow.
    ///
    /// A rotation and a first escrow are different events for a client: one completes account
    /// setup, and the other means the previous recovery secret has stopped working.
    pub replaced: bool,
}

/// Why an escrow was not stored.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum StoreEscrowRejection {
    /// The body cannot be an escrow at any version.
    #[error("{detail}")]
    #[problem(status = 400, title = "Malformed escrow")]
    Malformed {
        /// What was wrong, in English.
        detail: String,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The store could not answer.
    #[error("the escrow could not be stored")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Why an escrow was not returned.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum FetchEscrowRejection {
    /// This account has escrowed nothing.
    #[error("no escrow has been stored for this account")]
    #[problem(status = 404, title = "Not found")]
    NotStored {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The store could not answer.
    #[error("the escrow could not be read")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Store the caller's wrapped master key, replacing whatever they had.
///
/// `PUT`, because there is exactly one escrow per account and this is its address. Storing over
/// an existing escrow is the guided re-wrap, and it deletes the old blob in the same operation —
/// the lost recovery secret must stop working, which is the entire point of rotating.
#[kynos::put("/v1/auth/escrow", operation_id = "store_escrow", tag = EscrowTag)]
pub async fn store_escrow(
    Inject(escrow): Inject<EscrowContext>,
    Auth(credential): Auth<AccessToken>,
    body: EscrowBody,
) -> Result<Json<StoreEscrowResponse>, StoreEscrowRejection> {
    let user = UserId::new(credential.user.as_str());
    let blob = body.into_vec();

    admissible(&blob).map_err(|error| {
        tracing::info!(%user, %error, "an escrow was refused");
        StoreEscrowRejection::malformed(&error)
    })?;

    let stored_at = escrow.clock().now();
    let replaced = escrow
        .escrows()
        .store(EscrowRecord {
            user_id: user.clone(),
            blob,
            stored_at,
        })
        .await
        .map_err(|error| {
            tracing::error!(%error, %user, "the escrow store could not store");
            StoreEscrowRejection::Unavailable {
                code: error_codes::ESCROW_UNAVAILABLE,
            }
        })?;

    Ok(Json(StoreEscrowResponse {
        stored_at: stored_at.to_string(),
        replaced: replaced == Replaced::Yes,
    }))
}

/// Fetch the caller's wrapped master key, verbatim.
///
/// The bytes are what a client runs its KDF against, so they come back exactly as they went in.
/// The server never derives, unwraps or re-encodes: a re-encoded wrap is a wrap that no longer
/// opens, and the failure would look like a lost master key.
#[kynos::get("/v1/auth/escrow", operation_id = "fetch_escrow", tag = EscrowTag)]
pub async fn fetch_escrow(
    Inject(escrow): Inject<EscrowContext>,
    Auth(credential): Auth<AccessToken>,
) -> Result<Binary<OctetStream>, FetchEscrowRejection> {
    let user = UserId::new(credential.user.as_str());
    let held = escrow
        .escrows()
        .fetch(&user)
        .await
        .map_err(|error| {
            tracing::error!(%error, %user, "the escrow store could not answer");
            FetchEscrowRejection::Unavailable {
                code: error_codes::ESCROW_UNAVAILABLE,
            }
        })?
        .ok_or_else(|| {
            tracing::debug!(%user, "no escrow has been stored for this account");
            FetchEscrowRejection::NotStored {
                code: error_codes::ESCROW_NOT_STORED,
            }
        })?;

    tracing::debug!(%user, bytes = held.blob.len(), "serving an escrow");
    Ok(Binary::new(held.blob))
}

impl StoreEscrowRejection {
    /// The body cannot be an escrow.
    fn malformed(error: &MalformedEscrow) -> Self {
        Self::Malformed {
            detail: error.to_string(),
            code: error_codes::ESCROW_MALFORMED,
        }
    }
}
