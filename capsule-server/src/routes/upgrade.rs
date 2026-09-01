//! The album-upgrade ceremony's **server halves** (slice `S-C24`).
//!
//! [Versioning — Album Upgrade Ceremony](../../../capsule-docs/src/content/docs/design/versioning.md#album-upgrade-ceremony)
//! is a client ceremony carried on MLS application messages the server cannot read. Four of its
//! steps are nevertheless the server's, and every one of them is the server's *because* the
//! clients cannot do it themselves:
//!
//! | Step | Why the server |
//! | --- | --- |
//! | the deadline's expiry | it is `received_at + deadline` on the **server's trusted clock**, so that a skewed member clock can neither extend nor shorten the window |
//! | the `409` on a stale write | a v_old client that never saw the `UpgradeIntent` is precisely the party that will not stop writing on its own |
//! | the drain count | only the server knows how many sessions are still in flight against the album |
//! | the lineage | `upgraded_from` rides a manifest, and manifests are stored here |
//!
//! # What the server verifies, and what it must not
//!
//! It verifies the proposer's DSK signature over the intent against the account's **published
//! device directory** — the trust anchor `S-C42` established — so a quiescence it records is one
//! an admin device really asked for, and a member's client cannot freeze somebody's album by
//! POSTing an unsigned struct.
//!
//! It does **not** verify the `frozen_state_hash`, and there is no surface here that could carry
//! one. That hash is each member's independent statement about its own view of the album, and the
//! ceremony's *hostile member sabotage* defence is precisely that every member checks it for
//! itself. A server that adjudicated it would be the single point that defence exists to avoid.
//!
//! # Expiry is not a job
//!
//! Nothing sweeps expired ceremonies. An expired quiescence is treated *everywhere* as absent —
//! by the write gate, by this surface's phase, and by a fresh proposal, which replaces it rather
//! than conflicting with it. That is versioning.md step 3's *"on deadline expiry the upgrade
//! aborts cleanly"* implemented as an absence of state rather than as a worker, and it is what
//! stops a proposer who vanished from freezing an album forever.
//!
//! # `S-C28` audit
//!
//! Every status here is reachable and every reachable one is declared. The `409` is the one worth
//! naming: only *one* upgrade may be in flight per album, so a second proposal under a different
//! `intent_id` is refused with the live id — reached only by the album's owner, so it discloses
//! nothing.

use capsule_i18n::error_codes;
use kynos::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::album::{AlbumContext, UpgradeOutcome, UpgradeQuiescence};
use crate::auth::AccessToken;
use crate::body::{CodedMedia, OpaqueBody};
use crate::directory::DeviceDirectoryContext;
use crate::routes::albums::AlbumsTag;
use crate::store::{AlbumId, OwnerId, UserId};
use crate::upload::UploadContext;

/// The largest signed intent this surface accepts.
///
/// An `UpgradeIntent` is a handful of strings and two hybrid signatures; 64 KiB is three orders
/// of magnitude more than one needs and still small enough that a refusal costs nothing. The
/// transport backstop (`S-C33`) is 32 MiB, which is not a bound on a ceremony message.
pub const MAX_INTENT_BYTES: usize = 64 * 1024;

/// `application/cbor` — the encoding the intent is *signed* in.
#[derive(Clone, Copy, Debug)]
pub struct IntentCbor;

impl kynos::extract::media::MediaType for IntentCbor {
    const MEDIA_TYPE: &'static str = "application/cbor";
}

impl CodedMedia for IntentCbor {
    const UNSUPPORTED_MEDIA_TYPE: &'static str = error_codes::UPLOAD_UNSUPPORTED_MEDIA_TYPE;
    const UNREADABLE: &'static str = error_codes::ALBUM_UPGRADE_MALFORMED;
}

/// The signed upgrade intent, as it arrives.
pub type IntentBody = OpaqueBody<IntentCbor>;

/// The album an upgrade is addressed to.
#[derive(PathParams, Schema)]
pub struct AlbumPath {
    /// The album's id.
    pub album_id: String,
}

