//! Share links over the wire (slice `S-C4`) — one public serve path and two owner operations.
//!
//! [`crate::share`] owns the record, the liveness rule, and the three decisions that shaped
//! this surface: why the privacy strip cannot happen here, why every refusal is one answer, and
//! why rate limiting is absent rather than declared.
//!
//! # The public path takes no credential and must leak nothing
//!
//! `/s/{opaque-id}` is the only surface on this server reachable with no account. Three
//! consequences run through every handler below:
//!
//! - **The id is checked for shape before any lookup**, so the path is not an oracle over
//!   arbitrary strings — the same discipline [`crate::serve`] applies to a content address.
//! - **Not found, revoked and expired are one `404`** with one body. Never `410`, which would
//!   confirm a link once existed.
//! - **Nothing about the link is disclosed.** No owner, no album, no scope, no expiry. The
//!   contract is explicit that the URL leaks nothing about what it points to, and a response
//!   that named an album would leak it after one fetch.
//!
//! # `S-C28` audit
//!
//! | Status | Verdict |
//! | --- | --- |
//! | issue `201` / revoke `204` | the owner's two operations |
//! | issue `400 error.share.malformed` | a body that cannot be a link record |
//! | serve `200` / `206` / `416` | the metadata, the wrapped secret, and ranged ciphertext |
//! | serve `404` | **one answer** for a malformed id, an unknown link, a revoked one, an expired one, a blob the link does not name, and a link with no wrapped secret |
//! | `401` / `403` | the framework's, on the two owner operations only |
//! | `500 error.share.unavailable` | a store could not answer |
//!
//! **No `429`.** The contract's two limiters need `S-C32`'s counter; declaring the status would
//! promise something nothing can produce.

use std::collections::BTreeSet;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_i18n::error_codes;
use kynos::extract::body::binary::Binary;
use kynos::extract::media::OctetStream;
use kynos::http::etag::ETag;
use kynos::prelude::*;
use kynos::response::range::served::{Conditions, Delivery, Served};
use kynos::response::status::NoContent;
use serde::{Deserialize, Serialize};

use crate::auth::AccessToken;
use crate::blob::ContentAddress;
use crate::counter::{CounterContext, CounterKey, budgets};
use crate::serve::BlobSource;
use crate::share::{ShareContext, ShareRecord, is_opaque_id};
use crate::store::UserId;

/// The public share surface, and the owner operations behind it.
#[derive(Tag)]
#[tag(
    name = "shares",
    description = "Public share links. The only surface served without an account."
)]
pub struct SharesTag;

/// A link the owner's client has issued.
#[derive(Schema, Serialize, Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct IssueShareRequest {
    /// The 128-bit opaque id, 32 lowercase hex characters, drawn from the client's CSPRNG.
    ///
    /// Minted by the client rather than the server because the client is what knows the
    /// fragment secret the id is paired with; the server checks its shape and stores it.
    pub opaque_id: String,
    /// The metadata blob a viewer starts from. Must appear in `serves`.
    pub metadata_hash: String,
    /// Every blob this link may serve, and nothing else.
    ///
    /// Enumerated by the issuing client, which is what makes the boundary-crossing strip
    /// stick: the client points the link at blobs it prepared for export, and the server has no
    /// path from an opaque id to anything outside this set.
    pub serves: Vec<String>,
    /// The passphrase-wrapped scope material, base64, when the link is passphrase-protected.
    ///
    /// Opaque to this server. The passphrase never crosses the wire — unwrap is client-side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapped_secret: Option<String>,
    /// When the link stops being live, RFC 3339. Absent means no expiry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

/// The two ways issuing answers.
///
/// `201` because a link is a resource the caller has created, and it is the same status the
/// upload surface uses for the same reason. Re-issuing the same opaque id replaces the record
/// rather than conflicting: the client owns the id, and a `409` would leave a client that
/// retried a timed-out request unable to proceed.
#[derive(Reply)]
pub enum IssueShareReply {
    /// The link is servable.
    #[reply(
        status = 201,
        description = "The share link is registered and servable"
    )]
    Created(IssueShareResponse),
}

