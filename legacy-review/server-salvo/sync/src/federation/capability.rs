//! Federation capability tokens — the album-scoped, signed, expiring, revocable grant that gates
//! which peers may fetch what (slice `S-E2`; SSoT:
//! [Federation — Capabilities](https://docs/design/federation/#federation-capabilities)).
//!
//! A capability is an **EdDSA-JWT**, reusing the same machinery as the access token — no separate
//! macaroon/ZCAP format. Its claims and lifecycle are the normative contract every federated peer
//! parses. Deviations from RFC 7519 that this module deliberately enforces:
//!
//! - `aud` is the **album** the capability scopes to (`urn:capsule:album:<id>`), not the recipient
//!   — verifiers match it against the album, never against themselves.
//! - `iat` / `exp` / `nbf` are **RFC 3339 strings**, so jsonwebtoken's numeric-date validation is
//!   disabled and all temporal checks run here against a `jiff` clock.
//! - `exp` is never more than **24 h** after `iat` (issuance clamps; verification rejects a wider
//!   window).
//!
//! Signed under the home server's classical Ed25519 operational key (the
//! [operational-signature carve-out](https://docs/design/cryptography/primitives/#signature-scheme)).

use std::collections::HashSet;

use jiff::{SignedDuration, Timestamp};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use super::revocation::{RevocationList, RevocationVerdict};

/// The URN prefix an album-scoped `aud` claim carries.
pub const ALBUM_URN_PREFIX: &str = "urn:capsule:album:";

/// The hard ceiling on a capability's lifetime — `exp` is never more than this after `iat`.
#[must_use]
pub fn max_capability_ttl() -> SignedDuration {
    SignedDuration::from_hours(24)
}

/// Wrap an album id in its capability-`aud` URN.
#[must_use]
pub fn album_urn(album_id: &str) -> String {
    format!("{ALBUM_URN_PREFIX}{album_id}")
}

/// The scope a capability grants over an album's blobs. Enforced structurally against each blob's
/// server-visible **role**.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FederationScope {
    /// Full read — originals and derivatives.
    Read,
    /// Thumbnails and previews only — never originals.
    ReadDerivativeOnly,
}

impl FederationScope {
    /// Whether this scope permits fetching a blob of the given server-visible `role`. A
    /// derivative-only capability is refused an `original`.
    #[must_use]
    pub fn permits_role(self, role: &str) -> bool {
        match self {
            FederationScope::Read => true,
            FederationScope::ReadDerivativeOnly => role != "original",
        }
    }
}

/// The claims of a federation capability token. Serialized as the JWT payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityClaims {
    /// The issuing home server (`home.tld`).
    pub iss: String,
    /// The peer server identity the grant is for (`other.tld`).
    pub sub: String,
    /// The album id this capability scopes to, as `urn:capsule:album:<id>`.
    pub aud: String,
    /// `read` (full) or `read-derivative-only`.
    pub scope: FederationScope,
    /// Issued-at (RFC 3339); the `exp` window is bounded against it.
    pub iat: String,
    /// Expiry (RFC 3339); never more than 24 h after `iat`.
    pub exp: String,
    /// Not-before (RFC 3339); clock-skew tolerance against the peer's wall clock.
    pub nbf: String,
    /// Unique token identifier (UUIDv7) — the revocation key.
    pub jti: String,
    /// The album's pinned `protocol_version` — the peer selects its parser from this.
    pub min_protocol_version: String,
}

