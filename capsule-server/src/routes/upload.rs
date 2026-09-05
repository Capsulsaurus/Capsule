//! `POST /v1/upload`, `PATCH`, `HEAD` and `DELETE /v1/upload/{id}` — the upload session's life.
//!
//! The port of the Salvo `capsule-api/upload` surface onto Kynos (slice `S-C1`). What carries
//! over is the *contract*: the envelope gate ahead of every write, the chunk rules, the
//! idempotency tuple, the finalization order. What does not carry over is the Salvo handler,
//! its response writers, and its three-writes-per-chunk accounting.
//!
//! # The status audit (`S-C28`)
//!
//! Kynos makes an undeclared status unrepresentable — the status *is* the return type — so
//! every status the Salvo surface could render was audited as this port was written. The
//! verdict lives in the types; here it is in one table.
//!
//! | Salvo | Verdict here |
//! | --- | --- |
//! | create `201` | kept — [`CreateReply::Created`], with `Location` and `X-Capsule-Suggested-Chunk-Size` |
//! | create `200` (active session for the tuple) | **kept and now documented.** Salvo declared it `undocumented()`; it is [`CreateReply::Existing`], carrying `X-Capsule-Offset` so a resuming client needs no second round trip |
//! | create `400` "Bad request" (untyped) | **deleted.** Never constructed with a code; the 400 a malformed body actually produces is the `Json` extractor's, which Kynos declares |
//! | create `401` | kept, and now the framework's — `Auth<AccessToken>` declares it and fills the `WWW-Authenticate` challenge |
//! | create `403` | kept — album access, device authorization, a declared owner that is not the album's |
//! | create `409 duplicate_blob` | **restored, with `S-C22`'s structured `existing_asset`.** It was deleted while this crate had no asset index, because it must name the existing asset and answering from blob presence alone would tell one account what another holds. `S-C37` answers it honestly and owner-scoped |
//! | create `413` | kept — the declared size past the deployment ceiling |
//! | create `426` | kept — the manifest envelope's `protocol_version` pin, refused by the envelope gate. The *header* handshake is no longer this surface's: [`crate::negotiation::ProtocolGate`] answers it before the handler runs, and the accepted window rides `X-Capsule-Protocol-Min`/`-Max` on every response |
//! | create `500` | kept — a collaborator that could not answer, with `error.upload.unavailable` |
//! | chunk `204` | kept — with the authoritative `X-Capsule-Offset` |
//! | chunk `400` | kept, and now *coded*: missing offset, missing checksum, checksum mismatch, empty chunk, misalignment, size exceeded, and the two finalization failures each carry their own `error.upload.*` |
//! | chunk `403` | kept — only the uploader may append |
//! | chunk `404` | kept — unknown, expired and discarded sessions are one answer |
//! | chunk `409` | kept — offset mismatch (carrying the authoritative offset), chunk conflict, session not active |
//! | chunk `413` | kept — the 16 MiB protocol ceiling, reachable because the transport backstop sits above it (`S-C33`) |
//! | chunk `415` | kept, and it keeps its code. Kynos's own `Binary` rejection declares `400`, `415` *and* a `422` a raw-bytes body cannot produce, and carries no `error.*` code; [`ChunkBody`] delegates the enforcement to `Binary` and replaces only the rejection, so the `415` is `error.upload.unsupported_media_type` and the phantom `422` is gone |
//! | chunk `409 finalize_in_progress` | **deleted.** Losing the finalize claim is a normal race and the chunk that triggered it was still accepted, so it answers `204`. Telling a client its accepted chunk failed was the Salvo behaviour and it was wrong |
//! | chunk `500` | kept — storage inconsistency (the stage disagreeing with the counter) and collaborator failure |
//! | head `200` | kept — [`HeadReply::Progress`], carrying offset, declared length and state on headers, with `Cache-Control: no-store`. A `Reply` rather than a `NoContent`, because `200 with headers` and `204` are different answers |
//! | head `400` / `401` / `403` / `404` / `500` | kept; the `403` now covers the owner as well as the uploader, both of whom may look. The `400` is the handshake's, declared by the read gate rather than by this surface; the `426` is gone from `HEAD`, because a read is admitted at any protocol date (issue #404) |
//! | head `409` | **deleted as unreachable.** `HEAD` reports a state, it does not require one. It would have been declared for free by sharing a rejection type with `DELETE`, which is why they are two types |
//! | delete `204` | kept |
//! | delete `409` | kept — finalization is not interruptible, and a terminal session has nothing left to cancel |
//! | delete `400` / `401` / `403` / `404` / `426` / `500` | kept |
//! | list sessions (`GET /upload/sessions`) | **not ported in this pass.** `HEAD` is the resumption primitive the protocol names and it is here; the listing is for cross-restart discovery and nothing in the tree consumes it yet |
//! | receipt (`GET /upload/{id}/receipt`) | out of scope: `S-C15` owns custody receipts |
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
//! # `409 duplicate_blob` refuses, and nothing yet adopts
//!
//! The status says "you already hold these bytes" and names the asset that holds them. For a
//! **retry** — the same asset, the same blob, a lost `201` — that is a complete answer. For
//! genuine cross-asset deduplication, where a second asset legitimately shares a thumbnail with
//! a first, it is only half of one: the requesting asset is refused a session and has no way to
//! record the blob it now knows exists, so its feed entry will not list it.
//!
//! That gap is created by restoring the contracted status, not hidden by it. The alternative —
//! silently recording the existing blob against the requesting asset and answering `200` — is a
//! new reply variant and a wire-contract decision, and the idempotency table this surface is
//! written against specifies the `409`. Filed rather than improvised.
//!
//! # What this port does not have
//!
//! **Quota and the custody receipt**, owned by `S-C6` and `S-C15`; see [`crate::upload`].