/// Confirmation that a link is now servable.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct IssueShareResponse {
    /// The opaque id, echoed.
    pub opaque_id: String,
}

/// What a viewer needs to start.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SharedMetadataResponse {
    /// The metadata blob's content address; fetch it from `/s/{opaque_id}/blob/{hash}`.
    pub metadata_hash: String,
    /// Whether a passphrase is required before the scope material can be opened.
    ///
    /// The one property of the link this path discloses, and it has to: a viewer cannot know to
    /// ask for a passphrase otherwise. It says nothing about *what* the link points at.
    pub passphrase_protected: bool,
}

/// The link being addressed.
#[derive(PathParams, Schema)]
pub struct SharePath {
    /// The opaque id.
    pub opaque_id: String,
}

/// A blob within a link.
#[derive(PathParams, Schema)]
pub struct ShareBlobPath {
    /// The opaque id.
    pub opaque_id: String,
    /// The blob's content address.
    pub hash: String,
}

/// Why a link was not issued.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum IssueShareRejection {
    /// The body cannot be a link record.
    #[error("{detail}")]
    #[problem(status = 400, title = "Malformed share link")]
    Malformed {
        /// What was wrong, in English.
        detail: String,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The store could not answer.
    #[error("the share link could not be issued")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Why a share request served nothing.
///
/// **One refusal variant, carrying nothing.** A malformed id, an unknown link, a revoked link,
/// an expired link, a blob the link does not name, and a link with no wrapped secret all render
/// the same bytes. The catalog specifies a *bodyless* `404`; this is a constant problem document
/// with no extension members, which carries the same property through a framework whose error
/// type always has a body.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum ShareRejection {
    /// There is nothing here. Whatever the reason.
    #[error("not found")]
    #[problem(status = 404, title = "Not found")]
    NotFound,

    /// Too many requests against this link, or from this source.
    ///
    /// Charged on **every** `/s/{opaque-id}` request — metadata, blob and wrapped-secret alike —
    /// because enumeration does not care which of the three it probes with. Deliberately *not*
    /// folded into the indistinguishable `404`: a `404` that was really a throttle would teach a
    /// legitimate viewer that a live link is dead.
    #[error("too many requests")]
    #[problem(status = 429, title = "Too many requests")]
    RateLimited {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The store could not answer.
    ///
    /// Distinct from the `404` **on purpose**, and it is the one place this surface tells a
    /// caller anything: fail-closed means a serving process that cannot confirm a link is live
    /// must refuse, and answering `404` would be indistinguishable from "revoked" to a client
    /// that would then stop retrying a link that is perfectly good.
    #[error("the share could not be served")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Register a share link the caller's client has issued.
#[kynos::post("/v1/shares", operation_id = "issue_share", tag = SharesTag)]
pub async fn issue_share(
    Inject(share): Inject<ShareContext>,
    Auth(credential): Auth<AccessToken>,
    Json(request): Json<IssueShareRequest>,
) -> Result<IssueShareReply, IssueShareRejection> {
    let owner = UserId::new(credential.user.as_str());

    if !is_opaque_id(&request.opaque_id) {
        return Err(IssueShareRejection::malformed(
            "opaque_id must be 32 lowercase hex characters",
        ));
    }

    let metadata = ContentAddress::parse(&request.metadata_hash)
        .map_err(|_| IssueShareRejection::malformed("metadata_hash is not a content address"))?;

    let mut serves = BTreeSet::new();
    for hash in &request.serves {
        let address = ContentAddress::parse(hash)
            .map_err(|_| IssueShareRejection::malformed("serves carries a malformed address"))?;
        serves.insert(address);
    }
    if !serves.contains(&metadata) {
        // Otherwise the link points at a metadata blob it may not serve, and every viewer's
        // first request would be a `404` the owner has no way to diagnose.
        return Err(IssueShareRejection::malformed(
            "serves must include metadata_hash",
        ));
    }

    let expires_at = match &request.expires_at {
        None => None,
        Some(raw) => Some(
            raw.parse::<jiff::Timestamp>()
                .map_err(|_| IssueShareRejection::malformed("expires_at is not RFC 3339"))?,
        ),
    };

    let wrapped_secret = match &request.wrapped_secret {
        None => None,
        Some(raw) => Some(
            BASE64
                .decode(raw)
                .map_err(|_| IssueShareRejection::malformed("wrapped_secret is not base64"))?,
        ),
    };

    share
        .shares()
        .issue(ShareRecord {
            opaque_id: request.opaque_id.clone(),
            owner_id: owner.clone(),
            // The scope is the client's to know. The server records what the link *serves*, and
            // an album id here would be a field the public path could one day leak.
            scope: capsule_core::sharing::ShareScope::Album(uuid::Uuid::nil()),
            serves,
            metadata,
            wrapped_secret,
            expires_at,
            revoked_at: None,
        })
        .await
        .map_err(|error| {
            tracing::error!(%error, %owner, "the share store could not issue");
            IssueShareRejection::Unavailable {
                code: error_codes::SHARE_UNAVAILABLE,
            }
        })?;

    Ok(IssueShareReply::Created(IssueShareResponse {
        opaque_id: request.opaque_id,
    }))
}

/// Revoke one of the caller's links.
///
/// Idempotent from the caller's side and **indistinguishable**: a link that was never theirs, a
/// link that does not exist, and a link they already revoked are all `204`. Revocation is the
/// one operation where saying "there was nothing to revoke" would be a lookup.
#[kynos::delete(
    "/v1/shares/{opaque_id}",
    operation_id = "revoke_share",
    tag = SharesTag
)]
pub async fn revoke_share(
    Inject(share): Inject<ShareContext>,
    Auth(credential): Auth<AccessToken>,
    Path(path): Path<SharePath>,
) -> Result<NoContent, IssueShareRejection> {
    let owner = UserId::new(credential.user.as_str());
    let now = share.clock().now();

    let revoked = share
        .shares()
        .revoke(&owner, &path.opaque_id, now)
        .await
        .map_err(|error| {
            tracing::error!(%error, %owner, "the share store could not revoke");
            IssueShareRejection::Unavailable {
                code: error_codes::SHARE_UNAVAILABLE,
            }
        })?;

    tracing::info!(%owner, revoked, "a share revocation was processed");
    Ok(NoContent)
}

