//! `GET /v1/sync` — the key-free sync feed (slice `S-C2`).
//!
//! The port of the retired `capsule.sync.v1` gRPC feed onto Kynos REST, per
//! design/api-surfaces.md. The transport changed; the cursor, the per-album `sync_seq`
//! monotonicity and the `original_held` completeness fact did not.
//!
//! # The status audit (`S-C28`)
//!
//! | Retired gRPC | Verdict here |
//! | --- | --- |
//! | `OK` with a page | `200` — [`SyncPageResponse`], always with a `next_cursor` |
//! | `INVALID_ARGUMENT` (`error.sync.cursor_invalid`) | kept as `400`, and it now also fires for a cursor issued to another owner, which the retired MAC could not detect |
//! | `UNAUTHENTICATED` (`error.sync.unauthenticated`) | **the framework's `401` now.** `Auth<AccessToken>` declares it and fills the `WWW-Authenticate` challenge. The catalog key survives, unused, because giving a framework rejection an `error.*` code is `S-C36` and is the same fix for every surface |
//! | `INTERNAL` | `500`, and now *coded* — `error.sync.unavailable`, a key this slice added because the retired feed had none and a client could not tell a broken server from a broken cursor |
//! | page size out of range | **not a rejection.** Clamped; see [`crate::sync::clamp_page_size`] |
//! | *(new, #697)* a cursor from before the owner's sync epoch moved | `409` (`error.sync.cursor_stale`) |
//!
//! # A stale cursor is not an invalid one (#697)
//!
//! An index rebuild that cannot prove an owner's sequence complete moves the owner to a new sync
//! epoch ([`crate::index::AssetIndex::bump_sync_epoch`]). A cursor minted before that is
//! authentic, and resuming it would be wrong: the numbers after it may have been issued again.
//! It is refused with its own code, `409 error.sync.cursor_stale`, because the client's recovery
//! differs from a forged cursor's in one way that matters: it drops its anti-rewind high-water
//! marks too, since the rebuilt numbering may sit below them, and it keeps everything it holds.
//! The refusal names no epoch; the client starts from an empty cursor, which is valid in every
//! epoch.
//!
//! # A peer reads the same page (`S-E5`)
//!
//! `Authorization: Bearer` also carries a federation capability. The album arm is then bound to
//! the capability's album — a peer has no "own feed", so `album_id` absent or different is
//! `403 error.federation.audience_mismatch` — and the member the capability was minted for must
//! still be on the roster at the epoch it was granted, which is the same
//! `403 error.sync.album_access_denied` an account's arm gives. Before any of that the
//! capability is *admitted* ([`federation::admit`]): revoked `403`, blocked peer `403`, over
//! budget `429`. The cursor is bound to `(peer, album)`, its own scope.
//!
//! # What a tombstone discloses
//!
//! A `deleted` entry carries no manifest, no metadata reference and no blob list. The row still
//! holds them — GC decides what happens to bytes, not this surface — but sending them would
//! invite a client to fetch blobs that may already be collected, and a deletion needs none of
//! it to be applied. The index reports the row truthfully; the wire decides what a tombstone
//! says.
//!
//! # Base64 is not a re-serialization
//!
//! `manifest_cbor` carries the provenance blob's exact bytes, base64-encoded because JSON has
//! no byte string. That is a transport encoding: `decode(encode(b)) == b` for every `b`, so the
//! bytes the client signed are the bytes it verifies. The retired feed's defect was categorically
//! different — it re-encoded a *parsed projection*, producing bytes carrying neither `device_sig`
//! nor `write_sig` (`S-C30`).

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_i18n::error_codes;
use kynos::extract::params::query::Query;
use kynos::prelude::*;
use kynos::security::auth::Auth;
use serde::{Deserialize, Serialize};