/// The ceremony this album is in, as a client polls it.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct UpgradePhaseResponse {
    /// The album, echoed.
    pub album_id: String,
    /// The ceremony in flight, or absent when the album is in normal operation.
    ///
    /// Absent also covers *expired*: the deadline passing aborts the upgrade, so there is nothing
    /// left to be in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent_id: Option<String>,
    /// The protocol version the fork will be pinned to, when a ceremony is in flight.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_protocol_version: Option<String>,
    /// When the window closes, RFC 3339, on the **server's** clock.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// How many upload sessions are still in flight against this album.
    ///
    /// The drain signal of versioning.md step 3: the proposer waits for zero. A count rather than
    /// a listing, because the proposer needs to know *whether* to wait and has no business seeing
    /// other members' upload identifiers to find out.
    pub in_flight: u64,
}

/// Why a **proposal** was refused.
///
/// One enum per operation rather than one shared across three, because a shared enum
/// over-declares: the phase read cannot be malformed and cannot conflict, and a status an
/// operation cannot produce is the `S-C28` defect however plausible it looks in a list.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum UpgradeRejection {
    /// The body is not a signed intent this server can read, or the album id is not one.
    #[error("{detail}")]
    #[problem(status = 400, title = "Malformed request")]
    Malformed {
        /// What was wrong, in English.
        detail: String,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The intent is not signed by a device in the caller's published directory.
    #[error("the upgrade intent's proposer could not be verified")]
    #[problem(status = 403, title = "Proposer not authorized")]
    ProposerNotAuthorized {
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

    /// A different ceremony is already in flight, and only one may be.
    #[error("album is already upgrading under {intent_id}")]
    #[problem(status = 409, title = "Upgrade in flight")]
    AlreadyUpgrading {
        /// The ceremony that holds the album.
        #[problem(extension)]
        intent_id: String,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A collaborator could not answer.
    #[error("the upgrade could not be recorded")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

impl UpgradeRejection {
    /// The request was not a well-formed proposal.
    fn malformed(detail: impl Into<String>) -> Self {
        Self::Malformed {
            detail: detail.into(),
            code: error_codes::ALBUM_UPGRADE_MALFORMED,
        }
    }

    /// No such album, or not this caller's.
    fn not_found() -> Self {
        Self::NotFound {
            code: error_codes::ALBUM_UPGRADE_NOT_FOUND,
        }
    }

    /// A collaborator could not answer.
    fn unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::ALBUM_UNAVAILABLE,
        }
    }
}

/// Why a **phase read** was refused.
///
/// Two statuses, and that is the whole surface: a read cannot be malformed (its only input is a
/// path segment, and any string is an album id it does not hold) and cannot conflict with
/// anything. Declaring the proposal's `400`, `403` and `409` here would be three promises this
/// operation cannot keep.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum PhaseRejection {
    /// No such album, or not this caller's.
    #[error("no such album")]
    #[problem(status = 404, title = "Not found")]
    NotFound {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A collaborator could not answer.
    #[error("the album's upgrade phase could not be read")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

impl From<UpgradeRejection> for PhaseRejection {
    /// The two answers a phase read shares with a proposal, and nothing else.
    ///
    /// Total by construction: the `phase` helper produces only these two, so the fallback arm is
    /// unreachable — and it is written as an outage rather than a panic because a response path
    /// is the wrong place to be certain.
    fn from(rejection: UpgradeRejection) -> Self {
        match rejection {
            UpgradeRejection::NotFound { code } => Self::NotFound { code },
            _ => Self::Unavailable {
                code: error_codes::ALBUM_UNAVAILABLE,
            },
        }
    }
}

/// Why an **abort** was refused.
///
/// No `403`: aborting verifies no signature, because the id is the authority — a caller that does
/// not hold the live `intent_id` gets a `409` rather than a chance to argue about who they are.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum AbortRejection {
    /// The `intent_id` is not a UUID.
    #[error("{detail}")]
    #[problem(status = 400, title = "Malformed request")]
    Malformed {
        /// What was wrong, in English.
        detail: String,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// No such album, or not this caller's.
    #[error("no such album")]
    #[problem(status = 404, title = "Not found")]
    NotFound {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A different ceremony holds the album.
    #[error("album is already upgrading under {intent_id}")]
    #[problem(status = 409, title = "Upgrade in flight")]
    AlreadyUpgrading {
        /// The ceremony that holds it.
        #[problem(extension)]
        intent_id: String,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A collaborator could not answer.
    #[error("the upgrade could not be ended")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

impl From<UpgradeRejection> for AbortRejection {
    fn from(rejection: UpgradeRejection) -> Self {
        match rejection {
            UpgradeRejection::NotFound { code } => Self::NotFound { code },
            UpgradeRejection::AlreadyUpgrading { intent_id, code } => {
                Self::AlreadyUpgrading { intent_id, code }
            }
            UpgradeRejection::Malformed { detail, code } => Self::Malformed { detail, code },
            _ => Self::Unavailable {
                code: error_codes::ALBUM_UNAVAILABLE,
            },
        }
    }
}

/// Put an album into upgrade quiescence.
///
/// Idempotent under its own `intent_id`: versioning.md is explicit that the same `UpgradeIntent`
/// never produces two forks, and a proposer that lost an acknowledgement re-POSTs the same bytes.
#[kynos::post(
    "/v1/albums/{album_id}/upgrade",
    operation_id = "begin_album_upgrade",
    tag = AlbumsTag
)]
pub async fn begin_album_upgrade(
    Inject(albums): Inject<AlbumContext>,
    Inject(directories): Inject<DeviceDirectoryContext>,
    Inject(uploads): Inject<UploadContext>,
    Auth(credential): Auth<AccessToken>,
    Path(path): Path<AlbumPath>,
    body: IntentBody,
) -> Result<Json<UpgradePhaseResponse>, UpgradeRejection> {
    let user = UserId::new(credential.user.as_str());
    let owner = OwnerId::new(credential.user.as_str());
    let album = AlbumId::new(&path.album_id);

    let bytes = body.into_vec();
    if bytes.len() > MAX_INTENT_BYTES {
        return Err(UpgradeRejection::malformed(format!(
            "an upgrade intent may be at most {MAX_INTENT_BYTES} bytes"
        )));
    }
    let Ok(signed) = capsule_core::cbor::from_slice::<
        capsule_core::crypto::upgrade::SignedUpgradeIntent,
    >(&bytes) else {
        return Err(UpgradeRejection::malformed(
            "the body is not a signed upgrade intent",
        ));
    };

    // The proposer, against the account's published directory (`S-C42`'s anchor). Without this
    // any member's client — or anyone holding a token — could freeze an album by POSTing a
    // struct, which is the opposite of a ceremony keyed to an admin device.
    let published = directories
        .store()
        .fetch(&user)
        .await
        .map_err(|error| {
            tracing::error!(%error, %user, "the directory store could not answer an upgrade");
            UpgradeRejection::unavailable()
        })?
        .ok_or_else(|| {
            tracing::info!(%user, "an upgrade was refused: no published device directory");
            UpgradeRejection::ProposerNotAuthorized {
                code: error_codes::ALBUM_UPGRADE_PROPOSER,
            }
        })?;
    let Ok(directory) = capsule_core::cbor::from_slice::<capsule_core::crypto::keys::DeviceDirectory>(
        &published.document,
    ) else {
        // A document this server itself accepted and can no longer read. Its own inconsistency,
        // answered as an outage rather than as the caller's fault.
        tracing::error!(%user, "a stored device directory does not decode");
        return Err(UpgradeRejection::unavailable());
    };
    if let Err(error) = signed.verify(&directory) {
        tracing::info!(%user, %error, "an upgrade intent's proposer did not verify");
        return Err(UpgradeRejection::ProposerNotAuthorized {
            code: error_codes::ALBUM_UPGRADE_PROPOSER,
        });
    }

    let now = albums.clock().now();
    // The window starts *now*, on this server's clock, and nowhere else. `received_at` is the
    // whole reason the deadline is a duration rather than an instant.
    let outcome = albums
        .albums()
        .begin_upgrade(
            &album,
            &owner,
            UpgradeQuiescence {
                intent: signed.intent.clone(),
                received_at: now,
            },
            now,
        )
        .await
        .map_err(|error| {
            tracing::error!(%error, %album, "the album store could not record an upgrade");
            UpgradeRejection::unavailable()
        })?;

    match outcome {
        UpgradeOutcome::Quiescing(_) | UpgradeOutcome::Cleared(_) => {
            Ok(Json(phase(&uploads, &albums, &album, &owner, now).await?))
        }
        UpgradeOutcome::Conflict { intent_id } => Err(UpgradeRejection::AlreadyUpgrading {
            intent_id: intent_id.to_string(),
            code: error_codes::ALBUM_UPGRADE_IN_FLIGHT,
        }),
        UpgradeOutcome::NotFound => Err(UpgradeRejection::not_found()),
    }
}

/// Read the ceremony's phase and the drain count.
///
/// The one call a proposer polls between steps 2 and 4. `in_flight` reaching zero is the signal
/// that the tombstone may be committed.
#[kynos::get(
    "/v1/albums/{album_id}/upgrade",
    operation_id = "album_upgrade_phase",
    tag = AlbumsTag
)]
pub async fn album_upgrade_phase(
    Inject(albums): Inject<AlbumContext>,
    Inject(uploads): Inject<UploadContext>,
    Auth(credential): Auth<AccessToken>,
    Path(path): Path<AlbumPath>,
) -> Result<Json<UpgradePhaseResponse>, PhaseRejection> {
    let owner = OwnerId::new(credential.user.as_str());
    let album = AlbumId::new(&path.album_id);
    let now = albums.clock().now();
    Ok(Json(phase(&uploads, &albums, &album, &owner, now).await?))
}

/// Abort a ceremony, returning the album to normal operation.
///
/// Named by `intent_id` in the path's own query so that aborting is a statement about *which*
/// upgrade — a caller that does not hold the live id gets a `409` rather than the power to
/// cancel somebody else's ceremony.
#[kynos::delete(
    "/v1/albums/{album_id}/upgrade",
    operation_id = "abort_album_upgrade",
    tag = AlbumsTag
)]
pub async fn abort_album_upgrade(
    Inject(albums): Inject<AlbumContext>,
    Inject(uploads): Inject<UploadContext>,
    Auth(credential): Auth<AccessToken>,
    Path(path): Path<AlbumPath>,
    Query(query): Query<AbortQuery>,
) -> Result<Json<UpgradePhaseResponse>, AbortRejection> {
    let owner = OwnerId::new(credential.user.as_str());
    let album = AlbumId::new(&path.album_id);
    let Ok(intent_id) = Uuid::parse_str(&query.intent_id) else {
        return Err(AbortRejection::Malformed {
            detail: "intent_id is not a UUID".to_owned(),
            code: error_codes::ALBUM_UPGRADE_MALFORMED,
        });
    };
    let now = albums.clock().now();

    match albums
        .albums()
        .end_upgrade(&album, &owner, intent_id, now)
        .await
        .map_err(|error| {
            tracing::error!(%error, %album, "the album store could not end an upgrade");
            AbortRejection::Unavailable {
                code: error_codes::ALBUM_UNAVAILABLE,
            }
        })? {
        UpgradeOutcome::Cleared(_) | UpgradeOutcome::Quiescing(_) => {
            Ok(Json(phase(&uploads, &albums, &album, &owner, now).await?))
        }
        UpgradeOutcome::Conflict { intent_id } => Err(AbortRejection::AlreadyUpgrading {
            intent_id: intent_id.to_string(),
            code: error_codes::ALBUM_UPGRADE_IN_FLIGHT,
        }),
        UpgradeOutcome::NotFound => Err(AbortRejection::NotFound {
            code: error_codes::ALBUM_UPGRADE_NOT_FOUND,
        }),
    }
}

