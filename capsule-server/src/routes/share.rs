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
//! | issue `400 error.share.malformed` | a body that cannot be a link record, including a `metadata_blob` that is not base64, exceeds 8 MiB, or does not hash to `metadata_hash` |
//! | issue `403 error.quota.grace_locked` | any link from an account past its quota grace window (`S-C6`) |
//! | issue `403 error.quota.exceeded` | a `metadata_blob` whose new bytes would carry the account to its hard limit (`S-C6`) |
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
use capsule_core::crypto::hash::hash_bytes;
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
use crate::counter::{CounterContext, CounterKey, Verdict, budgets, unix_seconds};
use crate::quota::{QuotaContext, WriteClass};
use crate::serve::BlobSource;
use crate::share::{
    IssueOutcome, MAX_SHARE_METADATA_BYTES, ShareContext, ShareRecord, is_opaque_id,
};
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
    ///
    /// The blob is the issuer's sealed share metadata record (`share-metadata/v1`, built by
    /// `capsule_core::sharing::publish_share`), opaque to this server.
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
    /// Absent for a link with no passphrase, whose scope travels inside the sealed metadata blob.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapped_secret: Option<String>,
    /// When the link stops being live, RFC 3339. Absent means no expiry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// The sealed metadata record itself, base64.
    ///
    /// Its SHA-256 must equal `metadata_hash`, and it may be at most 8 MiB decoded. The server
    /// stores it before it records the link, so a viewer's first fetch finds it, and keeps it
    /// for as long as the link is live. It counts against the caller's storage quota, and is
    /// refused when its new bytes would carry the account to its hard limit. Opaque to this
    /// server: it is sealed under material derived from the URL fragment, which never reaches
    /// it.
    ///
    /// Optional so an older client still registers a link; such a link serves its metadata
    /// only if the blob is already in the store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_blob: Option<String>,
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

    /// The account is over its storage limit and past the grace window.
    ///
    /// The same refusal, and the same code, a lifecycle write's metadata blob gets: a share
    /// link is metadata growth (`S-C6`), whether or not it carries its record. Never returned
    /// by a revocation, which frees nothing but must never be refused either.
    #[error("this account is over its storage limit and past the grace window")]
    #[problem(status = 403, title = "Quota grace expired")]
    QuotaGraceExpired {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// Storing the link's metadata record would carry the account to its hard storage limit.
    ///
    /// The refusal an upload of the same bytes gets, under the same code. Never returned for a
    /// record the ledger already attributes, which adds nothing.
    #[error("this share link's metadata record would cross the account's storage limit")]
    #[problem(status = 403, title = "Quota exceeded")]
    QuotaExceeded {
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
    ///
    /// Two causes, one status, told apart by `code`: `error.share.rate_limited` is this link's
    /// own budget spent, `error.share.at_capacity` is the limiter's per-link partition full. The
    /// second used to render the `500` below, which told a client to report an outage and an
    /// operator to go looking for one, when the limiter was working exactly as designed and
    /// would clear itself inside the window.
    #[error("too many requests")]
    #[problem(status = 429, title = "Too many requests")]
    RateLimited {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
        /// When the caller may retry, as Unix seconds. An **upper** bound: one limiter window,
        /// by which time a live window has lapsed and freed room.
        #[problem(extension)]
        retry_after: u64,
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
    Inject(quota): Inject<QuotaContext>,
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

    let record = request
        .metadata_blob
        .as_deref()
        .map(|encoded| metadata_record(encoded, &metadata))
        .transpose()?;
    // Every issue, with a record or without: past the grace window a new share link is refused
    // (quota.md, "Grace expired"), and a link is metadata growth whether or not this request
    // carries the bytes.
    admit_metadata_growth(&quota, &owner).await?;

    if let Some(record) = record {
        // Stored before the link is recorded, so no viewer ever resolves a link whose record is
        // not yet servable. A link refused below leaves a blob no live link names, which the
        // collector reclaims on its ordinary terms; the reverse order would leave a live link
        // pointing at nothing.
        share
            .blobs()
            .put(&metadata, &record)
            .await
            .map_err(|error| {
                tracing::error!(%error, %owner, "the blob store could not hold a share's metadata record");
                IssueShareRejection::Unavailable {
                    code: error_codes::SHARE_UNAVAILABLE,
                }
            })?;
        tracing::info!(%owner, size = record.len(), "stored a share's sealed metadata record");
        charge_metadata(&quota, &owner, &metadata, record.len() as u64).await?;
    }

    let outcome = share
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
    if outcome == IssueOutcome::Conflict {
        // One answer whoever holds the id, the caller included, so this is no oracle over
        // whose a known id is. A `400` under the existing malformed code rather than a new
        // `409`: a client's CSPRNG never repeats 128 bits, so only a replayed or hostile
        // request lands here (#553).
        return Err(IssueShareRejection::malformed(
            "opaque_id is already in use",
        ));
    }

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
/// The octets the issuer registered as `wrapped_secret`, byte for byte. A viewer fetches this only
/// for a passphrase-protected link; a link with no passphrase carries its scope in the sealed
/// metadata blob instead.
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
            // Fail closed, like every other limiter here — but say which failure it was. A full
            // partition is the limiter working as designed and clears inside the window; only a
            // store that could not answer is a `500`.
            if let Some(retry_after) = counters.capacity_refusal(&error, budgets::SHARE_LINK) {
                tracing::warn!(%error, "the share limiter is at capacity");
                ShareRejection::at_capacity(retry_after)
            } else {
                tracing::error!(%error, "the share limiter could not be reached");
                ShareRejection::unavailable()
            }
        })?;
    match verdict {
        Verdict::Admitted { .. } => Ok(()),
        Verdict::Limited { retry_after } => Err(ShareRejection::RateLimited {
            code: error_codes::SHARE_RATE_LIMITED,
            retry_after: unix_seconds(retry_after),
        }),
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

/// Decode an inline metadata record and prove it is the one `metadata_hash` names (#653).
///
/// The size is checked on the encoded length first, so an oversized body is refused before it
/// is decoded into a second copy, and then on the decoded bytes, which is the bound itself.
fn metadata_record(
    encoded: &str,
    metadata: &ContentAddress,
) -> Result<Vec<u8>, IssueShareRejection> {
    let too_large = "metadata_blob exceeds the 8 MiB bound on a share's metadata record";
    if encoded.len() / 4 * 3 > MAX_SHARE_METADATA_BYTES + 2 {
        return Err(IssueShareRejection::malformed(too_large));
    }
    let record = BASE64
        .decode(encoded)
        .map_err(|_| IssueShareRejection::malformed("metadata_blob is not base64"))?;
    if record.len() > MAX_SHARE_METADATA_BYTES {
        return Err(IssueShareRejection::malformed(too_large));
    }
    // The server addresses a blob by the SHA-256 of its bytes, and a committed blob is
    // immutable and deduplicated by address: bytes stored under the wrong address would be what
    // every reader of that address gets.
    if hash_bytes(&record).to_hex() != metadata.as_str() {
        return Err(IssueShareRejection::malformed(
            "metadata_blob's SHA-256 is not metadata_hash",
        ));
    }
    Ok(record)
}

/// Refuse a share link from an account past its quota grace window (`S-C6`).
async fn admit_metadata_growth(
    quota: &QuotaContext,
    owner: &UserId,
) -> Result<(), IssueShareRejection> {
    let state = crate::quota::current_state(quota, owner)
        .await
        .map_err(|error| {
            tracing::error!(%error, %owner, "the quota ledger could not answer");
            IssueShareRejection::Unavailable {
                code: error_codes::SHARE_UNAVAILABLE,
            }
        })?;
    if crate::quota::admits(state, WriteClass::MetadataGrowth, 0, 0, quota.limits()) {
        return Ok(());
    }
    tracing::info!(%owner, ?state, "a share link was refused: past the quota grace window");
    Err(IssueShareRejection::QuotaGraceExpired {
        code: error_codes::QUOTA_GRACE_LOCKED,
    })
}

/// Attribute a stored metadata record to the link's owner, refusing one that would cross the
/// hard limit (`S-C6`).
///
/// Charged, not only admitted: a link holds its record from the collector for as long as it is
/// live, which may be forever, so an uncharged record would be storage an account could grow
/// without limit by issuing links. Charged **after** the store, so a failed store charges
/// nothing; a record the ledger already attributes — the same bytes under a second link — is
/// not charged twice, and is never refused. The collector credits it back when it sweeps the
/// blob, as it does every blob (`release_attribution`).
///
/// Refused, like an upload, when the new bytes would carry the account to its hard limit
/// ([`crate::quota::charge_upload`] charges, checks, and releases a refused charge). Without
/// that, an account over the limit but inside its grace window could store any number of
/// distinct 8 MiB records, each publicly servable through its link. A refused record is left in
/// the store uncharged and unlinked, and the collector reclaims it on its ordinary terms.
async fn charge_metadata(
    quota: &QuotaContext,
    owner: &UserId,
    address: &ContentAddress,
    size: u64,
) -> Result<(), IssueShareRejection> {
    let outcome = crate::quota::charge_upload(quota, owner, address, size)
        .await
        .map_err(|error| {
            tracing::error!(%error, %owner, "the quota ledger could not charge a share's metadata record");
            IssueShareRejection::Unavailable {
                code: error_codes::SHARE_UNAVAILABLE,
            }
        })?;
    tracing::debug!(%owner, size, ?outcome, "charged a share's metadata record");
    match outcome {
        crate::quota::UploadCharge::Admitted => Ok(()),
        crate::quota::UploadCharge::Refused => {
            tracing::info!(%owner, size, "a share link was refused: its record would cross the hard quota limit");
            Err(IssueShareRejection::QuotaExceeded {
                code: error_codes::QUOTA_EXCEEDED,
            })
        }
    }
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

    /// The limiter is holding as many distinct links as it will hold.
    ///
    /// A `429` and not the `500` this used to be: the limiter is working as designed and clears
    /// itself inside the window, so the caller is told to wait rather than told the server is
    /// broken. Deliberately still distinct from the indistinguishable `404` — a `404` that was
    /// really a capacity refusal would teach a legitimate viewer that a live link is dead.
    fn at_capacity(retry_after: jiff::Timestamp) -> Self {
        Self::RateLimited {
            code: error_codes::SHARE_AT_CAPACITY,
            retry_after: unix_seconds(retry_after),
        }
    }
}
