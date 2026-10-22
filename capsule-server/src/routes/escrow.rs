//! The master-key escrow surface (slice `S-C12`), and the sealed identity beside it (#544).
//!
//! Two operations on one blob per account: store it, and fetch it back. There is no delete —
//! see below. The same two operations exist a second time at `/v1/auth/escrow/identity`, for
//! the account's user identity key sealed under its master key: the other half of what a
//! recovered device needs to open as the account it recovers. [`crate::escrow`] says why it is
//! a second slot and not a second root; on the wire it is the escrow's twin, with the same
//! media type, the same scoping, the same ceiling and the same refusals, except that a missing
//! one is `error.escrow.identity_not_stored` — a different remedy from a missing escrow.
//!
//! A third pair holds the account's **album keys** (#741): `PUT /v1/auth/escrow/albums/{album_id}`
//! stores one album's whole key state, sealed by the client under the master key, in a slot of
//! that album's own, and `GET /v1/auth/escrow/albums` lists every one the account holds, each
//! blob standard-base64 in a JSON listing. The listing is never a `404`: an account with no
//! albums escrowed is an empty list, which is what a recovery of an account that never created
//! an album has to read.
//!
//! [`crate::escrow`] owns the port, the single-active-escrow rule and the reason replacement is
//! one store operation. This module is the wire shape and its refusals.
//!
//! # Scoped to the caller, with no path parameter
//!
//! Unlike the device directory, an escrow is **not** public: the directory carries public keys
//! and exists to be read by strangers, while an escrow is a wrapped master key and the only
//! account entitled to it is its own. So there is no `{user_id}` segment to get wrong — the
//! account comes from the credential, which makes "fetching somebody else's escrow" not a
//! forbidden request but an unrepresentable one.
//!
//! # There is no delete, deliberately
//!
//! design/backup-recovery.md gives escrow three verbs — store, fetch, replace — and replace is
//! `PUT` over the same path. A standalone delete would be a way to remove an account's last
//! recovery path in one authenticated request, which is exactly the capability a stolen session
//! token should not have; rotation replaces, and account deletion takes the escrow with it.
//!
//! # `S-C28` audit
//!
//! | Status | Verdict |
//! | --- | --- |
//! | store `200` | the escrow is held; the body says whether it displaced one |
//! | store `400 error.escrow.malformed` | empty, or past the coarse ceiling — the only judgement the server is entitled to make |
//! | store `415` | the body is not `application/octet-stream`; [`crate::body`] replaces the framework's uncoded rejection |
//! | fetch `404 error.escrow.not_stored` | this account has escrowed nothing |
//! | identity fetch `404 error.escrow.identity_not_stored` | this account has stored no sealed identity |
//! | album keys store `400 error.album.invalid_id` | the path's album id is not a canonical UUID |
//! | album keys store `400 error.escrow.malformed` | empty, or past the album keys' own ceiling |
//! | album keys list `200` | every album escrowed, possibly none |
//! | `401` / `403` | the framework's, through `Auth` |
//! | `500 error.escrow.unavailable` | the store could not answer |
//!
//! No `409`. Storing an escrow over an existing one is the *documented* operation, not a
//! conflict, and answering `409` would make a client's guided re-wrap fail on the one path it
//! exists for.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use capsule_i18n::error_codes;
use kynos::extract::body::binary::Binary;
use kynos::extract::media::MediaType;
use kynos::prelude::*;
use serde::{Deserialize, Serialize};

use crate::album::is_canonical_album_id;
use crate::auth::AccessToken;
use crate::body::{CodedMedia, OpaqueBody};
use crate::escrow::{
    AlbumKeyRecord, EscrowContext, EscrowRecord, MalformedEscrow, Replaced, admissible,
    admissible_album_keys,
};
use crate::store::{AlbumId, UserId};

/// The recovery surface: the one blob a server holds that can reconstruct a library.
#[derive(Tag)]
#[tag(
    name = "escrow",
    description = "Storing and fetching the wrapped account master key. Opaque to the server."
)]
pub struct EscrowTag;

/// `application/octet-stream` — the escrow is ciphertext and has no schema the server knows.
///
/// Deliberately *not* a vendor type like the directory's `application/cbor`: that document has a
/// shape a generated client decodes, and this one does not. Claiming a structured media type
/// for bytes the server cannot parse would be a promise nobody can keep.
#[derive(Clone, Copy, Debug)]
pub struct OctetStream;

impl MediaType for OctetStream {
    const MEDIA_TYPE: &'static str = "application/octet-stream";
}

impl CodedMedia for OctetStream {
    const UNSUPPORTED_MEDIA_TYPE: &'static str = error_codes::ESCROW_MALFORMED;
    const UNREADABLE: &'static str = error_codes::ESCROW_MALFORMED;
}

