//! `POST /v1/upload`, `PATCH`, `HEAD` and `DELETE /v1/upload/{id}` — the upload session's life.
//!
//! The port of the Salvo `capsule-api/upload` surface onto Kynos. What carries
//! over is the *contract*: the envelope gate ahead of every write, the chunk rules, the
//! idempotency tuple, the finalization order. What does not carry over is the Salvo handler,
//! its response writers, and its three-writes-per-chunk accounting.
//!
//! # The status audit
//!
//! Kynos makes an undeclared status unrepresentable — the status *is* the return type — so
//! every status the Salvo surface could render was audited as this port was written. The
//! verdict lives in the types; here it is in one table.
//!
//! | Salvo | Verdict here |
//! | --- | --- |
//! | create `201` | kept — [`CreateReply::Created`], with `Location`, `X-Capsule-Advice` (which replaced `X-Capsule-Suggested-Chunk-Size`) and the session's relative expiry |
//! | create `200` (active session for the tuple) | **kept and now documented.** Salvo declared it `undocumented()`; it is [`CreateReply::Existing`], carrying `X-Capsule-Offset` so a resuming client needs no second round trip |
//! | create `400` "Bad request" (untyped) | **deleted.** Never constructed with a code; the 400 a malformed body actually produces is the `Json` extractor's, which Kynos declares |
//! | create `401` | kept, and now the framework's — `Auth<AccessToken>` declares it and fills the `WWW-Authenticate` challenge |
//! | create `403` | kept — album access, device authorization, a declared owner that is not the album's |
//! | create `409 duplicate_blob` | **carries a structured `existing_asset`.** It must name the existing asset, and answering from blob presence alone would tell one account what another holds, so the answer comes from the asset index, owner-scoped, and a `create` for a different asset adopts the blob before it is refused (#672) |
//! | create `413` | kept — the declared size past the deployment ceiling |
//! | create `426` | kept — the manifest envelope's `protocol_version` pin, refused by the envelope gate. The *header* handshake is no longer this surface's: [`crate::negotiation::ProtocolGate`] answers it before the handler runs, and the accepted window rides `X-Capsule-Protocol-Min`/`-Max` on every response |
//! | create `500` | **now `503`** — a collaborator that could not answer, with `error.upload.unavailable` and a retry instant (transfer/upload.md, Hostile Clients) |
//! | create `429` / `507` | **added** — the account's or its source address's session budget, or the live-session cap (`error.upload.rate_limited`), and a full disk (`error.upload.storage_full`); both carry a retry instant |
//! | chunk `204` | kept — with the authoritative `X-Capsule-Offset` and `X-Capsule-Upload-Status`. The completing chunk is answered `waiting_for_processing` the moment it holds the finalize claim, never once finalization ends (#1040) |
//! | chunk `400` | kept, and now *coded*: missing offset, missing checksum, checksum mismatch, empty chunk, misalignment and size exceeded each carry their own `error.upload.*`. The two finalization failures are no longer a chunk's answer: no chunk waits for finalization, so they are `HEAD`'s `X-Capsule-Upload-Error` and the session list's `error_code` |
//! | chunk `403` | **folded into the `404`.** Only the uploader may append, and anyone else is told what a caller naming an unknown session is told — "foreign is unknown" (transfer/upload.md, Hostile Clients). The `403` that remains declared is the bearer scheme's |
//! | chunk `404` | kept — unknown, expired, discarded and foreign sessions are one answer |
//! | chunk `409` | kept — offset mismatch (carrying the authoritative offset), chunk conflict, session not active. `replace_incomplete` and `stale_revival` moved to `X-Capsule-Upload-Error` with the other finalization failures |
//! | chunk `413` | kept — the 16 MiB protocol ceiling, reachable because the transport backstop sits above it |
//! | chunk `415` | kept, and it keeps its code. Kynos's own `Binary` rejection declares `400`, `415` *and* a `422` a raw-bytes body cannot produce, and carries no `error.*` code; [`ChunkBody`] delegates the enforcement to `Binary` and replaces only the rejection, so the `415` is `error.upload.unsupported_media_type` and the phantom `422` is gone |
//! | chunk `408` | **added** (#717) — a body cut for stalling or for falling below the minimum sustained rate, never for its total duration (#1041), `error.upload.chunk_timeout`. [`ChunkBody`] bounds the read and nothing after it, so finalization is never cut short by it |
//! | chunk `409 finalize_in_progress` | **deleted.** Losing the finalize claim is a normal race and the chunk that triggered it was still accepted, so it answers `204`. Telling a client its accepted chunk failed was the Salvo behaviour and it was wrong |
//! | chunk `500` | kept, narrowed — only a stage **shorter** than its counter, the one inconsistency the server cannot repair. A stage that ran ahead is cut back ([`chunk::append`]) and a collaborator failure is a `503` |
//! | chunk `429` / `503` / `507` | **added** — the account's or its source address's chunk budget, a collaborator that could not answer, and a full disk. Each carries a retry instant, on the body and on `Retry-After` ([`crate::problem::RetryAfter`]) |
//! | head `200` | kept — [`HeadReply::Progress`], carrying offset, declared length, state, advice and relative expiry on headers, with `Cache-Control: no-store`, and a `failed_processing` session's `error.*` code in `X-Capsule-Upload-Error`. A `Reply` rather than a `NoContent`, because `200 with headers` and `204` are different answers. It starts the finalization of a full, unclaimed session and never waits for it |
//! | head `400` / `401` / `403` / `404` / `500` | kept, except that a session neither the caller's upload nor filed under the caller is the `404`, not a `403` of this surface's own, and the `500` is a `503` with a retry instant. The `403` left is the bearer scheme's. The `400` is the handshake's, declared by the read gate rather than by this surface; the `426` is gone from `HEAD`, because a read is admitted at any protocol date (issue #404) |
//! | head `409` | **deleted as unreachable.** `HEAD` reports a state, it does not require one. It would have been declared for free by sharing a rejection type with `DELETE`, which is why they are two types |
//! | delete `204` | kept |
//! | delete `409` | kept — finalization is not interruptible, and a terminal session has nothing left to cancel |
//! | delete `400` / `401` / `403` / `404` / `426` / `500` | kept, with `HEAD`'s two changes: a foreign session is the `404`, and the `500` is a `503` |
//! | list sessions (`GET /upload/sessions`) | served by [`super::sessions`] |
//! | receipt (`GET /upload/{id}/receipt`) | out of scope: custody receipts have their own surface |
//!
//! Every status above is produced by a test in `tests/upload.rs`, because
//! `assert_declared_responses_covered` fails on any the document promises and none produced.
//!
//! # One place the protocol asks for a header this surface cannot send
//!
//! A Kynos `ApiError` renders an RFC 9457 problem and has **no seam for a response header**, so
//! the `X-Capsule-Offset` the protocol's census puts on a `409` rides as a problem **extension
//! member** instead. The data a client needs to recover is there and is machine-readable; the
//! spelling is not the one the census names. The alternative was to render the rejection as a
//! plain-JSON `Reply` variant, which would have cost it its `error.*` code — a worse trade,
//! since the code is what a client switches on. Recorded rather than hidden.
//!
//! The `X-Capsule-Protocol-Min`/`-Max` pair used to be the second such place. It is not any
//! more: issue #404 moved the handshake onto [`crate::negotiation`], whose advertising
//! interceptor sits outside every rejection and stamps the window on all of them. The seam an
//! `ApiError` lacks, an `Interceptor` has.
//!
//! # `409 duplicate_blob` refuses the session, and adopts the blob
//!
//! The status says "you already hold these bytes" and names the asset that holds them. For a
//! **retry** — the same asset, the same blob, a lost `201` — that is a complete answer. For
//! genuine cross-asset deduplication, where a second asset legitimately shares a thumbnail with
//! a first, it is only half of one unless the requesting asset also comes to name the blob: the
//! client resolves the `409` as a merge and never uploads the bytes, so a requesting asset that
//! never recorded them left the collector counting one holder where two depended on the bytes,
//! and purging the first asset swept the second's (#672).
//!
//! So a `create` for a different asset is **adopted** before it is refused: the existing blob is
//! recorded against the requesting asset, with its own custody receipt and ledger entries
//! ([`crate::upload::adopt`]). The wire answer is unchanged — the `409` the idempotency table
//! specifies and the SDK's merge path already handles — rather than a new `200` reply variant,
//! which would have been a contract change every client had to learn.
//!
//! # What this port does not have
//!
//! **Quota and the custody receipt** are owned by [`crate::upload`], not by this surface. A
//! cancellation gives its reservation back through [`crate::upload::reservation`], as every
//! other way a session ends without its bytes does.

use capsule_core::crypto::hash::hash_bytes;
use capsule_i18n::error_codes;
use kynos::extract::params::header::Headers;
use kynos::prelude::*;
use kynos::response::headers::WithHeaders;
use kynos::response::status::NoContent;
use kynos::security::auth::Auth;
use serde::{Deserialize, Serialize};
use tracing::Instrument as _;
use uuid::Uuid;

use crate::auth::AccessToken;
use crate::blob::{BlobError, ContentAddress};
use crate::client_addr::ClientAddr;
use crate::counter::{Budget, CounterContext, CounterKey, Verdict, budgets, unix_seconds};
use crate::store::{
    AlbumId, AssetId, BlobRole, Clock, OwnerId, StoreError, UploadId, UploadSessionRecord,
    UploadSessionStatus, UserId,
};
use crate::upload::advice::{Advice, SessionExpiry};
use crate::upload::body::ChunkBody;
use crate::upload::chunk::{
    self, MAX_CHUNK_BYTES, parse_checksum, parse_offset, suggested_chunk_size,
};
use crate::upload::envelope::{DeclaredBlob, GateContext, GateReject, ManifestEnvelope};
use crate::upload::finalize::{self, FinalizeFailure};
use crate::upload::{AlbumWriteAccess, UploadContext};

/// The operations that move an asset's bytes to the server.
#[derive(Tag)]
#[tag(
    name = "upload",
    description = "Opening a resumable upload session and feeding it chunks."
)]
pub struct UploadTag;

// ===========================================================================================
// Wire types
// ===========================================================================================

/// A blob's role in its asset bundle, as the wire spells it.
///
/// A wire type of its own rather than a serde derive on [`BlobRole`]: the state ports'
/// records deliberately derive no serde traits, so that a record cannot be smuggled through a
/// store built for another. The mapping is one `match` in one direction.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WireBlobRole {
    /// The encrypted original.
    Original,
    /// An encrypted derivative — a thumbnail or a preview.
    Derivative,
    /// The encrypted CBOR metadata blob.
    Metadata,
    /// The signed manifest, stored verbatim.
    Provenance,
    /// A backup copy.
    Backup,
    /// A derivative tier's signed `DerivativeManifest`, stored verbatim (#556). Never listed in
    /// the sync feed's `blobs`: the feed carries these bytes inline on the entry's
    /// `derivative_manifests`, so a client that predates the role never meets the value.
    DerivativeManifest,
}

