//! The federation capability's lifecycle: minting, revoking and refreshing (`S-E2`, `S-C49`).
//!
//! Not the pull path. design/federation.md is explicit that federation adds **no new data
//! protocol** — a peer pulls through `GET /v1/sync?album_id=` and `GET /v1/blob/{hash}`, which
//! is [`crate::routes::sync`] and [`crate::routes::blob`]. What is here is the credential those
//! two reads accept and the three operations that manage it.
//!
//! ```text
//! POST   /v1/albums/{album_id}/capabilities        { peer, member, scope, ttl_seconds? }
//! 201 { token, jti, album_id, peer, member, scope, issued_at, expires_at, min_protocol_version }
//! 400 error.federation.capability_malformed
//! 403 error.federation.not_configured | error.moderation.server_blocked
//! 404 error.federation.album_not_found
//! 409 error.federation.member_not_on_roster
//! 500 error.federation.unavailable
//!
//! DELETE /v1/albums/{album_id}/capabilities/{jti}
//! 204 (idempotent)
//! 404 error.federation.album_not_found
//! 500 error.federation.unavailable
//!
//! POST   /v1/federation/capabilities/refresh       (the capability itself is the credential)
//! 200 { token, jti, expires_at, replayed }
//! 403 error.federation.capability_invalid | error.federation.capability_revoked
//!     | error.moderation.server_blocked | error.federation.not_configured
//! 429 error.federation.rate_budget_exceeded
//! 500 error.federation.unavailable
//! ```
//!
//! # Who mints, and against what
//!
//! **The album's owner, through its own client.** design/federation.md puts minting on the home
//! server at the moment the owner shares, so the credential is `Auth<AccessToken>` and the album
//! must be the caller's — answered `404` when it is not, the album ceremonies' "not yours is not
//! found", so a member holding somebody else's album id learns nothing.
//!
//! The mint needs no key of the peer's: the token is signed with **this** server's operational
//! Ed25519 key, the one `/.well-known/capsule/server-info` publishes, and the peer verifies it
//! against that. A pinned peer key is needed only to verify a signed moderation report.
//!
//! Two facts are checked before anything is signed. The peer must not be **blocked** — the
//! server-level blocklist operates at exactly this layer (design/moderation.md) — and the member
//! must be on the album's **current roster**, whose `granted_epoch` is copied into the record.
//! That epoch is the server-side half of the grant: at every presentation the member's current
//! epoch must still equal it, so a member removed and re-admitted later gets a fresh membership
//! and the old capability dies without anyone revoking it. It is a stored fact rather than a
//! claim because the token format is normative and parsed by every peer.
//!
//! # Revoking is not gated on the deployment federating
//!
//! Minting and refreshing refuse `403 error.federation.not_configured` when `FEDERATION_URL` is
//! unset: a server that does not federate does not hand out new grants. **Revoking is not**, and
//! deliberately: turning federation off must never be the thing that takes away an operator's
//! ability to cut a grant that is already out there. Nor does an unset `FEDERATION_URL` stop an
//! already-minted capability verifying — a token is not un-minted by a configuration change, and
//! silently refusing one would cut a peer off with no revocation anybody can see.
//!
//! # Refresh, and why it is idempotent by construction
//!
//! The **previous capability** is the credential (design/federation.md: "refresh authenticated
//! by the previous token"), so this operation takes `Auth<ReadBearer>` and refuses a session
//! principal: an account has nothing to refresh here. The store issues the successor, links the
//! predecessor to it and revokes the predecessor in one critical section, so a replayed refresh
//! finds the link and is answered with **the same token** — the grant is re-signed from its
//! stored record, and because every instant is at whole seconds and Ed25519 is deterministic the
//! bytes are the bytes the peer already holds. That is the `(peer, jti)` idempotency
//! threat-model/validation.md asks for, without an idempotency table.
//!
//! A successor answered to a replay may itself have been revoked since — a block cascades over
//! every live capability of a peer — so its liveness is re-checked before it is re-signed.

