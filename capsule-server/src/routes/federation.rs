//! The federation capability's lifecycle: minting, revoking and refreshing.
//!
//! Not the pull path. design/federation.md is explicit that federation adds **no new data
//! protocol** — a peer pulls through `GET /v1/sync?album_id=` and `GET /v1/blob/{hash}`, which
//! is [`crate::routes::sync`] and [`crate::routes::blob`]. What is here is the credential those
//! two reads accept and the three operations that manage it.
//!
//! ```text
//! POST   /v1/albums/{album_id}/capabilities
//!        { peer, member, scope, ttl_seconds?, renewable_until? }
//! 201 { token, jti, album_id, peer, member, scope, issued_at, expires_at, not_after,
//!       renewable, min_protocol_version }
//! 400 error.federation.capability_malformed
//! 403 error.moderation.server_blocked
//! 404 error.federation.album_not_found | error.federation.not_configured
//! 409 error.federation.member_not_on_roster
//! 429 error.federation.rate_budget_exceeded
//! 500 error.federation.unavailable
//!
//! DELETE /v1/albums/{album_id}/capabilities/{jti}
//! 204 (idempotent)
//! 404 error.federation.album_not_found | error.federation.not_configured
//! 500 error.federation.unavailable
//!
//! POST   /v1/federation/reports                    (the report's own signature is the credential)
//! 202 { report_id, received_at }
//! 400 error.moderation.report_malformed
//! 401 error.moderation.report_unsigned
//! 403 error.federation.peer_unknown | error.moderation.server_blocked
//! 404 error.federation.not_configured
//! 429 error.moderation.report_rate_limited
//! 500 error.moderation.unavailable
//!
//! POST   /v1/federation/capabilities/refresh       (the capability itself is the credential)
//! 200 { token, jti, expires_at, replayed }
//! 403 error.federation.capability_invalid | error.federation.capability_revoked
//!     | error.federation.capability_expired | error.moderation.server_blocked
//! 404 error.federation.not_configured
//! 409 error.federation.member_not_on_roster
//! 429 error.federation.rate_budget_exceeded
//! 500 error.federation.unavailable
//! ```
//!
//! # Off unless switched on
//!
//! Federation is deferred (#476), and a deployment that has not asked for it should not carry
//! a surface peers can reach: report intake is the server's one write whose caller holds no
//! credential of this server's, and a capability is a bearer token. So the whole surface sits behind
//! `FEDERATION_ENABLED` (#509), **off by default**. Off, [`FederationGate`] refuses each operation
//! with `404 error.federation.not_configured` before its credential, its body, a
//! budget or a store is touched, and the read primitives honour no capability at all. Every
//! operation stays mounted and described either way, so the committed contract does not depend
//! on a runtime setting; the `404` is in it, and so is this rule, in each operation's
//! description.
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
//! # Revoking is gated too, and why that cuts nothing loose
//!
//! An earlier version left revoking open on a deployment that did not federate, so turning
//! federation off could not take away an owner's ability to cut a grant already out there. That
//! reasoning held while an unconfigured server still *honoured* old capabilities. It no longer
//! does: off, [`ReadBearer`] has no capability arm, so a grant minted while federation was on
//! pulls nothing, and there is nothing a revoke would be needed to stop. Switching federation
//! back on restores those grants exactly as they were — revocable, and on the published list if
//! they were revoked — because the capability store is untouched by the switch.
//!
//! # Renewability is asked for, never assumed
//!
//! A refresh mints a **successor**, and a successor with a fresh TTL is a grant that outlives the
//! lifetime its owner chose unless something stops it. Nothing about "same peer, same album, same
//! member" does: an owner who mints a deliberate sixty-second capability would get a peer that
//! refreshes inside the minute and chains forever, leaving `ttl_seconds` advisory for exactly one
//! hop and revocation of a `jti` the owner never saw as the only remaining control.
//!
//! So the record carries an **absolute deadline**, [`CapabilityRecord::not_after`], fixed at the
//! original mint and copied unchanged into every successor — the store refuses one that carries a
//! different deadline, so this is structural rather than a property of the route that happens to
//! compute the TTL. The default is `not_after == expires_at`: **a grant is not renewable unless
//! the owner said so**, by naming `renewable_until` at mint. Each successor is minted for
//! `min(DEFAULT_TTL, not_after − now)`, so the last token of a grant is short rather than
//! overhanging, and a refresh past the deadline is `403 error.federation.capability_expired`.
//!
//! The mint response states both `not_after` and a plain `renewable` flag, because an owner
//! deciding how long to share for should not have to infer it from two timestamps.
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

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_i18n::error_codes;
use jiff::SignedDuration;
use kynos::di::Provides;
use kynos::http::Request;
use kynos::middleware::{Continued, Interceptor, Next};
use kynos::prelude::*;
use kynos::response::status::NoContent;
use kynos::security::auth::Auth;
use serde::{Deserialize, Serialize};

use crate::album::AlbumContext;
use crate::auth::AccessToken;
use crate::counter::{CounterContext, CounterKey, budgets};
use crate::federation::scheme::RequestSignatureHeader;
use crate::federation::{
    self, CapabilityRecord, FederationContext, MAX_GRANT_LIFETIME, MintRequest, PeerId,
    Presentation, Principal, ReadBearer, Refusal, ReportClaim, Scope,
};
use crate::membership::{Membership, MembershipContext};
use crate::moderation::{FederatedReport, ModerationContext};
use crate::store::{AlbumId, UserId};