use crate::blob::ContentAddress;
use crate::counter::CounterContext;
use crate::federation::{
    self, FederationContext, Principal, ReadBearer, Refusal, VerifiedCapability,
};
use crate::index::{ChangeKind, FeedEntry};
use crate::membership::Membership;
use crate::routes::upload::WireBlobRole;
use crate::store::{AlbumId, OwnerId, UserId};
use crate::sync::{CursorError, CursorScope, MAX_MANIFEST_BYTES, SyncContext, clamp_page_size};

/// The operation that tells a client what changed.
#[derive(Tag)]
#[tag(
    name = "sync",
    description = "Discovering what changed in a library, as an opaque, resumable feed."
)]
pub struct SyncTag;

// ===========================================================================================
// Wire types
// ===========================================================================================

/// How a page is requested.
///
/// Both parameters are optional, and the absence of `cursor` is the first-sync case rather than
/// an error: "I have seen nothing" and "resume after position 0" are one request.
#[derive(Schema, QueryParams, Debug)]
pub struct SyncQuery {
    /// The opaque cursor a previous page returned. Absent means "from the beginning".
    pub cursor: Option<String>,
    /// How many entries to return. Clamped into the range this server serves.
    ///
    /// `u32` and not `usize`: Kynos refuses to describe a platform-width integer, and it is
    /// right to — a schema whose bounds depend on the server's pointer size is a schema no
    /// client can rely on.
    pub page_size: Option<u32>,
    /// One album's page rather than the caller's own feed (`S-C51`).
    ///
    /// For the album's owner or any account on its current roster. Positions are the owner's
    /// sequence numbers filtered to the album, and the cursor is bound to `(caller, album)`, so
    /// it cannot be presented on the caller's own feed or on another album. Absent: the caller's
    /// own library, as before.
    pub album_id: Option<String>,
}

/// What an entry is, relative to the client that asked for it.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WireChangeKind {
    /// The client has never seen this asset.
    Created,
    /// The client has seen it and something has changed since.
    Updated,
    /// It is deleted.
    Deleted,
}

impl From<ChangeKind> for WireChangeKind {
    fn from(change: ChangeKind) -> Self {
        match change {
            ChangeKind::Created => Self::Created,
            ChangeKind::Updated => Self::Updated,
            ChangeKind::Deleted => Self::Deleted,
        }
    }
}

/// One blob an asset holds.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SyncBlobRef {
    /// The blob's role in the bundle.
    pub role: WireBlobRole,
    /// Its ciphertext content address, lowercase hex.
    pub hash: String,
    /// Its size in bytes, so a client can budget a fetch before issuing one.
    pub size: u64,
}

/// One change in a library.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SyncEntry {
    /// The asset that changed.
    pub asset_id: String,
    /// The album it belongs to. The client keeps its anti-rewind high-water mark per album.
    pub album_id: String,
    /// The album's pinned protocol date. A client refuses an entry above its own maximum
    /// rather than applying it partially.
    pub protocol_version: String,
    /// The entry's position. Strictly increasing within a page, and therefore within any album
    /// the page touches.
    pub sync_seq: u64,
    /// What this is to the client that asked.
    pub change: WireChangeKind,
    /// The signed manifest, base64 of the provenance blob's exact bytes (`S-C30`).
    ///
    /// Absent on a tombstone, and absent — with a loud server-side log — when the index names a
    /// provenance blob the store cannot produce.
    pub manifest_cbor: Option<String>,
    /// The encrypted metadata blob's content address.
    pub metadata_blob: Option<String>,
    /// The asset's original and derivative blobs.
    pub blobs: Vec<SyncBlobRef>,
    /// The asset's signed derivative manifests, each base64 of its `derivative_manifest`
    /// blob's exact bytes, in address order (#556).
    ///
    /// What a receiver opens a thumbnail or preview with: the manifest names the derivative's
    /// `ciphertext_hash`, `nonce_prefix` and AMK epoch, and the blob list carries none of
    /// them. Served verbatim for the reason [`Self::manifest_cbor`] is (`S-C30`). Empty on a
    /// tombstone; a manifest the store cannot produce is left out with a loud server-side log.
    #[serde(default)]
    pub derivative_manifests: Vec<String>,
    /// Whether the original has landed. `false` is the derived `awaiting-original` state.
    pub original_held: bool,
    /// When the change happened, RFC 3339.
    pub changed_at: String,
}