use capsule_i18n::error_codes;
use jiff::SignedDuration;
use kynos::prelude::*;
use kynos::response::status::NoContent;
use kynos::security::auth::Auth;
use serde::{Deserialize, Serialize};

use crate::album::AlbumContext;
use crate::auth::AccessToken;
use crate::counter::CounterContext;
use crate::federation::{
    self, CapabilityRecord, FederationContext, MintRequest, PeerId, Presentation, Principal,
    ReadBearer, Refusal, Scope,
};
use crate::membership::{Membership, MembershipContext};
use crate::store::{AlbumId, UserId};

/// The federation surface: the capability a peer server pulls a shared album with.
#[derive(Tag)]
#[tag(
    name = "federation",
    description = "Minting, revoking and refreshing the capability a peer server pulls with."
)]
pub struct FederationTag;

/// The default life of a minted capability when the caller names none.
///
/// Six hours: long enough that a peer pulling an evening's photos never refreshes mid-pull,
/// short enough that a grant nobody revokes is not a day-long hole. The ceiling is the
/// contract's 24 hours and the codec clamps to it whatever is asked for.
pub const DEFAULT_TTL: SignedDuration = SignedDuration::from_hours(6);

/// What a capability permits, on the wire.
///
/// A mirror of [`Scope`] rather than the type itself, for the reason
/// [`WireBlobRole`](crate::routes::upload::WireBlobRole) is one: the domain enum is not a schema
/// type, and the wire spelling is a contract that should not move when an internal name does.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum WireScope {
    /// Everything a member reads: originals, derivatives, metadata, provenance.
    Read,
    /// Thumbnails and previews only — never originals.
    ReadDerivativeOnly,
}

impl From<WireScope> for Scope {
    fn from(scope: WireScope) -> Self {
        match scope {
            WireScope::Read => Self::Read,
            WireScope::ReadDerivativeOnly => Self::ReadDerivativeOnly,
        }
    }
}

impl From<Scope> for WireScope {
    fn from(scope: Scope) -> Self {
        match scope {
            Scope::Read => Self::Read,
            Scope::ReadDerivativeOnly => Self::ReadDerivativeOnly,
        }
    }
}

/// The mint request.
#[derive(Schema, Serialize, Deserialize, Debug, Clone)]
pub struct MintCapabilityRequest {
    /// The peer server the grant is for, as its own `server-info` names it (`other.tld`).
    pub peer: String,
    /// The roster member whose access the grant carries, as the owner listed them.
    pub member: String,
    /// What the grant permits.
    pub scope: WireScope,
    /// How long it should live, in seconds. Clamped to the 24-hour ceiling; absent is six hours.
    pub ttl_seconds: Option<u64>,
}

/// A freshly minted capability.
///
/// The token is returned **once**. Nothing on this server can produce it again — a stored grant
/// re-signs byte-for-byte, but only the refresh operation does that, and only for its holder.
#[derive(Schema, Serialize, Deserialize, Debug, Clone)]
pub struct MintedCapabilityResponse {
    /// The signed capability, to be carried as `Authorization: Bearer`.
    pub token: String,
    /// Its identifier, and the key it is revoked by.
    pub jti: String,
    /// The album it scopes to.
    pub album_id: String,
    /// The peer it was minted for.
    pub peer: String,
    /// The roster member whose access it carries.
    pub member: String,
    /// What it permits.
    pub scope: WireScope,
    /// When it was minted, RFC 3339.
    pub issued_at: String,
    /// When it stops being honoured, RFC 3339.
    pub expires_at: String,
    /// The album's pinned protocol date, which the peer must speak to pull.
    pub min_protocol_version: String,
}

/// A refreshed capability.
#[derive(Schema, Serialize, Deserialize, Debug, Clone)]
pub struct RefreshedCapabilityResponse {
    /// The successor token.
    pub token: String,
    /// Its identifier.
    pub jti: String,
    /// When it stops being honoured, RFC 3339.
    pub expires_at: String,
    /// Whether this call issued the successor, or answered one an earlier call already issued.
    ///
    /// Advisory. A peer never branches on it: both answers mean "here is the token to keep
    /// pulling with".
    pub replayed: bool,
}