/// The federation surface: the capability a peer server pulls a shared album with.
#[derive(Tag)]
#[tag(
    name = "federation",
    description = "Minting, revoking and refreshing the capability a peer server pulls with."
)]
pub struct FederationTag;

/// Refuses every federation operation, **before it runs**, on a deployment that does not
/// federate.
///
/// The gate `FEDERATION_ENABLED` (#509) closes. An interceptor on the federation group rather
/// than a check in each body, for the one property that matters here: it runs before the
/// operation's own chain, so it answers before the credential is authenticated (a handler's
/// guard is always its first argument, so no extractor could come before it), before the body
/// is parsed and before any budget is charged or store is read. A deployment that does not
/// federate does no federated work for anyone — which is the point, because report intake and
/// minting both did store work for unauthenticated callers while federation was deferred
/// (#476).
///
/// The router is the same either way and so is the document: the four operations are mounted
/// and described, and each declares the `404` this answers. Unmounting them instead would make
/// the emitted contract depend on a runtime setting, and the contract is committed.
#[derive(Debug, Clone, Copy, Default)]
pub struct FederationGate;

impl FederationGate {
    /// The interceptor.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// Why a federation operation was not served: this deployment does not federate.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum NotFederating {
    /// `FEDERATION_ENABLED` is off. `404 error.federation.not_configured`.
    ///
    /// `404` because the surface is not there, the answer an unmounted route gives; the code
    /// says why, so a peer's operator can tell "this server does not federate" from a typo.
    #[error("this server does not federate")]
    #[problem(status = 404, title = "Not found")]
    Disabled {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Reads nothing a consumer sends and adds no header; its one contribution to the description is
/// the `404` its refusal declares.
impl<C> Interceptor<C> for FederationGate
where
    C: Provides<FederationContext> + Sync + 'static,
{
    type Reads = ();
    type Adds = ();
    type Short = NotFederating;

    async fn intercept(
        &self,
        request: Request,
        (): (),
        context: &C,
        next: Next<'_, C>,
    ) -> Result<Continued<()>, NotFederating> {
        let federation: FederationContext = context.provide();
        if federation.is_enabled() {
            return Ok(next.run(request).await);
        }
        tracing::debug!(
            method = %request.method(),
            path = request.uri().path(),
            "a federation operation was refused: this deployment does not federate"
        );
        Err(NotFederating::Disabled {
            code: error_codes::FEDERATION_NOT_CONFIGURED,
        })
    }
}

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
    /// How long **one token** should live, in seconds. Clamped to the 24-hour ceiling; absent is
    /// six hours.
    pub ttl_seconds: Option<u64>,
    /// The absolute deadline the whole grant dies at, RFC 3339 — and the only thing that makes
    /// it **renewable**.
    ///
    /// Absent, the default, is a grant that cannot be refreshed at all: it lives exactly
    /// `ttl_seconds` and then the owner mints again if they still mean to share. Present, it
    /// must be in the future and at most ninety days out.
    pub renewable_until: Option<String>,
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
    /// When **this token** stops being honoured, RFC 3339.
    pub expires_at: String,
    /// When the **whole grant** dies, RFC 3339. Equal to `expires_at` when it is not renewable.
    pub not_after: String,
    /// Whether a refresh may issue a successor from this grant.
    ///
    /// Stated plainly rather than left to be inferred from the two timestamps above: how long
    /// an owner is sharing for is the decision this response reports back to them.
    pub renewable: bool,
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
    /// When this token stops being honoured, RFC 3339.
    pub expires_at: String,
    /// When the whole grant dies, RFC 3339 — unchanged by this or any refresh.
    pub not_after: String,
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

    /// The account's mint budget is spent, or the mint counter is holding as many accounts as
    /// it will hold. The code says which, so the detail names neither: a full counter is not
    /// the caller's budget spent.
    #[error("no capability can be minted right now; try again later")]
    #[problem(status = 429, title = "Rate budget exceeded")]
    RateLimited {
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

    /// The grant's absolute deadline has passed, or the owner never made it renewable.
    ///
    /// The end of the sharing relationship rather than of one token: no successor will ever be
    /// issued from it, and the peer's next move is to ask the album's owner, not this server.
    #[error("this grant cannot be renewed any further")]
    #[problem(status = 403, title = "Capability expired")]
    GrantExpired {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The member the grant was minted for is no longer on the album's roster at its epoch.
    #[error("that member is not on this album's roster")]
    #[problem(status = 409, title = "Member not on roster")]
    NotOnRoster {
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
///
/// Answers `404 error.federation.not_configured` when this deployment does not federate —
/// `FEDERATION_ENABLED` is off, which is the default — before the credential is read or a store
/// is touched.
#[kynos::post(
    "/v1/albums/{album_id}/capabilities",
    operation_id = "issue_capability",
    tag = FederationTag
)]
pub async fn issue_capability(
    Auth(credential): Auth<AccessToken>,
    Inject(federation): Inject<FederationContext>,
    Inject(albums): Inject<AlbumContext>,
    Inject(membership): Inject<MembershipContext>,
    Inject(counters): Inject<CounterContext>,
    Path(path): Path<CapabilitiesPath>,
    Json(request): Json<MintCapabilityRequest>,
) -> Result<MintReply, MintRejection> {
    // A peer id is an origin, and anything that is not a host name — a blank, a URL, a port, a
    // megabyte of text — is a client bug rather than an unknown server. Checked before any store
    // or counter, because the value becomes a row, a token's `sub` and a log field. The member is
    // bounded the way a report's account is, for the same reason.
    let Ok(peer) = PeerId::parse(&request.peer) else {
        tracing::info!(
            length = request.peer.len(),
            "a mint named a peer that is not a host name"
        );
        return Err(MintRejection::Malformed {
            code: error_codes::FEDERATION_CAPABILITY_MALFORMED,
        });
    };
    let member = request.member.trim();
    if member.is_empty() || member.len() > report_bounds::IDENTIFIER {
        tracing::info!(length = member.len(), "a mint named a member out of bounds");
        return Err(MintRejection::Malformed {
            code: error_codes::FEDERATION_CAPABILITY_MALFORMED,
        });
    }
    let member = UserId::new(member);
    let album = AlbumId::new(&path.album_id);

    // Every mint writes a capability row, so an account's mints are budgeted before any store is
    // read. Fail closed: a counter that cannot answer is a `500`, never an admission, and a full
    // partition is the limiter working, so it is a `429` — under its own at-capacity code, as
    // every route that asks `is_at_capacity` answers it, because the caller's own budget is not
    // what was spent.
    let key = CounterKey::CapabilityMints(UserId::new(credential.user.as_str()));
    match counters
        .hit(&key, budgets::CAPABILITY_MINTS)
        .await
        .map_err(|error| {
            if let Some(retry_after) = counters.capacity_refusal(&error, budgets::CAPABILITY_MINTS) {
                tracing::warn!(%error, user = %credential.user, %retry_after, "a mint was refused: the mint counter is at capacity");
                MintRejection::RateLimited {
                    code: error_codes::FEDERATION_MINT_AT_CAPACITY,
                }
            } else {
                tracing::error!(%error, user = %credential.user, "the mint counter could not be reached");
                MintRejection::unavailable()
            }
        })? {
        crate::counter::Verdict::Admitted { .. } => {}
        crate::counter::Verdict::Limited { retry_after } => {
            tracing::info!(user = %credential.user, %retry_after, "a mint was refused: the account's mint budget is spent");
            return Err(MintRejection::RateLimited {
                code: error_codes::FEDERATION_RATE_BUDGET_EXCEEDED,
            });
        }
    }

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

    // The absolute deadline, and the only way a grant becomes renewable at all. Parsed before
    // anything is signed, so a malformed date costs a `400` rather than a recorded grant.
    let now = federation.clock().now();
    let renewable_until = match request.renewable_until.as_deref() {
        None => None,
        Some(text) => {
            let Ok(until) = text.parse::<jiff::Timestamp>() else {
                tracing::info!(%album, "a mint named an unreadable renewable_until");
                return Err(MintRejection::Malformed {
                    code: error_codes::FEDERATION_CAPABILITY_MALFORMED,
                });
            };
            if until <= now {
                tracing::info!(%album, %until, "a mint named a deadline in the past");
                return Err(MintRejection::Malformed {
                    code: error_codes::FEDERATION_CAPABILITY_MALFORMED,
                });
            }
            Some(until)
        }
    };
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

    // A deadline earlier than the token's own expiry would be a grant that dies before its first
    // token does, which is not a thing an owner can mean; the token wins and the grant is simply
    // not renewable.
    let not_after = renewable_until.map_or(minted.grant.expires_at, |until| {
        until.max(minted.grant.expires_at)
    });
    // The lifetime ceiling, measured from the instant the store measures it from: the token's
    // `issued_at`, which the codec floors to a whole second. Measured from the request's `now`
    // instead, a deadline at exactly ninety days passed here and was refused by the store as a
    // `500` (#529). Nothing has been recorded yet, so this is still a plain `400`.
    if not_after > crate::store::deadline(minted.grant.issued_at, MAX_GRANT_LIFETIME) {
        tracing::info!(%album, %not_after, "a mint named a deadline past the lifetime ceiling");
        return Err(MintRejection::Malformed {
            code: error_codes::FEDERATION_CAPABILITY_MALFORMED,
        });
    }
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
            not_after,
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
        not_after: not_after.to_string(),
        renewable: not_after > minted.grant.expires_at,
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
///
/// Answers `404 error.federation.not_configured` when this deployment does not federate —
/// `FEDERATION_ENABLED` is off, which is the default — before the credential is read or a store
/// is touched. No capability is honoured while federation is off, so there is nothing a revoke
/// would need to cut in the meantime.
#[kynos::delete(
    "/v1/albums/{album_id}/capabilities/{jti}",
    operation_id = "revoke_capability",
    tag = FederationTag
)]
pub async fn revoke_capability(
    Auth(credential): Auth<AccessToken>,
    Inject(federation): Inject<FederationContext>,
    Inject(albums): Inject<AlbumContext>,
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
///
/// Answers `404 error.federation.not_configured` when this deployment does not federate —
/// `FEDERATION_ENABLED` is off, which is the default — before the credential is verified, a
/// budget is charged or a store is touched.
#[kynos::post(
    "/v1/federation/capabilities/refresh",
    operation_id = "refresh_capability",
    tag = FederationTag
)]
pub async fn refresh_capability(
    Auth(principal): Auth<ReadBearer>,
    Inject(federation): Inject<FederationContext>,
    Inject(membership): Inject<MembershipContext>,
    Inject(counters): Inject<CounterContext>,
    // Declared for the contract; `ReadBearer` has already verified it (see its module docs).
    Headers(_signature): Headers<RequestSignatureHeader>,
) -> Result<Json<RefreshedCapabilityResponse>, RefreshRejection> {
    let Principal::Peer(capability) = principal else {
        tracing::info!("a session token was presented on the capability refresh");
        return Err(RefreshRejection::NotRefreshable {
            code: error_codes::FEDERATION_CAPABILITY_INVALID,
        });
    };
    // The same admission every federated read passes: live grant, unblocked peer, budget.
    federation::admit(&federation, &counters, &capability, Presentation::Refresh).await?;

    let predecessor = &capability.record;
    let now = federation.clock().now();

    // The absolute deadline the original mint fixed. A grant the owner did not make renewable
    // has `not_after == expires_at` and fails here on its own first refresh, which is the
    // point: renewability is asked for, not assumed.
    if !predecessor.may_refresh_at(now) {
        tracing::info!(
            peer = %predecessor.peer_id,
            jti = %predecessor.jti,
            not_after = %predecessor.not_after,
            renewable = predecessor.is_renewable(),
            "a refresh was refused: the grant's deadline has passed"
        );
        return Err(RefreshRejection::GrantExpired {
            code: error_codes::FEDERATION_CAPABILITY_EXPIRED,
        });
    }

    // The membership the grant was minted for, re-asked. The read path checks this too, so
    // nothing is *exposed* by skipping it — but a server that kept minting successors for a
    // membership that has ended would be issuing tokens that can never be used, and writing a
    // row for each.
    match membership
        .members()
        .membership(&predecessor.album_id, &predecessor.member)
        .await
        .map_err(|error| {
            tracing::error!(%error, album = %predecessor.album_id, "the membership store could not answer a refresh");
            RefreshRejection::Unavailable {
                code: error_codes::FEDERATION_UNAVAILABLE,
            }
        })? {
        Membership::Member { granted_epoch, .. } if granted_epoch == predecessor.granted_epoch => {}
        membership => {
            tracing::info!(
                peer = %predecessor.peer_id,
                member = %predecessor.member,
                album = %predecessor.album_id,
                ?membership,
                granted_epoch = predecessor.granted_epoch,
                "a refresh was refused: its member is not on the roster at the granted epoch"
            );
            return Err(RefreshRejection::NotOnRoster {
                code: error_codes::FEDERATION_MEMBER_NOT_ON_ROSTER,
            });
        }
    }

    // The successor carries everything the predecessor granted, unchanged: the store refuses a
    // successor that names another peer, album, member **or deadline**, so a refresh can neither
    // widen a grant nor outlive one. Its TTL is whatever is left of the grant, capped at the
    // default — so the last token of a grant is short rather than overhanging its deadline.
    let remaining = predecessor.not_after.duration_since(now);
    let minted = federation
        .codec()
        .mint(&MintRequest {
            peer: predecessor.peer_id.clone(),
            album: predecessor.album_id.clone(),
            scope: predecessor.scope,
            min_protocol_version: predecessor.min_protocol_version.clone(),
            ttl: DEFAULT_TTL.min(remaining),
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
        not_after: predecessor.not_after,
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
        not_after: record.not_after.to_string(),
        replayed,
    }))
}