/// A structural reason a capability token was refused. Each maps to a stable `error.federation.*`
/// code and an HTTP status; the enum itself is the fine-grained reason unit tests assert on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityReject {
    /// A required claim was empty/blank.
    MissingClaim(&'static str),
    /// The signature did not verify under the expected key.
    BadSignature,
    /// A claim was structurally malformed (unparseable timestamp, undecodable payload).
    Malformed(&'static str),
    /// `iss` is not the expected issuer.
    WrongIssuer,
    /// `nbf` is in the future.
    NotYetValid,
    /// `exp` is in the past.
    Expired,
    /// `exp` is more than 24 h after `iat`.
    ExpiryTooFar,
    /// `aud` does not match the album being pulled.
    AudienceMismatch,
    /// The scope does not cover the requested blob role.
    ScopeInsufficient,
    /// The `jti` is on the revocation list.
    Revoked,
    /// The revocation list is too stale to confirm the `jti` — fail closed.
    RevocationUnverifiable,
}

/// A capability issuance failure.
#[derive(Debug, Error)]
pub enum IssueError {
    /// Signing the JWT failed.
    #[error("failed to sign capability token: {0}")]
    Sign(#[from] jsonwebtoken::errors::Error),
    /// A timestamp could not be represented as RFC 3339.
    #[error("capability timestamp is unrepresentable")]
    Timestamp,
}

/// A minted capability: its wire token and the claims it carries.
#[derive(Debug, Clone)]
pub struct MintedCapability {
    /// The signed JWT to hand to the peer.
    pub token: String,
    /// The claims embedded in the token (for the issuer's own records / revocation).
    pub claims: CapabilityClaims,
}

/// Parameters for minting a capability.
#[derive(Debug, Clone)]
pub struct IssueParams<'a> {
    /// The peer server identity the grant is for (`sub`).
    pub peer: &'a str,
    /// The album the capability scopes to.
    pub album_id: &'a str,
    /// The scope granted.
    pub scope: FederationScope,
    /// The album's pinned protocol version.
    pub min_protocol_version: &'a str,
    /// Requested lifetime; clamped to the 24 h ceiling.
    pub ttl: SignedDuration,
}

/// Mints capabilities under a home server's Ed25519 signing key.
pub struct CapabilityIssuer {
    encoding_key: EncodingKey,
    issuer: String,
}

impl CapabilityIssuer {
    /// Build an issuer from the home server's identity and its Ed25519 signing key.
    pub fn new(issuer: impl Into<String>, encoding_key: EncodingKey) -> Self {
        Self {
            encoding_key,
            issuer: issuer.into(),
        }
    }

    /// Mint a capability for `params`, `iat = now`, `nbf = now`, `exp = now + min(ttl, 24h)`,
    /// `jti` a fresh UUIDv7. Signed under the issuer's key.
    #[tracing::instrument(skip(self), fields(issuer = %self.issuer, peer = %params.peer, album = %params.album_id))]
    pub fn issue(
        &self,
        params: &IssueParams<'_>,
        now: Timestamp,
    ) -> Result<MintedCapability, IssueError> {
        let ttl = params.ttl.min(max_capability_ttl());
        let exp = now.checked_add(ttl).map_err(|_| IssueError::Timestamp)?;
        let jti = Uuid::now_v7().to_string();
        let claims = CapabilityClaims {
            iss: self.issuer.clone(),
            sub: params.peer.to_string(),
            aud: album_urn(params.album_id),
            scope: params.scope,
            iat: now.to_string(),
            exp: exp.to_string(),
            nbf: now.to_string(),
            jti: jti.clone(),
            min_protocol_version: params.min_protocol_version.to_string(),
        };
        let token = encode(&Header::new(Algorithm::EdDSA), &claims, &self.encoding_key)?;
        tracing::info!(jti, exp = %claims.exp, scope = ?claims.scope, "issued federation capability");
        Ok(MintedCapability { token, claims })
    }
}

/// The invariant-19 verification context: what the verifier expects the token to name.
#[derive(Debug, Clone)]
pub struct VerifyContext<'a> {
    /// The identity the verifier expects in `iss` (its own identity when verifying its own tokens,
    /// or the remote issuer's identity when verifying a remote grant).
    pub expected_issuer: &'a str,
    /// The album id the peer is pulling — matched against `aud`.
    pub album_id: &'a str,
    /// The verifier's current clock.
    pub now: Timestamp,
}

