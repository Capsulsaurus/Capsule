//! Guest drops over the wire (slice `S-C5`) — the link, the deposit, and the adoption.
//!
//! [`crate::drop`] owns the store, the caps, and the reasons adoption is claimed rather than
//! taken. This module is the wire shape and its refusals.
//!
//! # Two audiences on one surface
//!
//! `/d/{opaque-id}` and its chunk path take **no credential**: the party using them is a guest
//! with no account. Everything under `/v1/drops` takes one, because it is the provisioning
//! owner reviewing what arrived. The two never overlap — a guest cannot list an inbox, and an
//! owner does not deposit through their own link.
//!
//! # Chunks are the album path's, verbatim
//!
//! `PATCH /d/{opaque-id}/{upload_id}` runs [`crate::upload::chunk::append`] — the same function
//! `PATCH /v1/upload/{id}` runs. Invariants 9–12 mean the same thing on both surfaces, and the
//! contract says so in as many words. What differs is authorization: an album chunk is the
//! session's uploader, a drop chunk is whoever holds the link, and that one difference is the
//! only thing this module reimplements.
//!
//! # `S-C28` audit
//!
//! | Status | Verdict |
//! | --- | --- |
//! | provision `201` / revoke `204` / discard `204` | the owner's operations |
//! | create `201` | a drop session, with its suggested chunk size |
//! | chunk `204` | staged, with the authoritative offset |
//! | adopt `200` | the asset exists and the inbox row is gone |
//! | create/chunk `404` | **one answer** for a malformed id, an unknown link, an expired one, a revoked one, a spent single-use one, and an unknown session — the guest path carries no credential |
//! | create `409 error.drop.cap_exhausted` | invariant 26: a cumulative cap on an otherwise-live link, which is *not* the indistinguishable `404` because the guest was handed a real link and needs to ask for a new one |
//! | create `413 error.drop.file_too_large` | invariant 28 |
//! | create `400 error.drop.malformed` | invariants 27 and 30 |
//! | create `403 error.quota.exceeded` | invariant 29, charged to the **link owner** |
//! | adopt `404` / `409` | the row is not the caller's, or another adoption holds it |
//! | `500` | a store could not answer |
//!
//! **No `429`.** Invariant 31's two limiters need `S-C32`'s counter. Declaring the status would
//! promise something nothing can produce; the caps bound total damage but not request rate, and
//! saying so is the point.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_core::crypto::hash::hash_bytes;
use capsule_i18n::error_codes;
use kynos::prelude::*;
use kynos::response::status::NoContent;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::AccessToken;
use crate::blob::ContentAddress;
use crate::drop::{Admission, DropContext, InboxEntry, LinkCaps, UploadLinkRecord, is_opaque_id};
use crate::store::{BlobRole, OwnerId, UploadId, UploadSessionRecord, UploadSessionStatus, UserId};
use crate::upload::body::ChunkBody;
use crate::upload::chunk::{self, parse_checksum, parse_offset};

/// The drop surface: upload links, the guest deposit, and the owner's inbox.
#[derive(Tag)]
#[tag(
    name = "drops",
    description = "Guest uploads through a capability link, and the inbox they land in."
)]
pub struct DropsTag;

/// A link the owner is provisioning.
#[derive(Schema, Serialize, Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct ProvisionLinkRequest {
    /// The 128-bit opaque id, 32 lowercase hex characters, from the client's CSPRNG.
    pub opaque_id: String,
    /// The Drop Key's public half, base64. Opaque here — the server never decapsulates.
    pub drop_pubkey: String,
    /// The suite a drop must be sealed under.
    pub crypto_suite_id: u16,
    /// When the link stops accepting drops, RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Cumulative bytes across every drop on this link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_total_bytes: Option<u64>,
    /// How many files the link may deposit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_file_count: Option<u32>,
    /// The largest single file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_file_size: Option<u64>,
    /// Whether the link dies after its first successful drop.
    #[serde(default)]
    pub single_use: bool,
    /// An Argon2id **verifier**, base64, when the link is passphrase-gated.
    ///
    /// A verifier and never a passphrase: this is an abuse gate the server checks, which is why
    /// it is stored here at all — unlike a share link's passphrase, which protects decryption
    /// and which the server never sees in any form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passphrase_verifier: Option<String>,
}

/// Confirmation that a link is live.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ProvisionLinkResponse {
    /// The opaque id, echoed.
    pub opaque_id: String,
}

/// The two ways provisioning answers.
#[derive(Reply)]
pub enum ProvisionLinkReply {
    /// The link is live.
    #[reply(
        status = 201,
        description = "The upload link is provisioned and accepting drops"
    )]
    Created(ProvisionLinkResponse),
}

