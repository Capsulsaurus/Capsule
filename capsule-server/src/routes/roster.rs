//! `PUT /v1/albums/{album_id}/roster` — publishing an album's membership roster (slice `S-C51`).
//!
//! The one endpoint that tells the key-free server who may read and write a shared album.
//! [`crate::membership`] owns the port and the reasoning; this is the wire shape.
//!
//! ```text
//! PUT /v1/albums/{album_id}/roster    { "roster_cbor": "<base64 of a SignedAlbumRoster>" }
//!
//! 200 { "album_id": …, "roster_version": 3, "amk_epoch": 2, "member_count": 4, "replayed": false }
//! 400 error.album.roster_malformed
//! 403 error.album.roster_attester
//! 404 error.album.roster_not_found
//! 409 error.album.roster_stale        + current_version
//! 500 error.album.unavailable
//! ```
//!
//! **Only the owner account publishes.** The trust anchor is the album owner's published device
//! directory — the same anchor the upgrade ceremony verifies its intent against — so the caller
//! must be the album's owner, the roster's `attested_by_user` must be the caller, and the
//! attesting device must be a live device in that directory. A member who tries, even one the
//! MLS group calls an admin, gets the album ceremonies' `404`: not yours is not found.
//!
//! **JSON with base64 CBOR, not `application/cbor`.** The signed bytes are canonical CBOR and
//! must reach the server verbatim, which a CBOR body would carry more directly — but spargen
//! cannot lower `application/cbor`, so a CBOR operation is one the generated SDK cannot call
//! and the client library would need a hand-written request for. Base64 inside a JSON field
//! keeps the bytes verbatim *and* the operation generated.
//!
//! **Removal is a new roster that omits the member.** There is no delete; the epoch bump that
//! accompanies an MLS `Remove` rides `amk_epoch`, and the store records the version and epoch at
//! which the member vanished — the stored fact a former member's blob-route `403` is rendered
//! from once that route consults membership.
//!
//! **Idempotent under `(album_id, roster_version)`.** The same bytes again are a `200` with
//! `replayed: true`; the same version with different bytes is the `409`.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_core::crypto::membership::SignedAlbumRoster;
use capsule_i18n::error_codes;
use kynos::prelude::*;
use serde::{Deserialize, Serialize};

use crate::album::AlbumContext;
use crate::auth::AccessToken;
use crate::directory::DeviceDirectoryContext;
use crate::membership::{MemberRole, MembershipContext, RosterOutcome, RosterRecord};
use crate::routes::albums::AlbumsTag;
use crate::routes::upgrade::AlbumPath;
use crate::store::{AlbumId, UserId};

/// The largest signed roster this surface accepts, decoded.
///
/// A roster member is a UUID and a role — well under a hundred bytes each in canonical CBOR —
/// so 512 KiB is several thousand members and still far below the transport backstop
/// (`S-C33`, 32 MiB), which is not a bound on a membership document.
pub const MAX_ROSTER_BYTES: usize = 512 * 1024;

/// The publish request.
#[derive(Schema, Serialize, Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct RosterRequest {
    /// The signed roster, as standard base64 of its canonical CBOR encoding.
    pub roster_cbor: String,
}

/// What the server now holds for the album.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct RosterResponse {
    /// The album, echoed.
    pub album_id: String,
    /// The roster version the server holds after this call.
    pub roster_version: u64,
    /// The AMK epoch that roster reflects.
    pub amk_epoch: u64,
    /// How many members the held roster names, the owner excluded.
    pub member_count: u64,
    /// Whether this call was a replay of the roster already held. Advisory: both answers mean
    /// "the server holds this roster".
    pub replayed: bool,
}