// ===========================================================================================
// Federated moderation report intake
// ===========================================================================================

/// The most bytes any one field of a federated report may carry.
///
/// Every one of them ends up in a store row, a log line or a counter key, and none of them has a
/// natural bound from the type system: `reported_user`, `asset_hash` and `album_id` are strings a
/// peer chooses. The caps are generous against the real values — a DNS name is at most 253 bytes,
/// a UUID is 36, a SHA-256 hex digest is 64 — and their point is that *some* bound exists before
/// anything is stored or keyed on.
mod report_bounds {
    /// A peer's origin: RFC 1035's ceiling on a domain name.
    pub(super) const ORIGIN: usize = 253;
    /// An account or album identifier: a UUID with room to spare.
    pub(super) const IDENTIFIER: usize = 64;
    /// A content address: a SHA-256 digest as lowercase hex.
    pub(super) const HASH: usize = 64;
    /// The peer's short reason. A sentence, not a case file — the contract's "short reason".
    pub(super) const REASON: usize = 256;
    /// An RFC 3339 instant, with room for any offset spelling.
    pub(super) const INSTANT: usize = 64;
    /// A base64 Ed25519 signature is 88 bytes; this leaves room for padding variants.
    pub(super) const SIGNATURE: usize = 128;
    /// A nonce: at least 16 bytes so it is not a counter, at most a UUID with room to spare.
    pub(super) const NONCE_MIN: usize = 16;
    /// See [`NONCE_MIN`].
    pub(super) const NONCE_MAX: usize = 64;
}

