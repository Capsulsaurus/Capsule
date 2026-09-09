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

use crate::auth::AccessToken;
use crate::blob::ContentAddress;
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

    /// The one collaborator rejection.
    fn unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::SYNC_UNAVAILABLE,
        }
    }
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
    Inject(sync): Inject<SyncContext>,
    Auth(credential): Auth<AccessToken>,
    Query(query): Query<SyncQuery>,
) -> Result<Json<SyncPageResponse>, SyncRejection> {
    // The caller's own feed, or — with `album_id` — one album's page, which the caller reads as
    // its owner or as a member of its current roster (`S-C51`). The relationship is decided
    // first, and one refusal covers unprovisioned, not-a-member and removed alike.
    let owner = OwnerId::new(credential.user.as_str());
    let album = query.album_id.as_deref().map(AlbumId::new);
    // The album's owner, from the album record: the page is bound to the rows that account
    // filed, which is also what the index is keyed on.
    let album = match album {
        Some(album) => {
            let filed_by = album_read_access(&sync, &credential.user, &album).await?;
            Some((album, filed_by))
        }
        None => None,
    };
    let scope = match &album {
        Some((album, _)) => CursorScope::album(&owner, album),
        None => CursorScope::feed(&owner),
    };

    let after = sync
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
        after,
        limit,
        served = entries.len(),
        head,
        "served a sync page"
    );

    Ok(Json(SyncPageResponse {
        entries,
        next_cursor: sync.cursors().encode(&scope, position),
        // Strictly greater: `position == head` is a caught-up client, and telling it otherwise
        // would make every idle client poll one extra time forever.
        has_more: head > position,
    }))
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
        Some(address) => read_manifest(sync, address, &entry).await,
        None => None,
    };

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
        original_held: !deleted && entry.original_held,
        changed_at: entry.at.to_string(),
    }
}

/// Read a provenance blob and encode it for the wire, or explain in the log why not.
///
/// Every failure here is the **server's** inconsistency, never the client's, so none of them is
/// a rejection: an asset whose manifest cannot be produced is still an asset the client should
/// know about, and a page that failed wholesale because one blob was missing would make a
/// single storage fault look like an outage.
async fn read_manifest(
    sync: &SyncContext,
    address: &ContentAddress,
    entry: &FeedEntry,
) -> Option<String> {
    let stat = match sync.blobs().stat(address).await {
        Ok(stat) => stat,
        Err(error) => {
            tracing::error!(
                %error, %address, asset = %entry.asset_id,
                "the blob store could not stat a provenance blob"
            );
            return None;
        }
    };
    let Some(stat) = stat else {
        tracing::error!(
            %address, asset = %entry.asset_id,
            "the index names a provenance blob the store does not hold"
        );
        return None;
    };
    if stat.size > MAX_MANIFEST_BYTES {
        // The upload ceiling should have refused this long before it reached a feed.
        tracing::error!(
            %address, asset = %entry.asset_id, size = stat.size, cap = MAX_MANIFEST_BYTES,
            "a provenance blob is past the inline cap and was omitted from the feed"
        );
        return None;
    }

    let len = usize::try_from(stat.size).unwrap_or(usize::MAX);
    match sync.blobs().read_at(address, 0, len).await {
        Ok(Some(bytes)) => Some(BASE64.encode(bytes)),
        Ok(None) => {
            tracing::error!(
                %address, asset = %entry.asset_id,
                "a provenance blob vanished between stat and read"
            );
            None
        }
        Err(error) => {
            tracing::error!(
                %error, %address, asset = %entry.asset_id,
                "the blob store could not read a provenance blob"
            );
            None
        }
    }
}