/// What a viewer needs to begin, for a live link.
#[kynos::get("/s/{opaque_id}", operation_id = "share_metadata", tag = SharesTag)]
pub async fn share_metadata(
    Inject(share): Inject<ShareContext>,
    Inject(counters): Inject<CounterContext>,
    Path(path): Path<SharePath>,
) -> Result<Json<SharedMetadataResponse>, ShareRejection> {
    throttle(&counters, &path.opaque_id).await?;
    let record = live(&share, &path.opaque_id).await?;
    Ok(Json(SharedMetadataResponse {
        metadata_hash: record.metadata.as_str().to_owned(),
        passphrase_protected: record.wrapped_secret.is_some(),
    }))
}

/// The passphrase-wrapped scope material, when there is one.
///
/// A link with no passphrase answers `404` rather than `204` or an empty body: whether a link is
/// passphrase-protected is already disclosed by the metadata record, and a *second* way to ask
/// the same question with a different shape is a second thing to keep consistent.
#[kynos::get(
    "/s/{opaque_id}/wrapped-secret",
    operation_id = "share_wrapped_secret",
    tag = SharesTag
)]
pub async fn share_wrapped_secret(
    Inject(share): Inject<ShareContext>,
    Inject(counters): Inject<CounterContext>,
    Path(path): Path<SharePath>,
) -> Result<Binary<OctetStream>, ShareRejection> {
    throttle(&counters, &path.opaque_id).await?;
    let record = live(&share, &path.opaque_id).await?;
    let Some(wrapped) = record.wrapped_secret else {
        return Err(ShareRejection::NotFound);
    };
    Ok(Binary::new(wrapped))
}

