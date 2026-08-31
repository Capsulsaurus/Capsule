//! `POST /v1/albums` — album provisioning (slice `S-C25`).
//!
//! The one endpoint that lets a client tell the server an album exists. A container album's id
//! is derived from the account master key, so the client already knows it; this binds that id
//! to the authenticated caller so invariant 6 can pass for a real, client-named album.
//! [`crate::album`] owns the port and the reasoning; this is the wire shape.
//!
//! ```text
//! POST /v1/albums     { "album_id": "0198f3c2-9c4a-7b3d-8f21-4d7c9a1b2e35" }
//!
//! 201 { "album_id": …, "protocol_version": …, "created": true  }
//! 200 { "album_id": …, "protocol_version": …, "created": false }
//! 400 error.album.invalid_id
//! 403 error.album.not_available
//! ```
//!
//! **`created` is for logs, not for branching.** A client re-provisions on every device and
//! after recovery; both answers mean "the album is yours and writable", and a client that
//! branched on the flag would be treating a normal case as an error.
//!
//! **`403` is uninformative on purpose.** One code, one fixed message, whatever the reason — a
//! derived album id is unguessable before creation, and an answer that said "somebody else
//! holds this" would turn the endpoint into an existence oracle over other accounts' ids.
//!
//! **No name is accepted.** The body is strict and its only field is the id, so a `name` or
//! `description` is a `400` rather than a silently-dropped extra: a client is told the server
//! will not hold album titles rather than left to assume it did.
//!
//! **The pin comes back.** A client that has just provisioned learns which protocol the album
//! is pinned to without a second call — which is the value invariant 6 will compare its next
//! upload against.

use capsule_i18n::error_codes;
use kynos::prelude::*;
use serde::{Deserialize, Serialize};

use crate::album::{AlbumContext, AlbumRecord, ProvisionOutcome, is_canonical_album_id};
use crate::auth::AccessToken;
use crate::store::{AlbumId, OwnerId};

/// The albums surface: telling the server an album exists.
#[derive(Tag)]
#[tag(
    name = "albums",
    description = "Binding a client-derived album id to its owner."
)]
pub struct AlbumsTag;

/// The provisioning request.
#[derive(Schema, Serialize, Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct ProvisionAlbumRequest {
    /// The client-derived album id, as a canonical lowercase hyphenated UUID.
    pub album_id: String,
}

/// What provisioning did.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ProvisionAlbumResponse {
    /// The album, echoed.
    pub album_id: String,
    /// The protocol date the album is pinned to — the server's, fixed at creation.
    pub protocol_version: String,
    /// Whether this call created the album. Advisory; both answers mean the same thing.
    pub created: bool,
}

/// The two ways provisioning succeeds.
///
/// Both mean "the album is yours and writable"; the distinction is whether this call was the
/// one that made it so. A client never branches on it — which is why the `created` flag in the
/// body says the same thing as the status, for a reader looking at a log line rather than at a
/// response head.
#[derive(Reply)]
pub enum ProvisionReply {
    /// The album row was created and bound to the caller.
    #[reply(
        status = 201,
        description = "The album was created and bound to the caller"
    )]
    Created(ProvisionAlbumResponse),
    /// The id was already this caller's. Nothing was written.
    #[reply(
        status = 200,
        description = "The album id was already provisioned to this account; nothing was written"
    )]
    AlreadyProvisioned(ProvisionAlbumResponse),
}

/// Why an album was not provisioned.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum ProvisionRejection {
    /// The id is not a canonical hyphenated UUID.
    #[error("album_id must be a canonical lowercase hyphenated UUID")]
    #[problem(status = 400, title = "Invalid album id")]
    InvalidId {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The id cannot be bound to this account. One answer for every reason.
    #[error("that album id is not available")]
    #[problem(status = 403, title = "Album not available")]
    NotAvailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The store could not answer.
    #[error("the album could not be provisioned")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Bind an album id to the authenticated caller.
///
/// Idempotent: the same id from a second device, or after a recovery, is a success that writes
/// nothing.
#[kynos::post("/v1/albums", operation_id = "provision_album", tag = AlbumsTag)]
pub async fn provision_album(
    Inject(albums): Inject<AlbumContext>,
    Auth(credential): Auth<AccessToken>,
    Json(request): Json<ProvisionAlbumRequest>,
) -> Result<ProvisionReply, ProvisionRejection> {
    if !is_canonical_album_id(&request.album_id) {
        return Err(ProvisionRejection::InvalidId {
            code: error_codes::ALBUM_INVALID_ID,
        });
    }

    let owner = OwnerId::new(credential.user.as_str());
    let outcome = albums
        .albums()
        .provision(AlbumRecord {
            album_id: AlbumId::new(&request.album_id),
            owner_id: owner.clone(),
            // The server's own protocol, never the request's (`S-C19`). An album whose pin came
            // from a request would have invariant 6 comparing a write against itself.
            protocol_version: capsule_core::crypto::primitives::PROTOCOL_VERSION.to_owned(),
            created_at: albums.clock().now(),
        })
        .await
        .map_err(|error| {
            tracing::error!(%error, %owner, "the album store could not provision");
            ProvisionRejection::Unavailable {
                code: error_codes::ALBUM_UNAVAILABLE,
            }
        })?;

    match outcome {
        ProvisionOutcome::Created(record) => Ok(ProvisionReply::Created(describe(&record, true))),
        ProvisionOutcome::AlreadyProvisioned(record) => {
            Ok(ProvisionReply::AlreadyProvisioned(describe(&record, false)))
        }
        ProvisionOutcome::NotAvailable => Err(ProvisionRejection::NotAvailable {
            code: error_codes::ALBUM_NOT_AVAILABLE,
        }),
    }
}

/// The response an accepted provisioning renders.
fn describe(record: &AlbumRecord, created: bool) -> ProvisionAlbumResponse {
    ProvisionAlbumResponse {
        album_id: record.album_id.as_str().to_owned(),
        protocol_version: record.protocol_version.clone(),
        created,
    }
}