/// A guest's declared drop.
///
/// **No `album_id`, no `amk_version`, no manifest, no provenance.** `deny_unknown_fields` is
/// what enforces invariant 30's *absence* clause: a drop that names an album is a `400` rather
/// than a field the server quietly ignores, because ignoring it would let a guest believe they
/// had written into an album.
#[derive(Schema, Serialize, Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct CreateDropRequest {
    /// The declared content type, from the link's pinned protocol enum (invariant 27).
    pub content_type: String,
    /// The ciphertext's total length.
    pub size: u64,
    /// The lowercase-hex SHA-256 finalization verifies against.
    pub ciphertext_hash: String,
    /// `K` encapsulated to the link's Drop Key, base64. Length fixed by the suite
    /// (invariant 30).
    pub kem_ct: String,
    /// Guest-supplied and unverified. Advisory only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_filename: Option<String>,
}

/// The session a guest uploads into.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CreateDropResponse {
    /// The session id, and the last path segment of the chunk endpoint.
    pub upload_id: String,
    /// The chunk size to start with.
    pub suggested_chunk_size: u64,
}

/// The two ways creating a drop session answers.
#[derive(Reply)]
pub enum CreateDropReply {
    /// The session is open.
    #[reply(
        status = 201,
        description = "A drop session is open and accepting chunks"
    )]
    Created(CreateDropResponse),
}

/// One drop waiting for the owner.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct InboxEntryResponse {
    /// The drop's identifier, which adoption and discard name.
    pub drop_id: String,
    /// The link it arrived through.
    pub opaque_id: String,
    /// The ciphertext's content address.
    pub ciphertext_hash: String,
    /// How many bytes.
    pub size: u64,
    /// The guest's declared content type.
    pub content_type: String,
    /// `K` encapsulated to the link's Drop Key, base64. The owner decapsulates.
    pub kem_ct: String,
    /// Guest-supplied and **unverified**.
    ///
    /// A guest chose this text. A client rendering it treats it as untrusted input — it is the
    /// one field on this surface an anonymous party authored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_filename: Option<String>,
    /// When it landed, RFC 3339.
    pub received_at: String,
    /// Whether an adoption currently holds this row.
    ///
    /// Surfaced rather than hidden: a crash between claim and settle leaves a row here, and an
    /// owner who cannot see it cannot act on it.
    pub adopting: bool,
}

/// The owner's pending drops.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct InboxResponse {
    /// Everything waiting, oldest first.
    pub drops: Vec<InboxEntryResponse>,
}

/// What adoption produced.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AdoptResponse {
    /// The asset the drop became.
    pub asset_id: String,
}

/// The link being addressed.
#[derive(PathParams, Schema)]
pub struct LinkPath {
    /// The opaque id.
    pub opaque_id: String,
}

/// A chunk within a drop session.
#[derive(PathParams, Schema)]
pub struct DropChunkPath {
    /// The opaque id of the link the session belongs to.
    pub opaque_id: String,
    /// The session id.
    pub upload_id: String,
}

/// A pending drop.
#[derive(PathParams, Schema)]
pub struct DropPath {
    /// The drop's identifier.
    pub drop_id: String,
}

/// The headers a drop chunk carries.
#[derive(HeaderParams)]
pub struct DropChunkHeaders {
    /// Where in the blob this chunk starts.
    #[header(rename = "X-Capsule-Offset")]
    offset: Option<String>,
    /// The chunk's SHA-256, bare lowercase hex. Required: the idempotency tuple is undefined
    /// without it.
    #[header(rename = "X-Capsule-Checksum")]
    checksum: Option<String>,
}

/// The authoritative offset, returned on every accepted chunk.
#[derive(HeaderParams)]
pub struct DropOffsetHeader {
    /// Where the session is now.
    #[header(rename = "X-Capsule-Offset")]
    offset: u64,
}