/// The `intent_id` an abort names.
#[derive(QueryParams, Schema)]
pub struct AbortQuery {
    /// The ceremony to abort.
    pub intent_id: String,
}

/// The album's phase, with its drain count.
async fn phase(
    uploads: &UploadContext,
    albums: &AlbumContext,
    album: &AlbumId,
    owner: &OwnerId,
    now: jiff::Timestamp,
) -> Result<UpgradePhaseResponse, UpgradeRejection> {
    let record = albums
        .albums()
        .read(album)
        .await
        .map_err(|error| {
            tracing::error!(%error, %album, "the album store could not answer a phase");
            UpgradeRejection::unavailable()
        })?
        .filter(|record| &record.owner_id == owner)
        .ok_or_else(UpgradeRejection::not_found)?;

    let in_flight = uploads
        .sessions()
        .in_flight_for_album(album)
        .await
        .map_err(|error| {
            tracing::error!(%error, %album, "the upload sessions could not be counted");
            UpgradeRejection::unavailable()
        })?;

    // An expired ceremony is reported as none: the deadline passing *is* the abort.
    let live = record
        .upgrade
        .as_ref()
        .filter(|quiescence| !quiescence.is_expired(now));

    Ok(UpgradePhaseResponse {
        album_id: album.as_str().to_owned(),
        intent_id: live.map(|q| q.intent.intent_id.to_string()),
        to_protocol_version: live.map(|q| q.intent.to_protocol_version.clone()),
        expires_at: live.map(|q| q.expires_at().to_string()),
        in_flight,
    })
}