/// A page of the feed.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SyncPageResponse {
    /// The changes, in `sync_seq` order.
    pub entries: Vec<SyncEntry>,
    /// The cursor that resumes after the last entry.
    ///
    /// Always present, including on an empty page, where it re-mints the position the client
    /// arrived with. A client therefore never has to decide whether to keep its old cursor.
    pub next_cursor: String,
    /// Whether the server holds changes beyond this page.
    ///
    /// Answered from the owner's high-water mark rather than by fetching one more entry, so a
    /// caught-up client is told so without paying for a page it will not receive.
    pub has_more: bool,
}

// ===========================================================================================
// Rejections
// ===========================================================================================

/// Why a page was not served.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum SyncRejection {
    /// The cursor is not one this server issued to this owner.
    ///
    /// One answer for every way a cursor can fail — malformed, mutated, foreign, or minted
    /// under a rotated key. Telling a caller *which* tells a forger which byte to change next,
    /// and the client's recovery is the same in every case: discard and resync from empty.
    #[error("the sync cursor is not valid")]
    #[problem(status = 400, title = "Invalid sync cursor")]
    CursorInvalid {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The cursor is authentic but was minted before the owner's sync epoch moved (#697).
    ///
    /// The client drops its cursor and its high-water marks, resyncs from an empty cursor, and
    /// discards nothing it holds.
    #[error("the sync cursor predates a rebuild of this library")]
    #[problem(status = 409, title = "Stale sync cursor")]
    CursorStale {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The album is not the caller's and the caller is not on its roster (`S-C51`).
    ///
    /// One answer for unprovisioned, never-a-member and removed alike, as the write routes give.
    #[error("no access to that album")]
    #[problem(status = 403, title = "Album access denied")]
    AlbumAccessDenied {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The capability is revoked (`S-E5`).
    #[error("this capability has been revoked")]
    #[problem(status = 403, title = "Capability revoked")]
    CapabilityRevoked {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The capability is for another album, or no album was named (`S-E5`).
    #[error("this capability is for a different album")]
    #[problem(status = 403, title = "Audience mismatch")]
    AudienceMismatch {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The peer is on this server's blocklist (`S-C49`).
    #[error("this server is blocked")]
    #[problem(status = 403, title = "Server blocked")]
    PeerBlocked {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The peer's events-per-hour budget is spent (invariant 21).
    #[error("this peer has reached its request budget")]
    #[problem(status = 429, title = "Rate budget exceeded")]
    RateLimited {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A collaborator could not answer.
    #[error("the sync feed could not be read")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

impl SyncRejection {
    /// The album page is not the caller's to read.
    fn album_access_denied() -> Self {
        Self::AlbumAccessDenied {
            code: error_codes::SYNC_ALBUM_ACCESS_DENIED,
        }
    }

    /// The one cursor rejection.
    fn cursor_invalid() -> Self {
        Self::CursorInvalid {
            code: error_codes::SYNC_CURSOR_INVALID,
        }
    }

    /// The cursor predates the owner's current sync epoch.
    fn cursor_stale() -> Self {
        Self::CursorStale {
            code: error_codes::SYNC_CURSOR_STALE,
        }
    }

    /// The one collaborator rejection.
    fn unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::SYNC_UNAVAILABLE,
        }
    }

    /// The capability names another album, or none was asked for.
    fn audience_mismatch() -> Self {
        Self::AudienceMismatch {
            code: error_codes::FEDERATION_AUDIENCE_MISMATCH,
        }
    }
}

impl From<Refusal> for SyncRejection {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::Revoked => Self::CapabilityRevoked {
                code: error_codes::FEDERATION_CAPABILITY_REVOKED,
            },
            Refusal::PeerBlocked => Self::PeerBlocked {
                code: error_codes::MODERATION_SERVER_BLOCKED,
            },
            Refusal::RateLimited { .. } => Self::RateLimited {
                code: error_codes::FEDERATION_RATE_BUDGET_EXCEEDED,
            },
            Refusal::Unavailable => Self::Unavailable {
                code: error_codes::FEDERATION_UNAVAILABLE,
            },
        }
    }
}

/// Who is reading, once the credential has been decided.
///
/// The account's id or the peer's origin, each in its own type: the cursor scope and the log
/// field both need to know which, and a string would let the two be confused.
enum Reader {
    Account(OwnerId),
    Peer(Box<VerifiedCapability>),
}

// ===========================================================================================
// The operation
// ===========================================================================================

/// Returns the changes in the caller's library after `cursor`.
///
/// Read-only and idempotent: two calls with the same cursor return the same page, because the
/// cursor names a position rather than consuming one. That is what makes a lost response
/// harmless and a retry free.
#[kynos::get("/v1/sync", operation_id = "sync_feed", tag = SyncTag)]
pub async fn sync_feed(
    Auth(principal): Auth<ReadBearer>,
    Inject(sync): Inject<SyncContext>,
    Inject(federation): Inject<FederationContext>,
    Inject(counters): Inject<CounterContext>,
    Query(query): Query<SyncQuery>,
) -> Result<Json<SyncPageResponse>, SyncRejection> {
    let requested = query.album_id.as_deref().map(AlbumId::new);

    // Who is asking, and which page they may have. An account reads its own feed, or — with
    // `album_id` — one album's page as its owner or a member of its current roster (`S-C51`).
    // A peer reads exactly the album its capability names, as the member it was minted for
    // (`S-E5`). Each relationship is decided first, and one refusal covers every way it fails.
    let (reader, album) = match principal {
        Principal::Session(credential) => {
            let album = match requested {
                Some(album) => {
                    let filed_by = album_read_access(&sync, &credential.user, &album).await?;
                    Some((album, filed_by))
                }
                None => None,
            };
            (
                Reader::Account(OwnerId::new(credential.user.as_str())),
                album,
            )
        }
        Principal::Peer(capability) => {
            federation::admit(
                &federation,
                &counters,
                &capability,
                federation::Presentation::Read,
            )
            .await?;
            let album = match requested {
                Some(album) if album == capability.record.album_id => album,
                _ => {
                    tracing::info!(
                        peer = %capability.record.peer_id,
                        jti = %capability.record.jti,
                        "a peer asked for a page its capability does not cover"
                    );
                    return Err(SyncRejection::audience_mismatch());
                }
            };
            let filed_by = peer_album_access(&sync, &capability, &album).await?;
            (Reader::Peer(capability), Some((album, filed_by)))
        }
    };
    let scope = match (&reader, &album) {
        (Reader::Account(owner), Some((album, _))) => CursorScope::album(owner, album),
        (Reader::Account(owner), None) => CursorScope::feed(owner),
        (Reader::Peer(capability), Some((album, _))) => {
            CursorScope::peer(&capability.record.peer_id, album)
        }
        // A peer always has an album by the time it is here; the arm above returned otherwise.
        (Reader::Peer(_), None) => return Err(SyncRejection::audience_mismatch()),
    };
    // The account whose rows are paged: the caller's own, or the album owner's.
    let owner = match (&reader, &album) {
        (_, Some((_, filed_by))) => filed_by.clone(),
        (Reader::Account(owner), None) => owner.clone(),
        (Reader::Peer(_), None) => return Err(SyncRejection::audience_mismatch()),
    };

    // The epoch of the owner whose rows are paged: the caller's own, or the album owner's.
    let pager = match &album {
        Some((_, filed_by)) => filed_by,
        None => &owner,
    };
    let epoch = sync.index().sync_epoch(pager).await.map_err(|error| {
        tracing::error!(%error, %owner, "the asset index could not report a sync epoch");
        SyncRejection::unavailable()
    })?;

    let resumed = sync
        .cursors()
        .decode(&scope, query.cursor.as_deref())
        .map_err(|error| {
            // Logged at `info`, not `warn`: a cursor that stopped authenticating is the normal
            // consequence of a key rotation, and an operator who has just rotated should not be
            // reading a warning per client.
            let reason = match error {
                CursorError::Malformed => "malformed",
                CursorError::NotAuthentic => "did not authenticate",
            };
            tracing::info!(%owner, reason, "a sync cursor was refused; the client will resync");
            SyncRejection::cursor_invalid()
        })?;
    let after = match resumed {
        None => 0,
        Some(resumed) if resumed.epoch == epoch => resumed.position,
        Some(resumed) if resumed.epoch < epoch => {
            tracing::info!(
                %owner,
                cursor_epoch = resumed.epoch,
                epoch,
                "a sync cursor from before a rebuild was refused; the client will resync from zero"
            );
            return Err(SyncRejection::cursor_stale());
        }
        // An epoch this owner has not reached. The server never minted it, so it is a cursor
        // from another deployment's database under the same key: invalid, not stale.
        Some(resumed) => {
            tracing::warn!(
                %owner,
                cursor_epoch = resumed.epoch,
                epoch,
                "a sync cursor named an epoch this owner has not reached"
            );
            return Err(SyncRejection::cursor_invalid());
        }
    };

    let limit = clamp_page_size(
        query
            .page_size
            .map(|size| usize::try_from(size).unwrap_or(usize::MAX)),
    );
    let rows = match &album {
        Some((album, filed_by)) => {
            sync.index()
                .album_feed_page(filed_by, album, after, limit)
                .await
        }
        None => sync.index().feed_page(&owner, after, limit).await,
    }
    .map_err(|error| {
        tracing::error!(%error, %owner, "the asset index could not serve a feed page");
        SyncRejection::unavailable()
    })?;

    let head = match &album {
        Some((album, filed_by)) => sync.index().album_head_seq(filed_by, album).await,
        None => sync.index().head_seq(&owner).await,
    }
    .map_err(|error| {
        tracing::error!(%error, %owner, "the asset index could not report its head");
        SyncRejection::unavailable()
    })?;

    let position = rows.last().map_or(after, |entry| entry.sync_seq);
    let mut entries = Vec::with_capacity(rows.len());
    for row in rows {
        entries.push(render(&sync, row).await);
    }

    tracing::debug!(
        %owner,
        peer = match &reader {
            Reader::Peer(capability) => Some(capability.record.peer_id.as_str()),
            Reader::Account(_) => None,
        },
        after,
        limit,
        served = entries.len(),
        head,
        "served a sync page"
    );

    Ok(Json(SyncPageResponse {
        entries,
        next_cursor: sync.cursors().encode(&scope, epoch, position),
        // Strictly greater: `position == head` is a caught-up client, and telling it otherwise
        // would make every idle client poll one extra time forever.
        has_more: head > position,
    }))
}

/// Whether the member `capability` was minted for is still on `album`'s roster at the epoch the
/// grant was made at — answering the album's owner, whose rows the page is (`S-E5`).
///
/// The epoch is the server-side half of the grant: a member removed and re-admitted at a later
/// epoch gets a fresh membership, and a capability minted for the earlier one is refused without
/// anyone having revoked it. One `403` for every failure, as the account arm gives — the album
/// id is the capability's own, so the answer discloses nothing a peer does not hold already.
async fn peer_album_access(
    sync: &SyncContext,
    capability: &VerifiedCapability,
    album: &AlbumId,
) -> Result<OwnerId, SyncRejection> {
    let record = sync.albums().read(album).await.map_err(|error| {
        tracing::error!(%error, %album, "the album store could not answer a peer's sync page");
        SyncRejection::unavailable()
    })?;
    let Some(record) = record else {
        tracing::info!(peer = %capability.record.peer_id, %album, "a peer's page was refused: no such album");
        return Err(SyncRejection::album_access_denied());
    };
    match sync
        .members()
        .membership(album, &capability.record.member)
        .await
        .map_err(|error| {
            tracing::error!(%error, %album, "the membership store could not answer a peer's sync page");
            SyncRejection::unavailable()
        })? {
        Membership::Member { granted_epoch, .. }
            if granted_epoch == capability.record.granted_epoch =>
        {
            Ok(record.owner_id)
        }
        membership => {
            tracing::info!(
                peer = %capability.record.peer_id,
                member = %capability.record.member,
                %album,
                ?membership,
                granted_epoch = capability.record.granted_epoch,
                "a peer's page was refused: its member is not on the roster at the granted epoch"
            );
            Err(SyncRejection::album_access_denied())
        }
    }
}

/// Whether `caller` may read `album`'s page — its owner, or an account on its current roster —
/// answering the album's owner, whose rows the page is.
///
/// One `403` for every refusal — unprovisioned, never a member, removed — matching the write
/// routes' uniform `album_access_denied`: the album id is client-derived and unguessable, and a
/// distinct answer per reason would say whether it is taken and whether the caller was ever on
/// it. (An unprovisioned id costs one store read and the other two cost two; with a UUIDv7 id
/// space that timing difference buys a guesser nothing, as it does not on the write path.) A
/// store that cannot answer is an outage, never a refusal.
///
/// # A member's page starts at the album's beginning, not at their `granted_epoch` (#525)
///
/// Deliberately. The membership's `granted_epoch` is an AMK epoch, and nothing maps it to a feed
/// position: positions are the owner's sequence numbers, and the only epoch a row carries is the
/// one its *latest* manifest was written under, so "entries at or after the grant" would hide an
/// old asset until somebody next touched it. More to the point, hiding them is not what the
/// album's history policy asks for under either setting ([MLS — History Delivery]): *full
/// history*, the default for shared albums, hands a joiner every prior AMK so it can read every
/// photo, and *capped history* keeps older photos "visible but not decryptable — show
/// placeholders", which needs their entries too. The policy lives in the MLS group, which this
/// server cannot read, and what a joiner may decrypt is the `Welcome`'s decision, not the
/// feed's. The roster is a transport control over who is handed bytes, and every current member
/// is handed the album's.
///
/// [MLS — History Delivery]: ../../../capsule-docs/src/content/docs/design/cryptography/mls.md
async fn album_read_access(
    sync: &SyncContext,
    caller: &UserId,
    album: &AlbumId,
) -> Result<OwnerId, SyncRejection> {
    let record = sync.albums().read(album).await.map_err(|error| {
        tracing::error!(%error, %album, "the album store could not answer a sync page");
        SyncRejection::unavailable()
    })?;
    let Some(record) = record else {
        tracing::info!(%caller, %album, "an album page was refused: no such album");
        return Err(SyncRejection::album_access_denied());
    };
    if record.owner_id.as_str() == caller.as_str() {
        return Ok(record.owner_id);
    }
    match sync
        .members()
        .membership(album, caller)
        .await
        .map_err(|error| {
            tracing::error!(%error, %album, "the membership store could not answer a sync page");
            SyncRejection::unavailable()
        })? {
        // Whatever epoch the membership was granted at: see the section above on why a member's
        // page is the album's whole history.
        Membership::Member { .. } => Ok(record.owner_id),
        Membership::Revoked(_) | Membership::Never => {
            tracing::info!(%caller, %album, "an album page was refused: not a member");
            Err(SyncRejection::album_access_denied())
        }
    }
}

/// Render one index entry onto the wire, reading its manifest bytes.
async fn render(sync: &SyncContext, entry: FeedEntry) -> SyncEntry {
    let deleted = entry.change == ChangeKind::Deleted;
    let manifest = match entry.provenance.as_ref().filter(|_| !deleted) {
        Some(address) => read_manifest(sync, address, &entry, ManifestKind::Asset).await,
        None => None,
    };
    let mut derivative_manifests = Vec::new();
    if !deleted {
        for address in &entry.derivative_manifests {
            if let Some(encoded) =
                read_manifest(sync, address, &entry, ManifestKind::Derivative).await
            {
                derivative_manifests.push(encoded);
            }
        }
    }

    SyncEntry {
        asset_id: entry.asset_id.to_string(),
        album_id: entry.album_id.to_string(),
        protocol_version: entry.protocol_version,
        sync_seq: entry.sync_seq,
        change: entry.change.into(),
        manifest_cbor: manifest,
        metadata_blob: if deleted {
            None
        } else {
            entry.metadata.map(|address| address.to_string())
        },
        blobs: if deleted {
            Vec::new()
        } else {
            entry
                .blobs
                .into_iter()
                .map(|blob| SyncBlobRef {
                    role: blob.role.into(),
                    hash: blob.address.to_string(),
                    size: blob.size,
                })
                .collect()
        },
        derivative_manifests,
        original_held: !deleted && entry.original_held,
        changed_at: entry.at.to_string(),
    }
}

/// Which signed manifest a feed entry is inlining — named in every log line, so an operator
/// reading one knows which role's blob went missing.
#[derive(Debug, Clone, Copy)]
enum ManifestKind {
    /// The asset's `provenance` blob.
    Asset,
    /// One of its `derivative_manifest` blobs.
    Derivative,
}

impl ManifestKind {
    /// The blob role the manifest is stored under.
    fn role(self) -> &'static str {
        match self {
            Self::Asset => crate::store::BlobRole::Provenance.as_str(),
            Self::Derivative => crate::store::BlobRole::DerivativeManifest.as_str(),
        }
    }
}

/// Read a manifest blob — the asset's provenance blob or one of its derivative manifests — and
/// encode it for the wire, or explain in the log why not.
///
/// Every failure here is the **server's** inconsistency, never the client's, so none of them is
/// a rejection: an asset whose manifest cannot be produced is still an asset the client should
/// know about, and a page that failed wholesale because one blob was missing would make a
/// single storage fault look like an outage.
async fn read_manifest(
    sync: &SyncContext,
    address: &ContentAddress,
    entry: &FeedEntry,
    kind: ManifestKind,
) -> Option<String> {
    let role = kind.role();
    let stat = match sync.blobs().stat(address).await {
        Ok(stat) => stat,
        Err(error) => {
            tracing::error!(
                %error, %address, asset = %entry.asset_id, role,
                "the blob store could not stat a manifest blob"
            );
            return None;
        }
    };
    let Some(stat) = stat else {
        tracing::error!(
            %address, asset = %entry.asset_id, role,
            "the index names a manifest blob the store does not hold"
        );
        return None;
    };
    if stat.size > MAX_MANIFEST_BYTES {
        // The upload ceiling should have refused this long before it reached a feed.
        tracing::error!(
            %address, asset = %entry.asset_id, role, size = stat.size, cap = MAX_MANIFEST_BYTES,
            "a manifest blob is past the inline cap and was omitted from the feed"
        );
        return None;
    }

    let len = usize::try_from(stat.size).unwrap_or(usize::MAX);
    match sync.blobs().read_at(address, 0, len).await {
        Ok(Some(bytes)) => Some(BASE64.encode(bytes)),
        Ok(None) => {
            tracing::error!(
                %address, asset = %entry.asset_id, role,
                "a manifest blob vanished between stat and read"
            );
            None
        }
        Err(error) => {
            tracing::error!(
                %error, %address, asset = %entry.asset_id, role,
                "the blob store could not read a manifest blob"
            );
            None
        }
    }
}