/// Why an owner operation failed.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum LinkRejection {
    /// The body cannot be a link.
    #[error("{detail}")]
    #[problem(status = 400, title = "Malformed upload link")]
    Malformed {
        /// What was wrong, in English.
        detail: String,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A store could not answer.
    #[error("the upload link could not be provisioned")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Why a guest's chunk was refused.
///
/// A separate enum from [`DropRejection`] rather than a shared one, because the chunk path
/// cannot produce a quota refusal, a cap exhaustion or a too-large file — those are decided once
/// when the session opens. Sharing would make this operation declare three statuses it can never
/// return, which is the `S-C28` defect running the other way.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum DropChunkRejection {
    /// The chunk's own headers or body are unusable.
    #[error("{detail}")]
    #[problem(status = 400, title = "Malformed chunk")]
    Malformed {
        /// What was wrong, in English.
        detail: String,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The link or the session is not here. One answer, as on session creation.
    #[error("not found")]
    #[problem(status = 404, title = "Not found")]
    NotFound,

    /// The chunk was refused by invariants 9–12.
    #[error("{detail}")]
    #[problem(status = 409, title = "Chunk refused")]
    Refused {
        /// What was wrong, in English.
        detail: String,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A store could not answer.
    #[error("the chunk could not be accepted")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

impl DropChunkRejection {
    /// The chunk's headers or body are unusable.
    fn malformed(detail: &str) -> Self {
        Self::Malformed {
            detail: detail.to_owned(),
            code: error_codes::DROP_MALFORMED,
        }
    }

    /// Invariants 9–12 refused it.
    fn refused(detail: &str) -> Self {
        Self::Refused {
            detail: detail.to_owned(),
            code: error_codes::DROP_CHUNK_REFUSED,
        }
    }

    /// A store could not answer.
    fn unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::DROP_UNAVAILABLE,
        }
    }
}

/// Why a guest's drop was refused.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum DropRejection {
    /// The declaration is not a usable drop (invariants 27 and 30).
    #[error("{detail}")]
    #[problem(status = 400, title = "Malformed drop")]
    Malformed {
        /// What was wrong, in English.
        detail: String,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The link, or the session, is not here.
    ///
    /// **One answer** for a malformed id, an unknown link, an expired one, a revoked one, a
    /// spent single-use one, and an unknown session. The guest path carries no credential, so
    /// anything that distinguished them would be an enumeration oracle.
    #[error("not found")]
    #[problem(status = 404, title = "Not found")]
    NotFound,

    /// The owner's quota will not admit the drop (invariant 29).
    #[error("the link owner's quota will not admit this drop")]
    #[problem(status = 403, title = "Quota exceeded")]
    QuotaExceeded {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A cumulative cap on an otherwise-live link (invariant 26).
    ///
    /// Deliberately **not** the indistinguishable `404`: the guest was handed a real link by
    /// somebody who wants their photos, and needs to be told to ask for a new one rather than
    /// to conclude the link never existed.
    #[error("this link has no room left")]
    #[problem(status = 409, title = "Link capacity exhausted")]
    CapExhausted {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The file alone is larger than the link permits (invariant 28).
    #[error("this link accepts files up to {limit} bytes")]
    #[problem(status = 413, title = "File too large")]
    FileTooLarge {
        /// The largest single file this link accepts.
        #[problem(extension)]
        limit: u64,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A store could not answer.
    #[error("the drop could not be accepted")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Why the inbox could not be read.
///
/// One variant, because one thing can go wrong: the caller is authenticated, the account is
/// theirs, and there is nothing to look up. A shared enum across the three inbox operations
/// would make this operation *declare* a `404` and a `400` it cannot produce, which is exactly
/// the `S-C28` defect in reverse.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum InboxReadRejection {
    /// The store could not answer.
    #[error("the inbox could not be read")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Why a discard failed.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum DiscardRejection {
    /// No such drop in the caller's inbox.
    #[error("no such drop")]
    #[problem(status = 404, title = "Not found")]
    NotFound {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The store could not answer.
    #[error("the drop could not be discarded")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Why an adoption failed.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum AdoptRejection {
    /// No such drop in the caller's inbox.
    ///
    /// One answer for unknown and for somebody else's, so the endpoint is not an oracle over
    /// guessed drop ids.
    #[error("no such drop")]
    #[problem(status = 404, title = "Not found")]
    NotFound {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The adoption's manifest or its target was refused.
    #[error("{detail}")]
    #[problem(status = 400, title = "Adoption refused")]
    Refused {
        /// What was wrong, in English.
        detail: String,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A store could not answer.
    #[error("the inbox could not be read")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Provision an upload link.
#[kynos::post("/v1/drops/links", operation_id = "provision_link", tag = DropsTag)]
pub async fn provision_link(
    Inject(drops): Inject<DropContext>,
    Auth(credential): Auth<AccessToken>,
    Json(request): Json<ProvisionLinkRequest>,
) -> Result<ProvisionLinkReply, LinkRejection> {
    let owner = UserId::new(credential.user.as_str());

    if !is_opaque_id(&request.opaque_id) {
        return Err(LinkRejection::malformed(
            "opaque_id must be 32 lowercase hex characters",
        ));
    }
    let drop_pubkey = BASE64
        .decode(&request.drop_pubkey)
        .map_err(|_| LinkRejection::malformed("drop_pubkey is not base64"))?;
    if drop_pubkey.is_empty() {
        return Err(LinkRejection::malformed("drop_pubkey is empty"));
    }
    let passphrase_verifier = match &request.passphrase_verifier {
        None => None,
        Some(raw) => Some(
            BASE64
                .decode(raw)
                .map_err(|_| LinkRejection::malformed("passphrase_verifier is not base64"))?,
        ),
    };
    let expires_at = match &request.expires_at {
        None => None,
        Some(raw) => Some(
            raw.parse::<jiff::Timestamp>()
                .map_err(|_| LinkRejection::malformed("expires_at is not RFC 3339"))?,
        ),
    };

    drops
        .drops()
        .provision(UploadLinkRecord {
            opaque_id: request.opaque_id.clone(),
            owner_id: owner.clone(),
            drop_pubkey,
            crypto_suite_id: request.crypto_suite_id,
            protocol_version: capsule_core::crypto::primitives::PROTOCOL_VERSION.to_owned(),
            caps: LinkCaps {
                expires_at,
                max_total_bytes: request.max_total_bytes,
                max_file_count: request.max_file_count,
                max_file_size: request.max_file_size,
                single_use: request.single_use,
            },
            passphrase_verifier,
            used_bytes: 0,
            used_files: 0,
            revoked_at: None,
        })
        .await
        .map_err(|error| {
            tracing::error!(%error, %owner, "the drop store could not provision a link");
            LinkRejection::unavailable()
        })?;

    Ok(ProvisionLinkReply::Created(ProvisionLinkResponse {
        opaque_id: request.opaque_id,
    }))
}

/// Revoke one of the caller's links.
///
/// Indistinguishable and idempotent, for the same reason a share revocation is: saying "there
/// was nothing to revoke" would be a lookup.
#[kynos::delete(
    "/v1/drops/links/{opaque_id}",
    operation_id = "revoke_link",
    tag = DropsTag
)]
pub async fn revoke_link(
    Inject(drops): Inject<DropContext>,
    Auth(credential): Auth<AccessToken>,
    Path(path): Path<LinkPath>,
) -> Result<NoContent, LinkRejection> {
    let owner = UserId::new(credential.user.as_str());
    drops
        .drops()
        .revoke(&owner, &path.opaque_id, drops.clock().now())
        .await
        .map_err(|error| {
            tracing::error!(%error, %owner, "the drop store could not revoke a link");
            LinkRejection::unavailable()
        })?;
    Ok(NoContent)
}

/// Open a drop session through a link.
///
/// Invariants 26–30 in order: the link admits the file and reserves its caps in one store
/// operation, then the owner's quota is charged, then the declaration is checked.
#[kynos::post("/d/{opaque_id}", operation_id = "create_drop", tag = DropsTag)]
pub async fn create_drop(
    Inject(drops): Inject<DropContext>,
    Inject(quota): Inject<crate::quota::QuotaContext>,
    Path(path): Path<LinkPath>,
    Json(request): Json<CreateDropRequest>,
) -> Result<CreateDropReply, DropRejection> {
    if !is_opaque_id(&path.opaque_id) {
        return Err(DropRejection::NotFound);
    }

    // Invariant 30's shape, before the link is touched: a declaration that cannot be a drop
    // must not spend a cap on its way to being refused.
    if request.size == 0 {
        return Err(DropRejection::malformed("size must be greater than zero"));
    }
    let Ok(address) = ContentAddress::parse(&request.ciphertext_hash) else {
        return Err(DropRejection::malformed(
            "ciphertext_hash is not a content address",
        ));
    };
    let kem_ct = BASE64
        .decode(&request.kem_ct)
        .map_err(|_| DropRejection::malformed("kem_ct is not base64"))?;
    if kem_ct.is_empty() {
        return Err(DropRejection::malformed("kem_ct is empty"));
    }
    if request.content_type.trim().is_empty() {
        return Err(DropRejection::malformed("content_type is required"));
    }

    let now = drops.clock().now();
    let admission = drops
        .drops()
        .charge(&path.opaque_id, request.size, now)
        .await
        .map_err(|error| {
            tracing::error!(%error, "the drop store could not charge a link");
            DropRejection::unavailable()
        })?;

    let (owner, protocol_version) = match admission {
        Admission::Admitted {
            owner_id,
            protocol_version,
            ..
        } => (owner_id, protocol_version),
        Admission::NotLive => return Err(DropRejection::NotFound),
        Admission::CapExhausted => {
            return Err(DropRejection::CapExhausted {
                code: error_codes::DROP_CAP_EXHAUSTED,
            });
        }
        Admission::FileTooLarge { limit } => {
            return Err(DropRejection::FileTooLarge {
                limit,
                code: error_codes::DROP_FILE_TOO_LARGE,
            });
        }
    };

    // Invariant 29: the *link owner's* quota, not the guest's — the guest has no account. The
    // same enforcement point the album path uses, with `upload_user_id = owner_id`.
    let usage = quota.quotas().usage(&owner).await.map_err(|error| {
        tracing::error!(%error, %owner, "the quota ledger could not answer for a drop");
        DropRejection::unavailable()
    })?;
    let state = crate::quota::state_of(usage.used, usage.over_since, now, quota.limits());
    if !crate::quota::admits(
        state,
        crate::quota::WriteClass::Upload,
        usage.used,
        request.size,
        quota.limits(),
    ) {
        // The reservation goes back: a drop the quota refused never happened, and leaving the
        // cap spent would let a full account burn a guest's link down.
        refund(&drops, &path.opaque_id, request.size).await;
        return Err(DropRejection::QuotaExceeded {
            code: error_codes::QUOTA_EXCEEDED,
        });
    }

    let upload_id = UploadId::new(Uuid::new_v4().to_string());
    let record = UploadSessionRecord {
        upload_id: upload_id.clone(),
        // The drop id the inbox will file it under. Minted now so the session and the row it
        // becomes are one identifier from the start.
        asset_id: crate::store::AssetId::new(DropContext::new_drop_id()),
        owner_id: OwnerId::new(owner.as_str()),
        upload_user_id: owner.clone(),
        // No album, ever (invariant 30). A drop can only land in the inbox.
        album_id: None,
        content_type: Some(request.content_type.clone()),
        expected_hash: address.as_str().to_owned(),
        crypto_suite_id: 0,
        protocol_version,
        blob_role: BlobRole::Original,
        intent_id: None,
        // A drop carries no manifest. The album path's envelope gate never runs here, which is
        // invariant 30's *absence* clause made structural rather than checked.
        manifest_envelope: String::new(),
        received_bytes: 0,
        total_size: request.size,
        status: UploadSessionStatus::Pending,
        created_at: now,
        last_progress_at: now,
    };

    let drop_id = record.asset_id.to_string();
    if let Err(error) = drops.sessions().open(record).await {
        tracing::error!(%error, "the upload session store could not open a drop");
        refund(&drops, &path.opaque_id, request.size).await;
        return Err(DropRejection::unavailable());
    }

    // The guest's declaration, held until the bytes land. It has nowhere to live on the upload
    // session record — `kem_ct` is a drop's field and an album upload has none.
    if let Err(error) = drops
        .drops()
        .reserve(
            crate::drop::PendingDeposit {
                drop_id,
                opaque_id: path.opaque_id.clone(),
                owner_id: owner.clone(),
                kem_ct,
                content_type: request.content_type.clone(),
                suggested_filename: request.suggested_filename.clone(),
                size: request.size,
            },
            &upload_id,
        )
        .await
    {
        tracing::error!(%error, "a drop declaration could not be held");
        refund(&drops, &path.opaque_id, request.size).await;
        return Err(DropRejection::unavailable());
    }

    if let Err(error) = drops.blobs().begin(&upload_id).await {
        tracing::error!(%error, "the blob store could not stage a drop");
        refund(&drops, &path.opaque_id, request.size).await;
        return Err(DropRejection::unavailable());
    }

    tracing::info!(%owner, size = request.size, "a guest opened a drop session");
    Ok(CreateDropReply::Created(CreateDropResponse {
        upload_id: upload_id.to_string(),
        suggested_chunk_size: chunk::suggested_chunk_size(request.size),
    }))
}

/// Append one chunk to a drop session.
///
/// The link is the credential: possession of the opaque id, plus a session that belongs to it.
/// Everything after that is [`crate::upload::chunk::append`] — the album path's own function.
#[kynos::patch(
    "/d/{opaque_id}/{upload_id}",
    operation_id = "append_drop_chunk",
    tag = DropsTag
)]
pub async fn append_drop_chunk(
    Inject(drops): Inject<DropContext>,
    Path(path): Path<DropChunkPath>,
    Headers(headers): Headers<DropChunkHeaders>,
    body: ChunkBody,
) -> Result<WithHeaders<NoContent, DropOffsetHeader>, DropChunkRejection> {
    if !is_opaque_id(&path.opaque_id) {
        return Err(DropChunkRejection::NotFound);
    }

    let link = drops
        .drops()
        .resolve(&path.opaque_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, "the drop store could not resolve a link");
            DropChunkRejection::unavailable()
        })?;
    // A revoked link stops accepting chunks for sessions already open under it. That is the
    // point of revoking: a guest mid-upload is exactly who the owner is revoking against.
    let Some(link) = link.filter(|link| link.is_live_for_chunks_at(drops.clock().now())) else {
        return Err(DropChunkRejection::NotFound);
    };

    let id = UploadId::new(&path.upload_id);
    let record = drops.sessions().read(&id).await.map_err(|error| {
        tracing::error!(%error, "the upload session store could not answer for a drop");
        DropChunkRejection::unavailable()
    })?;
    // The session must belong to *this* link's owner, or a guest holding one link could append
    // to another's session by guessing its id.
    let Some(record) = record.filter(|record| record.upload_user_id == link.owner_id) else {
        return Err(DropChunkRejection::NotFound);
    };
    if !record.status.is_active() || record.status == UploadSessionStatus::WaitingForProcessing {
        return Err(DropChunkRejection::refused("this session is not active"));
    }

    let Some(offset) = headers.offset.as_deref().and_then(parse_offset) else {
        return Err(DropChunkRejection::malformed(
            "X-Capsule-Offset is required",
        ));
    };
    let Some(checksum) = headers.checksum.as_deref().and_then(parse_checksum) else {
        return Err(DropChunkRejection::malformed(
            "X-Capsule-Checksum is required",
        ));
    };

    let bytes = body.bytes();
    if bytes.is_empty() {
        return Err(DropChunkRejection::malformed("a chunk cannot be empty"));
    }
    if !chunk::within_chunk_ceiling(bytes.len() as u64) {
        return Err(DropChunkRejection::malformed(
            "the chunk is past the ceiling",
        ));
    }
    let received_hash = hash_bytes(bytes).to_hex();
    if received_hash != checksum {
        return Err(DropChunkRejection::refused("the chunk is not its checksum"));
    }

    let accepted = chunk::append(
        drops.sessions(),
        drops.blobs(),
        drops.clock(),
        &record,
        offset,
        bytes,
        &received_hash,
    )
    .await
    .map_err(|error| {
        tracing::error!(%error, "a drop chunk could not be appended");
        DropChunkRejection::unavailable()
    })?;

    let next_offset = match accepted {
        chunk::Accepted::Advanced {
            next_offset,
            complete,
        } => {
            if complete {
                deposit(&drops, &link, &record).await?;
            }
            next_offset
        }
        chunk::Accepted::Replayed { next_offset } => next_offset,
        chunk::Accepted::Conflict => {
            return Err(DropChunkRejection::refused(
                "that offset already holds different bytes",
            ));
        }
        chunk::Accepted::OffsetMismatch { expected } => {
            return Err(DropChunkRejection::refused(&format!(
                "this session is at offset {expected}"
            )));
        }
        chunk::Accepted::NotAligned => {
            return Err(DropChunkRejection::refused("the chunk is not aligned"));
        }
        chunk::Accepted::SizeExceeded => {
            return Err(DropChunkRejection::refused(
                "the chunk would exceed the declared size",
            ));
        }
        chunk::Accepted::StorageInconsistent => return Err(DropChunkRejection::unavailable()),
        chunk::Accepted::SessionGone => return Err(DropChunkRejection::NotFound),
    };

    Ok(WithHeaders::new(
        NoContent,
        DropOffsetHeader {
            offset: next_offset,
        },
    ))
}

/// The owner's signed `create` over a drop already in their inbox.
///
/// The same shape a `POST /v1/upload` create carries, minus everything about transferring bytes:
/// the blob is already committed, so there is no size to negotiate and no session to open. What
/// remains is the manifest, which is the whole point — a drop becomes an asset only when the
/// **owner** signs for it.
#[derive(Schema, Serialize, Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct AdoptRequest {
    /// The album to adopt into.
    pub album_id: String,
    /// The asset the drop becomes.
    pub asset_id: String,
    /// The blob's declared size — the inbox row's, restated and checked against it.
    pub size: u64,
    /// The ciphertext hash, which must name **this drop's** blob.
    pub hash: String,
    /// The declared content type.
    pub content_type: String,
    /// The crypto suite.
    pub crypto_suite_id: u16,
    /// The protocol version the manifest is written against.
    pub protocol_version: String,
    /// How the asset's key is carried. `derived` or `wrapped` (invariant 32).
    pub key_mode: String,
    /// The signed manifest envelope, verbatim.
    pub manifest_envelope: crate::upload::ManifestEnvelope,
}

/// Adopt a pending drop into an album.
///
/// Invariant 32. The manifest re-runs the create battery — a drop that skipped it would be the
/// one write on this server that entered an album unvalidated — and its `ciphertext_hash` must
/// name a blob in **the caller's own inbox**, which is what stops an adoption from minting an
/// asset over somebody else's bytes.
///
/// The row is **claimed, written, then settled**. Across two ports there is no transaction, and
/// the two failure directions are not equal: writing first and deleting after can duplicate a
/// photo, taking first and failing to write loses one. A claim leaves a crash visible in the
/// owner's own inbox instead, marked `adopting`.
#[kynos::post(
    "/v1/drops/{drop_id}/adopt",
    operation_id = "adopt_drop",
    tag = DropsTag
)]
pub async fn adopt_drop(
    Inject(drops): Inject<DropContext>,
    Inject(upload): Inject<crate::upload::UploadContext>,
    Auth(credential): Auth<AccessToken>,
    Path(path): Path<DropPath>,
    Json(request): Json<AdoptRequest>,
) -> Result<Json<AdoptResponse>, AdoptRejection> {
    let owner = UserId::new(credential.user.as_str());

    // `key_mode`'s closed enum (invariant 32). Checked before the claim, so a malformed request
    // does not hold a row it is going to be refused for.
    if !matches!(request.key_mode.as_str(), "derived" | "wrapped") {
        return Err(AdoptRejection::refused(
            "key_mode must be derived or wrapped",
        ));
    }

    let claimed = drops
        .drops()
        .claim(&owner, &path.drop_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, %owner, "the drop store could not claim");
            AdoptRejection::unavailable()
        })?;
    // Unknown, somebody else's, and already-claimed answer the same: the first two must not be
    // an oracle over guessed drop ids, and the third is a concurrent adoption whose loser has
    // nothing to do but stand down.
    let Some(entry) = claimed else {
        return Err(AdoptRejection::not_found());
    };

    match adopt_claimed(&drops, &upload, &owner, &entry, &request).await {
        Ok(asset_id) => {
            drops
                .drops()
                .settle(&entry.drop_id)
                .await
                .map_err(|error| {
                    // The asset exists and the row does not go. Loud, because the owner will
                    // see a drop they have already adopted and needs it to be explicable.
                    tracing::error!(
                        %error,
                        drop_id = %entry.drop_id,
                        "an adopted drop's inbox row could not be removed"
                    );
                    AdoptRejection::unavailable()
                })?;
            tracing::info!(%owner, %asset_id, "a drop was adopted into an album");
            Ok(Json(AdoptResponse {
                asset_id: asset_id.to_string(),
            }))
        }
        Err(rejection) => {
            // The write was refused, so the drop goes back to the inbox rather than being lost.
            if let Err(error) = drops.drops().release(&entry.drop_id).await {
                tracing::error!(%error, "a refused adoption could not release its drop");
            }
            Err(rejection)
        }
    }
}

