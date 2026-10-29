//! [`ReadBearer`] — the one bearer carriage the two read primitives accept two principals on.
//!
//! # One component key, two principals
//!
//! design/api-surfaces.md: "Session access tokens and federation capabilities are different token
//! types verified by their owning modules, even though both use the standard HTTP carriage."
//! Kynos registers a scheme under its component name, and `GET /v1/sync` and
//! `GET /v1/blob/{hash}` must keep declaring the same `bearer` requirement every other operation
//! does — the generated SDK client attaches its credential by that key, and a second key would
//! split one carriage into two in the document for a difference the wire does not have. So this
//! scheme registers under **the same name, with a byte-identical description**, as
//! [`AccessToken`]; Kynos accepts a duplicate registration exactly when the two descriptions are
//! equal, and `tests::the_two_schemes_describe_one_component` pins that they are.
//!
//! # Session first, capability second
//!
//! The authenticator asks the session module first, exactly as `Auth<AccessToken>` would — the
//! session-ledger check included — and only on *unauthenticated* tries the capability codec,
//! and only when the deployment federates at all (`FEDERATION_ENABLED`, #509): off, the
//! capability arm does not exist and a non-session bearer is the plain `401` it would be on any
//! other operation.
//! A session token that is live and the wrong kind stays `403`: a refresh token is an
//! insufficient credential on this operation whichever module reads it. A capability that
//! verifies must also be one this server **recorded**: an unknown `jti` under a valid signature
//! is a token this server did not issue, and the record is what carries the member and epoch the
//! route checks membership against.
//!
//! # A capability is presented by its peer, not by whoever holds it
//!
//! A capability on its own is a bearer token: a leaked share invite or a log line on the peer
//! would let anyone pull the album until the grant expired. So a capability is admitted only
//! with an [`X-Capsule-Request-Signature`](REQUEST_SIGNATURE_HEADER) made by the peer its `sub`
//! names, verified here against the key this server's operator pinned for that peer
//! ([`PeerStore`](super::PeerStore)), over the request's method, its path and a hash of the
//! token itself ([`RequestClaim`]). A signature is fresh for [`REQUEST_FRESHNESS`] either side
//! of this server's clock and is accepted **once**: its nonce is remembered per peer for
//! [`REQUEST_NONCE_WINDOW`] through the counter port, the same one-hit budget the federated
//! report's replay check uses, so no second store exists for it. design/federation.md
//! ("Request Signatures") is the normative format; this module is one implementation of it and
//! `capsule-sdk`'s `FederationPull` the other, and both pin the same vector.
//!
//! This is why [`ReadBearer`]'s carrier is written by hand rather than derived: the derived
//! bearer carrier hands the authenticator the token and nothing else, and a signature cannot be
//! checked without the method and path it covers. A session token ignores the header.
//!
//! The nonce is spent only once the signature verifies, so a third party cannot spend a peer's
//! nonces, and it is **not** spent for a peer that is blocked or over its events budget: the
//! route's [`admit`](super::admit) answers those with their coded `403` and `429`, and a
//! nonce spent on a request that was going to be refused anyway would only let a refused peer
//! grow the replay partition past what its budget admits. A request whose nonce was left
//! unspent carries the refusal the authenticator foresaw
//! ([`VerifiedCapability::foreseen`]), and `admit` answers that refusal even when the peer
//! row or the budget window has changed since: a signature whose nonce was never recorded is
//! never admitted, so an unblock or a window rollover between the two reads cannot turn it into
//! one that can be replayed.
//!
//! # What this authenticator refuses, and what it deliberately does not
//!
//! Only the structural refusals — does not verify, expired, unknown, and every way a request
//! signature fails (absent, malformed, stale, unpinned, forged, replayed) — are decided here,
//! and they
//! render as the framework's uncoded `401`, the recorded limitation of
//! [`crate::auth::scheme`]. Everything a client can act on with a code — revoked, wrong album,
//! insufficient scope, over budget, blocked peer — is decided by the **route** from the admitted
//! [`VerifiedCapability`] through [`admit`](super::admit), the same way `Membership::Revoked`
//! becomes a route's `403`. The credential never carries the raw token.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as BASE64URL;
use jiff::{SignedDuration, Timestamp};
use kynos::error::rejection::AuthRejection;
use kynos::http::Parts;
use kynos::prelude::*;
use kynos::security::Authenticator;
use kynos::security::carrier::{self, BearerToken, Carries};
use serde::Serialize;