use capsule_core::crypto::hash::hash_bytes;
use capsule_i18n::error_codes;
use kynos::extract::params::header::Headers;
use kynos::prelude::*;
use kynos::response::headers::WithHeaders;
use kynos::response::status::NoContent;
use kynos::security::auth::Auth;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::AccessToken;
use crate::blob::ContentAddress;
use crate::store::{
    AlbumId, AssetId, BlobRole, OwnerId, StoreError, UploadId, UploadSessionRecord,
    UploadSessionStatus, UserId,
};
use crate::upload::body::ChunkBody;
use crate::upload::chunk::{
    self, MAX_CHUNK_BYTES, parse_checksum, parse_offset, suggested_chunk_size,
};
use crate::upload::envelope::{DeclaredBlob, GateContext, GateReject, ManifestEnvelope};
use crate::upload::finalize::{self, FinalizeFailure, Outcome};
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
#[serde(rename_all = "lowercase")]
pub enum WireBlobRole {
    /// The encrypted original.
    Original,
    /// An encrypted derivative — a thumbnail or a preview.
    Derivative,
    /// The encrypted CBOR metadata blob.
    Metadata,
    /// The signed manifest, stored verbatim (`S-C30`).
    Provenance,
    /// A backup copy.
    Backup,
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
    /// when the uploader is a writer on its roster (`S-C51`). A declared owner that is anyone
    /// else, the uploading member included, is refused `error.upload.owner_not_permitted`.
    pub owner_id: Option<String>,
    /// The album-upgrade intent this write belongs to, when it belongs to one.
    ///
    /// Carried onto the session verbatim and read by nobody in this port; the ceremony that
    /// gives it meaning is `S-C24`.
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
    /// A fresh session. `Location` names it and `X-Capsule-Suggested-Chunk-Size` sizes the
    /// first chunk.
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
/// Every field is optional because the two replies carry different subsets and a Kynos header
/// group is described across all of a reply's statuses; a required header that one status
/// omitted would be a promise the document could not keep.
#[derive(HeaderParams)]
pub struct CreateHeaders {
    /// Where the session lives.
    #[header(rename = "Location")]
    location: Option<String>,
    /// The starting chunk size.
    #[header(rename = "X-Capsule-Suggested-Chunk-Size")]
    suggested_chunk_size: Option<u64>,
    /// The authoritative offset, on a resumed session.
    #[header(rename = "X-Capsule-Offset")]
    offset: Option<u64>,
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

/// The offset a chunk's acknowledgement carries.
#[derive(HeaderParams)]
pub struct OffsetHeader {
    /// The next byte the server expects.
    #[header(rename = "X-Capsule-Offset")]
    offset: u64,
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
/// Every field is required and always sent: a resumption primitive that sometimes omits the
/// offset would be one a client cannot rely on.
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
    /// The album is quiescing under an upgrade ceremony this write does not name (`S-C24`).
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