/// The write half of an adoption, for a row this caller holds.
async fn adopt_claimed(
    drops: &DropContext,
    upload: &crate::upload::UploadContext,
    owner: &UserId,
    entry: &InboxEntry,
    request: &AdoptRequest,
) -> Result<crate::store::AssetId, AdoptRejection> {
    // The manifest must name **this** drop's blob. Without it an owner could adopt their own
    // inbox row while pointing the manifest at any address the store happens to hold.
    if request.hash != entry.address.as_str() {
        return Err(AdoptRejection::refused(
            "the manifest does not name this drop's blob",
        ));
    }
    if request.size != entry.size {
        return Err(AdoptRejection::refused(
            "the manifest does not declare this drop's size",
        ));
    }

    let owner_id = OwnerId::new(owner.as_str());
    let album = crate::store::AlbumId::new(&request.album_id);

    // Invariant 6, unchanged: adoption is a write into an album and needs the same capability
    // any other write does.
    let crate::upload::AlbumWriteAccess::Writable { protocol_pin } = upload
        .authority()
        .album_write_access(&owner_id, &album)
        .await
        .map_err(|error| {
            tracing::error!(%error, "the write authority could not answer for an adoption");
            AdoptRejection::unavailable()
        })?
    else {
        return Err(AdoptRejection::refused(
            "no write capability for that album",
        ));
    };

    // Invariant 7, unchanged.
    let device = crate::upload::envelope::created_by_device(&request.manifest_envelope)
        .map_err(|_| AdoptRejection::refused("the manifest names no usable device"))?;
    let Some(device_added_at) = upload
        .authority()
        .device_added_at(owner, device)
        .await
        .map_err(|error| {
            tracing::error!(%error, "the write authority could not answer for a device");
            AdoptRejection::unavailable()
        })?
    else {
        return Err(AdoptRejection::refused(
            "that device is not in your directory",
        ));
    };

    let declared = crate::upload::DeclaredBlob {
        size: request.size,
        hash: &request.hash,
        content_type: &request.content_type,
        crypto_suite_id: request.crypto_suite_id,
        protocol_version: &request.protocol_version,
        album_id: Some(request.album_id.as_str()),
        blob_role: crate::store::BlobRole::Original,
    };
    crate::upload::envelope::check_create(
        &declared,
        &request.manifest_envelope,
        &crate::upload::envelope::GateContext {
            policy: upload.policy(),
            album_pin: &protocol_pin,
            device_added_at,
            server_clock: drops.clock().now(),
        },
    )
    .map_err(|reject| AdoptRejection::refused(&format!("the manifest is refused: {reject:?}")))?;

    let asset_id = crate::store::AssetId::new(&request.asset_id);
    let now = drops.clock().now();
    upload
        .index()
        .reserve(crate::index::PendingAsset {
            asset_id: asset_id.clone(),
            owner_id: owner_id.clone(),
            album_id: album,
            protocol_version: protocol_pin,
            crypto_suite_id: request.crypto_suite_id,
            created_at: now,
        })
        .await
        .map_err(|error| {
            tracing::error!(%error, "an adoption could not reserve its asset");
            AdoptRejection::unavailable()
        })?;

    // The bytes are already committed — that is what makes adoption an in-place promotion
    // rather than a re-upload — so this records the blob the guest deposited against the asset
    // the owner just signed for.
    upload
        .index()
        .record_blob(
            &asset_id,
            crate::index::BlobRecord {
                role: crate::store::BlobRole::Original,
                address: entry.address.clone(),
                size: entry.size,
                finalized_at: now,
            },
        )
        .await
        .map_err(|error| {
            tracing::error!(%error, "an adoption could not record its blob");
            AdoptRejection::unavailable()
        })?;

    Ok(asset_id)
}