/// The wrapped master key, as it arrives.
pub type EscrowBody = OpaqueBody<OctetStream>;

/// What storing an escrow did.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct StoreEscrowResponse {
    /// When the server accepted it, RFC 3339.
    ///
    /// Echoed so a client can tell whether a cached copy is current — the stale-cache rule,
    /// which exists because a rotation from another device would otherwise manufacture false
    /// verification failures on this one.
    pub stored_at: String,
    /// Whether this displaced an earlier escrow.
    ///
    /// A rotation and a first escrow are different events for a client: one completes account
    /// setup, and the other means the previous recovery secret has stopped working.
    pub replaced: bool,
}

/// Why an escrow was not stored.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum StoreEscrowRejection {
    /// The body cannot be an escrow at any version.
    #[error("{detail}")]
    #[problem(status = 400, title = "Malformed escrow")]
    Malformed {
        /// What was wrong, in English.
        detail: String,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The store could not answer.
    #[error("the escrow could not be stored")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Why an escrow was not returned.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum FetchEscrowRejection {
    /// This account has escrowed nothing.
    #[error("no escrow has been stored for this account")]
    #[problem(status = 404, title = "Not found")]
    NotStored {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The store could not answer.
    #[error("the escrow could not be read")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Store the caller's wrapped master key, replacing whatever they had.
///
/// `PUT`, because there is exactly one escrow per account and this is its address. Storing over
/// an existing escrow is the guided re-wrap, and it deletes the old blob in the same operation —
/// the lost recovery secret must stop working, which is the entire point of rotating.
#[kynos::put("/v1/auth/escrow", operation_id = "store_escrow", tag = EscrowTag)]
pub async fn store_escrow(
    Inject(escrow): Inject<EscrowContext>,
    Auth(credential): Auth<AccessToken>,
    body: EscrowBody,
) -> Result<Json<StoreEscrowResponse>, StoreEscrowRejection> {
    let user = UserId::new(credential.user.as_str());
    let blob = body.into_vec();

    admissible(&blob).map_err(|error| {
        tracing::info!(%user, %error, "an escrow was refused");
        StoreEscrowRejection::malformed(&error)
    })?;

    // Truncated to the microsecond before it is stored, so the instant this response echoes is
    // the instant every adapter keeps: the durable one holds microseconds, and a `stored_at`
    // that disagreed with the held one would manufacture the false stale-cache verdict the
    // field exists to prevent.
    let stored_at = crate::postgres::time::stored(escrow.clock().now());
    let replaced = escrow
        .escrows()
        .store(EscrowRecord {
            user_id: user.clone(),
            blob,
            stored_at,
        })
        .await
        .map_err(|error| {
            tracing::error!(%error, %user, "the escrow store could not store");
            StoreEscrowRejection::Unavailable {
                code: error_codes::ESCROW_UNAVAILABLE,
            }
        })?;

    Ok(Json(StoreEscrowResponse {
        stored_at: stored_at.to_string(),
        replaced: replaced == Replaced::Yes,
    }))
}

/// Fetch the caller's wrapped master key, verbatim.
///
/// The bytes are what a client runs its KDF against, so they come back exactly as they went in.
/// The server never derives, unwraps or re-encodes: a re-encoded wrap is a wrap that no longer
/// opens, and the failure would look like a lost master key.
#[kynos::get("/v1/auth/escrow", operation_id = "fetch_escrow", tag = EscrowTag)]
pub async fn fetch_escrow(
    Inject(escrow): Inject<EscrowContext>,
    Auth(credential): Auth<AccessToken>,
) -> Result<Binary<OctetStream>, FetchEscrowRejection> {
    let user = UserId::new(credential.user.as_str());
    let held = escrow
        .escrows()
        .fetch(&user)
        .await
        .map_err(|error| {
            tracing::error!(%error, %user, "the escrow store could not answer");
            FetchEscrowRejection::Unavailable {
                code: error_codes::ESCROW_UNAVAILABLE,
            }
        })?
        .ok_or_else(|| {
            tracing::debug!(%user, "no escrow has been stored for this account");
            FetchEscrowRejection::NotStored {
                code: error_codes::ESCROW_NOT_STORED,
            }
        })?;

    tracing::debug!(%user, bytes = held.blob.len(), "serving an escrow");
    Ok(Binary::new(held.blob))
}

/// What storing a sealed identity did.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct StoreSealedIdentityResponse {
    /// When the server accepted it, RFC 3339.
    pub stored_at: String,
    /// Whether this displaced an earlier sealed identity — a reseal after a master-key
    /// rotation, rather than the account's first.
    pub replaced: bool,
}

/// Why a sealed identity was not returned.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum FetchSealedIdentityRejection {
    /// This account has stored no sealed identity.
    #[error("no sealed identity has been stored for this account")]
    #[problem(status = 404, title = "Not found")]
    NotStored {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The store could not answer.
    #[error("the sealed identity could not be read")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Store the caller's user identity key sealed under their master key, replacing whatever they
/// had.
///
/// The sealed identity is ciphertext the server cannot open — it opens only under the master
/// key, which the escrow holds wrapped under a secret the server never sees. It is stored
/// independently of the escrow: a recovery-secret rotation replaces the escrow and leaves this
/// alone, and storing this never touches the escrow. Refused exactly as an escrow is: empty or
/// past the same coarse ceiling.
#[kynos::put(
    "/v1/auth/escrow/identity",
    operation_id = "store_sealed_identity",
    tag = EscrowTag
)]
pub async fn store_sealed_identity(
    Inject(escrow): Inject<EscrowContext>,
    Auth(credential): Auth<AccessToken>,
    body: EscrowBody,
) -> Result<Json<StoreSealedIdentityResponse>, StoreEscrowRejection> {
    let user = UserId::new(credential.user.as_str());
    let blob = body.into_vec();

    admissible(&blob).map_err(|error| {
        tracing::info!(%user, %error, "a sealed identity was refused");
        StoreEscrowRejection::malformed(&error)
    })?;

    // Truncated to the microsecond for the same reason the escrow's is: the instant echoed is
    // the instant every adapter keeps.
    let stored_at = crate::postgres::time::stored(escrow.clock().now());
    let replaced = escrow
        .escrows()
        .store_identity(EscrowRecord {
            user_id: user.clone(),
            blob,
            stored_at,
        })
        .await
        .map_err(|error| {
            tracing::error!(%error, %user, "the escrow store could not store a sealed identity");
            StoreEscrowRejection::Unavailable {
                code: error_codes::ESCROW_UNAVAILABLE,
            }
        })?;

    Ok(Json(StoreSealedIdentityResponse {
        stored_at: stored_at.to_string(),
        replaced: replaced == Replaced::Yes,
    }))
}

