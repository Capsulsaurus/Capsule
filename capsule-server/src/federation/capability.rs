//! The federation capability token, and the codec that mints and reads it.
//!
//! # The format is the contract
//!
//! design/federation.md makes the claim set normative — it is what every federated peer parses
//! and what this server signs — so the shape here is that table verbatim and nothing more:
//!
//! ```text
//! { "iss": <home server>, "sub": <peer server>, "aud": "urn:capsule:album:<UUID>",
//!   "scope": "read" | "read-derivative-only",
//!   "iat": <RFC 3339>, "exp": <RFC 3339>, "nbf": <RFC 3339>,
//!   "jti": <UUIDv7>, "min_protocol_version": <the album's pinned protocol date> }
//! ```
//!
//! Three deviations from RFC 7519 defaults, each the design's and each enforced here rather
//! than left to a peer's discretion:
//!
//! - **`aud` names the album, never the recipient.** The recipient is `sub`. A verifier that
//!   matched `aud` against itself would accept every capability for every album, so
//!   `jsonwebtoken`'s audience check is off and [`CapabilityCodec::verify`] hands the album back
//!   for the *route* to match against the album being pulled.
//! - **The three instants are RFC 3339 strings**, not numeric dates, so the library's own
//!   `exp`/`nbf` checks — against the system clock, with sixty seconds of leeway — are off and
//!   every temporal decision is made here against the injected [`Clock`]. The same rule
//!   [`crate::auth::tokens`] applies to session tokens, for the same reason: a deadline a test
//!   cannot walk over is a deadline nobody tests.
//! - **`exp` is never more than 24 hours after `iat`.** Minting clamps; verification refuses a
//!   wider window even under a valid signature, because the published revocation list is
//!   bounded *by* that ceiling and one long-lived token would quietly break the bound.
//!
//! # One key, two token types
//!
//! The codec signs with the **same** Ed25519 key `SessionTokens` does — the operational key
//! `server-info` publishes — and the two token types cannot be confused with each other: a
//! session token carries `iss = "capsule-api"` and a required `kind`, a capability carries
//! `iss = <server id>` and no `kind`, so each verifier finds the other's tokens unreadable by
//! construction.
//!
//! # Whole seconds, deliberately
//!
//! Every instant a capability carries is truncated to the second at mint. That is what lets a
//! grant be **re-signed byte-for-byte** from its stored record ([`CapabilityCodec::sign`]):
//! the refresh operation is idempotent on `(peer, jti)` and must answer a replay with the same
//! successor token, and a store that keeps microseconds cannot reproduce a nanosecond string.
//! Ed25519 signatures are deterministic, so the same claims sign to the same bytes.

use std::fmt;
use std::sync::Arc;

use jiff::{SignedDuration, Timestamp};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

use super::PeerId;
use crate::auth::tokens::SigningKeyError;
use crate::discovery::revocation::MAX_TOKEN_TTL;
use crate::store::{AlbumId, BlobRole, Clock};

/// The URN prefix an album-scoped `aud` claim carries.
pub const ALBUM_URN_PREFIX: &str = "urn:capsule:album:";

/// The `aud` claim for `album`.
#[must_use]
pub fn album_urn(album: &AlbumId) -> String {
    format!("{ALBUM_URN_PREFIX}{}", album.as_str())
}

/// The album an `aud` claim names, or `None` for a claim that is not an album URN.
///
/// The suffix must be a UUID, because an album id is one: a URN over any other text is not a
/// claim this server ever minted.
#[must_use]
pub fn album_from_urn(aud: &str) -> Option<AlbumId> {
    let id = aud.strip_prefix(ALBUM_URN_PREFIX)?;
    uuid::Uuid::parse_str(id).ok().map(|_| AlbumId::new(id))
}

/// What a capability grants over an album's blobs.
///
/// Enforced structurally against each blob's server-visible **role**, which is on its index row
/// and named by its signed envelope: a derivative-only capability is refused an `original` at
/// `GET /v1/blob/{hash}` whatever the peer claims to be fetching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    /// Everything a member reads: originals, derivatives, metadata, provenance.
    Read,
    /// Thumbnails and previews only — never originals.
    ReadDerivativeOnly,
}