/// A moderation report one peer server files against an account on this one.
///
/// Every field except `signature` is covered by the signature, in canonical CBOR — see
/// [`ReportClaim`].
#[derive(Schema, Serialize, Deserialize, Debug, Clone)]
pub struct FederatedReportRequest {
    /// The peer filing the report, as its own `server-info` names it.
    pub reporting_server: String,
    /// The server the report is addressed to — this one, as its own `server-info` names it.
    pub recipient_server: String,
    /// A value the peer never signs twice: 16 to 64 bytes, a UUID being the obvious choice.
    pub nonce: String,
    /// The account on this server the report is about.
    pub reported_user: String,
    /// The content address of the asset complained about.
    pub asset_hash: String,
    /// The album it was pulled from.
    pub album_id: String,
    /// A short reason, where the peer gives one.
    pub reason: Option<String>,
    /// When the peer signed the report, RFC 3339. Refused when more than five minutes from this
    /// server's clock either way.
    pub reported_at: String,
    /// The peer's Ed25519 signature over the canonical CBOR of the fields above, base64.
    pub signature: String,
}

/// An accepted report.
///
/// The identifier is this server's, so an operator and the reporting peer can talk about one
/// report. Nothing about the reported account is echoed — accepting a report says nothing about
/// whether it is true, and a body that reported on the account's standing would say it does.
#[derive(Schema, Serialize, Deserialize, Debug, Clone)]
pub struct FederatedReportResponse {
    /// This server's identifier for the report.
    pub report_id: String,
    /// When this server accepted it, RFC 3339.
    pub received_at: String,
}