/// Fetch the caller's sealed identity, verbatim.
///
/// Byte for byte, like the escrow: the client opens it under the master key it recovered, and
/// a re-encoded seal is a seal that no longer opens.
#[kynos::get(
    "/v1/auth/escrow/identity",
    operation_id = "fetch_sealed_identity",
    tag = EscrowTag
)]
pub async fn fetch_sealed_identity(
    Inject(escrow): Inject<EscrowContext>,
    Auth(credential): Auth<AccessToken>,
) -> Result<Binary<OctetStream>, FetchSealedIdentityRejection> {
    let user = UserId::new(credential.user.as_str());
    let held = escrow
        .escrows()
        .fetch_identity(&user)
        .await
        .map_err(|error| {
            tracing::error!(%error, %user, "the escrow store could not answer for a sealed identity");
            FetchSealedIdentityRejection::Unavailable {
                code: error_codes::ESCROW_UNAVAILABLE,
            }
        })?
        .ok_or_else(|| {
            tracing::debug!(%user, "no sealed identity has been stored for this account");
            FetchSealedIdentityRejection::NotStored {
                code: error_codes::ESCROW_IDENTITY_NOT_STORED,
            }
        })?;

    tracing::debug!(%user, bytes = held.blob.len(), "serving a sealed identity");
    Ok(Binary::new(held.blob))
}

impl StoreEscrowRejection {
    /// The body cannot be an escrow.
    fn malformed(error: &MalformedEscrow) -> Self {
        Self::Malformed {
            detail: error.to_string(),
            code: error_codes::ESCROW_MALFORMED,
        }
    }
}

/// The album whose keys an album key escrow is.
#[derive(PathParams, Schema)]
pub struct AlbumKeysPath {
    /// The album's id, as a canonical lowercase hyphenated UUID.
    pub album_id: String,
}

/// What storing an album key escrow did.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct StoreAlbumKeysResponse {
    /// The album, echoed.
    pub album_id: String,
    /// When the server accepted it, RFC 3339.
    pub stored_at: String,
    /// Whether this displaced an earlier escrow of this album's keys — the album changed since
    /// (an epoch was rotated in), rather than this being its first.
    pub replaced: bool,
}