/// Why a roster was not published.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum RosterRejection {
    /// The body is not a signed roster this server can accept for this album.
    #[error("{detail}")]
    #[problem(status = 400, title = "Malformed roster")]
    Malformed {
        /// What was wrong, in English.
        detail: String,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The roster is not attested by a live device in the album owner's published directory.
    #[error("the roster's attester could not be verified")]
    #[problem(status = 403, title = "Attester not authorized")]
    AttesterNotAuthorized {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// No such album, or not this caller's. One answer for both.
    #[error("no such album")]
    #[problem(status = 404, title = "Not found")]
    NotFound {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The server already holds a roster this one does not supersede.
    #[error("the server holds roster version {current_version}, which this does not supersede")]
    #[problem(status = 409, title = "Roster stale")]
    Stale {
        /// The version the server holds.
        #[problem(extension)]
        current_version: u64,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A collaborator could not answer.
    #[error("the roster could not be recorded")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

impl RosterRejection {
    /// The request was not a well-formed roster for this album.
    fn malformed(detail: impl Into<String>) -> Self {
        Self::Malformed {
            detail: detail.into(),
            code: error_codes::ALBUM_ROSTER_MALFORMED,
        }
    }

    /// The attester did not verify.
    fn attester() -> Self {
        Self::AttesterNotAuthorized {
            code: error_codes::ALBUM_ROSTER_ATTESTER,
        }
    }

    /// No such album, or not this caller's.
    fn not_found() -> Self {
        Self::NotFound {
            code: error_codes::ALBUM_ROSTER_NOT_FOUND,
        }
    }

    /// A collaborator could not answer.
    fn unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::ALBUM_UNAVAILABLE,
        }
    }
}

/// Decode and shape-check a request into the signed roster and its verbatim bytes.
///
/// Everything here is decidable from the request alone: the encoding, the size, that the
/// document is for the album in the path, and that the member list is one the store can take
/// as a set. The account-level checks — owner, attester — need stores and follow.
fn decode(
    request: &RosterRequest,
    album: &AlbumId,
) -> Result<(SignedAlbumRoster, Vec<u8>), RosterRejection> {
    // The cap is on the decoded document, and it is applied to the encoded string first so an
    // oversized body is refused before it is decoded into a second buffer: base64 inflates by a
    // third, so anything longer than this cannot decode to under the cap.
    if request.roster_cbor.len() > MAX_ROSTER_BYTES / 3 * 4 + 4 {
        return Err(RosterRejection::malformed(format!(
            "a roster may be at most {MAX_ROSTER_BYTES} bytes"
        )));
    }
    let bytes = BASE64
        .decode(&request.roster_cbor)
        .map_err(|_| RosterRejection::malformed("roster_cbor is not standard base64"))?;
    if bytes.len() > MAX_ROSTER_BYTES {
        return Err(RosterRejection::malformed(format!(
            "a roster may be at most {MAX_ROSTER_BYTES} bytes"
        )));
    }
    let Ok(signed) = capsule_core::cbor::from_slice::<SignedAlbumRoster>(&bytes) else {
        return Err(RosterRejection::malformed(
            "roster_cbor is not a signed album roster",
        ));
    };
    // The bytes are stored verbatim and a replay is decided on them, so they must be the one
    // canonical encoding: a non-canonical or trailing-garbage document would verify (the
    // signature covers the re-encoded roster) and then make its own re-encoding a `409`.
    if capsule_core::cbor::canonicalize(&bytes).ok().as_deref() != Some(bytes.as_slice()) {
        return Err(RosterRejection::malformed(
            "roster_cbor is not canonical CBOR",
        ));
    }
    if signed.roster.album_id.to_string() != album.as_str() {
        return Err(RosterRejection::malformed(
            "the roster's album_id is not the album this request was addressed to",
        ));
    }
    if signed
        .roster
        .members
        .iter()
        .any(|member| member.user_id == signed.roster.attested_by_user)
    {
        return Err(RosterRejection::malformed(
            "the owner is not listed on their own roster",
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    if !signed
        .roster
        .members
        .iter()
        .all(|member| seen.insert(member.user_id))
    {
        return Err(RosterRejection::malformed(
            "a roster lists each account at most once",
        ));
    }
    Ok((signed, bytes))
}

/// Publish the caller's roster for one of their albums.
#[kynos::put(
    "/v1/albums/{album_id}/roster",
    operation_id = "publish_album_roster",
    tag = AlbumsTag
)]
pub async fn publish_album_roster(
    Inject(albums): Inject<AlbumContext>,
    Inject(directories): Inject<DeviceDirectoryContext>,
    Inject(membership): Inject<MembershipContext>,
    Auth(credential): Auth<AccessToken>,
    Path(path): Path<AlbumPath>,
    Json(request): Json<RosterRequest>,
) -> Result<Json<RosterResponse>, RosterRejection> {
    let user = UserId::new(credential.user.as_str());
    let album = AlbumId::new(&path.album_id);

    let (signed, bytes) = decode(&request, &album)?;

    // The album must be the caller's. Before the attester check, and answered as not-found, so
    // a member holding a valid roster for somebody else's album learns nothing about whether
    // the owner's directory would have accepted it.
    let record = albums.albums().read(&album).await.map_err(|error| {
        tracing::error!(%error, %album, "the album store could not answer a roster publish");
        RosterRejection::unavailable()
    })?;
    match record {
        Some(record) if record.owner_id.as_str() == user.as_str() => {}
        _ => {
            tracing::info!(%user, %album, "a roster was refused: no such album, or not the caller's");
            return Err(RosterRejection::not_found());
        }
    }
    if signed.roster.attested_by_user.to_string() != user.as_str() {
        tracing::info!(%user, %album, "a roster was refused: attested_by_user is not the caller");
        return Err(RosterRejection::attester());
    }

    // The attester, against the owner's published directory (`S-C42`'s anchor), exactly as the
    // upgrade ceremony verifies its proposer. Without this any holder of the owner's token could
    // rewrite who may read the album by PUTting a struct.
    let published = directories
        .store()
        .fetch(&user)
        .await
        .map_err(|error| {
            tracing::error!(%error, %user, "the directory store could not answer a roster publish");
            RosterRejection::unavailable()
        })?
        .ok_or_else(|| {
            tracing::info!(%user, "a roster was refused: no published device directory");
            RosterRejection::attester()
        })?;
    let Ok(directory) = capsule_core::cbor::from_slice::<capsule_core::crypto::keys::DeviceDirectory>(
        &published.document,
    ) else {
        // A document this server itself accepted and can no longer read. Its own inconsistency,
        // answered as an outage rather than as the caller's fault.
        tracing::error!(%user, "a stored device directory does not decode");
        return Err(RosterRejection::unavailable());
    };
    if let Err(error) = signed.verify(&directory) {
        tracing::info!(%user, %album, %error, "a roster's attester did not verify");
        return Err(RosterRejection::attester());
    }

    let members: Vec<(UserId, MemberRole)> = signed
        .roster
        .members
        .iter()
        .map(|member| (UserId::new(member.user_id.to_string()), member.role))
        .collect();
    let outcome = membership
        .members()
        .apply_roster(
            RosterRecord {
                album_id: album.clone(),
                roster_version: signed.roster.roster_version,
                amk_epoch: u64::from(signed.roster.amk_epoch.0),
                attested_by_device: signed.roster.attested_by_device,
                received_at: membership.clock().now(),
                document: bytes,
            },
            members,
        )
        .await
        .map_err(|error| {
            tracing::error!(%error, %album, "the membership store could not apply a roster");
            RosterRejection::unavailable()
        })?;

    let member_count = u64::try_from(signed.roster.members.len()).unwrap_or(u64::MAX);
    match outcome {
        RosterOutcome::Applied(record) => Ok(Json(describe(&record, member_count, false))),
        RosterOutcome::Replayed(record) => Ok(Json(describe(&record, member_count, true))),
        RosterOutcome::Stale { current_version } => Err(RosterRejection::Stale {
            current_version,
            code: error_codes::ALBUM_ROSTER_STALE,
        }),
        RosterOutcome::EpochRegressed {
            current_version,
            stored,
        } => {
            tracing::info!(
                %album,
                stored_epoch = stored,
                submitted_epoch = signed.roster.amk_epoch.0,
                "a roster was refused: its AMK epoch regressed"
            );
            // The held version, so the client's action is the same re-sync a stale version asks
            // for: the roster it holds is not the one the server does.
            Err(RosterRejection::Stale {
                current_version,
                code: error_codes::ALBUM_ROSTER_STALE,
            })
        }
    }
}

/// The response an accepted roster renders.
fn describe(record: &RosterRecord, member_count: u64, replayed: bool) -> RosterResponse {
    RosterResponse {
        album_id: record.album_id.as_str().to_owned(),
        roster_version: record.roster_version,
        amk_epoch: record.amk_epoch,
        member_count,
        replayed,
    }
}