use super::capability::CapabilityGrant;
use super::store::CapabilityRecord;
use super::{FederationContext, PeerId, Refusal};
use crate::app::App;
use crate::auth::{AccessToken, AuthContext, AuthenticatedSession};
use crate::counter::{CounterKey, Verdict, budgets};

/// The header a peer signs each capability presentation in.
///
/// Lower case because that is how a header map stores it; design/federation.md spells it
/// `X-Capsule-Request-Signature`, and header names are case-insensitive.
pub const REQUEST_SIGNATURE_HEADER: &str = "x-capsule-request-signature";

/// The domain separator a request signature's claim opens with.
///
/// The same key signs federated reports and, one day, other server-to-server traffic; naming
/// what the bytes are inside the bytes is what keeps one kind of signature from being presented
/// as another.
pub const REQUEST_CONTEXT: &str = "capsule-federation-request-v1";

/// How far a request signature's `t` may sit from this server's clock, either way.
///
/// The report's [`REPORT_FRESHNESS`](super::REPORT_FRESHNESS), for the same reason: two honest
/// servers' clocks disagree a little and a request can sit in a retry queue, and a captured
/// signature should be useless within minutes.
pub const REQUEST_FRESHNESS: SignedDuration = SignedDuration::from_mins(5);

/// How long a request nonce is remembered: every instant at which its signature could still be
/// fresh. A signature made at `t` is accepted from `t − REQUEST_FRESHNESS` to
/// `t + REQUEST_FRESHNESS`, so a nonce first seen anywhere in that span must still be
/// remembered at its far end.
pub const REQUEST_NONCE_WINDOW: SignedDuration = SignedDuration::from_mins(10);

/// The shortest nonce accepted: 22 base64url characters carry 16 bytes.
pub const MIN_NONCE_CHARS: usize = 22;

/// The longest nonce accepted, so a peer cannot make the replay key arbitrarily long.
pub const MAX_NONCE_CHARS: usize = 128;

/// The bytes a peer signs for one request: canonical CBOR of this map.
///
/// Canonical CBOR sorts a map's keys, so the encoding depends on the field *names* and values
/// and not on their order here — which is what lets a peer implement the format from
/// design/federation.md rather than from this file. `tests::the_signing_bytes_are_stable` pins
/// the encoding, and `capsule-sdk` pins the same vector, so neither side can drift alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RequestClaim {
    /// Always [`REQUEST_CONTEXT`].
    pub context: String,
    /// The request method, upper case.
    pub method: String,
    /// The operation's path as this server routes it, without the query: a prefix in the
    /// peer's base URL is not part of it (design/federation.md, "Request Signatures").
    pub path: String,
    /// Lowercase hex SHA-256 of the presented capability.
    pub token_sha256: String,
    /// When the peer signed, in Unix seconds.
    pub signed_at: i64,
    /// The nonce the header carries.
    pub nonce: String,
}

impl RequestClaim {
    /// The claim for presenting `token` on `method path`, signed at `signed_at` with `nonce`.
    pub fn new(method: &str, path: &str, token: &str, signed_at: i64, nonce: &str) -> Self {
        Self {
            context: REQUEST_CONTEXT.to_owned(),
            method: method.to_ascii_uppercase(),
            path: path.to_owned(),
            token_sha256: capsule_core::crypto::hash::hash_bytes(token.as_bytes()).to_hex(),
            signed_at,
            nonce: nonce.to_owned(),
        }
    }

    /// The exact bytes a peer signs.
    ///
    /// # Errors
    ///
    /// [`RequestSignatureError::Unencodable`] if the claim does not serialize, which nothing a
    /// peer sends can cause: every field is a string or an integer.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, RequestSignatureError> {
        capsule_core::cbor::to_canonical_vec(self).map_err(|error| {
            tracing::error!(%error, "a request signature's claim did not serialize");
            RequestSignatureError::Unencodable
        })
    }
}