/// Decode and structurally verify a capability token's signature against `key`, returning its
/// claims. Temporal/audience/issuer/scope/revocation checks are layered on by
/// [`verify_capability`]; this is the signature + payload-shape gate only.
pub fn decode_capability(
    token: &str,
    key: &DecodingKey,
) -> Result<CapabilityClaims, CapabilityReject> {
    // The claims use RFC-3339 string times and an album `aud`, so disable jsonwebtoken's numeric
    // temporal validation and audience matching — we do all of it here against a jiff clock.
    let mut validation = Validation::new(Algorithm::EdDSA);
    validation.required_spec_claims = HashSet::new();
    validation.validate_exp = false;
    validation.validate_nbf = false;
    validation.validate_aud = false;

    decode::<CapabilityClaims>(token, key, &validation)
        .map(|data| data.claims)
        .map_err(|err| match err.kind() {
            jsonwebtoken::errors::ErrorKind::InvalidSignature => CapabilityReject::BadSignature,
            _ => CapabilityReject::Malformed("token"),
        })
}

/// Full invariant-19 verification: signature, non-empty claims, issuer, audience (the album),
/// the 24 h window ceiling, `nbf`/`exp` against the clock, and the revocation list (fail-closed on
/// a stale cache). Returns the verified claims. **Scope is enforced separately, per blob role**,
/// via [`authorize_blob_role`], because it depends on the specific blob being fetched.
#[tracing::instrument(skip(token, key, revocation), fields(album = %ctx.album_id))]
pub fn verify_capability(
    token: &str,
    key: &DecodingKey,
    ctx: &VerifyContext<'_>,
    revocation: &RevocationList,
) -> Result<CapabilityClaims, CapabilityReject> {
    let claims = decode_capability(token, key)?;

    // Every string identity claim must be present.
    for (name, value) in [
        ("iss", &claims.iss),
        ("sub", &claims.sub),
        ("aud", &claims.aud),
        ("jti", &claims.jti),
        ("min_protocol_version", &claims.min_protocol_version),
    ] {
        if value.trim().is_empty() {
            return Err(CapabilityReject::MissingClaim(name));
        }
    }

    if claims.iss != ctx.expected_issuer {
        return Err(CapabilityReject::WrongIssuer);
    }
    if claims.aud != album_urn(ctx.album_id) {
        return Err(CapabilityReject::AudienceMismatch);
    }

    let iat: Timestamp = claims
        .iat
        .parse()
        .map_err(|_| CapabilityReject::Malformed("iat"))?;
    let exp: Timestamp = claims
        .exp
        .parse()
        .map_err(|_| CapabilityReject::Malformed("exp"))?;
    let nbf: Timestamp = claims
        .nbf
        .parse()
        .map_err(|_| CapabilityReject::Malformed("nbf"))?;

    if exp.as_second() - iat.as_second() > max_capability_ttl().as_secs() {
        return Err(CapabilityReject::ExpiryTooFar);
    }
    if ctx.now < nbf {
        return Err(CapabilityReject::NotYetValid);
    }
    if ctx.now >= exp {
        return Err(CapabilityReject::Expired);
    }

    match revocation.check(&claims.jti, ctx.now) {
        RevocationVerdict::NotRevoked => {}
        RevocationVerdict::Revoked => return Err(CapabilityReject::Revoked),
        RevocationVerdict::Unverifiable => return Err(CapabilityReject::RevocationUnverifiable),
    }

    tracing::debug!(jti = %claims.jti, sub = %claims.sub, "capability verified");
    Ok(claims)
}

/// Invariant-19 scope enforcement for a specific blob fetch: refuse a blob whose server-visible
/// `role` the capability's scope does not cover (a derivative-only capability fetching an
/// original).
pub fn authorize_blob_role(scope: FederationScope, role: &str) -> Result<(), CapabilityReject> {
    if scope.permits_role(role) {
        Ok(())
    } else {
        Err(CapabilityReject::ScopeInsufficient)
    }
}