/// The outbound direction, for the sync feed's blob list.
///
/// Both directions are written out rather than one derived from the other: a `From` in one
/// direction and a `TryFrom` back would let the two disagree about a role the wire has and the
/// port does not, and this enum is closed on both sides.
impl From<BlobRole> for WireBlobRole {
    fn from(role: BlobRole) -> Self {
        match role {
            BlobRole::Original => Self::Original,
            BlobRole::Derivative => Self::Derivative,
            BlobRole::Metadata => Self::Metadata,
            BlobRole::Provenance => Self::Provenance,
            BlobRole::Backup => Self::Backup,
            BlobRole::DerivativeManifest => Self::DerivativeManifest,
        }
    }
}

impl From<WireBlobRole> for BlobRole {
    fn from(role: WireBlobRole) -> Self {
        match role {
            WireBlobRole::Original => Self::Original,
            WireBlobRole::Derivative => Self::Derivative,
            WireBlobRole::Metadata => Self::Metadata,
            WireBlobRole::Provenance => Self::Provenance,
            WireBlobRole::Backup => Self::Backup,
            WireBlobRole::DerivativeManifest => Self::DerivativeManifest,
        }
    }
}

/// The body of `POST /v1/upload`.
///
/// Strict (`deny_unknown_fields`): an unknown field is a client bug and is refused rather than
/// ignored. Plaintext metadata — a filename, a capture date, dimensions — is deliberately
/// absent: it rides the encrypted metadata blob and never the wire request.
#[derive(Schema, Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct CreateUploadRequest {
    /// The ciphertext length in bytes. Immutable for the session's life.
    pub size: u64,
    /// The ciphertext content hash, lowercase hex; the digest length is the suite's.
    pub hash: String,
    /// The media type, from the closed enum this protocol version fixes.
    pub content_type: String,
    /// The crypto suite the blob was sealed under.
    pub crypto_suite_id: u16,
    /// The protocol date (`YYYY-MM-DD`) this session is pinned to.
    pub protocol_version: String,
    /// The blob's role in its bundle.
    pub blob_role: WireBlobRole,
    /// The unencrypted manifest fields the server validates.
    pub manifest_envelope: ManifestEnvelope,
    /// The album the asset is filed into.
    ///
    /// Optional on the wire because the contract reserves the shape for owner-scoped kinds and
    /// for the album-upgrade ceremony; **required by this server**, which has no way to check
    /// invariant 6 without one and refuses rather than skipping it.
    pub album_id: Option<String>,
    /// The owner the asset is filed under, when the client wants to say so.
    ///
    /// Advisory, never decisive: the asset is filed under the **album's** owner, which the write
    /// authority answers from the album record — the uploader when it is their album, the owner
    /// when the uploader is a writer on its roster. A declared owner that is anyone
    /// else, the uploading member included, is refused `error.upload.owner_not_permitted`.
    pub owner_id: Option<String>,
    /// The album-upgrade intent this write belongs to, when it belongs to one.
    ///
    /// Carried onto the session verbatim and read by nobody in this port; the album-upgrade
    /// ceremony gives it meaning.
    pub intent_id: Option<String>,
}

/// What a client needs to start sending bytes.
#[derive(Schema, Serialize, Deserialize, Debug)]
pub struct CreateUploadResponse {
    /// The session's identifier.
    pub id: String,
    /// Where to send chunks.
    pub upload_url: String,
    /// A starting chunk size. A suggestion only — the client owns adaptation.
    pub suggested_chunk_size: u64,
}

/// The two ways opening a session succeeds.
///
/// Both are successes and the client treats them the same way; the distinction is whether a
/// transfer starts at zero or resumes. Salvo rendered the second one undeclared.
#[derive(Reply)]
pub enum CreateReply {
    /// A fresh session. `Location` names it, `X-Capsule-Advice` tunes the transfer, and the two
    /// `X-Capsule-Session-*-Expires-In` headers say how long it lives.
    #[reply(status = 201, description = "Upload session created")]
    Created(CreateUploadResponse),
    /// The session already open for this `(owner, hash, album)` tuple, with its authoritative
    /// offset on `X-Capsule-Offset` — never a second session for the same bytes.
    #[reply(
        status = 200,
        description = "The active session for these bytes, to resume"
    )]
    Existing(CreateUploadResponse),
}

/// The response headers `POST /v1/upload` carries.
///
/// `Location` and the offset are optional because the two replies carry different subsets and
/// a Kynos header group is described across all of a reply's statuses; a required header that
/// one status omitted would be a promise the document could not keep. The advice and the
/// expiry are on both, so they are required.
#[derive(HeaderParams)]
pub struct CreateHeaders {
    /// Where the session lives.
    #[header(rename = "Location")]
    location: Option<String>,
    /// The server's advice, an RFC 9651 dictionary of integers: `chunk`, `sessions`, `ranges`
    /// and `ttl`. Advisory only; a client clamps every member into its own bounds.
    #[header(rename = "X-Capsule-Advice")]
    advice: String,
    /// Seconds until the session's lifetime cap.
    #[header(rename = "X-Capsule-Session-Expires-In")]
    expires_in: u64,
    /// Seconds until the session's survival floor, after which it may be discarded under
    /// pressure. Never more than `X-Capsule-Session-Expires-In`.
    #[header(rename = "X-Capsule-Session-Idle-Expires-In")]
    idle_expires_in: u64,
    /// The authoritative offset, on a resumed session.
    #[header(rename = "X-Capsule-Offset")]
    offset: Option<u64>,
    /// Where a resumed session is in its state machine — `waiting_for_processing` once a
    /// duplicate create has started the finalization of a full session nobody had claimed.
    #[header(rename = "X-Capsule-Upload-Status")]
    upload_status: Option<String>,
}

impl CreateHeaders {
    /// The headers for `record`'s session, as of the context's clock.
    fn of(upload: &UploadContext, record: &UploadSessionRecord, offset: Option<u64>) -> Self {
        let expiry = SessionExpiry::of(record, upload.sessions().ttl(), upload.clock().now());
        Self {
            location: Some(session_url(&record.upload_id)),
            advice: Advice::for_size(record.total_size).to_string(),
            expires_in: expiry.expires_in,
            idle_expires_in: expiry.idle_expires_in,
            upload_status: offset.map(|_| record.status.as_str().to_owned()),
            offset,
        }
    }
}

/// The headers a chunk carries.
///
/// The `X-Capsule-Protocol` handshake is not among them: [`crate::negotiation::ProtocolGate`]
/// reads and declares it for every operation on this surface, so a chunk handler only sees a
/// request the handshake already admitted.
#[derive(HeaderParams)]
pub struct ChunkHeaders {
    /// Where in the blob this chunk starts.
    #[header(rename = "X-Capsule-Offset")]
    offset: Option<String>,
    /// The chunk's SHA-256, bare lowercase hex. Required: the idempotency tuple is undefined
    /// without it.
    #[header(rename = "X-Capsule-Checksum")]
    checksum: Option<String>,
}

/// What a chunk's acknowledgement carries.
#[derive(HeaderParams)]
pub struct OffsetHeader {
    /// The next byte the server expects.
    #[header(rename = "X-Capsule-Offset")]
    offset: u64,
    /// Where the session stands: `uploading`, or `waiting_for_processing` once it holds every
    /// declared byte. The completing chunk is answered as soon as finalization is claimed, never
    /// once it finishes; the client reads the outcome from `HEAD`.
    #[header(rename = "X-Capsule-Upload-Status")]
    upload_status: String,
}

/// The one way `HEAD /v1/upload/{id}` succeeds.
///
/// A `Reply` with a single variant rather than a bare `NoContent`, because the protocol's
/// answer is `200` with headers and no body — not `204`. The distinction is the difference
/// between "here is the state, in the headers" and "there is nothing to say".
#[derive(Reply)]
pub enum HeadReply {
    /// The session's progress and state, on the headers below.
    #[reply(
        status = 200,
        description = "Progress and state on X-Capsule-* headers, no body"
    )]
    Progress,
}

/// What a `HEAD` answer carries.
///
/// Every field but the failure code is required and always sent: a resumption primitive that
/// sometimes omits the offset would be one a client cannot rely on. The failure code exists only
/// for a `failed_processing` session.
#[derive(HeaderParams)]
pub struct HeadHeaders {
    /// The next byte the server expects.
    #[header(rename = "X-Capsule-Offset")]
    offset: u64,
    /// The declared total, fixed at creation.
    #[header(rename = "X-Capsule-Content-Length")]
    content_length: u64,
    /// Where the session is in its state machine.
    #[header(rename = "X-Capsule-Upload-Status")]
    upload_status: String,
    /// The server's advice, as on session create.
    #[header(rename = "X-Capsule-Advice")]
    advice: String,
    /// Seconds until the session's lifetime cap.
    #[header(rename = "X-Capsule-Session-Expires-In")]
    expires_in: u64,
    /// Seconds until the session's survival floor. For a session past `uploading` it equals
    /// the lifetime cap's, because such a session is never discarded under pressure.
    #[header(rename = "X-Capsule-Session-Idle-Expires-In")]
    idle_expires_in: u64,
    /// For a `failed_processing` session, the `error.*` code finalization failed with.
    ///
    /// The one channel through which a client learns `content_hash_mismatch`,
    /// `envelope_rejected`, `replace_incomplete` or `stale_revival`: no request waits on
    /// finalization, so the request that completed the session never hears it.
    #[header(rename = "X-Capsule-Upload-Error")]
    upload_error: Option<String>,
    /// `no-store`: progress is not cacheable.
    #[header(rename = "Cache-Control")]
    cache_control: String,
}

/// The session identifier in the path.
#[derive(PathParams, Schema)]
pub struct UploadPath {
    /// The session's identifier, as `POST /v1/upload` returned it.
    pub id: String,
}

// ===========================================================================================
// Rejections
// ===========================================================================================