/// The caller's pending drops.
#[kynos::get("/v1/drops", operation_id = "list_inbox", tag = DropsTag)]
pub async fn list_inbox(
    Inject(drops): Inject<DropContext>,
    Auth(credential): Auth<AccessToken>,
) -> Result<Json<InboxResponse>, InboxReadRejection> {
    let owner = UserId::new(credential.user.as_str());
    let entries = drops.drops().inbox(&owner).await.map_err(|error| {
        tracing::error!(%error, %owner, "the drop store could not read an inbox");
        InboxReadRejection::Unavailable {
            code: error_codes::DROP_UNAVAILABLE,
        }
    })?;

    Ok(Json(InboxResponse {
        drops: entries.into_iter().map(describe).collect(),
    }))
}

/// Discard a pending drop.
///
/// The bytes become unreferenced and the collector reclaims them; the link's cap is **not**
/// refunded, because the drop did happen — a guest deposited a file and the owner chose not to
/// keep it, which is not the same as a link slot never having been used.
#[kynos::delete("/v1/drops/{drop_id}", operation_id = "discard_drop", tag = DropsTag)]
pub async fn discard_drop(
    Inject(drops): Inject<DropContext>,
    Auth(credential): Auth<AccessToken>,
    Path(path): Path<DropPath>,
) -> Result<NoContent, DiscardRejection> {
    let owner = UserId::new(credential.user.as_str());
    let discarded = drops
        .drops()
        .discard(&owner, &path.drop_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, %owner, "the drop store could not discard");
            DiscardRejection::Unavailable {
                code: error_codes::DROP_UNAVAILABLE,
            }
        })?;

    if !discarded {
        return Err(DiscardRejection::NotFound {
            code: error_codes::DROP_NOT_FOUND,
        });
    }
    Ok(NoContent)
}