/// Why a report was not accepted.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum ReportRejection {
    /// A field of the body is not what it must be.
    #[error("the report is malformed")]
    #[problem(status = 400, title = "Malformed report")]
    Malformed {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The signature does not verify under the peer's pinned key — or it does, over a report
    /// addressed to another server, signed outside the freshness window, or already filed.
    #[error("the report's signature could not be verified")]
    #[problem(status = 401, title = "Report unsigned")]
    Unsigned {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// No operator has pinned a key for the reporting server.
    #[error("this server is not one we know")]
    #[problem(status = 403, title = "Peer unknown")]
    PeerUnknown {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The reporting server is on this server's blocklist.
    #[error("this server is blocked")]
    #[problem(status = 403, title = "Server blocked")]
    PeerBlocked {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// Too many reports from this peer about this account, or a report counter is holding as
    /// many keys as it will hold. The code says which, so the detail names neither: a full
    /// counter is not this peer's budget spent.
    #[error("no report can be filed right now; try again later")]
    #[problem(status = 429, title = "Report rate limited")]
    RateLimited {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A collaborator could not answer, so nothing was filed.
    #[error("the report could not be filed")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

impl ReportRejection {
    /// A field of the body is not what it must be.
    fn malformed() -> Self {
        Self::Malformed {
            code: error_codes::MODERATION_REPORT_MALFORMED,
        }
    }

    /// A collaborator could not answer.
    fn unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::MODERATION_UNAVAILABLE,
        }
    }

    /// The report does not authenticate a fresh report addressed to this server.
    fn unsigned() -> Self {
        Self::Unsigned {
            code: error_codes::MODERATION_REPORT_UNSIGNED,
        }
    }
}

/// File a signed moderation report from a peer server.
///
/// Answers `404 error.federation.not_configured` when this deployment does not federate —
/// `FEDERATION_ENABLED` is off, which is the default — before the body is read, a budget is
/// charged or a store is touched.
///
/// # Only a pinned peer can file
///
/// A report is verified against the peer's **operator-pinned** key, which an operator writes
/// with `capsule-server peer pin`. A peer nobody pinned is answered
/// `403 error.federation.peer_unknown`, whatever it signed.
///
/// # No bearer, and why that is not "unauthenticated"
///
/// The reporting peer holds no capability here — it is reporting *this* server's content, not
/// pulling it — so there is nothing to present. What it does hold is a key an operator has
/// **pinned**, and the report carries its own Ed25519 signature over the canonical CBOR of every
/// other field. A report from a server nobody has pinned is `403`: intake is not the moment a
/// peer becomes trusted (design/federation.md's TOFU is explicitly not done here).
///
/// # The order the checks run in
///
/// Bounds, then whether it is addressed here and signed just now, then who is speaking, then
/// whether they are welcome, then whether they really said it, then whether they said it before,
/// then how much they may ask for at all, then whose account it is, then whether they have said
/// it too often.
///
/// Every field is length-capped first, and `reporting_server` must be a host name, before a store
/// is read or a byte is keyed on. The recipient and the freshness window are pure checks on the
/// body. **No budget is charged before the signature verifies** (design/moderation.md): a budget
/// keyed on an origin a body merely claims is one a stranger spends in a real peer's name. What an
/// anonymous caller gets before that is one peer-store read and one Ed25519 verification — the
/// work every bearer check costs on any authenticated route.
///
/// Once verified: the nonce is checked (a replay is refused before anything is charged to the
/// peer), then [`CounterKey::FederatedIntake`] bounds the peer's total intake, then the nonce is
/// spent. Then the **policy** budgets: the contract's per-`(server, account)` limit, and a
/// per-peer ceiling that ignores the account, because `reported_user` is a string the peer
/// chooses and a peer cycling accounts would otherwise mint itself a fresh allowance each time.
///
/// # A signed report is accepted once, here, now
///
/// The claim names its recipient and carries a nonce and the instant it was signed; a report
/// addressed to another server, signed more than
/// [`REPORT_FRESHNESS`](crate::federation::REPORT_FRESHNESS) from this server's clock, or
/// carrying a nonce this peer already used is `401 error.moderation.report_unsigned`. A peer that
/// retries — after a `429`, a timeout, a `500` — signs again with a fresh nonce.
///
/// Before any of that, the bytes parsed are bounded too: the federation group's body cap,
/// [`MAX_FEDERATION_BODY_BYTES`](crate::limits::MAX_FEDERATION_BODY_BYTES), refuses a body past
/// 16 KiB with `413` before it is read — far above any real report, far below the upload
/// backstop every other group carries.
///
/// # What accepting one does
///
/// It writes a row an operator will read ([`ModerationStore::pending_reports`](crate::moderation::ModerationStore::pending_reports)) and **nothing
/// else**. A peer's report is an input to a decision, never a decision: no standing changes, no
/// serving hold appears, and the reported account sees nothing — because nothing has been done
/// to them.
///
/// # `202` whether or not the account exists
///
/// A report naming an account this server does not host is **accepted on the wire and dropped**,
/// with a `warn` for the operator. It is not filed: an unresolvable report is a permanent orphan
/// row that nobody can act on, which is the reason the check exists at all.
///
/// The answer is deliberately the same one a filed report gets. An earlier version refused with a
/// distinct coded `404`, and that manufactured an account-enumeration oracle out of a check that
/// did not need one: a pinned peer could walk identifiers and read existence off the status line.
/// "Pinned" is not "trusted with enumeration" — a peer key can be compromised, and a peer can be
/// adversarial toward its own users while remaining an operator's legitimate partner — and this
/// codebase treats exists-versus-does-not as a first-order defect nearly everywhere else
/// ([`crate::routes::enroll`]'s indistinguishable code refusal, the album ceremonies' "not yours
/// is not found", [`crate::serve::authority`]'s `404`/`403` boundary).
///
/// Probing is not free even so: every budget above is charged before this point is reached, so a
/// peer sweeping identifiers spends its allowance doing it and an operator sees the `warn`.
#[kynos::post(
    "/v1/federation/reports",
    operation_id = "submit_federated_report",
    tag = FederationTag
)]
pub async fn submit_federated_report(
    Inject(federation): Inject<FederationContext>,
    Inject(moderation): Inject<ModerationContext>,
    Inject(auth): Inject<crate::auth::AuthContext>,
    Inject(counters): Inject<CounterContext>,
    Json(request): Json<FederatedReportRequest>,
) -> Result<ReportReply, ReportRejection> {
    // Structural bounds first, on every field, before a store is touched or a byte is keyed on.
    // Each of these ends up in a row, a log line or a counter key, and none of them is bounded
    // by anything but this: the body cap the federation group mounts stops a caller sending
    // megabytes, and this stops one field of a legal body being all of them.
    for (name, value, cap) in [
        (
            "reporting_server",
            request.reporting_server.trim(),
            report_bounds::ORIGIN,
        ),
        (
            "recipient_server",
            request.recipient_server.trim(),
            report_bounds::ORIGIN,
        ),
        ("nonce", request.nonce.trim(), report_bounds::NONCE_MAX),
        (
            "reported_user",
            request.reported_user.trim(),
            report_bounds::IDENTIFIER,
        ),
        ("asset_hash", request.asset_hash.trim(), report_bounds::HASH),
        (
            "album_id",
            request.album_id.trim(),
            report_bounds::IDENTIFIER,
        ),
        (
            "reported_at",
            request.reported_at.trim(),
            report_bounds::INSTANT,
        ),
        (
            "signature",
            request.signature.trim(),
            report_bounds::SIGNATURE,
        ),
        (
            "reason",
            request.reason.as_deref().unwrap_or("x").trim(),
            report_bounds::REASON,
        ),
    ] {
        if value.is_empty() || value.len() > cap {
            tracing::info!(
                field = name,
                length = value.len(),
                "a federated report's field is out of bounds"
            );
            return Err(ReportRejection::malformed());
        }
    }
    if request.nonce.trim().len() < report_bounds::NONCE_MIN {
        tracing::info!(
            length = request.nonce.trim().len(),
            "a federated report's nonce is too short"
        );
        return Err(ReportRejection::malformed());
    }
    let Ok(peer) = PeerId::parse(&request.reporting_server) else {
        tracing::info!("a federated report named a reporting_server that is not a host name");
        return Err(ReportRejection::malformed());
    };
    let Ok(reported_at) = request.reported_at.trim().parse::<jiff::Timestamp>() else {
        tracing::info!(%peer, "a federated report carried an unreadable reported_at");
        return Err(ReportRejection::malformed());
    };
    let Ok(signature) = BASE64.decode(request.signature.trim().as_bytes()) else {
        tracing::info!(%peer, "a federated report's signature is not base64");
        return Err(ReportRejection::malformed());
    };

    // Addressed here, and signed just now. Both are refused before the peer is looked up — they
    // cost nothing to decide and the signature, checked below, is what makes them binding. The
    // answer is the `401` an unverifiable report gets: a report signed for another server, or
    // long ago, does not authenticate a report filed with this one now.
    if PeerId::new(request.recipient_server.trim()).as_str()
        != PeerId::new(federation.codec().server_id()).as_str()
    {
        tracing::info!(%peer, "a federated report was addressed to another server");
        return Err(ReportRejection::unsigned());
    }
    if !federation::report::is_fresh(reported_at, federation.clock().now()) {
        tracing::info!(%peer, %reported_at, "a federated report is outside the freshness window");
        return Err(ReportRejection::unsigned());
    }

    // Who is speaking. A peer nobody pinned, and a peer pinned without a key, are the same
    // answer: there is nothing to verify against, so nothing is verified.
    let record = federation.peers().read(&peer).await.map_err(|error| {
        tracing::error!(%error, %peer, "the peer store could not answer a report intake");
        ReportRejection::unavailable()
    })?;
    let Some(record) = record else {
        tracing::info!(%peer, "a report was refused: the peer is unknown");
        return Err(ReportRejection::PeerUnknown {
            code: error_codes::FEDERATION_PEER_UNKNOWN,
        });
    };
    if record.is_blocked() {
        tracing::info!(%peer, "a report was refused: the peer is blocked");
        return Err(ReportRejection::PeerBlocked {
            code: error_codes::MODERATION_SERVER_BLOCKED,
        });
    }
    let Some(key) = record.signing_key else {
        tracing::info!(%peer, "a report was refused: the peer has no pinned key");
        return Err(ReportRejection::PeerUnknown {
            code: error_codes::FEDERATION_PEER_UNKNOWN,
        });
    };

    // The claim is built from the body's fields with **surrounding whitespace trimmed and
    // nothing else** — the one normalization rule, written into design/federation.md so a peer
    // implementing from the doc signs the bytes this verifies. In particular the peer's own
    // `reporting_server` string is signed as sent, not folded to the canonical `PeerId` form
    // used for the lookup, and `reported_at` is the RFC 3339 text and not a re-rendered instant.
    // `reason` included: design/moderation.md's rule is that *every* signed field is trimmed,
    // and signing it untrimmed made a peer following the doc fail verification on a reason with
    // a trailing newline (#529).
    let claim = ReportClaim {
        reporting_server: request.reporting_server.trim().to_owned(),
        recipient_server: request.recipient_server.trim().to_owned(),
        nonce: request.nonce.trim().to_owned(),
        reported_user: request.reported_user.trim().to_owned(),
        asset_hash: request.asset_hash.trim().to_owned(),
        album_id: request.album_id.trim().to_owned(),
        reason: request
            .reason
            .as_deref()
            .map(|reason| reason.trim().to_owned()),
        reported_at: request.reported_at.trim().to_owned(),
    };
    // Kept verbatim: these are the bytes the signature covers, and the only thing an operator
    // re-verifying months later can use. Every stored field is derived from this claim.
    let signed = claim
        .signing_bytes()
        .map_err(|_| ReportRejection::unavailable())?;
    claim
        .verify(&signature, &key)
        .map_err(|error| match error {
            crate::federation::ReportError::NotAuthentic => ReportRejection::unsigned(),
            crate::federation::ReportError::Unencodable => ReportRejection::unavailable(),
        })?;

    // From here on the peer is who it says it is, so its budgets are its own and nobody else
    // can spend them (#529). A replay is refused **before** the intake budget is charged, so a
    // party that captured a report cannot silence its peer by sending it again; the nonce is
    // charged only after, so a pinned peer cannot grow the nonce partition past its intake
    // budget. The peek and the hit are two steps, and the hit is the one that decides: two
    // copies racing past the peek still meet a one-hit budget.
    let nonce = CounterKey::FederatedReportNonce(format!("{peer}:{}", claim.nonce));
    if !counters
        .peek(&nonce, budgets::FEDERATED_REPORT_NONCE)
        .await
        .map_err(|error| {
            tracing::error!(%error, %peer, "the report nonce counter could not be reached");
            ReportRejection::unavailable()
        })?
        .admits()
    {
        tracing::warn!(%peer, "a federated report replayed a nonce");
        return Err(ReportRejection::unsigned());
    }
    charge(
        &counters,
        &CounterKey::FederatedIntake(peer.as_str().to_owned()),
        budgets::FEDERATED_INTAKE,
    )
    .await?;
    // Spent by hand rather than through `charge`: a `Limited` verdict here is a replay, which
    // is a `401`, while a full partition is the limiter at capacity, which is the at-capacity
    // `429` every other report counter gives — never the `500` of a store that could not answer.
    match counters
        .hit(&nonce, budgets::FEDERATED_REPORT_NONCE)
        .await
        .map_err(|error| {
            refused_by_counter(&counters, &error, &nonce, budgets::FEDERATED_REPORT_NONCE)
        })? {
        crate::counter::Verdict::Admitted { .. } => {}
        crate::counter::Verdict::Limited { .. } => {
            tracing::warn!(%peer, "a federated report replayed a nonce");
            return Err(ReportRejection::unsigned());
        }
    }

    // Does the account exist here? The answer decides whether a row is written and **never what
    // the peer is told** — see the module docs. A report naming an account this server does not
    // host is accepted on the wire and dropped with a `warn`, which is the pattern
    // [`crate::routes::enroll`] uses for unknown-versus-spent-versus-expired codes: log so an
    // operator can act, never tell the asker.
    let reported_user = UserId::new(&claim.reported_user);
    let hosted = crate::auth::AccountProfiles::read(auth.profiles(), &reported_user)
        .await
        .map_err(|error| {
            tracing::error!(%error, %peer, "the account directory could not answer a report intake");
            ReportRejection::unavailable()
        })?
        .is_some();

    // Only now are the *policy* budgets charged: a spoofed `reporting_server` must not be able
    // to spend a real peer's allowance. Two of them — the contract bounds reports per
    // `(server, account)`, and a peer cycling accounts would mint itself a fresh allowance each
    // time, so a ceiling that ignores the account is what actually bounds the peer.
    charge(
        &counters,
        &CounterKey::PeerReports(peer.as_str().to_owned()),
        budgets::PEER_REPORTS,
    )
    .await?;
    charge(
        &counters,
        &CounterKey::FederatedReports(format!("{peer}:{}", claim.reported_user)),
        budgets::FEDERATED_REPORTS,
    )
    .await?;

    // At the precision the report store holds, so the instant this answer echoes is the one an
    // operator later reads off the queue. Taken *before* the hosted branch, so the dropped answer
    // has the same shape as the filed one — a sub-microsecond tail on only one of them would say
    // whether the account exists.
    let received_at = crate::postgres::time::stored(federation.clock().now());
    if !hosted {
        // Logged at `warn` rather than `info`: a peer repeatedly reporting accounts this server
        // does not host is either misrouting or probing, and both are things an operator wants
        // to see. The budgets above were charged either way, so probing is not free.
        tracing::warn!(
            %peer,
            "a federated report named an account this server does not host; accepted and dropped"
        );
        return Ok(ReportReply::Accepted(FederatedReportResponse {
            // A fresh identifier, as an accepted report gets. It names nothing this server
            // stored, and that is the point: the answer must not vary with what exists.
            report_id: uuid::Uuid::now_v7().to_string(),
            received_at: received_at.to_string(),
        }));
    }

    let report = FederatedReport {
        report_id: uuid::Uuid::now_v7().to_string(),
        reporting_server: peer.as_str().to_owned(),
        reported_user,
        asset_hash: claim.asset_hash.clone(),
        album_id: AlbumId::new(&claim.album_id),
        reason: claim.reason.clone(),
        reported_at,
        received_at,
        signature,
        signed,
    };
    let report_id = report.report_id.clone();
    moderation
        .store()
        .file_report(report)
        .await
        .map_err(|error| {
            tracing::error!(%error, %peer, "a federated report could not be filed");
            ReportRejection::unavailable()
        })?;

    Ok(ReportReply::Accepted(FederatedReportResponse {
        report_id,
        received_at: received_at.to_string(),
    }))
}

/// Charge `budget` under `key`, rendering the refusals this route gives.
///
/// One helper for three budgets so they cannot answer differently: a spent budget is `429` with
/// the contract's code, and a counter that cannot be reached is `500` and never an admission —
/// a limiter that failed open would be one an attacker turns off by loading the counter store.
async fn charge(
    counters: &CounterContext,
    key: &CounterKey,
    budget: crate::counter::Budget,
) -> Result<(), ReportRejection> {
    match counters
        .hit(key, budget)
        .await
        .map_err(|error| refused_by_counter(counters, &error, key, budget))?
    {
        crate::counter::Verdict::Admitted { .. } => Ok(()),
        crate::counter::Verdict::Limited { retry_after } => {
            tracing::info!(kind = key.as_str(), %retry_after, "a federated report budget is spent");
            Err(ReportRejection::RateLimited {
                code: error_codes::MODERATION_REPORT_RATE_LIMITED,
            })
        }
    }
}

/// What a report counter that refused to answer tells the peer.
///
/// Fail closed either way. A full partition is the limiter working as designed and clears
/// within its window, so it is a `429`, under the at-capacity code rather than the spent-budget
/// one because no budget of the peer's was spent; anything else is a store that could not
/// answer, which is the `500`.
fn refused_by_counter(
    counters: &CounterContext,
    error: &crate::store::StoreError,
    key: &CounterKey,
    budget: crate::counter::Budget,
) -> ReportRejection {
    if let Some(retry_after) = counters.capacity_refusal(error, budget) {
        tracing::warn!(%error, kind = key.as_str(), %retry_after, "a report counter is at capacity");
        ReportRejection::RateLimited {
            code: error_codes::MODERATION_REPORT_AT_CAPACITY,
        }
    } else {
        tracing::error!(%error, kind = key.as_str(), "a report counter could not be reached");
        ReportRejection::unavailable()
    }
}

/// The one way intake succeeds.
///
/// `202`, never `201`: this server has accepted the report for an operator to look at, and has
/// created nothing the reporting peer can address. A `200` would read as "handled".
#[derive(Reply)]
pub enum ReportReply {
    /// The report was filed for an operator to read.
    #[reply(status = 202, description = "The report was accepted for review")]
    Accepted(FederatedReportResponse),
}