    /// The account is suspended (`S-C8`).
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
    /// The structured `existing_asset` is `S-C22`'s deliverable: the English detail is a
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

    /// A collaborator could not answer, so the session neither opened nor was refused.
    #[error("the upload session could not be opened")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Why a chunk was not accepted, or the finalization it triggered did not commit.
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

    /// Only the uploader may append to a session.
    #[error("this session belongs to another uploader")]
    #[problem(status = 403, title = "Not the uploader")]
    Forbidden {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// Unknown, expired or discarded — deliberately one answer.
    ///
    /// The client's recovery is identical for all three: re-create the session and send that
    /// blob again. Distinguishing them would publish which identifiers once existed.
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

    /// A `replace`'s manifest arrived before the bundle it commits to (`S-C43`).
    ///
    /// Transient, and the only ordering rule this protocol has: a replace mutates an asset that
    /// is already visible, so it is applied as one act at the moment its manifest lands — which
    /// makes the manifest the member that lands **last**. Retrying it once the rest of the
    /// bundle has committed succeeds, so this is a `409` a client acts on rather than a `400`
    /// that says the request was wrong.
    #[error("the replace names bytes the server does not hold yet")]
    #[problem(status = 409, title = "Replace incomplete")]
    ReplaceIncomplete {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A `replace` did not chain onto the asset's current state (`S-C43`).
    ///
    /// Invariant 17's stale revival, invariant 18's epoch regression, or an asset that has been
    /// deleted since the session opened. One answer for all three: the client's move is the same
    /// — re-read the asset and rebase — and distinguishing them would report on state the caller
    /// may not be entitled to.
    #[error("this manifest does not follow the asset's current state")]
    #[problem(status = 409, title = "Stale revival")]
    ReplaceRefused {
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

    /// A collaborator could not answer.
    #[error("the chunk could not be accepted")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Why a request about an existing session was refused.
///
/// Shared by `HEAD` and `DELETE`, which ask the same three questions — does the session exist,
/// may this caller see it, is it in a state that admits this — and differ only in what they do
/// afterwards. Two identical enums would be two places for the answers to drift apart.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum SessionRejection {
    /// The caller is neither the session's uploader nor the owner it files under.
    #[error("this session belongs to another account")]
    #[problem(status = 403, title = "Not this caller's session")]
    Forbidden {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// Unknown, expired or discarded — deliberately one answer.
    #[error("there is no such upload session")]
    #[problem(status = 404, title = "Upload session not found")]
    SessionNotFound {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A collaborator could not answer.
    #[error("the session could not be read")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Why a cancellation was refused.
///
/// [`SessionRejection`]'s five answers plus the one only a cancellation can give. It is a
/// second enum rather than a sixth variant on the shared one because a Kynos rejection type
/// declares its statuses for *every* operation that returns it: a `409` on the shared enum
/// would put a `409` on `HEAD`, which cannot produce one, and a promise nothing keeps is the
/// exact `S-C28` defect this rebuild removes.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum CancelRejection {
    /// The caller is neither the session's uploader nor the owner it files under.
    #[error("this session belongs to another account")]
    #[problem(status = 403, title = "Not this caller's session")]
    Forbidden {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// Unknown, expired or discarded — deliberately one answer.
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
    /// terminal state by the finalizer that claimed it, and a terminal one has nothing left to
    /// cancel — its bytes are either a blob or already gone.
    #[error("this session can no longer be cancelled")]
    #[problem(status = 409, title = "Session not active")]
    SessionNotActive {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A collaborator could not answer.
    #[error("the session could not be cancelled")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

impl CancelRejection {
    fn not_active() -> Self {
        Self::SessionNotActive {
            code: error_codes::UPLOAD_SESSION_NOT_ACTIVE,
        }
    }

    fn unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::UPLOAD_UNAVAILABLE,
        }
    }
}

/// The shared answers, as a cancellation renders them. One arm each, so a code or a status can
/// only change in one place.
impl From<SessionRejection> for CancelRejection {
    fn from(rejection: SessionRejection) -> Self {
        match rejection {
            SessionRejection::Forbidden { code } => Self::Forbidden { code },
            SessionRejection::SessionNotFound { code } => Self::SessionNotFound { code },
            SessionRejection::Unavailable { code } => Self::Unavailable { code },
        }
    }
}

impl SessionRejection {
    fn forbidden() -> Self {
        Self::Forbidden {
            code: error_codes::UPLOAD_FORBIDDEN,
        }
    }

    fn not_found() -> Self {
        Self::SessionNotFound {
            code: error_codes::UPLOAD_SESSION_NOT_FOUND,
        }
    }

    fn unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::UPLOAD_UNAVAILABLE,
        }
    }
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
#[kynos::post("/v1/upload", operation_id = "create_upload", tag = UploadTag)]
pub async fn create_upload(
    Inject(upload): Inject<UploadContext>,
    Inject(quota): Inject<crate::quota::QuotaContext>,
    Inject(moderation): Inject<crate::moderation::ModerationContext>,
    Auth(credential): Auth<AccessToken>,
    Json(request): Json<CreateUploadRequest>,
) -> Result<WithHeaders<CreateReply, CreateHeaders>, CreateRejection> {
    let uploader = credential.user.clone();

    // Account standing (`S-C8`), checked before anything is reserved. A suspension removes the
    // ability to *write*, so refusing here — before a session exists, before a quota is charged
    // — is what keeps a suspended account from leaving half-built state behind that a
    // reinstatement would then have to reconcile.
    let standing = moderation
        .store()
        .standing(&crate::store::UserId::new(uploader.as_str()))
        .await
        .map_err(|error| {
            tracing::error!(%error, %uploader, "the moderation store could not answer");
            CreateRejection::unavailable()
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
            CreateRejection::unavailable()
        })?
    else {
        tracing::info!(%uploader, %album, "an upload was refused: no write capability");
        return Err(CreateRejection::album_access_denied());
    };
    tracing::debug!(%uploader, %owner, ?role, %album, "the album admits this upload");
    // The namespace is the authority's answer; a declared owner may only agree with it.
    resolve_owner(&uploader, &owner, request.owner_id.as_deref())?;

    // Upgrade quiescence (`S-C24`, versioning.md step 2). An album whose members have stopped
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
            CreateRejection::unavailable()
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
    // Two actions ride this surface and no others (`S-C43`): a write that moves blob bytes is an
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
    if let Some(existing) = active_session_for(&upload, &uploader, &owner, &request.hash, &album)
        .await
        .map_err(|error| {
            store_unavailable(&error, "list an uploader's sessions");
            CreateRejection::unavailable()
        })?
    {
        tracing::info!(
            upload_id = %existing.upload_id,
            received_bytes = existing.received_bytes,
            "returning the active session for a duplicate create"
        );
        let body = describe(&existing.upload_id, existing.total_size);
        return Ok(WithHeaders::new(
            CreateReply::Existing(body),
            CreateHeaders {
                location: Some(session_url(&existing.upload_id)),
                suggested_chunk_size: Some(suggested_chunk_size(existing.total_size)),
                offset: Some(existing.received_bytes),
            },
        ));
    }

    // The finalized half of the same rule, and the same key: `(owner_id, hash, album_id)`.
    // Both scopes sit in `find_by_address`'s signature rather than in a caller's discipline —
    // owner because the blob store could say whether *anyone* holds these bytes and answering
    // from that would tell one account what another holds, album because a `409` is the
    // client's *merge* trigger and across two albums there is nothing to merge.
    let address = ContentAddress::parse(&request.hash).map_err(|error| {
        // Unreachable: invariant 3 has already run. Cheap guard on an earlier check's promise.
        tracing::error!(%error, "a gate-passed hash is not a content address");
        CreateRejection::unavailable()
    })?;
    if let Some(existing) = upload
        .index()
        .find_by_address(&owner, &album, &address)
        .await
        .map_err(|error| {
            store_unavailable(&error, "look up a duplicate blob");
            CreateRejection::unavailable()
        })?
    {
        tracing::info!(%owner, existing_asset = %existing, "an upload was refused: already held");
        return Err(CreateRejection::duplicate_blob(&existing));
    }

    // The asset the manifest names — "the same id across the bundle's members" — reserved
    // before a session exists, so every blob of a bundle joins one row rather than minting one.
    let asset_id = AssetId::new(&request.manifest_envelope.file_id);
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
            CreateRejection::unavailable()
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

    // Quota's one hard enforcement point (`S-C6`). After the duplicate check and the
    // reservation, because a refused duplicate must not be charged; before the stage, because a
    // refusal must not leave bytes. The reserved-but-unpublished row a refusal leaves behind
    // publishes nothing and is the discard worker's, exactly as an abandoned session's is.
    match crate::quota::charge_upload(&quota, &uploader, &address, request.size).await {
        Ok(crate::quota::UploadCharge::Admitted) => {}
        Ok(crate::quota::UploadCharge::Refused) => return Err(CreateRejection::quota_exceeded()),
        Err(error) => {
            store_unavailable(&error, "charge an upload against its quota");
            return Err(CreateRejection::unavailable());
        }
    }

    let upload_id = new_upload_id();

    // The stage first, the record second. A stage with no session is an orphan the startup
    // scrub reclaims; a session with no stage would make every chunk a `500` until it expired.
    upload.blobs().begin(&upload_id).await.map_err(|error| {
        tracing::error!(%error, upload_id = %upload_id, "the stage could not be opened");
        CreateRejection::unavailable()
    })?;

    let record = UploadSessionRecord {
        upload_id: upload_id.clone(),
        // The reserved row, above. Taken from the manifest and never minted here: a fresh id
        // per session gave every blob of one bundle a different asset, which made the bundle
        // ungroupable and the visibility gate a conjunction over nothing.
        asset_id,
        owner_id: owner,
        upload_user_id: uploader,
        album_id: Some(album),
        content_type: Some(request.content_type.clone()),
        expected_hash: request.hash.clone(),
        crypto_suite_id: request.crypto_suite_id,
        protocol_version: request.protocol_version.clone(),
        blob_role: request.blob_role.into(),
        intent_id: request.intent_id.clone(),
        // The projection, stored so finalization re-runs the battery over what creation
        // validated. It is never re-encoded into manifest bytes — see `S-C30`.
        manifest_envelope: serde_json::to_string(&request.manifest_envelope).map_err(|error| {
            tracing::error!(%error, "the validated envelope could not be stored");
            CreateRejection::unavailable()
        })?,
        received_bytes: 0,
        total_size: request.size,
        status: UploadSessionStatus::Pending,
        created_at: now,
        last_progress_at: now,
    };

    upload.sessions().open(record).await.map_err(|error| {
        store_unavailable(&error, "open an upload session");
        CreateRejection::unavailable()
    })?;

    tracing::info!(
        upload_id = %upload_id,
        size = request.size,
        blob_role = ?request.blob_role,
        "opened an upload session"
    );
    let body = describe(&upload_id, request.size);
    let suggested = body.suggested_chunk_size;
    Ok(WithHeaders::new(
        CreateReply::Created(body),
        CreateHeaders {
            location: Some(session_url(&upload_id)),
            suggested_chunk_size: Some(suggested),
            offset: None,
        },
    ))
}

/// Append a chunk, and finalize when it completes the declared size.
///
/// Every rule the [chunk
/// contract](../../../capsule-docs/src/content/docs/design/import/upload-protocol.md) fixes is
/// checked before a byte is written, and the checksum is verified against the received bytes
/// *first*, so a chunk corrupted in transit persists nothing.
#[kynos::patch("/v1/upload/{id}", operation_id = "append_chunk", tag = UploadTag)]
pub async fn append_chunk(
    Inject(upload): Inject<UploadContext>,
    Inject(attestation): Inject<crate::attestation::AttestationContext>,
    Auth(credential): Auth<AccessToken>,
    Path(path): Path<UploadPath>,
    Headers(headers): Headers<ChunkHeaders>,
    body: ChunkBody,
) -> Result<WithHeaders<NoContent, OffsetHeader>, ChunkRejection> {
    let id = UploadId::new(path.id);
    let record = upload
        .sessions()
        .read(&id)
        .await
        .map_err(|error| {
            store_unavailable(&error, "read an upload session");
            ChunkRejection::unavailable()
        })?
        .ok_or_else(ChunkRejection::session_not_found)?;

    // Only the uploader appends. The owner may look, but the resuming party is the one that
    // holds the bytes.
    if record.upload_user_id != credential.user {
        tracing::info!(upload_id = %id, "a chunk was refused: not this session's uploader");
        return Err(ChunkRejection::forbidden());
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

    // The rules themselves live in `upload::chunk`, shared verbatim with the guest-drop path
    // (`S-C5`): invariants 9–12 mean the same thing on both surfaces, and two copies would
    // drift on exactly the case a client hits after losing a connection. What stays here is
    // what genuinely differs — who is allowed to append, and how a refusal is spelled on the
    // wire.
    let accepted = chunk::append(
        upload.sessions(),
        upload.blobs(),
        upload.clock(),
        &record,
        offset,
        bytes,
        &received_hash,
    )
    .await
    .map_err(|error| {
        store_unavailable(&error, "append a chunk");
        ChunkRejection::unavailable()
    })?;

    let next_offset = match accepted {
        chunk::Accepted::Advanced {
            next_offset,
            complete,
        } => {
            if complete {
                match finalize::finalize(&upload, &attestation, &id).await {
                    Ok(Outcome::Committed { .. } | Outcome::AlreadyClaimed) => {}
                    Ok(Outcome::NotFound) => return Err(ChunkRejection::session_not_found()),
                    Err(failure) => return Err(ChunkRejection::from(failure)),
                }
            }
            next_offset
        }
        chunk::Accepted::Replayed { next_offset } => next_offset,
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
        // Expired, discarded, or cancelled between the read and the write. The staged bytes are
        // now an orphan the scrub reclaims.
        chunk::Accepted::SessionGone => return Err(ChunkRejection::session_not_found()),
    };

    Ok(WithHeaders::new(
        NoContent,
        OffsetHeader {
            offset: next_offset,
        },
    ))
}

/// Report a session's progress and state.
///
/// The resumption primitive: a client that lost a connection, an acknowledgement, or a process
/// asks here and learns the authoritative offset, the declared length and the session's state.
/// The answer carries **no body** — HTTP forbids one on `HEAD`, which is why the protocol puts
/// all three on headers.
#[kynos::head("/v1/upload/{id}", operation_id = "head_upload", tag = UploadTag)]
pub async fn head_upload(
    Inject(upload): Inject<UploadContext>,
    Auth(credential): Auth<AccessToken>,
    Path(path): Path<UploadPath>,
) -> Result<WithHeaders<HeadReply, HeadHeaders>, SessionRejection> {
    let record = session_for(&upload, &path, &credential.user).await?;

    Ok(WithHeaders::new(
        HeadReply::Progress,
        HeadHeaders {
            offset: record.received_bytes,
            content_length: record.total_size,
            upload_status: record.status.as_str().to_owned(),
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
    Inject(upload): Inject<UploadContext>,
    Inject(quota): Inject<crate::quota::QuotaContext>,
    Auth(credential): Auth<AccessToken>,
    Path(path): Path<UploadPath>,
) -> Result<NoContent, CancelRejection> {
    let record = session_for(&upload, &path, &credential.user).await?;

    if !record.status.is_active() || record.status == UploadSessionStatus::WaitingForProcessing {
        return Err(CancelRejection::not_active());
    }

    let id = record.upload_id;
    // The bytes first: a discarded record with a stage still on disk is an orphan the scrub has
    // to find, while a dropped stage with a live record is a session whose next chunk fails
    // loudly. Neither is good, and this order is the one the scrub already covers.
    if let Err(error) = upload.blobs().abandon(&id).await {
        tracing::error!(%error, upload_id = %id, "a cancelled upload's stage could not be dropped");
    }
    upload.sessions().discard(&id).await.map_err(|error| {
        store_unavailable(&error, "discard a cancelled session");
        CancelRejection::unavailable()
    })?;

    // The reservation goes back (`S-C6`). Best-effort and logged rather than fatal: the session
    // is already gone, so failing the response here would tell a client its cancellation did
    // not happen when it did. A stranded attribution is what the scrub reconciles.
    if let Ok(address) = ContentAddress::parse(&record.expected_hash)
        && let Err(error) = quota
            .quotas()
            .release(&record.upload_user_id, &address)
            .await
    {
        tracing::error!(%error, upload_id = %id, "a cancelled upload's quota was not released");
    }

    tracing::info!(upload_id = %id, "cancelled an upload session");
    Ok(NoContent)
}

// ===========================================================================================
// Helpers
// ===========================================================================================

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
            SessionRejection::unavailable()
        })?
        .ok_or_else(SessionRejection::not_found)?;

    if record.upload_user_id != *caller && record.owner_id.as_str() != caller.as_str() {
        tracing::info!(upload_id = %id, "a session was hidden: neither uploader nor owner");
        return Err(SessionRejection::forbidden());
    }
    Ok(record)
}

/// Check a declared `owner_id` against the namespace the authority filed the write under.
///
/// The owner is **not** taken from the request: it is the album's own, answered by the write
/// authority from the album record, and an uploader who is a writer member of somebody else's
/// album is filed under that owner whether or not they said so (`S-C51`). What the request may
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
async fn active_session_for(
    upload: &UploadContext,
    uploader: &UserId,
    owner: &OwnerId,
    hash: &str,
    album: &AlbumId,
) -> Result<Option<UploadSessionRecord>, StoreError> {
    Ok(upload
        .sessions()
        .sessions_for_uploader(uploader)
        .await?
        .into_iter()
        .find(|record| {
            record.status.is_active()
                && &record.owner_id == owner
                && record.expected_hash == hash
                && record.album_id.as_ref() == Some(album)
        }))
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

/// Drop a session's bytes and mark it failed, for a chunk that broke the declaration.
async fn abandon_failed(upload: &UploadContext, id: &UploadId) {
    if let Err(error) = upload.blobs().abandon(id).await {
        tracing::error!(%error, upload_id = %id, "a failed upload's stage could not be dropped");
    }
    if let Err(error) = upload
        .sessions()
        .set_status(id, UploadSessionStatus::FailedProcessing)
        .await
    {
        tracing::error!(%error, upload_id = %id, "a failed upload's session could not be marked");
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

    fn unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::UPLOAD_UNAVAILABLE,
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

    fn content_hash_mismatch() -> Self {
        Self::invalid(
            error_codes::UPLOAD_CONTENT_HASH_MISMATCH,
            "the stored bytes do not hash to the declared content hash",
        )
    }

    fn envelope_rejected() -> Self {
        Self::invalid(
            error_codes::UPLOAD_ENVELOPE_REJECTED,
            "the manifest envelope did not survive re-validation at finalization",
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

    fn forbidden() -> Self {
        Self::Forbidden {
            code: error_codes::UPLOAD_FORBIDDEN,
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

    fn unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::UPLOAD_UNAVAILABLE,
        }
    }
}

impl From<FinalizeFailure> for ChunkRejection {
    fn from(failure: FinalizeFailure) -> Self {
        match failure {
            FinalizeFailure::ContentHashMismatch { .. } => Self::content_hash_mismatch(),
            FinalizeFailure::EnvelopeRejected(_) => Self::envelope_rejected(),
            FinalizeFailure::StorageInconsistent { .. } => Self::storage_inconsistent(),
            FinalizeFailure::ReplaceIncomplete { .. } => Self::ReplaceIncomplete {
                code: error_codes::UPLOAD_REPLACE_INCOMPLETE,
            },
            FinalizeFailure::ReplaceRefused { .. } => Self::ReplaceRefused {
                code: error_codes::UPLOAD_STALE_REVIVAL,
            },
            FinalizeFailure::Unavailable(_) => Self::unavailable(),
        }
    }
}