/// A parsed `X-Capsule-Request-Signature` value: `t=<seconds>, n=<nonce>, s=<signature>`.
#[derive(Clone, PartialEq, Eq)]
pub struct RequestSignature {
    /// When the peer signed, in Unix seconds.
    pub signed_at: i64,
    /// A value the peer never signs twice.
    pub nonce: String,
    /// The Ed25519 signature over [`RequestClaim::signing_bytes`].
    pub signature: [u8; 64],
}

impl std::fmt::Debug for RequestSignature {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Not secret, but a signature in a log line is noise nobody can act on.
        formatter
            .debug_struct("RequestSignature")
            .field("signed_at", &self.signed_at)
            .field("nonce", &self.nonce)
            .finish_non_exhaustive()
    }
}

/// Why a request signature was refused. Every variant is an uncoded `401` on the wire; the
/// variant is what the log line says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RequestSignatureError {
    /// A capability was presented with no signature header.
    #[error("the request carries no signature")]
    Missing,
    /// The header is not `t=…, n=…, s=…` with each member well formed and present once.
    #[error("the request signature is malformed: {0}")]
    Malformed(&'static str),
    /// `t` is further than [`REQUEST_FRESHNESS`] from this server's clock.
    #[error("the request signature is not fresh")]
    Stale,
    /// The peer the capability names has no key pinned here.
    #[error("the peer has no pinned key")]
    Unpinned,
    /// The signature does not verify under the pinned key.
    #[error("the request signature does not verify")]
    NotAuthentic,
    /// The nonce was already used by this peer inside [`REQUEST_NONCE_WINDOW`].
    #[error("the request nonce was already used")]
    Replayed,
    /// A store could not answer, so the request was refused closed.
    #[error("a store could not answer the signature check")]
    Unavailable,
    /// The claim could not be encoded, which can only be this server's own fault.
    #[error("the request signature's bytes could not be produced")]
    Unencodable,
}

impl RequestSignature {
    /// Parse a header value.
    ///
    /// Strict: exactly the three members, each once, in any order, with optional whitespace
    /// around each. An unknown member is refused rather than ignored, so a later format cannot
    /// be mistaken for this one.
    ///
    /// # Errors
    ///
    /// [`RequestSignatureError::Malformed`] naming which part is wrong.
    pub fn parse(value: &str) -> Result<Self, RequestSignatureError> {
        let (mut signed_at, mut nonce, mut signature) = (None, None, None);
        for member in value.split(',') {
            let (key, value) =
                member
                    .trim()
                    .split_once('=')
                    .ok_or(RequestSignatureError::Malformed(
                        "a member is not key=value",
                    ))?;
            let slot = match key {
                "t" => &mut signed_at,
                "n" => &mut nonce,
                "s" => &mut signature,
                _ => return Err(RequestSignatureError::Malformed("an unknown member")),
            };
            if slot.replace(value).is_some() {
                return Err(RequestSignatureError::Malformed("a member appears twice"));
            }
        }
        let signed_at = signed_at
            .filter(|t| !t.is_empty() && t.bytes().all(|byte| byte.is_ascii_digit()))
            .and_then(|t| t.parse::<i64>().ok())
            .ok_or(RequestSignatureError::Malformed("t is not Unix seconds"))?;
        let nonce = nonce
            .filter(|n| {
                (MIN_NONCE_CHARS..=MAX_NONCE_CHARS).contains(&n.len())
                    && n.bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
            })
            .ok_or(RequestSignatureError::Malformed(
                "n is not 22 to 128 base64url characters",
            ))?
            .to_owned();
        let signature = signature
            .and_then(|s| BASE64URL.decode(s).ok())
            .and_then(|bytes| <[u8; 64]>::try_from(bytes).ok())
            .ok_or(RequestSignatureError::Malformed(
                "s is not a base64url Ed25519 signature",
            ))?;
        Ok(Self {
            signed_at,
            nonce,
            signature,
        })
    }

    /// The header value, in the member order design/federation.md shows.
    pub fn to_header_value(&self) -> String {
        format!(
            "t={}, n={}, s={}",
            self.signed_at,
            self.nonce,
            BASE64URL.encode(self.signature)
        )
    }

    /// Whether this signature's `t` is fresh at `now`.
    ///
    /// Symmetric, as the report's freshness is: a peer whose clock runs a little fast is not
    /// refused, and one whose clock runs far fast cannot pre-sign requests to replay later.
    pub fn is_fresh(&self, now: Timestamp) -> bool {
        Timestamp::from_second(self.signed_at)
            .is_ok_and(|signed_at| super::report::is_within(signed_at, now, REQUEST_FRESHNESS))
    }