/// Why a session was not opened.
///
/// Each variant publishes its stable `error.*` code as an RFC 9457 extension named `code`,
/// exactly as [`crate::routes::auth`] does, so a client switches on the code rather than on
/// the bare status.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum CreateRejection {
    /// The album is quiescing under an upgrade ceremony this write does not name.
    ///
    /// Transient by construction: the ceremony either completes — and the client writes into the
    /// fork — or its deadline passes and the album returns to normal operation. Carries the live
    /// `intent_id` so a client that *is* participating can tell "I sent the wrong ticket" from
    /// "somebody else's upgrade is in flight", which are different bugs.
    #[error("this album is quiescing under upgrade {intent_id}")]
    #[problem(status = 409, title = "Album quiescing")]
    UpgradeQuiescing {
        /// The ceremony in flight.
        #[problem(extension)]
        intent_id: String,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The account is suspended.
    ///
    /// Distinct from a quota refusal and from a permission one, deliberately: the three send a
    /// client to three different screens, and design/moderation.md asks for a *structured*
    /// code here precisely so the right remediation is surfaced. A suspension is access-level —
    /// the user's data is untouched and the block is reversible.
    #[error("this account is suspended and cannot upload")]
    #[problem(status = 403, title = "Account suspended")]
    AccountSuspended {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// Invariant 1: the manifest envelope pins a `protocol_version` outside the window this
    /// server accepts.
    ///
    /// The header handshake never reaches here — the gate answered it — so this is the
    /// *body's* pin, which an album carries for life. The accepted window is not restated as
    /// extension members: it rides `X-Capsule-Protocol-Min`/`-Max` on this response like every
    /// other, which is where the SDK reads it.
    #[error("the envelope pins a protocol version this server does not accept")]
    #[problem(status = 426, title = "Protocol version unsupported")]
    ProtocolUnsupported {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// One of the structural invariants refused the request. The `code` says which.
    #[error("{detail}")]
    #[problem(status = 400, title = "Invalid upload")]
    Invalid {
        /// What was wrong, in English. Reaches the client as the problem's `detail`, via
        /// `Display`, rather than as a second extension member saying the same thing.
        detail: String,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The write is not permitted: the album, the device, or the owner on whose behalf it was
    /// attempted.
    #[error("{detail}")]
    #[problem(status = 403, title = "Upload not permitted")]
    Forbidden {
        /// What was refused, in English. Carried by `Display` as the problem's `detail`.
        detail: String,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// Invariant 4's upper half: the declared size is past this deployment's per-blob ceiling.
    #[error("the declared size exceeds this server's per-file limit")]
    #[problem(status = 413, title = "File too large")]
    FileTooLarge {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The owner already holds these bytes, in the asset named.
    ///
    /// The structured `existing_asset` exists because the English detail is a
    /// sentence, and a typed client needs the id to switch on. Owner-scoped by construction —
    /// the lookup behind it takes an owner — because answering across owners would tell one
    /// account that another holds a particular ciphertext, which content addressing makes a
    /// real cross-tenant disclosure.
    #[error("this library already holds these bytes")]
    #[problem(status = 409, title = "Duplicate blob")]
    DuplicateBlob {
        /// The asset that already holds the blob.
        #[problem(extension)]
        existing_asset: String,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The declared size would cross the account's hard quota limit.
    ///
    /// The **only** hard enforcement point. Once a session is open the declared size is the
    /// cap and the transfer is allowed to finish: refusing at finalization would refuse bytes
    /// the server already holds, which costs storage rather than saving it.
    #[error("this upload would cross the account's storage limit")]
    #[problem(status = 403, title = "Quota exceeded")]
    QuotaExceeded {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The account's quota is suspended by the server's operator (#705).
    ///
    /// Distinct from [`Self::QuotaExceeded`], which freeing space lifts, and from
    /// [`Self::AccountSuspended`], a moderation act: only the operator lifts this one, and the
    /// account may still read, delete and restore.
    #[error("this account's storage is suspended")]
    #[problem(status = 403, title = "Quota suspended")]
    QuotaSuspended {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The account opened sessions faster than its budget allows, or already holds as many live
    /// sessions as the server keeps for one account.
    ///
    /// `Retry-After` carries the wait; `retry_after` repeats it in the body as Unix seconds, the
    /// spelling every other `429` on this server uses.
    #[error("too many upload sessions")]
    #[problem(status = 429, title = "Too many requests")]
    RateLimited {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
        /// When the caller may retry, as Unix seconds.
        #[problem(extension)]
        retry_after: u64,
    },

    /// The server's disk is full, so the session's stage could not be created.
    #[error("the server has no space left for this upload")]
    #[problem(status = 507, title = "Insufficient storage")]
    StorageFull {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
        /// When the caller may retry, as Unix seconds.
        #[problem(extension)]
        retry_after: u64,
    },

    /// A collaborator could not answer, so the session neither opened nor was refused.
    #[error("the upload session could not be opened")]
    #[problem(status = 503, title = "Service unavailable")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
        /// When the caller may retry, as Unix seconds.
        #[problem(extension)]
        retry_after: u64,
    },
}

/// Why a chunk was not accepted.
///
/// Never why a finalization failed: no chunk waits for one. A finalization's failure is read
/// from `HEAD`'s `X-Capsule-Upload-Error` and the session list.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum ChunkRejection {
    /// The request is not a well-formed chunk: a missing or unreadable offset or checksum, an
    /// empty body, a misaligned chunk, a checksum that does not match the bytes, or bytes past
    /// the declared size.
    #[error("{detail}")]
    #[problem(status = 400, title = "Invalid chunk")]
    Invalid {
        /// What was wrong, in English. Reaches the client as the problem's `detail`, via
        /// `Display`, rather than as a second extension member saying the same thing.
        detail: String,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// Unknown, expired, discarded, or somebody else's — deliberately one answer.
    ///
    /// The client's recovery is identical for the first three: re-create the session and send
    /// that blob again. Distinguishing them would publish which identifiers once existed, and
    /// answering a session another account holds with anything else would confirm that it
    /// exists (transfer/upload.md, Hostile Clients: "foreign is unknown").
    #[error("there is no such upload session")]
    #[problem(status = 404, title = "Upload session not found")]
    SessionNotFound {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The offset is not the one the server is waiting for. Carries the authoritative offset.
    ///
    /// As an extension member rather than the `X-Capsule-Offset` header the census names, for
    /// the reason [`CreateRejection::ProtocolUnsupported`] records.
    #[error("the session is at offset {offset}")]
    #[problem(status = 409, title = "Offset mismatch")]
    OffsetMismatch {
        /// The offset to resume from.
        #[problem(extension)]
        offset: u64,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The same offset arrived twice with different bytes — a client retrying with garbage.
    #[error("a different chunk was already accepted at this offset")]
    #[problem(status = 409, title = "Chunk conflict")]
    ChunkConflict {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The session is finalizing or already terminal; its bytes are settled.
    #[error("this session is no longer accepting chunks")]
    #[problem(status = 409, title = "Session not active")]
    SessionNotActive {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The chunk is larger than the protocol's 16 MiB ceiling.
    #[error("a chunk may be at most {MAX_CHUNK_BYTES} bytes")]
    #[problem(status = 413, title = "Chunk too large")]
    ChunkTooLarge {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The server's own inconsistency: the stage and the session's counter disagree.
    #[error("the staged upload and the session's byte count disagree")]
    #[problem(status = 500, title = "Internal server error")]
    StorageInconsistent {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The account appended chunks faster than its budget allows.
    #[error("too many chunks")]
    #[problem(status = 429, title = "Too many requests")]
    RateLimited {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
        /// When the caller may retry, as Unix seconds.
        #[problem(extension)]
        retry_after: u64,
    },

    /// The server's disk is full. Nothing of this chunk was kept, so the same chunk at the same
    /// offset lands once space is freed.
    #[error("the server has no space left for this chunk")]
    #[problem(status = 507, title = "Insufficient storage")]
    StorageFull {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
        /// When the caller may retry, as Unix seconds.
        #[problem(extension)]
        retry_after: u64,
    },

    /// A collaborator could not answer.
    #[error("the chunk could not be accepted")]
    #[problem(status = 503, title = "Service unavailable")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
        /// When the caller may retry, as Unix seconds.
        #[problem(extension)]
        retry_after: u64,
    },
}

/// Why a request about an existing session was refused.
///
/// Shared by `HEAD` and `DELETE`, which ask the same three questions — does the session exist,
/// may this caller see it, is it in a state that admits this — and differ only in what they do
/// afterwards. Two identical enums would be two places for the answers to drift apart.
///
/// The first two questions have **one** answer: a session this caller may not see is the same
/// `404` as one that never existed (transfer/upload.md, Hostile Clients: "foreign is unknown").
/// A distinct `403` would confirm to anyone holding a guessed identifier that it names a live
/// session.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum SessionRejection {
    /// Unknown, expired, discarded, or another account's — deliberately one answer.
    #[error("there is no such upload session")]
    #[problem(status = 404, title = "Upload session not found")]
    SessionNotFound {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A collaborator could not answer.
    #[error("the session could not be read")]
    #[problem(status = 503, title = "Service unavailable")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
        /// When the caller may retry, as Unix seconds.
        #[problem(extension)]
        retry_after: u64,
    },
}

/// Why a cancellation was refused.
///
/// [`SessionRejection`]'s answers plus the one only a cancellation can give. It is a
/// second enum rather than another variant on the shared one because a Kynos rejection type
/// declares its statuses for *every* operation that returns it: a `409` on the shared enum
/// would put a `409` on `HEAD`, which cannot produce one, and a promise nothing keeps is the
/// exact over-declared-status defect this rebuild removes.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum CancelRejection {
    /// Unknown, expired, discarded, or another account's — deliberately one answer.
    #[error("there is no such upload session")]
    #[problem(status = 404, title = "Upload session not found")]
    SessionNotFound {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// Finalization is running, or the session is already terminal.
    ///
    /// Finalization is not interruptible: a session in `WaitingForProcessing` is driven to a
    /// terminal state by the finalizer that claimed it — a task that outlives the request that
    /// started it, or, when the process that claimed it died, the next boot's recovery — and a
    /// terminal one has nothing left to cancel — its bytes are either a blob or already gone.
    #[error("this session can no longer be cancelled")]
    #[problem(status = 409, title = "Session not active")]
    SessionNotActive {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A collaborator could not answer.
    #[error("the session could not be cancelled")]
    #[problem(status = 503, title = "Service unavailable")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
        /// When the caller may retry, as Unix seconds.
        #[problem(extension)]
        retry_after: u64,
    },
}

impl CancelRejection {
    fn not_active() -> Self {
        Self::SessionNotActive {
            code: error_codes::UPLOAD_SESSION_NOT_ACTIVE,
        }
    }

    fn unavailable(clock: &dyn Clock) -> Self {
        Self::Unavailable {
            code: error_codes::UPLOAD_UNAVAILABLE,
            retry_after: retry_at(clock, UNAVAILABLE_RETRY),
        }
    }
}

/// The shared answers, as a cancellation renders them. One arm each, so a code or a status can
/// only change in one place.
impl From<SessionRejection> for CancelRejection {
    fn from(rejection: SessionRejection) -> Self {
        match rejection {
            SessionRejection::SessionNotFound { code } => Self::SessionNotFound { code },
            SessionRejection::Unavailable { code, retry_after } => {
                Self::Unavailable { code, retry_after }
            }
        }
    }
}

impl SessionRejection {
    fn not_found() -> Self {
        Self::SessionNotFound {
            code: error_codes::UPLOAD_SESSION_NOT_FOUND,
        }
    }

    fn unavailable(clock: &dyn Clock) -> Self {
        Self::Unavailable {
            code: error_codes::UPLOAD_UNAVAILABLE,
            retry_after: retry_at(clock, UNAVAILABLE_RETRY),
        }
    }
}

/// How long a client is told to wait after a collaborator could not answer.
///
/// Short, because an outage the server noticed is usually a reconnect away from over, and the
/// client resumes from `HEAD` rather than re-sending anything. The guest drop path tells its
/// guests the same.
pub(crate) const UNAVAILABLE_RETRY: jiff::SignedDuration = jiff::SignedDuration::from_secs(30);

/// How long a client is told to wait after the disk filled.
///
/// Longer than an outage's: space comes back when an operator or the collector frees it, not when
/// a connection recovers, and a client polling every few seconds would only fill the log. The
/// guest drop path tells its guests the same.
pub(crate) const STORAGE_FULL_RETRY: jiff::SignedDuration = jiff::SignedDuration::from_mins(5);

/// How long a client at the live-session cap is told to wait.
///
/// One minute: a session leaves the count when it finalizes, is cancelled, or expires, and a
/// client at the cap is usually one whose own transfers are about to finish.
const SESSION_CAP_RETRY: jiff::SignedDuration = jiff::SignedDuration::from_mins(1);

/// `after` from now, as the Unix seconds a `retry_after` member carries.
pub(crate) fn retry_at(clock: &dyn Clock, after: jiff::SignedDuration) -> u64 {
    unix_seconds(crate::store::deadline(clock.now(), after))
}

// ===========================================================================================
// Operations
// ===========================================================================================

/// Open an upload session for one blob of an asset bundle.
///
/// Runs the refuse-by-default envelope battery — invariants 1–8 and the top-level↔envelope
/// consistency family — **before** anything is written, then stages the session's file and
/// records the session. A request whose `(owner, hash, album)` tuple already has an active
/// session gets that session back rather than a second one.
///
/// The quota is checked here and nowhere later: a declared size that would cross the hard limit
/// is `403 error.quota.exceeded`, and an account whose quota the operator suspended is
/// `403 error.quota.suspended`, both before anything is charged or staged.
///
/// A finalized `(owner, hash, album)` is `409 error.upload.duplicate_blob` naming the asset that
/// holds the bytes. When the request is a `create` for a *different* asset, the server first
/// records the existing blob against the requesting asset — custody-receipted and ledgered as a
/// finalization is — so the merge the client performs on the `409` leaves both assets holding a
/// reference, and purging one cannot collect bytes the other depends on.
#[kynos::post("/v1/upload", operation_id = "create_upload", tag = UploadTag)]
#[allow(
    clippy::too_many_arguments,
    reason = "one extractor per argument; Kynos resolves each one, and bundling them would hide which the contract declares"
)]
pub async fn create_upload(
    Auth(credential): Auth<AccessToken>,
    Inject(upload): Inject<UploadContext>,
    Inject(attestation): Inject<crate::attestation::AttestationContext>,
    Inject(quota): Inject<crate::quota::QuotaContext>,
    Inject(moderation): Inject<crate::moderation::ModerationContext>,
    Inject(counters): Inject<CounterContext>,
    client: ClientAddr,
    Json(request): Json<CreateUploadRequest>,
) -> Result<WithHeaders<CreateReply, CreateHeaders>, CreateRejection> {
    let uploader = credential.user.clone();

    // The account's session budget, then its source address's (#718), both charged before
    // anything is read on its behalf, so a flood of creations costs the flooder rather than the
    // stores behind this check. The account goes first so a request its own budget refuses
    // spends nothing of the address's, which other accounts behind it share.
    charge_both(
        &counters,
        (
            &CounterKey::UploadSessions(uploader.clone()),
            budgets::UPLOAD_SESSIONS,
        ),
        (
            &CounterKey::UploadSessionsSource(client.source_key()),
            budgets::UPLOAD_SESSIONS_SOURCE,
        ),
    )
    .await
    .map_err(|throttled| match throttled {
        Throttled::Until(retry_after) => CreateRejection::rate_limited(retry_after),
        Throttled::Unavailable => CreateRejection::unavailable(upload.clock()),
    })?;

    // Account standing, checked before anything is reserved. A suspension removes the
    // ability to *write*, so refusing here — before a session exists, before a quota is charged
    // — is what keeps a suspended account from leaving half-built state behind that a
    // reinstatement would then have to reconcile.
    let standing = moderation
        .store()
        .standing(&crate::store::UserId::new(uploader.as_str()))
        .await
        .map_err(|error| {
            tracing::error!(%error, %uploader, "the moderation store could not answer");
            CreateRejection::unavailable(upload.clock())
        })?;
    if !standing.may_write() {
        tracing::info!(%uploader, "an upload was refused: the account is suspended");
        return Err(CreateRejection::AccountSuspended {
            code: error_codes::MODERATION_ACCOUNT_SUSPENDED,
        });
    }

    // Invariant 6, first half: this surface has no way to check an album it was not given.
    let Some(album) = request.album_id.as_deref().map(AlbumId::new) else {
        return Err(CreateRejection::album_access_denied());
    };

    let AlbumWriteAccess::Writable {
        owner_id: owner,
        role,
        protocol_pin,
        quiescing_under,
    } = upload
        .authority()
        .album_write_access(&uploader, &album)
        .await
        .map_err(|error| {
            tracing::error!(%error, "the write authority could not answer for an album");
            CreateRejection::unavailable(upload.clock())
        })?
    else {
        tracing::info!(%uploader, %album, "an upload was refused: no write capability");
        return Err(CreateRejection::album_access_denied());
    };
    tracing::debug!(%uploader, %owner, ?role, %album, "the album admits this upload");
    // The namespace is the authority's answer; a declared owner may only agree with it.
    resolve_owner(&uploader, &owner, request.owner_id.as_deref())?;

    // Upgrade quiescence (versioning.md step 2). An album whose members have stopped
    // writing and are draining accepts **only** the ceremony's own writes, so a stale client that
    // never saw the `UpgradeIntent` cannot write past the freeze. The intent is client-asserted
    // here and that is fine: it is not an authorization, it is a *ticket* the server checks
    // against a value only the ceremony's own proposal could have put there.
    if let Some(live) = quiescing_under
        && request.intent_id.as_deref() != Some(live.to_string().as_str())
    {
        tracing::info!(
            %owner,
            %album,
            intent = %live,
            "an upload was refused: the album is quiescing under a different ceremony"
        );
        return Err(CreateRejection::UpgradeQuiescing {
            intent_id: live.to_string(),
            code: error_codes::UPLOAD_ALBUM_QUIESCING,
        });
    }

    // Invariant 7's account half — and it belongs on **this** surface only.
    //
    // This endpoint admits exactly the two actions that move blob bytes, `create` and `replace`
    // (the dispatch below; a write that moves bytes is an upload by definition). Both mint
    // freshly authored ciphertext, and every client path that builds one sets the author to the
    // signing account: `lifecycle/import.rs`, `lifecycle/drops.rs` and `drop/mod.rs` all write
    // `created_by_user: self.account.user_id` — the last of them for an *adopted* web-upload
    // drop, which design/web-upload.md is explicit about ("set `created_by_user`/
    // `created_by_device` to the **adopter** (the cryptographic author)"). So on this surface
    // the author is the caller, and a mismatch is a client contradicting itself.
    //
    // `POST /v1/albums/{album_id}/ops` makes the **same** comparison, for the same reason. It
    // briefly did not: the check was removed there when a writer member's delete of the owner's
    // asset was refused, on the reading that a continuation's author travels down from the
    // creator. That was right about the symptom and wrong about the cause — `sign_lifecycle`
    // re-mints `created_by_user` and `created_by_device` per record, so on a continuation as on
    // a create the field names the signer of *this* record — and the check was restored once the
    // client half was understood. See that module's docs.
    //
    // Without this, a writer member could file a *new* asset into the owner's album attributed
    // to a third account: the manifest is stored verbatim and served back as provenance, and
    // nothing later re-derives who wrote it. A `400` with the envelope-mismatch code, like every
    // other field that contradicts what the request itself establishes.
    //
    // `replace` has no client builder in this tree yet (no `Action::Replace` is constructed
    // anywhere under `capsule-library/src/lifecycle/`), so whether a replace re-mints its
    // attribution or inherits it is still open — #475. It is held to the same rule as `create`
    // here because it authors new ciphertext under a fresh file key, which is what makes an
    // author an author on this surface.
    if request.manifest_envelope.created_by_user != uploader.as_str() {
        tracing::info!(%uploader, "an upload was refused: created_by_user is not the caller");
        return Err(CreateRejection::Invalid {
            detail: "created_by_user is not the authenticated caller".to_owned(),
            code: error_codes::UPLOAD_ENVELOPE_MISMATCH,
        });
    }

    // Invariant 7: the device the manifest names must be in the uploader's published
    // directory, and the battery compares the moment it was admitted against the manifest.
    let device = crate::upload::envelope::created_by_device(&request.manifest_envelope)
        .map_err(CreateRejection::from_gate)?;
    let Some(device_added_at) = upload
        .authority()
        .device_added_at(&uploader, device)
        .await
        .map_err(|error| {
            tracing::error!(%error, "the write authority could not answer for a device");
            CreateRejection::unavailable(upload.clock())
        })?
    else {
        tracing::info!(%uploader, %device, "an upload was refused: unknown device");
        return Err(CreateRejection::device_not_authorized());
    };

    let declared = DeclaredBlob {
        size: request.size,
        hash: &request.hash,
        content_type: &request.content_type,
        crypto_suite_id: request.crypto_suite_id,
        protocol_version: &request.protocol_version,
        album_id: request.album_id.as_deref(),
        blob_role: request.blob_role.into(),
    };
    let now = upload.clock().now();
    let gate_context = GateContext {
        policy: upload.policy(),
        album_pin: &protocol_pin,
        device_added_at,
        server_clock: now,
    };
    // Two actions ride this surface and no others: a write that moves blob bytes is an
    // upload by definition, and `create` and `replace` are the two that do. The dispatch is on
    // the envelope's own token so an unknown action reaches `check_create` and is refused there
    // by name, rather than falling into a default arm that would have to guess.
    match request.manifest_envelope.action.as_str() {
        "replace" => crate::upload::envelope::check_replace(
            &declared,
            &request.manifest_envelope,
            &gate_context,
        ),
        _ => crate::upload::envelope::check_create(
            &declared,
            &request.manifest_envelope,
            &gate_context,
        ),
    }
    .map_err(CreateRejection::from_gate)?;

    // Idempotent creation. The tuple is `(owner, hash, album)` and the *uploader's* live
    // sessions are where an active one for it can be: only the uploader may append, so
    // handing back a session opened by somebody else would hand back one this caller could not
    // use.
    let live = upload
        .sessions()
        .sessions_for_uploader(&uploader)
        .await
        .map_err(|error| {
            store_unavailable(&error, "list an uploader's sessions");
            CreateRejection::unavailable(upload.clock())
        })?;
    // A session the finish below refused is terminal now, and is neither handed back nor counted
    // against the cap: the client is given a fresh one to send its bytes again.
    let mut refused = None;
    if let Some(existing) = active_session_for(live.iter(), &owner, &request.hash, &album) {
        // A full session nobody claimed has its finalization started here, before the offset is
        // handed back: a client told it is at its size sends nothing more, so this answer may be
        // the last request it makes about these bytes (#830). Started, never awaited: the answer
        // is the session as it stands, `waiting_for_processing`, and the client reads the
        // outcome from `HEAD`.
        let finished = finish_if_full(&upload, &attestation, existing).await;
        match finished {
            None | Some(Ok(finalize::Started::Claimed(_) | finalize::Started::AlreadyClaimed)) => {
                // A finish moved the session past `Uploading`, and its expiry headers are the
                // post-finish record's: a session no longer evictable lives to the cap, so the
                // snapshot taken before the finish would report the survival floor instead.
                let current = if finished.is_some() {
                    reread_finished(&upload, existing).await?
                } else {
                    existing.clone()
                };
                tracing::info!(
                    upload_id = %current.upload_id,
                    received_bytes = current.received_bytes,
                    status = current.status.as_str(),
                    "returning the active session for a duplicate create"
                );
                let body = describe(&current.upload_id, current.total_size);
                let headers = CreateHeaders::of(&upload, &current, Some(current.received_bytes));
                return Ok(WithHeaders::new(CreateReply::Existing(body), headers));
            }
            // The claim could not be taken: the session is still full and unclaimed, and the
            // client's retry starts it.
            Some(Err(_)) => {
                return Err(CreateRejection::unavailable(upload.clock()));
            }
            // Gone between the listing and the claim: the bytes are not held, so the create
            // carries on and opens a session to receive them again.
            Some(Ok(finalize::Started::NotFound)) => {
                tracing::info!(
                    upload_id = %existing.upload_id,
                    "a duplicate create's full session did not finalize; opening a fresh one"
                );
                refused = Some(existing.upload_id.clone());
            }
        }
    }

    // The live-session cap, after the idempotent answer so a client resuming what it already
    // holds is never refused, and before anything is reserved or staged on its behalf.
    let open = live
        .iter()
        .filter(|record| record.status.is_active() && Some(&record.upload_id) != refused.as_ref())
        .count();
    if open >= budgets::UPLOAD_LIVE_SESSIONS {
        tracing::info!(
            %uploader,
            open,
            cap = budgets::UPLOAD_LIVE_SESSIONS,
            "an upload was refused: the account holds as many live sessions as it may"
        );
        return Err(CreateRejection::rate_limited(retry_at(
            upload.clock(),
            SESSION_CAP_RETRY,
        )));
    }

    // The finalized half of the same rule, and the same key: `(owner_id, hash, album_id)`.
    // Both scopes sit in `find_by_address`'s signature rather than in a caller's discipline —
    // owner because the blob store could say whether *anyone* holds these bytes and answering
    // from that would tell one account what another holds, album because a `409` is the
    // client's *merge* trigger and across two albums there is nothing to merge.
    let address = ContentAddress::parse(&request.hash).map_err(|error| {
        // Unreachable: invariant 3 has already run. Cheap guard on an earlier check's promise.
        tracing::error!(%error, "a gate-passed hash is not a content address");
        CreateRejection::unavailable(upload.clock())
    })?;
    // The asset the manifest names — "the same id across the bundle's members".
    let asset_id = AssetId::new(&request.manifest_envelope.file_id);
    let duplicate = upload
        .index()
        .find_by_address(&owner, &album, &address)
        .await
        .map_err(|error| {
            store_unavailable(&error, "look up a duplicate blob");
            CreateRejection::unavailable(upload.clock())
        })?;
    // Only a *create* of a *different* asset adopts (#672). The same asset holding the blob is a
    // retry with a lost `201`, whose row already names it; a `replace` member records nothing
    // until its manifest applies the whole bundle as one op, which names the blob itself.
    let adopting = match duplicate {
        Some(existing) if existing == asset_id || request.manifest_envelope.action != "create" => {
            tracing::info!(%owner, existing_asset = %existing, "an upload was refused: already held");
            return Err(CreateRejection::duplicate_blob(&existing));
        }
        other => other,
    };

    // Reserved before a session exists, so every blob of a bundle joins one row rather than
    // minting one — and before an adoption, which records against this row.
    match upload
        .index()
        .reserve(crate::index::PendingAsset {
            asset_id: asset_id.clone(),
            owner_id: owner.clone(),
            album_id: album.clone(),
            protocol_version: protocol_pin.clone(),
            crypto_suite_id: request.crypto_suite_id,
            created_at: now,
        })
        .await
        .map_err(|error| {
            store_unavailable(&error, "reserve an asset row");
            CreateRejection::unavailable(upload.clock())
        })? {
        // A new bundle, or a sibling session of one already open. Both are the normal case.
        crate::index::Reservation::Created(_) | crate::index::Reservation::Joined(_) => {}
        // The id names a row filed under another album's owner, or under a different album or
        // pin. Answered as a plain refusal carrying nothing: the id is client-chosen, so a
        // guess costs the caller nothing and must buy them nothing.
        crate::index::Reservation::Conflict => {
            tracing::info!(%owner, %asset_id, "an upload was refused: the asset id is not this caller's");
            return Err(CreateRejection::album_access_denied());
        }
    }

    // The owner already holds these bytes under another asset in this album. The client will
    // resolve the `409` as a merge and never upload them, so the requesting asset must name the
    // blob *now* or the collector counts one holder where two depend on it (#672). The wire
    // answer is the `409` it always was; the reference is the side effect.
    if let Some(existing) = adopting {
        let adoption = crate::upload::adopt::Adoption {
            owner: &owner,
            uploader: &uploader,
            asset_id: &asset_id,
            role: request.blob_role.into(),
            address: &address,
            crypto_suite_id: request.crypto_suite_id,
            protocol_version: &protocol_pin,
        };
        match crate::upload::adopt::adopt(&upload, &attestation, adoption).await {
            Ok(
                outcome @ (crate::upload::adopt::Adopted::Recorded { .. }
                | crate::upload::adopt::Adopted::AlreadyHeld
                | crate::upload::adopt::Adopted::Declined),
            ) => {
                tracing::info!(%owner, existing_asset = %existing, %asset_id, adoption = ?outcome, "an upload was refused: already held by another asset");
                return Err(CreateRejection::duplicate_blob(&existing));
            }
            // Nothing to adopt: the bytes are gone from the store. An ordinary session puts
            // them back, which is the only repair a dangling reference has.
            Ok(crate::upload::adopt::Adopted::Missing) => {}
            // The asset already names other bytes under this singular role, so telling the
            // client it now holds these would be false. Finalization's answer to the same
            // contradiction.
            Ok(crate::upload::adopt::Adopted::Conflict) => {
                return Err(CreateRejection::from_gate(GateReject::EnvelopeMismatch(
                    "blob_role",
                )));
            }
            Err(failure) => {
                tracing::warn!(%failure, %asset_id, "a duplicate create could not adopt the blob");
                return Err(CreateRejection::unavailable(upload.clock()));
            }
        }
    }

    // Quota's one hard enforcement point. After the duplicate check and the
    // reservation, because a refused duplicate must not be charged; before the stage, because a
    // refusal must not leave bytes. The reserved-but-unpublished row a refusal leaves behind
    // publishes nothing and is the discard worker's, exactly as an abandoned session's is.
    let charged = match crate::quota::charge_upload(&quota, &uploader, &address, request.size).await
    {
        Ok(
            charge @ (crate::quota::UploadCharge::Admitted
            | crate::quota::UploadCharge::AlreadyAttributed),
        ) => charge,
        Ok(crate::quota::UploadCharge::Refused) => return Err(CreateRejection::quota_exceeded()),
        Ok(crate::quota::UploadCharge::Suspended) => {
            return Err(CreateRejection::QuotaSuspended {
                code: error_codes::QUOTA_SUSPENDED,
            });
        }
        Err(error) => {
            store_unavailable(&error, "charge an upload against its quota");
            return Err(CreateRejection::unavailable(upload.clock()));
        }
    };

    let upload_id = new_upload_id();

    // The stage first, the record second. A stage with no session is an orphan the startup
    // scrub reclaims; a session with no stage would make every chunk a `500` until it expired.
    upload
        .blobs()
        .begin(&upload_id)
        .await
        .map_err(|error| match error {
            BlobError::StorageFull { .. } => {
                tracing::warn!(%error, upload_id = %upload_id, "a session was refused: the disk is full");
                CreateRejection::StorageFull {
                    code: error_codes::UPLOAD_STORAGE_FULL,
                    retry_after: retry_at(upload.clock(), STORAGE_FULL_RETRY),
                }
            }
            error => {
                tracing::error!(%error, upload_id = %upload_id, "the stage could not be opened");
                CreateRejection::unavailable(upload.clock())
            }
        })?;

    let record = UploadSessionRecord {
        upload_id: upload_id.clone(),
        // The reserved row, above. Taken from the manifest and never minted here: a fresh id
        // per session gave every blob of one bundle a different asset, which made the bundle
        // ungroupable and the visibility gate a conjunction over nothing.
        asset_id,
        owner_id: owner,
        upload_user_id: uploader.clone(),
        album_id: Some(album),
        content_type: Some(request.content_type.clone()),
        expected_hash: request.hash.clone(),
        crypto_suite_id: request.crypto_suite_id,
        protocol_version: request.protocol_version.clone(),
        blob_role: request.blob_role.into(),
        intent_id: request.intent_id.clone(),
        // The projection, stored so finalization re-runs the battery over what creation
        // validated. It is never re-encoded into manifest bytes: the manifest is stored verbatim.
        manifest_envelope: serde_json::to_string(&request.manifest_envelope).map_err(|error| {
            tracing::error!(%error, "the validated envelope could not be stored");
            CreateRejection::unavailable(upload.clock())
        })?,
        received_bytes: 0,
        total_size: request.size,
        status: UploadSessionStatus::Pending,
        failure_code: None,
        created_at: now,
        last_progress_at: now,
    };

    let headers = CreateHeaders::of(&upload, &record, None);
    upload.sessions().open(record).await.map_err(|error| {
        store_unavailable(&error, "open an upload session");
        CreateRejection::unavailable(upload.clock())
    })?;

    // An `AlreadyAttributed` charge leans on an attribution somebody else's session made, and
    // that session can end and release it while this one was opening. Charged again now that
    // the session is visible to a release's live-session check: see
    // [`crate::upload::reservation`] for why the two second reads together close the race.
    if charged == crate::quota::UploadCharge::AlreadyAttributed {
        recharge_after_open(
            &upload,
            &quota,
            &upload_id,
            &uploader,
            &address,
            request.size,
        )
        .await?;
    }

    tracing::info!(
        upload_id = %upload_id,
        size = request.size,
        blob_role = ?request.blob_role,
        "opened an upload session"
    );
    Ok(WithHeaders::new(
        CreateReply::Created(describe(&upload_id, request.size)),
        headers,
    ))
}

/// Append a chunk, and finalize when it completes the declared size.
///
/// Every rule the [chunk
/// contract](../../../capsule-docs/src/content/docs/design/transfer/upload.md) fixes is
/// checked before a byte is written, and the checksum is verified against the received bytes
/// *first*, so a chunk corrupted in transit persists nothing.
#[kynos::patch("/v1/upload/{id}", operation_id = "append_chunk", tag = UploadTag)]
#[allow(
    clippy::too_many_arguments,
    reason = "one extractor per argument; Kynos resolves each one, and bundling them would hide which the contract declares"
)]
pub async fn append_chunk(
    Auth(credential): Auth<AccessToken>,
    Inject(upload): Inject<UploadContext>,
    Inject(attestation): Inject<crate::attestation::AttestationContext>,
    Path(path): Path<UploadPath>,
    Headers(headers): Headers<ChunkHeaders>,
    Inject(counters): Inject<CounterContext>,
    client: ClientAddr,
    body: ChunkBody,
) -> Result<WithHeaders<NoContent, OffsetHeader>, ChunkRejection> {
    let id = UploadId::new(path.id);

    // The account's chunk budget, then its source address's (#718), before the session is
    // read: a client hammering the head of a session it does not hold pays for that as much as
    // one hammering its own. Ordered as the create's are, for the same reason.
    charge_both(
        &counters,
        (
            &CounterKey::UploadChunks(credential.user.clone()),
            budgets::UPLOAD_CHUNKS,
        ),
        (
            &CounterKey::UploadChunksSource(client.source_key()),
            budgets::UPLOAD_CHUNKS_SOURCE,
        ),
    )
    .await
    .map_err(|throttled| match throttled {
        Throttled::Until(retry_after) => ChunkRejection::RateLimited {
            code: error_codes::UPLOAD_RATE_LIMITED,
            retry_after,
        },
        Throttled::Unavailable => ChunkRejection::unavailable(upload.clock()),
    })?;

    let record = upload
        .sessions()
        .read(&id)
        .await
        .map_err(|error| {
            store_unavailable(&error, "read an upload session");
            ChunkRejection::unavailable(upload.clock())
        })?
        .ok_or_else(ChunkRejection::session_not_found)?;

    // Only the uploader appends. The owner may look, but the resuming party is the one that
    // holds the bytes. Anyone else is told what a caller naming an unknown session is told:
    // "foreign is unknown" (transfer/upload.md, Hostile Clients).
    // A guest drop's session is the link owner's too, but it is not an album upload: finalizing
    // it here would fail it on its absent envelope and drop the guest's bytes. It is answered as
    // unknown, which on this surface it is.
    if record.upload_user_id != credential.user || record.is_guest_drop() {
        tracing::info!(
            upload_id = %id,
            caller = %credential.user,
            offset = ?headers.offset,
            code = error_codes::UPLOAD_SESSION_NOT_FOUND,
            "a chunk was refused: not an album upload of this caller's"
        );
        return Err(ChunkRejection::session_not_found());
    }
    if !record.status.is_active() || record.status == UploadSessionStatus::WaitingForProcessing {
        return Err(ChunkRejection::session_not_active());
    }

    let Some(offset) = headers.offset.as_deref().and_then(parse_offset) else {
        return Err(ChunkRejection::missing_offset());
    };
    let Some(checksum) = headers.checksum.as_deref().and_then(parse_checksum) else {
        return Err(ChunkRejection::missing_checksum());
    };

    let bytes = body.bytes();
    if bytes.is_empty() {
        return Err(ChunkRejection::empty_chunk());
    }
    if !chunk::within_chunk_ceiling(bytes.len() as u64) {
        return Err(ChunkRejection::chunk_too_large());
    }

    // Before any write: the bytes must be the bytes the header names. A mismatch persists
    // nothing and leaves the offset where it was, so the client re-sends the same chunk.
    let received_hash = hash_bytes(bytes).to_hex();
    if received_hash != checksum {
        tracing::info!(upload_id = %id, offset, "a chunk was refused: checksum mismatch");
        return Err(ChunkRejection::checksum_mismatch());
    }

    // The rules themselves live in `upload::chunk`, shared verbatim with the guest-drop path:
    // invariants 9–12 mean the same thing on both surfaces, and two copies would
    // drift on exactly the case a client hits after losing a connection. What stays here is
    // what genuinely differs — who is allowed to append, and how a refusal is spelled on the
    // wire.
    // The append and the finalize claim it leads to are one task, awaited here for the answer:
    // a client that disconnects drops this future, and neither may stop with it, because an
    // append whose record landed but whose claim was never taken leaves a full session nobody
    // finishes (#801, #818). The finalization itself runs detached and is never awaited: the
    // completing chunk is answered `waiting_for_processing` as soon as the claim is held, so the
    // answer does not depend on how long a re-hash of the blob takes.
    let declared_size = record.total_size;
    let landed = tokio::spawn(
        append_and_finish(
            upload.clone(),
            attestation.clone(),
            record,
            offset,
            body,
            received_hash,
        )
        .instrument(tracing::Span::current()),
    )
    .await
    .map_err(|join| {
        // A panic inside the append or the finalization. A claim it held is left for the next
        // boot's recovery to drive, and a full session it left unclaimed is finished by the
        // client's replay, duplicate create or `HEAD`, or else the next boot; this client is
        // told to come back.
        tracing::error!(upload_id = %id, error = %join, "the chunk task did not finish");
        ChunkRejection::unavailable(upload.clock())
    })?
    .map_err(|error| {
        store_unavailable(&error, "append a chunk");
        ChunkRejection::unavailable(upload.clock())
    })?;

    if let Some(started) = landed.finalized {
        match started {
            Ok(finalize::Started::Claimed(_) | finalize::Started::AlreadyClaimed) => {}
            Ok(finalize::Started::NotFound) => return Err(ChunkRejection::session_not_found()),
            // The claim could not be taken. The chunk is recorded and the session is full and
            // unclaimed, so the client's resend of it — or its `HEAD` — starts the finish.
            Err(failure) => {
                tracing::warn!(upload_id = %id, %failure, "a full session's finalization could not be claimed");
                return Err(ChunkRejection::unavailable(upload.clock()));
            }
        }
    }

    let next_offset = match landed.accepted {
        chunk::Accepted::Advanced { next_offset, .. }
        | chunk::Accepted::Replayed { next_offset, .. } => next_offset,
        chunk::Accepted::Conflict => return Err(ChunkRejection::chunk_conflict()),
        chunk::Accepted::OffsetMismatch { expected } => {
            return Err(ChunkRejection::offset_mismatch(expected));
        }
        chunk::Accepted::NotAligned => return Err(ChunkRejection::chunk_not_aligned()),
        chunk::Accepted::SizeExceeded => {
            // A declared size the bytes exceed is a client that has lost track of its own
            // upload; resuming would keep failing, so the session goes.
            abandon_failed(&upload, &id).await;
            return Err(ChunkRejection::size_exceeded());
        }
        chunk::Accepted::StorageInconsistent => {
            return Err(ChunkRejection::storage_inconsistent());
        }
        chunk::Accepted::StorageFull => {
            return Err(ChunkRejection::StorageFull {
                code: error_codes::UPLOAD_STORAGE_FULL,
                retry_after: retry_at(upload.clock(), STORAGE_FULL_RETRY),
            });
        }
        // A concurrent chunk completed the session while this one waited its turn.
        chunk::Accepted::NotActive => return Err(ChunkRejection::session_not_active()),
        // Expired, discarded, or cancelled between the read and the write. The staged bytes are
        // now an orphan the scrub reclaims.
        chunk::Accepted::SessionGone => return Err(ChunkRejection::session_not_found()),
    };

    // A session holding every byte is claimed — by this chunk, or by the request that raced it
    // — and finalizing; anything short of that is still uploading.
    let upload_status = if next_offset == declared_size {
        UploadSessionStatus::WaitingForProcessing
    } else {
        UploadSessionStatus::Uploading
    };
    tracing::debug!(
        upload_id = %id,
        next_offset,
        status = upload_status.as_str(),
        "acknowledged a chunk"
    );
    Ok(WithHeaders::new(
        NoContent,
        OffsetHeader {
            offset: next_offset,
            upload_status: upload_status.as_str().to_owned(),
        },
    ))
}

/// What one chunk did, with the finalization it started.
struct Landed {
    /// What the append did.
    accepted: chunk::Accepted,
    /// The finalization the chunk started — present when it completed the session, or replayed
    /// a chunk of a full session nobody had finished. Started, never awaited.
    finalized: Option<Result<finalize::Started, FinalizeFailure>>,
}

/// Append one chunk and, when the session then holds every byte and nobody has claimed its
/// finish, take the claim and start finalization detached.
///
/// Run as its own task by [`append_chunk`], so it reaches its end whether or not the request
/// does. Starting finalization on a replay is what finishes a session the completing request
/// left full and unclaimed — a request dropped while its progress write was in flight, or a
/// process that died before claiming: the client's resend of its last chunk is one request left
/// to finish it, and [`finalize::start`]'s claim makes a replay that races the original request
/// stand down rather than finalize twice.
async fn append_and_finish(
    upload: UploadContext,
    attestation: crate::attestation::AttestationContext,
    record: UploadSessionRecord,
    offset: u64,
    body: ChunkBody,
    received_hash: String,
) -> Result<Landed, StoreError> {
    let accepted = chunk::append(
        upload.sessions(),
        upload.blobs(),
        upload.clock(),
        &record,
        offset,
        body.bytes(),
        &received_hash,
    )
    .await?;
    let finishes = matches!(
        accepted,
        chunk::Accepted::Advanced { complete: true, .. }
            | chunk::Accepted::Replayed {
                unfinished: true,
                ..
            }
    );
    let finalized = if finishes {
        Some(finalize::start(&upload, &attestation, &record.upload_id).await)
    } else {
        None
    };
    Ok(Landed {
        accepted,
        finalized,
    })
}

/// Start the finalization of `record` when it holds every declared byte and nobody has claimed
/// it.
///
/// `None` when there is nothing to finish: the session is short of its size, already claimed or
/// terminal, or a guest drop — whose finish is the drop surface's deposit, never a finalization,
/// and which boot recovery deposits. Otherwise the claim is taken and finalization runs detached,
/// exactly as for a completing chunk: the caller answers the session as it stands,
/// `waiting_for_processing`, and never waits for the outcome.
async fn finish_if_full(
    upload: &UploadContext,
    attestation: &crate::attestation::AttestationContext,
    record: &UploadSessionRecord,
) -> Option<Result<finalize::Started, FinalizeFailure>> {
    if !record.is_full_and_unclaimed() || record.is_guest_drop() {
        return None;
    }
    tracing::info!(upload_id = %record.upload_id, "starting the finalization of a full session nobody claimed");
    Some(finalize::start(upload, attestation, &record.upload_id).await)
}

/// Charge a just-opened session's address again, after a create charged it `AlreadyAttributed`.
///
/// Still attributed is the common answer and changes nothing. Admitted means the attribution
/// the create leaned on was released while it opened, and this session now pays for its bytes.
/// A refusal or a failure to answer means the session may not hold bytes it is not charged for,
/// so it is undone — stage and record — before the refusal is returned.
async fn recharge_after_open(
    upload: &UploadContext,
    quota: &crate::quota::QuotaContext,
    upload_id: &UploadId,
    uploader: &UserId,
    address: &ContentAddress,
    size: u64,
) -> Result<(), CreateRejection> {
    let rejection = match crate::quota::charge_upload(quota, uploader, address, size).await {
        Ok(crate::quota::UploadCharge::AlreadyAttributed) => return Ok(()),
        Ok(crate::quota::UploadCharge::Admitted) => {
            tracing::info!(
                %upload_id,
                size,
                "the attribution a create leaned on was released while it opened; charged it"
            );
            return Ok(());
        }
        Ok(crate::quota::UploadCharge::Refused) => CreateRejection::quota_exceeded(),
        Ok(crate::quota::UploadCharge::Suspended) => CreateRejection::QuotaSuspended {
            code: error_codes::QUOTA_SUSPENDED,
        },
        Err(error) => {
            store_unavailable(&error, "re-charge an opened upload against its quota");
            CreateRejection::unavailable(upload.clock())
        }
    };
    tracing::info!(%upload_id, "an opened session could not be charged; undoing it");
    if let Err(error) = upload.blobs().abandon(upload_id).await {
        tracing::error!(%error, %upload_id, "an uncharged session's stage could not be dropped");
    }
    if let Err(error) = upload.sessions().discard(upload_id).await {
        tracing::error!(%error, %upload_id, "an uncharged session could not be discarded");
    }
    Err(rejection)
}

/// `existing` as the finish [`finish_if_full`] ran left it, for a duplicate create's answer.
///
/// A session gone since the finish — swept at its cap — is answered from `existing`: its
/// identity and size are unchanged, and the client's next request learns it is gone.
async fn reread_finished(
    upload: &UploadContext,
    existing: &UploadSessionRecord,
) -> Result<UploadSessionRecord, CreateRejection> {
    match upload.sessions().read(&existing.upload_id).await {
        Ok(Some(current)) => Ok(current),
        Ok(None) => {
            tracing::debug!(upload_id = %existing.upload_id, "a finished session was gone on re-read");
            Ok(existing.clone())
        }
        Err(error) => {
            store_unavailable(&error, "re-read a finished session");
            Err(CreateRejection::unavailable(upload.clock()))
        }
    }
}

/// Report a session's progress and state.
///
/// The resumption primitive: a client that lost a connection, an acknowledgement, or a process
/// asks here and learns the authoritative offset, the declared length and the session's state.
/// The answer carries **no body** — HTTP forbids one on `HEAD`, which is why the protocol puts
/// all three on headers.
///
/// A session that holds every byte and whose finalization nobody claimed has its finalization
/// started before it is reported: a client resuming at an offset equal to the size sends nothing
/// more, so this may be the last request it makes about the session. The finalization is never
/// awaited — the answer is the state as it stands, `waiting_for_processing` — and the client
/// polls this operation, with backoff, until the state is terminal. A `failed_processing` state
/// carries the failure's `error.*` code in `X-Capsule-Upload-Error`.
#[kynos::head("/v1/upload/{id}", operation_id = "head_upload", tag = UploadTag)]
pub async fn head_upload(
    Auth(credential): Auth<AccessToken>,
    Inject(upload): Inject<UploadContext>,
    Inject(attestation): Inject<crate::attestation::AttestationContext>,
    Path(path): Path<UploadPath>,
) -> Result<WithHeaders<HeadReply, HeadHeaders>, SessionRejection> {
    let mut record = session_for(&upload, &path, &credential.user).await?;
    if let Some(started) = finish_if_full(&upload, &attestation, &record).await {
        // Whatever the start did, the record now says so — claimed, or still full and unclaimed
        // when the claim could not be taken — and that is the answer, not the start's shape.
        if let Err(failure) = started {
            tracing::info!(upload_id = %record.upload_id, %failure, "a full session reported by HEAD could not be claimed");
        }
        record = upload
            .sessions()
            .read(&record.upload_id)
            .await
            .map_err(|error| {
                store_unavailable(&error, "re-read a finalized session");
                SessionRejection::unavailable(upload.clock())
            })?
            .ok_or_else(SessionRejection::not_found)?;
    }

    let expiry = SessionExpiry::of(&record, upload.sessions().ttl(), upload.clock().now());
    Ok(WithHeaders::new(
        HeadReply::Progress,
        HeadHeaders {
            offset: record.received_bytes,
            content_length: record.total_size,
            upload_status: record.status.as_str().to_owned(),
            advice: Advice::for_size(record.total_size).to_string(),
            expires_in: expiry.expires_in,
            idle_expires_in: expiry.idle_expires_in,
            upload_error: failure_code_of(&record),
            // A cached offset is a client that re-sends bytes the server already has, or skips
            // bytes it does not.
            cache_control: "no-store".to_owned(),
        },
    ))
}

/// Cancel a session: its record, its accepted chunks and its staged bytes, together.
///
/// Refused while finalization is running — it is not interruptible — and refused once the
/// session is terminal, because there is nothing left to cancel and the receipt is what a
/// client should read instead.
#[kynos::delete("/v1/upload/{id}", operation_id = "cancel_upload", tag = UploadTag)]
pub async fn cancel_upload(
    Auth(credential): Auth<AccessToken>,
    Inject(upload): Inject<UploadContext>,
    Path(path): Path<UploadPath>,
) -> Result<NoContent, CancelRejection> {
    let record = session_for(&upload, &path, &credential.user).await?;

    if !record.status.is_active() || record.status == UploadSessionStatus::WaitingForProcessing {
        return Err(CancelRejection::not_active());
    }

    let id = record.upload_id.clone();
    // The bytes first: a discarded record with a stage still on disk is an orphan the scrub has
    // to find, while a dropped stage with a live record is a session whose next chunk fails
    // loudly. Neither is good, and this order is the one the scrub already covers.
    if let Err(error) = upload.blobs().abandon(&id).await {
        tracing::error!(%error, upload_id = %id, "a cancelled upload's stage could not be dropped");
    }
    upload.sessions().discard(&id).await.map_err(|error| {
        store_unavailable(&error, "discard a cancelled session");
        CancelRejection::unavailable(upload.clock())
    })?;

    // The reservation goes back, unless the stored bytes or another of the uploader's sessions
    // still hold it. Best-effort and logged rather than fatal: the session is already gone, so
    // failing the response here would tell a client its cancellation did not happen when it did.
    crate::upload::reservation::release_logged(&upload, &record).await;

    tracing::info!(upload_id = %id, "cancelled an upload session");
    Ok(NoContent)
}

// ===========================================================================================
// Helpers
// ===========================================================================================

/// The `error.*` code a `failed_processing` session reports, and nothing for any other state.
///
/// A code is only meaningful beside the status it explains, so a record in any other state
/// reports none whatever it holds.
pub(crate) fn failure_code_of(record: &UploadSessionRecord) -> Option<String> {
    (record.status == UploadSessionStatus::FailedProcessing)
        .then(|| record.failure_code.clone())
        .flatten()
}

/// The session `path` names, if this caller may see it.
///
/// The uploader *or* the owner may look and cancel; only the uploader may append, which is why
/// [`append_chunk`] does its own narrower check rather than calling this.
async fn session_for(
    upload: &UploadContext,
    path: &UploadPath,
    caller: &UserId,
) -> Result<UploadSessionRecord, SessionRejection> {
    let id = UploadId::new(path.id.clone());
    let record = upload
        .sessions()
        .read(&id)
        .await
        .map_err(|error| {
            store_unavailable(&error, "read an upload session");
            SessionRejection::unavailable(upload.clock())
        })?
        .ok_or_else(SessionRejection::not_found)?;

    if record.upload_user_id != *caller && record.owner_id.as_str() != caller.as_str() {
        tracing::info!(
            upload_id = %id,
            %caller,
            code = error_codes::UPLOAD_SESSION_NOT_FOUND,
            "a session was hidden: neither uploader nor owner"
        );
        return Err(SessionRejection::not_found());
    }
    Ok(record)
}

/// Check a declared `owner_id` against the namespace the authority filed the write under.
///
/// The owner is **not** taken from the request: it is the album's own, answered by the write
/// authority from the album record, and an uploader who is a writer member of somebody else's
/// album is filed under that owner whether or not they said so. What the request may
/// do is *agree* — name the album owner, or, when the uploader is the owner, themselves. Naming
/// anyone else is refused: a member declaring their own account as owner would be asking for an
/// asset the owner's feed never carries, and a stranger's declaration is not a permission.
fn resolve_owner(
    uploader: &UserId,
    owner: &OwnerId,
    declared: Option<&str>,
) -> Result<(), CreateRejection> {
    match declared {
        None => Ok(()),
        Some(named) if named == owner.as_str() => Ok(()),
        Some(_) => {
            tracing::info!(%uploader, %owner, "an upload was refused: the declared owner is not the album's");
            Err(CreateRejection::owner_not_permitted())
        }
    }
}

/// The uploader's live session for this `(owner, hash, album)` tuple, if one is open.
fn active_session_for<'a>(
    mut live: impl Iterator<Item = &'a UploadSessionRecord>,
    owner: &OwnerId,
    hash: &str,
    album: &AlbumId,
) -> Option<&'a UploadSessionRecord> {
    live.find(|record| {
        record.status.is_active()
            && &record.owner_id == owner
            && record.expected_hash == hash
            && record.album_id.as_ref() == Some(album)
    })
}

/// Why a charge against an upload budget did not admit the request.
enum Throttled {
    /// The budget is spent, or the limiter is holding as many accounts as it will: wait until
    /// this Unix second.
    Until(u64),
    /// The limiter could not answer. Refused rather than waved through.
    Unavailable,
}

/// Charge the account's budget and then the source address's, stopping at the first refusal.
///
/// Ordered, never concurrent: a request the account's own budget refuses must not spend the
/// address's, or one account flooding from a shared address would lock every other account
/// behind it out (transfer/upload.md, Hostile Clients).
async fn charge_both(
    counters: &CounterContext,
    account: (&CounterKey, Budget),
    source: (&CounterKey, Budget),
) -> Result<(), Throttled> {
    charge(counters, account.0, account.1).await?;
    charge(counters, source.0, source.1).await
}

/// Charge one hit against `key`'s budget.
///
/// Fail-closed, as every limiter on this server is: a limiter that cannot answer refuses. A full
/// partition is the limiter working and is a `429` like a spent budget, never the `503` a broken
/// store gets.
async fn charge(
    counters: &CounterContext,
    key: &CounterKey,
    budget: Budget,
) -> Result<(), Throttled> {
    let verdict = counters.hit(key, budget).await.map_err(|error| {
        if let Some(retry_after) = counters.capacity_refusal(&error, budget) {
            tracing::warn!(%error, counter = key.as_str(), "an upload limiter is at capacity");
            Throttled::Until(unix_seconds(retry_after))
        } else {
            tracing::error!(%error, counter = key.as_str(), "an upload limiter could not answer");
            Throttled::Unavailable
        }
    })?;
    match verdict {
        Verdict::Admitted { .. } => Ok(()),
        Verdict::Limited { retry_after } => {
            tracing::info!(
                counter = key.as_str(),
                scope = key.scope(),
                %retry_after,
                code = error_codes::UPLOAD_RATE_LIMITED,
                "an upload request was throttled"
            );
            Err(Throttled::Until(unix_seconds(retry_after)))
        }
    }
}

/// The body both create replies carry.
fn describe(upload_id: &UploadId, total_size: u64) -> CreateUploadResponse {
    CreateUploadResponse {
        id: upload_id.to_string(),
        upload_url: session_url(upload_id),
        suggested_chunk_size: suggested_chunk_size(total_size),
    }
}

/// Where a session's chunks are sent.
fn session_url(upload_id: &UploadId) -> String {
    format!("/v1/upload/{upload_id}")
}

/// A new upload identifier.
///
/// UUIDv7: an upload id is a new identifier whose creation time is not a secret — the session
/// carries `created_at` in the clear beside it — and its hyphenated spelling is one every blob
/// adapter can name a staged file with.
fn new_upload_id() -> UploadId {
    UploadId::new(Uuid::now_v7().to_string())
}

/// Drop a session's bytes, mark it failed and give its reservation back, for a chunk that
/// broke the declaration.
async fn abandon_failed(upload: &UploadContext, id: &UploadId) {
    if let Err(error) = upload.blobs().abandon(id).await {
        tracing::error!(%error, upload_id = %id, "a failed upload's stage could not be dropped");
    }
    match upload
        .sessions()
        .set_status(id, UploadSessionStatus::FailedProcessing)
        .await
    {
        Ok(Some(failed)) => crate::upload::reservation::release_logged(upload, &failed).await,
        Ok(None) => tracing::debug!(upload_id = %id, "a failed upload's session was already gone"),
        Err(error) => {
            tracing::error!(%error, upload_id = %id, "a failed upload's session could not be marked");
        }
    }
}

/// One log line for every store failure, so a support report can name the operation.
fn store_unavailable(error: &StoreError, doing: &'static str) {
    tracing::error!(%error, operation = doing, "the upload session store could not answer");
}

impl CreateRejection {
    /// The upload would cross the hard quota limit.
    fn quota_exceeded() -> Self {
        Self::QuotaExceeded {
            code: error_codes::QUOTA_EXCEEDED,
        }
    }

    /// The owner already holds these bytes, in the asset named.
    fn duplicate_blob(existing: &AssetId) -> Self {
        Self::DuplicateBlob {
            existing_asset: existing.to_string(),
            code: error_codes::UPLOAD_DUPLICATE_BLOB,
        }
    }

    /// Map the gate's verdict onto the taxonomy's status and code.
    fn from_gate(reject: GateReject) -> Self {
        let invalid = |code, detail: &str| Self::Invalid {
            detail: detail.to_owned(),
            code,
        };
        match reject {
            GateReject::ProtocolMalformed => invalid(
                error_codes::UPLOAD_MALFORMED_REQUEST,
                "protocol_version is not a YYYY-MM-DD date",
            ),
            GateReject::ProtocolOutOfRange => Self::ProtocolUnsupported {
                code: error_codes::PROTOCOL_VERSION_UNSUPPORTED,
            },
            GateReject::UnknownCryptoSuite => invalid(
                error_codes::UPLOAD_UNKNOWN_CRYPTO_SUITE,
                "crypto_suite_id is not in this server's inventory",
            ),
            GateReject::InvalidHash => invalid(
                error_codes::UPLOAD_INVALID_HASH,
                "hash is not lowercase hex of the suite's digest length",
            ),
            GateReject::InvalidSize => invalid(
                error_codes::UPLOAD_INVALID_SIZE,
                "size must be greater than zero",
            ),
            GateReject::FileTooLarge => Self::FileTooLarge {
                code: error_codes::UPLOAD_FILE_TOO_LARGE,
            },
            GateReject::UnsupportedContentType => invalid(
                error_codes::UPLOAD_UNSUPPORTED_CONTENT_TYPE,
                "content_type is outside the closed enum this protocol version fixes",
            ),
            GateReject::EnvelopeMismatch(field) => Self::Invalid {
                detail: format!("{field} contradicts the manifest envelope"),
                code: error_codes::UPLOAD_ENVELOPE_MISMATCH,
            },
            GateReject::AlbumPinMismatch => Self::album_access_denied(),
            GateReject::DeviceNotAuthorized => Self::device_not_authorized(),
            GateReject::TimestampOutOfRange => invalid(
                error_codes::UPLOAD_TIMESTAMP_OUT_OF_RANGE,
                "the manifest timestamp is outside the accepted range",
            ),
            GateReject::ActionNotAllowed => invalid(
                error_codes::UPLOAD_INVALID_ACTION,
                "an upload session may only be opened for a create or a replace",
            ),
            GateReject::ReplaceDoesNotChain => invalid(
                error_codes::UPLOAD_INVALID_ACTION,
                "a replace carries no prior_provenance_hash, and every non-create action chains",
            ),
            GateReject::ReplaceIncomplete(field) => Self::Invalid {
                detail: format!(
                    "a replace's manifest must name the bundle it commits to; {field} is absent"
                ),
                code: error_codes::UPLOAD_ENVELOPE_MISMATCH,
            },
            GateReject::UnknownDerivativeFormat(format) => Self::Invalid {
                detail: format!(
                    "derivative_format {format:?} is outside the closed still-derivative format set"
                ),
                code: error_codes::UPLOAD_UNKNOWN_DERIVATIVE_FORMAT,
            },
            // Decided only at finalization, which holds the manifest's bytes; a create never
            // sees them. Mapped rather than unwrapped so the match stays total.
            GateReject::SignatureUnverified => invalid(
                error_codes::UPLOAD_ENVELOPE_REJECTED,
                "the manifest does not verify under its signing device's published key",
            ),
        }
    }

    fn album_access_denied() -> Self {
        Self::Forbidden {
            detail: "the album is missing, closed, or not writable by this owner".to_owned(),
            code: error_codes::UPLOAD_ALBUM_ACCESS_DENIED,
        }
    }

    fn device_not_authorized() -> Self {
        Self::Forbidden {
            detail: "the creating device is not authorized in the uploader's directory".to_owned(),
            code: error_codes::UPLOAD_DEVICE_NOT_AUTHORIZED,
        }
    }

    fn owner_not_permitted() -> Self {
        Self::Forbidden {
            detail: "the declared owner is not the album's owner".to_owned(),
            code: error_codes::UPLOAD_OWNER_NOT_PERMITTED,
        }
    }

    fn rate_limited(retry_after: u64) -> Self {
        Self::RateLimited {
            code: error_codes::UPLOAD_RATE_LIMITED,
            retry_after,
        }
    }

    fn unavailable(clock: &dyn Clock) -> Self {
        Self::Unavailable {
            code: error_codes::UPLOAD_UNAVAILABLE,
            retry_after: retry_at(clock, UNAVAILABLE_RETRY),
        }
    }
}

impl ChunkRejection {
    fn invalid(code: &'static str, detail: &str) -> Self {
        Self::Invalid {
            detail: detail.to_owned(),
            code,
        }
    }

    fn missing_offset() -> Self {
        Self::invalid(
            error_codes::UPLOAD_MISSING_OFFSET,
            "X-Capsule-Offset must be a decimal byte offset",
        )
    }

    fn missing_checksum() -> Self {
        Self::invalid(
            error_codes::UPLOAD_MISSING_CHECKSUM,
            "X-Capsule-Checksum must be the chunk's SHA-256 as bare lowercase hex",
        )
    }

    fn checksum_mismatch() -> Self {
        Self::invalid(
            error_codes::UPLOAD_CHECKSUM_MISMATCH,
            "the chunk does not hash to its declared checksum; nothing was written",
        )
    }

    fn empty_chunk() -> Self {
        Self::invalid(
            error_codes::UPLOAD_EMPTY_CHUNK,
            "an empty chunk is a client bug, never a no-op",
        )
    }

    fn chunk_not_aligned() -> Self {
        Self::invalid(
            error_codes::UPLOAD_CHUNK_NOT_ALIGNED,
            "every chunk but the final one must be a multiple of 4 KiB",
        )
    }

    fn size_exceeded() -> Self {
        Self::invalid(
            error_codes::UPLOAD_SIZE_EXCEEDED,
            "the chunk would carry the upload past its declared size",
        )
    }

    fn offset_mismatch(offset: u64) -> Self {
        Self::OffsetMismatch {
            offset,
            code: error_codes::UPLOAD_OFFSET_MISMATCH,
        }
    }

    fn chunk_conflict() -> Self {
        Self::ChunkConflict {
            code: error_codes::UPLOAD_CHUNK_CONFLICT,
        }
    }

    fn session_not_active() -> Self {
        Self::SessionNotActive {
            code: error_codes::UPLOAD_SESSION_NOT_ACTIVE,
        }
    }

    fn session_not_found() -> Self {
        Self::SessionNotFound {
            code: error_codes::UPLOAD_SESSION_NOT_FOUND,
        }
    }

    fn chunk_too_large() -> Self {
        Self::ChunkTooLarge {
            code: error_codes::UPLOAD_CHUNK_TOO_LARGE,
        }
    }

    fn storage_inconsistent() -> Self {
        Self::StorageInconsistent {
            code: error_codes::UPLOAD_STORAGE_INCONSISTENT,
        }
    }

    fn unavailable(clock: &dyn Clock) -> Self {
        Self::Unavailable {
            code: error_codes::UPLOAD_UNAVAILABLE,
            retry_after: retry_at(clock, UNAVAILABLE_RETRY),
        }
    }
}