impl Scope {
    /// The stable token this scope travels under, on the wire and in a column.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::ReadDerivativeOnly => "read-derivative-only",
        }
    }

    /// The scope a stored token names, or `None` for a token no version of this server wrote.
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "read" => Some(Self::Read),
            "read-derivative-only" => Some(Self::ReadDerivativeOnly),
            _ => None,
        }
    }

    /// Whether a blob of `role` may be fetched under this scope.
    ///
    /// A backup is refused under both: a peer pulls an album's assets, and a backup copy is the
    /// owner's own durability artefact rather than part of what was shared.
    pub fn permits(self, role: BlobRole) -> bool {
        match role {
            BlobRole::Backup => false,
            BlobRole::Original => matches!(self, Self::Read),
            BlobRole::Derivative | BlobRole::Metadata | BlobRole::Provenance => true,
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The claims a capability carries. Serialized as the JWT payload, verbatim from the design.
///
/// `deny_unknown_fields` because the set is closed: a claim the contract does not name is a
/// token this server did not mint, whatever its signature says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Claims {
    iss: String,
    sub: String,
    aud: String,
    scope: Scope,
    iat: String,
    exp: String,
    nbf: String,
    jti: String,
    min_protocol_version: String,
}

/// What a capability turned out to grant, once it verified.
///
/// Carries no raw token: everything downstream needs is here, and handing on the credential
/// itself is how one ends up in a log. `aud` has been parsed into the album it names, so a
/// route matches an [`AlbumId`] against an [`AlbumId`] rather than re-parsing a URN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityGrant {
    /// The peer server the grant was issued to (`sub`).
    pub peer: PeerId,
    /// The album it scopes to (`aud`).
    pub album: AlbumId,
    /// What it permits.
    pub scope: Scope,
    /// The revocation key (`jti`).
    pub jti: String,
    /// When it was issued; also its `nbf`.
    pub issued_at: Timestamp,
    /// When it stops being honoured.
    pub expires_at: Timestamp,
    /// The album's pinned protocol date, which the peer selects its parser from.
    pub min_protocol_version: String,
}

/// Why a presented capability was not honoured.
///
/// Deliberately carries no fragment of the token. The variants exist so the unit suite can
/// assert *which* mutation was refused; on the wire every one of them but [`Self::Expired`]
/// collapses into the framework's uncoded `401`, as the session scheme's do.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CapabilityError {
    /// The token did not verify: a bad signature, a malformed payload, or a missing claim.
    #[error("the capability could not be read")]
    Unreadable,
    /// The token verified and was issued by some other server.
    #[error("the capability was issued by another server")]
    WrongIssuer,
    /// A claim is present and is not the shape the contract fixes.
    #[error("the capability's {claim} claim is malformed")]
    Malformed {
        /// The claim that did not parse.
        claim: &'static str,
    },
    /// `exp` is more than the ceiling after `iat`.
    #[error("the capability's lifetime exceeds the {MAX_TOKEN_TTL} ceiling")]
    BeyondTtlCeiling,
    /// `nbf` is in the future on this server's clock.
    #[error("the capability is not valid yet")]
    NotYetValid,
    /// `exp` has passed.
    #[error("the capability has expired")]
    Expired,
}

/// The claims could not be signed. The server's fault, never the caller's.
#[derive(Debug, thiserror::Error)]
#[error("the capability could not be signed: {detail}")]
pub struct MintError {
    /// The signer's own description of the failure.
    pub detail: String,
}

/// What a mint asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintRequest {
    /// The peer server the grant is for.
    pub peer: PeerId,
    /// The album it scopes to.
    pub album: AlbumId,
    /// What it permits.
    pub scope: Scope,
    /// The album's pinned protocol date.
    pub min_protocol_version: String,
    /// The requested lifetime. Clamped into `1s ..= MAX_TOKEN_TTL`, never refused: a
    /// capability that expired before it was issued would be one the codec signs and cannot
    /// read.
    pub ttl: SignedDuration,
}