/// Ciphertext for one of the link's blobs, ranged.
///
/// The membership check is the security property: a link serves the addresses its record
/// enumerates and nothing else, so it cannot be walked sideways into the album's unstripped
/// metadata. A blob the link does not name is the same `404` as a link that does not exist.
#[kynos::get(
    "/s/{opaque_id}/blob/{hash}",
    operation_id = "share_blob",
    tag = SharesTag
)]
pub async fn share_blob(
    Inject(share): Inject<ShareContext>,
    Inject(counters): Inject<CounterContext>,
    Path(path): Path<ShareBlobPath>,
    conditions: Conditions,
) -> Result<Delivery<OctetStream>, ShareRejection> {
    throttle(&counters, &path.opaque_id).await?;
    let record = live(&share, &path.opaque_id).await?;

    let Ok(address) = ContentAddress::parse(&path.hash) else {
        return Err(ShareRejection::NotFound);
    };
    if !record.serves(&address) {
        tracing::info!("a share link was asked for a blob it does not serve");
        return Err(ShareRejection::NotFound);
    }

    let stat = share.blobs().stat(&address).await.map_err(|error| {
        tracing::error!(%error, "the blob store could not stat a shared address");
        ShareRejection::unavailable()
    })?;
    let Some(stat) = stat else {
        return Err(ShareRejection::NotFound);
    };

    let etag = ETag::strong(address.as_str());
    let source = BlobSource::new(share.blob_handle(), address, stat.size);
    Served::<_, OctetStream>::new(source)
        .etag(etag)
        // `private` even though the caller is anonymous: the bytes are one account's ciphertext,
        // the link is revocable, and a shared cache holding them would keep serving a share the
        // owner has taken back.
        .cache_control("private, max-age=3600")
        .deliver(&conditions)
        .await
        .map_err(|error| {
            tracing::error!(%error, "a shared blob vanished between resolution and delivery");
            ShareRejection::unavailable()
        })
}

/// Charge the per-link limiter, and refuse if it is spent (`S-C4`, `S-C32`).
///
/// Charged **before** the link is resolved, so probing costs the prober whether or not the id
/// exists — a limiter that only ran for real links would be a free oracle for the rest.
///
/// The contract names *two* limiters, per link and per source address. Only the first is here:
/// this server's request type carries no client address, because it is driven in-process by
/// `TestClient` and behind a proxy in production, where the address that matters is a header a
/// deployment must be configured to trust. Wiring a per-source key to an untrusted header would
/// be worse than having none — it would throttle by a value the attacker chooses. Recorded as
/// owed rather than faked; the key exists in [`CounterKey::ShareSource`] for when there is a
/// trusted address to put in it.
async fn throttle(counters: &CounterContext, opaque_id: &str) -> Result<(), ShareRejection> {
    let key = CounterKey::ShareLink(opaque_id.to_owned());
    let verdict = counters
        .hit(&key, budgets::SHARE_LINK)
        .await
        .map_err(|error| {
            // Fail closed, like every other limiter here.
            tracing::error!(%error, "the share limiter could not be reached");
            ShareRejection::unavailable()
        })?;
    if verdict.admits() {
        Ok(())
    } else {
        Err(ShareRejection::RateLimited {
            code: error_codes::SHARE_RATE_LIMITED,
        })
    }
}

/// Resolve `opaque_id` to a link that may serve right now, or refuse.
///
/// The single place the public path decides anything. Shape first, then existence, then
/// liveness — and all three failures leave through one variant, so the order cannot become
/// observable.
async fn live(share: &ShareContext, opaque_id: &str) -> Result<ShareRecord, ShareRejection> {
    if !is_opaque_id(opaque_id) {
        return Err(ShareRejection::NotFound);
    }

    let record = share.shares().resolve(opaque_id).await.map_err(|error| {
        tracing::error!(%error, "the share store could not resolve");
        ShareRejection::unavailable()
    })?;

    // Fail-closed: a store that cannot answer is a `500` above, never an implicit "live".
    record
        .filter(|record| record.is_live_at(share.clock().now()))
        .ok_or(ShareRejection::NotFound)
}

impl IssueShareRejection {
    /// The body cannot be a link record.
    fn malformed(detail: &str) -> Self {
        Self::Malformed {
            detail: detail.to_owned(),
            code: error_codes::SHARE_MALFORMED,
        }
    }
}

impl ShareRejection {
    /// A store could not answer.
    fn unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::SHARE_UNAVAILABLE,
        }
    }
}