    /// Whether the signature covers `claim` under `key`.
    ///
    /// # Errors
    ///
    /// [`RequestSignatureError::NotAuthentic`] when it does not.
    pub fn verify(
        &self,
        claim: &RequestClaim,
        key: &[u8; 32],
    ) -> Result<(), RequestSignatureError> {
        let bytes = claim.signing_bytes()?;
        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key.as_slice())
            .verify(&bytes, &self.signature)
            .map_err(|_| RequestSignatureError::NotAuthentic)
    }
}

/// The request signature, declared on the three operations a capability is presented to.
///
/// Declaration only: [`ReadBearer`]'s carrier reads the header, because verifying it is the
/// authenticator's work and a route extractor runs after the authenticator has decided. Declared
/// as a parameter so the contract names it and the generated client can send it; optional,
/// because a session token on the same operations carries none.
#[derive(HeaderParams)]
pub struct RequestSignatureHeader {
    /// `t=<Unix seconds>, n=<nonce>, s=<base64url Ed25519 signature>`, made by the peer a
    /// presented federation capability names, over the request's method, path and capability
    /// (design/federation.md, "Request Signatures"). Required with a capability; ignored with a
    /// session token.
    #[header(rename = "X-Capsule-Request-Signature")]
    pub request_signature: Option<String>,
}

/// What a request presented on the two read primitives and the refresh: the bearer, and what
/// a request signature needs to be checked.
///
/// The signature header is carried raw and parsed only on the capability arm, so a session
/// token is never refused for a header it has no use for.
#[derive(Clone)]
pub struct ReadPresentation {
    bearer: BearerToken,
    /// The header's value. `Some("")` stands for a value that is not text or that appears more
    /// than once, so it fails parsing rather than reading as absent.
    signature: Option<String>,
    method: String,
    path: String,
}

impl std::fmt::Debug for ReadPresentation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the bearer: a `Debug` that printed a live credential is how one reaches a log.
        formatter
            .debug_struct("ReadPresentation")
            .field("method", &self.method)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// A capability that verified and that this server recorded.
///
/// The grant is what the token said; the record is what the server knows about it — the member
/// it was minted for, the epoch their membership was granted at, whether it has been revoked.
/// A route decides from both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedCapability {
    /// The token's claims, verified.
    pub grant: CapabilityGrant,
    /// The issued record the `jti` names.
    pub record: CapabilityRecord,
    /// The refusal the authenticator foresaw when it left the request's nonce unspent: the
    /// peer was blocked, or over its events budget, when the signature was checked.
    ///
    /// [`admit`](super::admit) answers it whatever the peer row and the budget read by then,
    /// because the nonce was never recorded and admitting the request would leave its signature
    /// replayable. `None` when the nonce was spent, which is the only way a request is admitted.
    pub foreseen: Option<Refusal>,
}

/// Who a read is being served to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    /// An account, through a session access token.
    Session(AuthenticatedSession),
    /// A peer server, through a federation capability.
    ///
    /// Boxed: the grant and its record are several hundred bytes against a session's tens, and
    /// every session-authenticated read would otherwise carry the difference.
    Peer(Box<VerifiedCapability>),
}

/// The bearer carriage on the two read primitives a peer may pull through.
///
/// Registered under the same component as [`AccessToken`], with the same description: one
/// `bearer` in the document, one credential key in the SDK. The handler receives a
/// [`Principal`] and never the token.
#[derive(SecurityScheme)]
#[security(bearer(format = "JWT"))]
#[security(
    name = "bearer",
    credential = Principal,
    carrier = manual,
    description = "A short-lived Capsule access token, issued by `POST /v1/auth/login` and \
                   rotated by `POST /v1/auth/refresh`."
)]
pub struct ReadBearer;

/// The bearer, read exactly as the derived carrier reads it, plus what a request signature
/// covers. Written by hand only because the derived carrier hands the authenticator the token
/// and nothing else; see the module docs.
impl Carries for ReadBearer {
    type Presented = ReadPresentation;