/// File a finished drop in its owner's inbox.
async fn deposit(
    drops: &DropContext,
    link: &UploadLinkRecord,
    record: &UploadSessionRecord,
) -> Result<(), DropChunkRejection> {
    let address = ContentAddress::parse(&record.expected_hash)
        .map_err(|_| DropChunkRejection::unavailable())?;

    // The staged bytes are what the guest declared, or nothing is committed. Same check the
    // album path's finalization runs, and for the same reason: the hash is the only thing that
    // makes a content address an address.
    let staged = drops
        .blobs()
        .staged_len(&record.upload_id)
        .await
        .map_err(|_| DropChunkRejection::unavailable())?;
    if staged != Some(record.total_size) {
        return Err(DropChunkRejection::unavailable());
    }

    drops
        .blobs()
        .commit(&record.upload_id, &address)
        .await
        .map_err(|error| {
            tracing::error!(%error, "a drop could not be committed");
            DropChunkRejection::unavailable()
        })?;

    let pending = drops
        .drops()
        .take_reservation(&record.upload_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, "a drop declaration could not be read back");
            DropChunkRejection::unavailable()
        })?
        // A finished session with no held declaration is a server-side inconsistency, never a
        // guest's fault — and filing an inbox row with an empty `kem_ct` would hand the owner a
        // drop they can never decrypt.
        .ok_or_else(DropChunkRejection::unavailable)?;

    drops
        .drops()
        .deposit(InboxEntry {
            drop_id: pending.drop_id,
            owner_id: link.owner_id.clone(),
            opaque_id: link.opaque_id.clone(),
            address,
            size: record.total_size,
            content_type: pending.content_type,
            kem_ct: pending.kem_ct,
            suggested_filename: pending.suggested_filename,
            received_at: drops.clock().now(),
            adopting: false,
        })
        .await
        .map_err(|error| {
            tracing::error!(%error, "a drop could not be filed in an inbox");
            DropChunkRejection::unavailable()
        })?;

    drops
        .sessions()
        .set_status(&record.upload_id, UploadSessionStatus::Completed)
        .await
        .map_err(|error| {
            tracing::error!(%error, "a finished drop's session could not be closed");
            DropChunkRejection::unavailable()
        })?;
    Ok(())
}