/// Why an album key escrow was not stored.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum StoreAlbumKeysRejection {
    /// The path names no album: its id is not a canonical lowercase hyphenated UUID.
    #[error("album_id must be a canonical lowercase hyphenated UUID")]
    #[problem(status = 400, title = "Malformed escrow")]
    InvalidAlbumId {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The body cannot be an album key escrow at any version.
    #[error("{detail}")]
    #[problem(status = 400, title = "Malformed escrow")]
    Malformed {
        /// What was wrong, in English.
        detail: String,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The store could not answer.
    #[error("the album key escrow could not be stored")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// One album key escrow, as the listing carries it.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct EscrowedAlbumKeys {
    /// The album the keys are of.
    pub album_id: String,
    /// The sealed keys, standard base64 — exactly the bytes the client stored.
    pub blob: String,
    /// When the server accepted them, RFC 3339.
    pub stored_at: String,
}

/// Every album key escrow the caller holds.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AlbumKeysListing {
    /// One entry per escrowed album, in album id order. Empty when none is held: an account
    /// with no albums is a state, not a missing resource.
    pub albums: Vec<EscrowedAlbumKeys>,
}

/// Why the album key escrows were not listed.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum ListAlbumKeysRejection {
    /// The store could not answer.
    #[error("the album key escrows could not be read")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Store the caller's key escrow for one album, replacing whatever that album's slot held.
///
/// The album's whole key state — every epoch's content key, its signing seeds and its
/// authority — sealed by the client under the account master key, so a device recovered from the
/// escrow can open the album (#741). Opaque here: one slot per album, replaced in one operation,
/// never versioned, and refused only empty or past its coarse ceiling. The album id is the
/// slot's name and nothing more; the server does not check the album exists, because a client
/// escrows an album's keys **before** the album's first asset reaches the server.
#[kynos::put(
    "/v1/auth/escrow/albums/{album_id}",
    operation_id = "store_album_keys",
    tag = EscrowTag
)]
pub async fn store_album_keys(
    Inject(escrow): Inject<EscrowContext>,
    Auth(credential): Auth<AccessToken>,
    Path(path): Path<AlbumKeysPath>,
    body: EscrowBody,
) -> Result<Json<StoreAlbumKeysResponse>, StoreAlbumKeysRejection> {
    let user = UserId::new(credential.user.as_str());
    if !is_canonical_album_id(&path.album_id) {
        tracing::info!(%user, album = %path.album_id, "an album key escrow named no album");
        return Err(StoreAlbumKeysRejection::InvalidAlbumId {
            code: error_codes::ALBUM_INVALID_ID,
        });
    }
    let album = AlbumId::new(&path.album_id);
    let blob = body.into_vec();
    admissible_album_keys(&blob).map_err(|error| {
        tracing::info!(%user, %album, %error, "an album key escrow was refused");
        StoreAlbumKeysRejection::Malformed {
            detail: error.to_string(),
            code: error_codes::ESCROW_MALFORMED,
        }
    })?;

    // Truncated to the microsecond for the escrow's reason: the instant echoed is the instant
    // every adapter keeps.
    let stored_at = crate::postgres::time::stored(escrow.clock().now());
    let replaced = escrow
        .escrows()
        .store_album_keys(AlbumKeyRecord {
            user_id: user.clone(),
            album_id: album.clone(),
            blob,
            stored_at,
        })
        .await
        .map_err(|error| {
            tracing::error!(%error, %user, %album, "the escrow store could not store album keys");
            StoreAlbumKeysRejection::Unavailable {
                code: error_codes::ESCROW_UNAVAILABLE,
            }
        })?;

    Ok(Json(StoreAlbumKeysResponse {
        album_id: album.as_str().to_owned(),
        stored_at: stored_at.to_string(),
        replaced: replaced == Replaced::Yes,
    }))
}

/// List every album key escrow the caller holds, each blob verbatim.
///
/// What `capsule auth recover` reads to give a library created from the master key the
/// account's albums before anything creates one, and what a push compares its albums against
/// to re-escrow only the ones that changed.
#[kynos::get(
    "/v1/auth/escrow/albums",
    operation_id = "list_album_keys",
    tag = EscrowTag
)]
pub async fn list_album_keys(
    Inject(escrow): Inject<EscrowContext>,
    Auth(credential): Auth<AccessToken>,
) -> Result<Json<AlbumKeysListing>, ListAlbumKeysRejection> {
    let user = UserId::new(credential.user.as_str());
    let held = escrow
        .escrows()
        .list_album_keys(&user)
        .await
        .map_err(|error| {
            tracing::error!(%error, %user, "the escrow store could not list album keys");
            ListAlbumKeysRejection::Unavailable {
                code: error_codes::ESCROW_UNAVAILABLE,
            }
        })?;
    tracing::debug!(%user, albums = held.len(), "serving the album key escrows");
    Ok(Json(AlbumKeysListing {
        albums: held
            .into_iter()
            .map(|record| EscrowedAlbumKeys {
                album_id: record.album_id.as_str().to_owned(),
                blob: STANDARD.encode(&record.blob),
                stored_at: record.stored_at.to_string(),
            })
            .collect(),
    }))
}