    fn present(parts: &Parts) -> Result<Option<ReadPresentation>, AuthRejection> {
        let Some(bearer) = carrier::bearer(parts)? else {
            return Ok(None);
        };
        let mut values = parts.headers.get_all(REQUEST_SIGNATURE_HEADER).iter();
        let signature = match (values.next(), values.next()) {
            (None, _) => None,
            (Some(value), None) => Some(value.to_str().unwrap_or_default().to_owned()),
            // Two signatures on one request is not a choice this server makes for the peer.
            (Some(_), Some(_)) => Some(String::new()),
        };
        Ok(Some(ReadPresentation {
            bearer,
            signature,
            method: parts.method.as_str().to_owned(),
            // The path this server routed, which is the operation's own: a deployment served
            // under a prefix has it stripped before the request gets here, and the peer signs
            // the operation's path for the same reason (design/federation.md, `path`).
            path: parts.uri.path().to_owned(),
        }))
    }
}

impl FederationContext {
    /// Check that `presented` carries a request signature `grant`'s peer made, and spend its
    /// nonce, or return the refusal the route must answer instead of spending it.
    ///
    /// The order is the cheapest refusal first, and nothing a third party sends reaches the
    /// counter: the header is parsed and its freshness checked before the peer store is read,
    /// and the nonce is spent only after the signature verifies.
    async fn verify_request_signature(
        &self,
        grant: &CapabilityGrant,
        presented: &ReadPresentation,
        context: &App,
    ) -> Result<Option<Refusal>, RequestSignatureError> {
        let header = presented
            .signature
            .as_deref()
            .ok_or(RequestSignatureError::Missing)?;
        let signature = RequestSignature::parse(header)?;
        if !signature.is_fresh(self.clock().now()) {
            return Err(RequestSignatureError::Stale);
        }

        let peer = &grant.peer;
        let record = self.peers().read(peer).await.map_err(|error| {
            tracing::error!(%error, %peer, "the peer store could not answer a request signature check");
            RequestSignatureError::Unavailable
        })?;
        let Some(record) = record else {
            return Err(RequestSignatureError::Unpinned);
        };
        let key = record.signing_key.ok_or(RequestSignatureError::Unpinned)?;

        let claim = RequestClaim::new(
            &presented.method,
            &presented.path,
            presented.bearer.as_str(),
            signature.signed_at,
            &signature.nonce,
        );
        signature.verify(&claim, &key)?;

        // A request the route is about to refuse with a code spends no nonce, and carries that
        // refusal to the route so it is answered even if the state changes first: see the
        // module docs. A budget that cannot be read is not a reason to skip the replay check.
        let foreseen = if record.is_blocked() {
            Some(Refusal::PeerBlocked)
        } else {
            over_budget(peer, context)
                .await?
                .map(|retry_after| Refusal::RateLimited { retry_after })
        };
        if let Some(refusal) = foreseen {
            tracing::debug!(%peer, ?refusal, "a request signature verified; its nonce is left unspent for the refusal the route makes");
            return Ok(Some(refusal));
        }
        spend_nonce(peer, &signature.nonce, context).await?;
        Ok(None)
    }
}

/// When `peer`'s events budget resets, if it is already spent so the route will answer `429`.
async fn over_budget(
    peer: &PeerId,
    context: &App,
) -> Result<Option<Timestamp>, RequestSignatureError> {
    let verdict = context
        .counters()
        .peek(
            &CounterKey::PeerRequests(peer.as_str().to_owned()),
            budgets::PEER_REQUESTS,
        )
        .await
        .map_err(|error| {
            tracing::error!(%error, %peer, "the per-peer counter could not be read for a signature check");
            RequestSignatureError::Unavailable
        })?;
    Ok(match verdict {
        Verdict::Admitted { .. } => None,
        Verdict::Limited { retry_after } => Some(retry_after),
    })
}