/// A freshly minted capability.
///
/// `Debug` is hand-written: the token is a bearer credential and a derived impl would publish
/// it to any `tracing` field that formatted the struct.
#[derive(Clone, PartialEq, Eq)]
pub struct Minted {
    /// The signed token, to hand to the peer.
    pub token: String,
    /// What it grants, for the issuer's own record.
    pub grant: CapabilityGrant,
}

impl fmt::Debug for Minted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Minted")
            .field("token", &"<redacted>")
            .field("grant", &self.grant)
            .finish()
    }
}

/// Mints and reads capabilities under this server's operational key.
///
/// `Debug` is hand-written and prints no key material.
pub struct CapabilityCodec {
    signing: EncodingKey,
    verifying: DecodingKey,
    public_key: Vec<u8>,
    validation: Validation,
    server_id: String,
    clock: Arc<dyn Clock>,
}

impl fmt::Debug for CapabilityCodec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CapabilityCodec")
            .field("server_id", &self.server_id)
            .field("keys", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl CapabilityCodec {
    /// A codec over the operator's PKCS#8 Ed25519 key, issuing as `server_id`.
    ///
    /// The **same** bytes `SessionTokens::from_pkcs8` takes, so the public half this derives is
    /// the one `server-info` publishes and the one a peer verifies against — `boot` asserts the
    /// two agree. `from_pkcs8_maybe_unchecked` for the reason the session signer uses it: a v1
    /// PKCS#8 document, which is what `openssl genpkey` writes, lacks the public half that is
    /// being derived here anyway.
    ///
    /// # Errors
    ///
    /// Returns [`SigningKeyError`] if `pkcs8_der` is not a readable Ed25519 private key.
    pub fn from_pkcs8(
        pkcs8_der: &[u8],
        server_id: impl Into<String>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, SigningKeyError> {
        use ring::signature::KeyPair as _;

        let pair = ring::signature::Ed25519KeyPair::from_pkcs8_maybe_unchecked(pkcs8_der).map_err(
            |error| SigningKeyError {
                detail: error.to_string(),
            },
        )?;
        let public_key = pair.public_key().as_ref().to_vec();

        // Everything temporal is decided here against `clock`, and `aud` is matched by the
        // route against the album: the library checks the signature and the algorithm and that
        // the three identity claims are present, and nothing else.
        let mut validation = Validation::new(Algorithm::EdDSA);
        validation.set_required_spec_claims(&["iss", "sub", "aud"]);
        validation.validate_exp = false;
        validation.validate_nbf = false;
        validation.validate_aud = false;

        Ok(Self {
            signing: EncodingKey::from_ed_der(pkcs8_der),
            verifying: DecodingKey::from_ed_der(&public_key),
            public_key,
            validation,
            server_id: server_id.into(),
            clock,
        })
    }

    /// The issuer every capability from this codec carries.
    pub fn server_id(&self) -> &str {
        &self.server_id
    }

    /// The raw Ed25519 public key capabilities verify under. Thirty-two bytes, no encoding.
    pub fn public_key(&self) -> &[u8] {
        &self.public_key
    }

    /// Mint a capability for `request`, at the clock's now.
    ///
    /// `iat = nbf = now`, `exp = now + min(ttl, ceiling)`, a fresh UUIDv7 `jti`, all instants
    /// at whole seconds (see the module docs).
    ///
    /// # Errors
    ///
    /// Returns [`MintError`] if the claims cannot be signed.
    pub fn mint(&self, request: &MintRequest) -> Result<Minted, MintError> {
        let now = whole_seconds(self.clock.now());
        let ttl = request
            .ttl
            .clamp(SignedDuration::from_secs(1), MAX_TOKEN_TTL);
        let grant = CapabilityGrant {
            peer: request.peer.clone(),
            album: request.album.clone(),
            scope: request.scope,
            jti: uuid::Uuid::now_v7().to_string(),
            issued_at: now,
            expires_at: whole_seconds(crate::store::deadline(now, ttl)),
            min_protocol_version: request.min_protocol_version.clone(),
        };
        let token = self.sign(&grant)?;
        tracing::info!(
            peer = %grant.peer,
            album = %grant.album,
            scope = %grant.scope,
            jti = %grant.jti,
            expires_at = %grant.expires_at,
            "minted a federation capability"
        );
        Ok(Minted { token, grant })
    }

    /// Sign `grant` exactly as it was first minted.
    ///
    /// What answers a replayed refresh with the same successor: the grant is rebuilt from its
    /// stored record and re-signed, and because every instant is at whole seconds and Ed25519
    /// is deterministic, the bytes are the bytes the peer already holds.
    ///
    /// # Errors
    ///
    /// Returns [`MintError`] if the claims cannot be signed.
    pub fn sign(&self, grant: &CapabilityGrant) -> Result<String, MintError> {
        let claims = Claims {
            iss: self.server_id.clone(),
            sub: grant.peer.as_str().to_owned(),
            aud: album_urn(&grant.album),
            scope: grant.scope,
            iat: grant.issued_at.to_string(),
            exp: grant.expires_at.to_string(),
            nbf: grant.issued_at.to_string(),
            jti: grant.jti.clone(),
            min_protocol_version: grant.min_protocol_version.clone(),
        };
        jsonwebtoken::encode(&Header::new(Algorithm::EdDSA), &claims, &self.signing).map_err(
            |error| MintError {
                detail: error.to_string(),
            },
        )
    }

    /// Read a presented capability.
    ///
    /// Signature and issuer first, then the shape of every claim, then the window against the
    /// ceiling, then the clock. The order reports the most specific true reason without ever
    /// computing with a claim that has not yet been checked.
    ///
    /// # Errors
    ///
    /// Returns [`CapabilityError`] for every way a token can fail; none carries any of it.
    pub fn verify(&self, presented: &str) -> Result<CapabilityGrant, CapabilityError> {
        let claims = jsonwebtoken::decode::<Claims>(presented, &self.verifying, &self.validation)
            .map_err(|error| {
                // The *kind* names which check failed and never any part of the credential.
                tracing::debug!(reason = ?error.kind(), "a presented capability did not verify");
                CapabilityError::Unreadable
            })?
            .claims;

        if claims.iss != self.server_id {
            tracing::debug!("a presented capability names another issuer");
            return Err(CapabilityError::WrongIssuer);
        }
        if claims.sub.is_empty() {
            return Err(CapabilityError::Malformed { claim: "sub" });
        }
        // A UUIDv7, as the table says and as this server mints: any other `jti` is a token
        // this server did not issue, whatever key it verifies under.
        if !uuid::Uuid::parse_str(&claims.jti).is_ok_and(|id| id.get_version_num() == 7) {
            return Err(CapabilityError::Malformed { claim: "jti" });
        }
        if claims
            .min_protocol_version
            .parse::<jiff::civil::Date>()
            .is_err()
        {
            return Err(CapabilityError::Malformed {
                claim: "min_protocol_version",
            });
        }
        let album =
            album_from_urn(&claims.aud).ok_or(CapabilityError::Malformed { claim: "aud" })?;
        let issued_at = instant(&claims.iat, "iat")?;
        let expires_at = instant(&claims.exp, "exp")?;
        let not_before = instant(&claims.nbf, "nbf")?;
        if expires_at <= issued_at {
            return Err(CapabilityError::Malformed { claim: "exp" });
        }
        if expires_at.duration_since(issued_at) > MAX_TOKEN_TTL {
            tracing::debug!(jti = %claims.jti, "a presented capability outlives the ceiling");
            return Err(CapabilityError::BeyondTtlCeiling);
        }

        let now = self.clock.now();
        if now < not_before {
            tracing::debug!(jti = %claims.jti, "a presented capability is not valid yet");
            return Err(CapabilityError::NotYetValid);
        }
        if expires_at <= now {
            tracing::debug!(jti = %claims.jti, "a presented capability has expired");
            return Err(CapabilityError::Expired);
        }

        Ok(CapabilityGrant {
            peer: PeerId::new(claims.sub),
            album,
            scope: claims.scope,
            jti: claims.jti,
            issued_at,
            expires_at,
            min_protocol_version: claims.min_protocol_version,
        })
    }
}

/// `at` with its sub-second part dropped.
fn whole_seconds(at: Timestamp) -> Timestamp {
    Timestamp::from_second(at.as_second()).unwrap_or(at)
}

/// An RFC 3339 claim as an instant.
fn instant(text: &str, claim: &'static str) -> Result<Timestamp, CapabilityError> {
    text.parse::<Timestamp>()
        .map_err(|_| CapabilityError::Malformed { claim })
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use serde_json::{Value, json};

    use super::*;
    use crate::store::memory::ManualClock;

    const SERVER: &str = "home.test";

    fn der() -> Vec<u8> {
        ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
            .expect("the platform generates a key")
            .as_ref()
            .to_vec()
    }

    fn codec(clock: Arc<ManualClock>) -> CapabilityCodec {
        CapabilityCodec::from_pkcs8(&der(), SERVER, clock).expect("a fresh key parses")
    }

    fn request() -> MintRequest {
        MintRequest {
            peer: PeerId::new("other.test"),
            album: AlbumId::new("01937b7c-0000-7000-8000-00000000a1b0"),
            scope: Scope::ReadDerivativeOnly,
            min_protocol_version: "2026-06-01".to_owned(),
            ttl: SignedDuration::from_hours(6),
        }
    }

    /// The payload of `token`, as JSON.
    fn payload(token: &str) -> Value {
        let segment = token.split('.').nth(1).expect("a JWT has three segments");
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(segment).expect("base64url"))
            .expect("the payload is JSON")
    }

    /// `token` with its payload replaced by `edit(payload)`, re-signed under `key`.
    fn resigned(token: &str, key: &[u8], edit: impl FnOnce(&mut Value)) -> String {
        let mut claims = payload(token);
        edit(&mut claims);
        jsonwebtoken::encode(
            &Header::new(Algorithm::EdDSA),
            &claims,
            &EncodingKey::from_ed_der(key),
        )
        .expect("the edited claims sign")
    }

    #[test]
    fn a_minted_capability_verifies_and_carries_exactly_the_contracts_claims() {
        let clock = Arc::new(ManualClock::default());
        let codec = codec(clock);
        let minted = codec.mint(&request()).expect("it mints");

        let claims = payload(&minted.token);
        let keys: Vec<&str> = claims
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "aud",
                "exp",
                "iat",
                "iss",
                "jti",
                "min_protocol_version",
                "nbf",
                "scope",
                "sub"
            ],
            "the claim set is the design's table and nothing else"
        );
        assert_eq!(claims["iss"], SERVER);
        assert_eq!(claims["sub"], "other.test");
        assert_eq!(
            claims["aud"],
            "urn:capsule:album:01937b7c-0000-7000-8000-00000000a1b0"
        );
        assert_eq!(claims["scope"], "read-derivative-only");
        assert_eq!(claims["iat"], "1970-01-01T00:00:00Z");
        assert_eq!(claims["nbf"], "1970-01-01T00:00:00Z");
        assert_eq!(claims["exp"], "1970-01-01T06:00:00Z");
        assert_eq!(
            uuid::Uuid::parse_str(claims["jti"].as_str().expect("a string"))
                .expect("a uuid")
                .get_version_num(),
            7
        );

        let grant = codec.verify(&minted.token).expect("it verifies");
        assert_eq!(grant, minted.grant);
    }

    #[test]
    fn a_grant_re_signs_to_the_same_bytes() {
        // What refresh idempotency rests on: the record can reproduce the token.
        let codec = codec(Arc::new(ManualClock::default()));
        let minted = codec.mint(&request()).expect("it mints");
        assert_eq!(codec.sign(&minted.grant).expect("it signs"), minted.token);
    }

    #[test]
    fn the_lifetime_is_clamped_to_the_ceiling_and_the_instants_are_whole_seconds() {
        let clock = Arc::new(ManualClock::new(
            Timestamp::from_nanosecond(1_700_000_000_123_456_789).expect("an instant"),
        ));
        let codec = codec(clock);
        let minted = codec
            .mint(&MintRequest {
                ttl: SignedDuration::from_hours(48),
                ..request()
            })
            .expect("it mints");
        assert_eq!(
            minted.grant.issued_at,
            Timestamp::from_second(1_700_000_000).expect("an instant")
        );
        assert_eq!(
            minted
                .grant
                .expires_at
                .duration_since(minted.grant.issued_at),
            MAX_TOKEN_TTL
        );
        assert!(codec.verify(&minted.token).is_ok());
    }

    #[test]
    fn every_mutation_of_a_claim_is_refused_with_its_reason() {
        // The federation doc's own unit bullet: mutate each claim and assert the reason.
        let clock = Arc::new(ManualClock::default());
        let key = der();
        let codec = CapabilityCodec::from_pkcs8(&key, SERVER, clock.clone()).expect("parses");
        let minted = codec.mint(&request()).expect("it mints");
        let token = &minted.token;

        // A lifetime that is not positive is clamped up rather than signed unreadable.
        let instant = codec
            .mint(&MintRequest {
                ttl: SignedDuration::from_secs(-5),
                ..request()
            })
            .expect("it mints");
        assert_eq!(
            instant
                .grant
                .expires_at
                .duration_since(instant.grant.issued_at),
            SignedDuration::from_secs(1)
        );
        assert!(codec.verify(&instant.token).is_ok());

        // The signature: any other key, or a flipped payload byte under the right key.
        let forged = resigned(token, &der(), |_| {});
        assert_eq!(codec.verify(&forged), Err(CapabilityError::Unreadable));
        let mut tampered = token.clone();
        let payload_start = tampered.find('.').expect("a dot") + 1;
        let byte = tampered.as_bytes()[payload_start];
        tampered.replace_range(
            payload_start..=payload_start,
            if byte == b'A' { "B" } else { "A" },
        );
        assert_eq!(codec.verify(&tampered), Err(CapabilityError::Unreadable));

        // Each claim, under the real key.
        type Edit = Box<dyn FnOnce(&mut Value)>;
        let cases: [(&str, Edit, CapabilityError); 16] = [
            (
                "iss",
                Box::new(|c| c["iss"] = json!("elsewhere.test")),
                CapabilityError::WrongIssuer,
            ),
            (
                "sub",
                Box::new(|c| c["sub"] = json!("")),
                CapabilityError::Malformed { claim: "sub" },
            ),
            (
                "aud",
                Box::new(|c| c["aud"] = json!("urn:capsule:user:someone")),
                CapabilityError::Malformed { claim: "aud" },
            ),
            (
                "scope",
                Box::new(|c| c["scope"] = json!("write")),
                CapabilityError::Unreadable,
            ),
            (
                "iat",
                Box::new(|c| c["iat"] = json!(0)),
                CapabilityError::Unreadable,
            ),
            (
                "exp",
                Box::new(|c| c["exp"] = json!("tomorrow")),
                CapabilityError::Malformed { claim: "exp" },
            ),
            (
                "exp before iat",
                Box::new(|c| c["exp"] = json!("1969-12-31T23:00:00Z")),
                CapabilityError::Malformed { claim: "exp" },
            ),
            (
                "exp beyond the ceiling",
                Box::new(|c| c["exp"] = json!("1970-01-02T00:00:01Z")),
                CapabilityError::BeyondTtlCeiling,
            ),
            (
                "nbf",
                Box::new(|c| c["nbf"] = json!("1970-01-01T01:00:00Z")),
                CapabilityError::NotYetValid,
            ),
            (
                "jti",
                Box::new(|c| c["jti"] = json!("")),
                CapabilityError::Malformed { claim: "jti" },
            ),
            (
                "jti that is not a UUIDv7",
                Box::new(|c| c["jti"] = json!("2b6ed3a6-4c7e-4f3a-9d3c-1f1f1f1f1f1f")),
                CapabilityError::Malformed { claim: "jti" },
            ),
            (
                "aud whose suffix is not an album id",
                Box::new(|c| c["aud"] = json!("urn:capsule:album:not-an-id")),
                CapabilityError::Malformed { claim: "aud" },
            ),
            (
                "min_protocol_version",
                Box::new(|c| c["min_protocol_version"] = json!("soon")),
                CapabilityError::Malformed {
                    claim: "min_protocol_version",
                },
            ),
            (
                "an extra claim",
                Box::new(|c| c["kind"] = json!("access")),
                CapabilityError::Unreadable,
            ),
            (
                "a missing claim",
                Box::new(|c| {
                    c.as_object_mut().expect("an object").remove("jti");
                }),
                CapabilityError::Unreadable,
            ),
            (
                "a session token's shape",
                Box::new(|c| {
                    c["iss"] = json!("capsule-api");
                    c["kind"] = json!("access");
                }),
                CapabilityError::Unreadable,
            ),
        ];
        for (claim, edit, expected) in cases {
            let mutated = resigned(token, &key, edit);
            assert_eq!(
                codec.verify(&mutated),
                Err(expected),
                "mutating {claim} was not refused as expected"
            );
        }

        // And expiry, on the clock rather than by editing a claim.
        clock.advance(SignedDuration::from_hours(6));
        assert_eq!(codec.verify(token), Err(CapabilityError::Expired));
    }

    #[test]
    fn a_session_token_is_unreadable_to_the_capability_codec_and_vice_versa() {
        // The same key signs both, and the two verifiers still cannot be confused: a session
        // token carries `iss = capsule-api` and a `kind`, a capability neither.
        let clock = Arc::new(ManualClock::default());
        let key = der();
        let codec = CapabilityCodec::from_pkcs8(&key, SERVER, clock.clone()).expect("parses");
        let sessions = crate::auth::SessionTokens::from_pkcs8(&key, clock).expect("parses");
        assert_eq!(codec.public_key(), sessions.public_key());

        let issued = sessions
            .issue(
                &crate::store::UserId::new("user"),
                &crate::store::SessionId::new("session"),
                SignedDuration::from_hours(1),
            )
            .expect("it issues");
        assert_eq!(
            codec.verify(&issued.access_token),
            Err(CapabilityError::Unreadable),
            "a session token has no aud, which the capability codec requires"
        );

        let minted = codec.mint(&request()).expect("it mints");
        assert!(matches!(
            sessions.verify(&minted.token, crate::auth::TokenKind::Access),
            Err(crate::auth::TokenError::Unreadable)
        ));
    }

    #[test]
    fn scope_is_decided_by_the_blobs_role() {
        for role in [
            BlobRole::Derivative,
            BlobRole::Metadata,
            BlobRole::Provenance,
        ] {
            assert!(Scope::Read.permits(role));
            assert!(Scope::ReadDerivativeOnly.permits(role));
        }
        assert!(Scope::Read.permits(BlobRole::Original));
        assert!(!Scope::ReadDerivativeOnly.permits(BlobRole::Original));
        assert!(!Scope::Read.permits(BlobRole::Backup));
        assert!(!Scope::ReadDerivativeOnly.permits(BlobRole::Backup));
        for scope in [Scope::Read, Scope::ReadDerivativeOnly] {
            assert_eq!(Scope::from_token(scope.as_str()), Some(scope));
        }
        assert_eq!(Scope::from_token("write"), None);
    }

    #[test]
    fn the_album_urn_round_trips_and_nothing_else_parses() {
        let album = AlbumId::new("01937b7c-0000-7000-8000-00000000a1b0");
        assert_eq!(album_from_urn(&album_urn(&album)), Some(album));
        assert_eq!(album_from_urn("urn:capsule:album:"), None);
        assert_eq!(album_from_urn("home.test"), None);
    }

    #[test]
    fn nothing_prints_a_token_or_a_key() {
        let codec = codec(Arc::new(ManualClock::default()));
        let minted = codec.mint(&request()).expect("it mints");
        let rendered = format!("{minted:?} {codec:?}");
        assert!(!rendered.contains(&minted.token), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
    }
}