/// The album a capability is minted over.
#[derive(PathParams, Schema)]
pub struct CapabilitiesPath {
    /// The album's id.
    pub album_id: String,
}

/// One capability of one album.
#[derive(PathParams, Schema)]
pub struct CapabilityPath {
    /// The album's id.
    pub album_id: String,
    /// The capability's `jti`.
    pub jti: String,
}

/// Why a capability was not minted.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum MintRejection {
    /// A field of the body is not what it must be.
    #[error("the capability request is malformed")]
    #[problem(status = 400, title = "Malformed request")]
    Malformed {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// This deployment does not federate.
    #[error("this server does not federate")]
    #[problem(status = 403, title = "Federation not configured")]
    NotConfigured {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The peer is on this server's blocklist.
    #[error("this server is blocked")]
    #[problem(status = 403, title = "Server blocked")]
    PeerBlocked {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// No such album, or one owned by a different account. One answer for both.
    #[error("no such album")]
    #[problem(status = 404, title = "Not found")]
    NotFound {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The member named is not on the album's current roster.
    #[error("that member is not on this album's roster")]
    #[problem(status = 409, title = "Member not on roster")]
    NotOnRoster {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A collaborator could not answer, so nothing was minted.
    #[error("the capability could not be minted")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

impl MintRejection {
    /// A collaborator could not answer.
    fn unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::FEDERATION_UNAVAILABLE,
        }
    }

    /// No such album, or not the caller's.
    fn not_found() -> Self {
        Self::NotFound {
            code: error_codes::FEDERATION_ALBUM_NOT_FOUND,
        }
    }
}

/// Why a capability was not revoked.
///
/// No "unknown capability" answer: revoking is idempotent and a `jti` this album does not hold
/// is a `204` like any other, so the operation cannot be used to probe which `jti`s exist.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum RevokeRejection {
    /// No such album, or one owned by a different account.
    #[error("no such album")]
    #[problem(status = 404, title = "Not found")]
    NotFound {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A collaborator could not answer, so nothing was revoked.
    #[error("the capability could not be revoked")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Why a capability was not refreshed.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum RefreshRejection {
    /// The credential is not a capability, or names a grant that cannot be continued.
    #[error("this credential cannot be refreshed")]
    #[problem(status = 403, title = "Capability invalid")]
    NotRefreshable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The capability, or the successor a replay names, has been revoked.
    #[error("this capability has been revoked")]
    #[problem(status = 403, title = "Capability revoked")]
    Revoked {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The peer is on this server's blocklist.
    #[error("this server is blocked")]
    #[problem(status = 403, title = "Server blocked")]
    PeerBlocked {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// This deployment does not federate.
    #[error("this server does not federate")]
    #[problem(status = 403, title = "Federation not configured")]
    NotConfigured {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The peer's events-per-hour budget is spent.
    #[error("this peer has reached its request budget")]
    #[problem(status = 429, title = "Rate budget exceeded")]
    RateLimited {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A collaborator could not answer, so nothing was refreshed.
    #[error("the capability could not be refreshed")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

impl From<Refusal> for RefreshRejection {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::Revoked => Self::Revoked {
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

// ===========================================================================================
// Operations
// ===========================================================================================

/// Mint a capability letting one peer server pull one album.
///
/// The token is in the response and nowhere else: this server keeps the record, never the
/// credential.
#[kynos::post(
    "/v1/albums/{album_id}/capabilities",
    operation_id = "issue_capability",
    tag = FederationTag
)]
pub async fn issue_capability(
    Inject(federation): Inject<FederationContext>,
    Inject(albums): Inject<AlbumContext>,
    Inject(membership): Inject<MembershipContext>,
    Auth(credential): Auth<AccessToken>,
    Path(path): Path<CapabilitiesPath>,
    Json(request): Json<MintCapabilityRequest>,
) -> Result<MintReply, MintRejection> {
    if !federation.is_configured() {
        tracing::info!("a capability was refused: this deployment does not federate");
        return Err(MintRejection::NotConfigured {
            code: error_codes::FEDERATION_NOT_CONFIGURED,
        });
    }
    let peer = PeerId::new(request.peer.trim());
    // A peer id is an origin, and an empty or whitespace one is a client bug rather than an
    // unknown server. Checked before the store so a blank never becomes a row.
    if peer.as_str().is_empty() || request.member.trim().is_empty() {
        return Err(MintRejection::Malformed {
            code: error_codes::FEDERATION_CAPABILITY_MALFORMED,
        });
    }
    let member = UserId::new(request.member.trim());
    let album = AlbumId::new(&path.album_id);

    // The album must be the caller's, and "not yours" is "not found" — the same answer the
    // roster and upgrade ceremonies give, so an album id somebody else holds discloses nothing.
    let record = albums.albums().read(&album).await.map_err(|error| {
        tracing::error!(%error, %album, "the album store could not answer a capability mint");
        MintRejection::unavailable()
    })?;
    let Some(record) = record.filter(|record| record.owner_id.as_str() == credential.user.as_str())
    else {
        tracing::info!(user = %credential.user, %album, "a mint was refused: no such album, or not the caller's");
        return Err(MintRejection::not_found());
    };

    // The blocklist, at the layer design/moderation.md puts it: a blocked peer is refused a new
    // grant as surely as it is refused a presentation.
    let blocked = federation
        .peers()
        .read(&peer)
        .await
        .map_err(|error| {
            tracing::error!(%error, %peer, "the peer store could not answer a capability mint");
            MintRejection::unavailable()
        })?
        .is_some_and(|record| record.is_blocked());
    if blocked {
        tracing::info!(%peer, %album, "a mint was refused: the peer is blocked");
        return Err(MintRejection::PeerBlocked {
            code: error_codes::MODERATION_SERVER_BLOCKED,
        });
    }

    // The membership, and the epoch it was granted at — the server-side half of the grant.
    let membership = membership
        .members()
        .membership(&album, &member)
        .await
        .map_err(|error| {
            tracing::error!(%error, %album, "the membership store could not answer a capability mint");
            MintRejection::unavailable()
        })?;
    let Membership::Member { granted_epoch, .. } = membership else {
        tracing::info!(%album, %member, ?membership, "a mint was refused: the member is not on the roster");
        return Err(MintRejection::NotOnRoster {
            code: error_codes::FEDERATION_MEMBER_NOT_ON_ROSTER,
        });
    };

    let scope = Scope::from(request.scope);
    let ttl = request
        .ttl_seconds
        .and_then(|seconds| i64::try_from(seconds).ok())
        .map_or(DEFAULT_TTL, SignedDuration::from_secs);
    let minted = federation
        .codec()
        .mint(&MintRequest {
            peer: peer.clone(),
            album: album.clone(),
            scope,
            // The album's pin, not the server's: a peer must speak what the album speaks.
            min_protocol_version: record.protocol_version.clone(),
            ttl,
        })
        .map_err(|error| {
            tracing::error!(%error, %album, "a capability could not be signed");
            MintRejection::unavailable()
        })?;

    federation
        .capabilities()
        .issue(CapabilityRecord {
            jti: minted.grant.jti.clone(),
            album_id: album.clone(),
            peer_id: peer.clone(),
            member: member.clone(),
            scope,
            granted_epoch,
            min_protocol_version: minted.grant.min_protocol_version.clone(),
            issued_at: minted.grant.issued_at,
            expires_at: minted.grant.expires_at,
            revoked_at: None,
            refreshed_to: None,
        })
        .await
        .map_err(|error| {
            tracing::error!(%error, %album, jti = %minted.grant.jti, "a minted capability could not be recorded");
            MintRejection::unavailable()
        })?;

    Ok(MintReply::Created(MintedCapabilityResponse {
        token: minted.token,
        jti: minted.grant.jti,
        album_id: album.as_str().to_owned(),
        peer: peer.as_str().to_owned(),
        member: member.as_str().to_owned(),
        scope: scope.into(),
        issued_at: minted.grant.issued_at.to_string(),
        expires_at: minted.grant.expires_at.to_string(),
        min_protocol_version: minted.grant.min_protocol_version,
    }))
}

/// The one way minting succeeds.
///
/// A `Reply` rather than a bare `Json` because a mint **creates** a grant, and `201` is what
/// says so; Kynos's `Created` wants a `Location` and there is no URL for a capability — the
/// server never serves one back.
#[derive(Reply)]
pub enum MintReply {
    /// The grant was minted and recorded.
    #[reply(status = 201, description = "The capability was minted")]
    Created(MintedCapabilityResponse),
}

/// Revoke one capability of one album.
///
/// Idempotent, and silent about what it did: a `jti` that is not a live capability of this
/// album — never issued, already revoked, or another album's — is the same `204` a revocation
/// is, so the operation is not a probe over identifiers.
#[kynos::delete(
    "/v1/albums/{album_id}/capabilities/{jti}",
    operation_id = "revoke_capability",
    tag = FederationTag
)]
pub async fn revoke_capability(
    Inject(federation): Inject<FederationContext>,
    Inject(albums): Inject<AlbumContext>,
    Auth(credential): Auth<AccessToken>,
    Path(path): Path<CapabilityPath>,
) -> Result<NoContent, RevokeRejection> {
    let album = AlbumId::new(&path.album_id);
    let record = albums.albums().read(&album).await.map_err(|error| {
        tracing::error!(%error, %album, "the album store could not answer a capability revoke");
        RevokeRejection::Unavailable {
            code: error_codes::FEDERATION_UNAVAILABLE,
        }
    })?;
    if record.is_none_or(|record| record.owner_id.as_str() != credential.user.as_str()) {
        tracing::info!(user = %credential.user, %album, "a revoke was refused: no such album, or not the caller's");
        return Err(RevokeRejection::NotFound {
            code: error_codes::FEDERATION_ALBUM_NOT_FOUND,
        });
    }

    // The grant must be this album's. Without the check an owner could revoke a grant over
    // somebody else's album by naming its `jti`.
    let held = federation
        .capabilities()
        .find(&path.jti)
        .await
        .map_err(|error| {
            tracing::error!(%error, %album, "the capability store could not be read for a revoke");
            RevokeRejection::Unavailable {
                code: error_codes::FEDERATION_UNAVAILABLE,
            }
        })?;
    match held {
        Some(held) if held.album_id == album => {
            let outcome = federation
                .capabilities()
                .revoke_issued(&path.jti, federation.clock().now())
                .await
                .map_err(|error| {
                    tracing::error!(%error, %album, jti = %path.jti, "a capability could not be revoked");
                    RevokeRejection::Unavailable {
                        code: error_codes::FEDERATION_UNAVAILABLE,
                    }
                })?;
            tracing::info!(%album, jti = %path.jti, ?outcome, "an owner revoked a federation capability");
        }
        _ => {
            tracing::debug!(%album, jti = %path.jti, "a revoke named no live capability of this album");
        }
    }
    Ok(NoContent)
}

/// Exchange a capability for its successor.
///
/// The credential **is** the capability being refreshed; a session token has nothing to refresh
/// here and is refused.
#[kynos::post(
    "/v1/federation/capabilities/refresh",
    operation_id = "refresh_capability",
    tag = FederationTag
)]
pub async fn refresh_capability(
    Inject(federation): Inject<FederationContext>,
    Inject(counters): Inject<CounterContext>,
    Auth(principal): Auth<ReadBearer>,
) -> Result<Json<RefreshedCapabilityResponse>, RefreshRejection> {
    let Principal::Peer(capability) = principal else {
        tracing::info!("a session token was presented on the capability refresh");
        return Err(RefreshRejection::NotRefreshable {
            code: error_codes::FEDERATION_CAPABILITY_INVALID,
        });
    };
    // The same admission every federated read passes: live grant, unblocked peer, budget.
    federation::admit(&federation, &counters, &capability, Presentation::Refresh).await?;
    if !federation.is_configured() {
        // A grant minted while this server federated still *verifies* — a token is not
        // un-minted by a configuration change — but it is not continued.
        tracing::info!(peer = %capability.record.peer_id, "a refresh was refused: this deployment does not federate");
        return Err(RefreshRejection::NotConfigured {
            code: error_codes::FEDERATION_NOT_CONFIGURED,
        });
    }

    let predecessor = &capability.record;
    let now = federation.clock().now();
    // The successor carries everything the predecessor granted, unchanged: the store refuses a
    // successor that names another peer, album or member, so a refresh can never widen a grant.
    let minted = federation
        .codec()
        .mint(&MintRequest {
            peer: predecessor.peer_id.clone(),
            album: predecessor.album_id.clone(),
            scope: predecessor.scope,
            min_protocol_version: predecessor.min_protocol_version.clone(),
            ttl: DEFAULT_TTL,
        })
        .map_err(|error| {
            tracing::error!(%error, "a successor capability could not be signed");
            RefreshRejection::Unavailable {
                code: error_codes::FEDERATION_UNAVAILABLE,
            }
        })?;
    let successor = CapabilityRecord {
        jti: minted.grant.jti.clone(),
        album_id: predecessor.album_id.clone(),
        peer_id: predecessor.peer_id.clone(),
        member: predecessor.member.clone(),
        scope: predecessor.scope,
        granted_epoch: predecessor.granted_epoch,
        min_protocol_version: predecessor.min_protocol_version.clone(),
        issued_at: minted.grant.issued_at,
        expires_at: minted.grant.expires_at,
        revoked_at: None,
        refreshed_to: None,
    };

    let outcome = federation
        .capabilities()
        .refresh(&predecessor.jti, successor, now)
        .await
        .map_err(|error| {
            tracing::error!(%error, jti = %predecessor.jti, "a capability could not be refreshed");
            RefreshRejection::Unavailable {
                code: error_codes::FEDERATION_UNAVAILABLE,
            }
        })?;

    let (record, token, replayed) = match outcome {
        crate::federation::RefreshOutcome::Issued(record) => (record, minted.token, false),
        crate::federation::RefreshOutcome::AlreadyRefreshed(record) => {
            // The successor an earlier call issued. It may have been revoked since — a block
            // cascades over every live grant of a peer — so its liveness is asked again before
            // it is handed back.
            if !record.is_live(now) {
                tracing::info!(jti = %record.jti, "a replayed refresh named a successor that is no longer live");
                return Err(RefreshRejection::Revoked {
                    code: error_codes::FEDERATION_CAPABILITY_REVOKED,
                });
            }
            let token = federation.codec().sign(&record.grant()).map_err(|error| {
                tracing::error!(%error, jti = %record.jti, "a stored grant could not be re-signed");
                RefreshRejection::Unavailable {
                    code: error_codes::FEDERATION_UNAVAILABLE,
                }
            })?;
            (record, token, true)
        }
        // The predecessor was revoked without a successor, or was never issued here. Neither is
        // continuable, and both are what a revoked grant is told.
        outcome => {
            tracing::info!(jti = %predecessor.jti, ?outcome, "a refresh named a grant that cannot be continued");
            return Err(RefreshRejection::Revoked {
                code: error_codes::FEDERATION_CAPABILITY_REVOKED,
            });
        }
    };

    tracing::info!(
        peer = %record.peer_id,
        album = %record.album_id,
        predecessor = %predecessor.jti,
        successor = %record.jti,
        replayed,
        "a federation capability was refreshed"
    );
    Ok(Json(RefreshedCapabilityResponse {
        token,
        jti: record.jti,
        expires_at: record.expires_at.to_string(),
        replayed,
    }))
}