/// Remember `nonce` for `peer`, refusing it if it was already used inside the window.
///
/// The hit decides, not a peek: two copies of one request racing each other still meet a
/// one-hit budget. A full partition fails closed like a store that cannot answer — a replay
/// memory that admitted when it ran out of room would be a replay memory a flood turns off.
async fn spend_nonce(
    peer: &PeerId,
    nonce: &str,
    context: &App,
) -> Result<(), RequestSignatureError> {
    let key = CounterKey::FederatedRequestNonce(format!("{peer}:{nonce}"));
    match context
        .counters()
        .hit(&key, budgets::FEDERATED_REQUEST_NONCE)
        .await
        .map_err(|error| {
            tracing::error!(
                %error,
                %peer,
                at_capacity = crate::counter::is_at_capacity(&error),
                "the request nonce counter could not record a nonce, so the request was refused closed"
            );
            RequestSignatureError::Unavailable
        })? {
        Verdict::Admitted { .. } => Ok(()),
        Verdict::Limited { .. } => Err(RequestSignatureError::Replayed),
    }
}

impl Authenticator<ReadBearer, App> for FederationContext {
    async fn authenticate(
        &self,
        presented: ReadPresentation,
        context: &App,
    ) -> Result<Principal, AuthRejection> {
        // The session module first, and its answer is final unless it is "not a session": a
        // live refresh token is `403` here as everywhere, and a session it admits is admitted.
        match <AuthContext as Authenticator<AccessToken, App>>::authenticate(
            context.auth(),
            presented.bearer.clone(),
            context,
        )
        .await
        {
            Ok(session) => return Ok(Principal::Session(session)),
            // `AuthRejection` is non-exhaustive; anything that is not "insufficient" is "not a
            // session", which is the one answer that opens the capability path.
            Err(rejection @ AuthRejection::Forbidden { .. }) => return Err(rejection),
            Err(_) => {}
        }

        // A deployment that does not federate has no capability arm (#509): the token is not
        // verified and no store is read for it, so a stranger presenting garbage costs nothing
        // past the session check every bearer already pays.
        if !self.is_enabled() {
            tracing::debug!(
                "a non-session credential was refused: this deployment does not federate"
            );
            return Err(AuthRejection::unauthenticated());
        }

        let grant = self.codec().verify(presented.bearer.as_str()).map_err(|reason| {
            // Which check failed is safe to log — it names no part of the credential — and it
            // is the only thing that makes "my capability is refused" actionable.
            tracing::debug!(%reason, "a request presented a credential that is neither a session nor a capability");
            AuthRejection::unauthenticated()
        })?;

        let record = match self.capabilities().find(&grant.jti).await {
            Ok(Some(record)) => record,
            Ok(None) => {
                tracing::info!(
                    peer = %grant.peer,
                    jti = %grant.jti,
                    "a capability verified under this server's key but was never issued here"
                );
                return Err(AuthRejection::unauthenticated());
            }
            // Fail closed, as the session ledger does. `401` is the only refusal this trait can
            // render; the route-level `admit` renders the honest `500` for the same outage.
            Err(error) => {
                tracing::error!(
                    %error,
                    jti = %grant.jti,
                    "the capability store could not be read, so the request was refused closed"
                );
                return Err(AuthRejection::unauthenticated());
            }
        };
        if record.grant() != grant {
            // The token and the record disagree about what was granted. Nothing this server
            // wrote can produce that, so it is refused rather than reconciled.
            tracing::error!(jti = %grant.jti, "a capability's claims do not match its record");
            return Err(AuthRejection::unauthenticated());
        }

        // The capability is genuine; is its presenter the peer it names? After the record, so
        // a token this server never issued costs no peer-store read and spends no nonce.
        let foreseen = match self
            .verify_request_signature(&grant, &presented, context)
            .await
        {
            Ok(foreseen) => foreseen,
            Err(reason) => {
                tracing::info!(
                    peer = %grant.peer,
                    jti = %grant.jti,
                    %reason,
                    "a capability was presented without a valid request signature from its peer"
                );
                return Err(AuthRejection::unauthenticated());
            }
        };

        tracing::trace!(
            peer = %grant.peer,
            album = %grant.album,
            jti = %grant.jti,
            "a request presented a recorded federation capability"
        );
        Ok(Principal::Peer(Box::new(VerifiedCapability {
            grant,
            record,
            foreseen,
        })))
    }

    async fn authorize(
        &self,
        _credential: &Principal,
        _scopes: &[kynos::security::Unscoped],
        _context: &App,
    ) -> Result<(), AuthRejection> {
        // Neither token type carries OAuth-style scopes; a capability's `scope` is decided
        // against a blob's role by the route. Exists because the trait requires it.
        Ok(())
    }
}

#[cfg(test)]
mod tests;