/// Put a reservation back, logging rather than failing: the caller is already refusing.
async fn refund(drops: &DropContext, opaque_id: &str, size: u64) {
    if let Err(error) = drops.drops().refund(opaque_id, size).await {
        tracing::error!(%error, "a drop reservation could not be refunded");
    }
}

/// The wire projection of one inbox entry.
fn describe(entry: InboxEntry) -> InboxEntryResponse {
    InboxEntryResponse {
        drop_id: entry.drop_id,
        opaque_id: entry.opaque_id,
        ciphertext_hash: entry.address.as_str().to_owned(),
        size: entry.size,
        content_type: entry.content_type,
        kem_ct: BASE64.encode(&entry.kem_ct),
        suggested_filename: entry.suggested_filename,
        received_at: entry.received_at.to_string(),
        adopting: entry.adopting,
    }
}

impl LinkRejection {
    /// The body cannot be a link.
    fn malformed(detail: &str) -> Self {
        Self::Malformed {
            detail: detail.to_owned(),
            code: error_codes::DROP_MALFORMED,
        }
    }

    /// A store could not answer.
    fn unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::DROP_UNAVAILABLE,
        }
    }
}

impl DropRejection {
    /// The declaration is not a usable drop.
    fn malformed(detail: &str) -> Self {
        Self::Malformed {
            detail: detail.to_owned(),
            code: error_codes::DROP_MALFORMED,
        }
    }

    /// A store could not answer.
    fn unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::DROP_UNAVAILABLE,
        }
    }
}

impl AdoptRejection {
    /// The adoption was refused on its merits.
    fn refused(detail: &str) -> Self {
        Self::Refused {
            detail: detail.to_owned(),
            code: error_codes::DROP_ADOPTION_REFUSED,
        }
    }

    /// No such drop in the caller's inbox.
    fn not_found() -> Self {
        Self::NotFound {
            code: error_codes::DROP_NOT_FOUND,
        }
    }

    /// A store could not answer.
    fn unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::DROP_UNAVAILABLE,
        }
    }
}
