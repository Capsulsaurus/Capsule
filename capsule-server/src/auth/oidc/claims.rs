//! [`verify_id_token`] — every check an ID token has to pass, as one pure function.
//!
//! # No I/O, no clock, no configuration
//!
//! The function takes the token, the key set it must verify under, what the relying party
//! expects, and the instant to judge expiry against. Nothing is fetched and nothing is read
//! from the environment, which is what lets every negative case here be a unit test with no
//! socket: a foreign key, a wrong audience, an expired token and a replayed nonce are each a
//! few lines against a key generated in the test.
//!
//! # Why the checks are Capsule's and not `jsonwebtoken`'s
//!
//! `jsonwebtoken` can validate `exp`, `iss` and `aud` itself, and it is deliberately asked to
//! do **only the signature**. Its temporal checks read the system clock, which would put the one
//! part of this module that has to be deterministic in tests behind a clock a test cannot move —
//! and each of the claim checks below is a security decision this repository documents, so it
//! is written where a reader can see it rather than delegated to a struct of booleans.
//!
//! # One answer on the wire, many in the log
//!
//! [`ClaimRejection`] names the check that failed, for the operator. The route collapses every
//! variant to one `error.auth.oidc_token_invalid`, so the callback is not an oracle over which
//! checks the relying party runs; the distinction that *does* reach a caller is between a token
//! that failed and an identity provider that could not be reached, which are different remedies.

use std::collections::HashSet;

use jiff::Timestamp;
use jsonwebtoken::jwk::{AlgorithmParameters, JwkSet};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;

/// The signature algorithms an ID token may carry.
///
/// Asymmetric only. `none` is refused by `jsonwebtoken` before it reaches here, and the HMAC
/// family is refused here because a JWKS can carry an `oct` key — and an ID token "signed" with
/// a symmetric key the provider published is a token anyone could have minted.
pub const ALLOWED_ALGORITHMS: [Algorithm; 3] =
    [Algorithm::RS256, Algorithm::ES256, Algorithm::EdDSA];

/// How far the relying party's clock may disagree with the provider's, in seconds.
///
/// Sixty, applied symmetrically to `exp`, `nbf` and `iat`. Generous enough for an unsynchronized
/// virtual machine, tight enough that a token is not honoured minutes after its provider said to
/// stop.
pub const CLOCK_SKEW_SECONDS: i64 = 60;

/// The longest `sub` the relying party will store.
///
/// OpenID Connect Core §2 bounds `sub` at 255 ASCII characters; a longer one is a provider
/// that is not conforming, and an unbounded value is a column nothing sized.
pub const MAX_SUBJECT_LENGTH: usize = 255;

/// The most of a provider-supplied string a log line or an error will carry.
///
/// A token's `iss`, its `kid` and a token endpoint's `error_description` all come from the
/// other side of the wire and all end up in a `WARN`. Bounded here, at construction, so a
/// provider that answers with a megabyte cannot put a megabyte in the log.
pub const MAX_QUOTED_BYTES: usize = 255;

/// `text` cut to [`MAX_QUOTED_BYTES`] on a character boundary, with a marker when it was cut.
#[must_use]
pub fn bounded(text: &str) -> String {
    if text.len() <= MAX_QUOTED_BYTES {
        return text.to_owned();
    }
    let mut end = MAX_QUOTED_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// What the relying party expects the token to say about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expectations {
    /// The configured issuer, compared for exact string equality with `iss`.
    pub issuer: String,
    /// This relying party's `client_id`; `aud` must contain it and `azp`, if present, must be it.
    pub client_id: String,
    /// The nonce the authorization request carried, which the token must echo exactly — or
    /// `None` for a grant that sends none, under which the token must carry **no** `nonce`.
    ///
    /// `None` is the device authorization grant (RFC 8628), which has no front channel for a
    /// nonce to bind. What the nonce proves on the code flow — this token answers this request —
    /// is proven there by the channel: the token arrives on the relying party's own TLS request
    /// to the token endpoint, in answer to a device code only it holds. A token that does carry
    /// a nonce was minted for some authorization request, so it is not the answer to this grant
    /// and is refused (design/authentication.md, "Signing In Without a Browser").
    pub nonce: Option<String>,
}

/// The facts a verified ID token establishes about a person.
///
/// `sub` and `iss` together are the account's federated key. The address is carried for the
/// one decision the route makes with it — refusing to create a second account for an address
/// that already has one — and is never a link key; see `auth::oidc::accounts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedIdentity {
    /// The issuer the token verified under, exactly as configured.
    pub issuer: String,
    /// The provider's stable identifier for the person. Never an address.
    pub subject: String,
    /// The address the provider asserted, if it asserted one.
    pub email: Option<String>,
    /// Whether the provider says it verified that address.
    pub email_verified: bool,
}

/// Why an ID token was refused.
///
/// Logged at `WARN` with its detail; rendered on the wire as one code.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClaimRejection {
    /// The token is not a compact JWS, or its header does not parse.
    #[error("the ID token is not a well-formed JWS: {detail}")]
    Malformed {
        /// The parser's own description.
        detail: String,
    },
    /// The header names an algorithm outside [`ALLOWED_ALGORITHMS`].
    #[error("the ID token is signed with {algorithm}, which this relying party refuses")]
    AlgorithmRefused {
        /// The header's `alg`, as written.
        algorithm: String,
    },
    /// The header carries a non-empty `crit` (RFC 7515 §4.1.11).
    ///
    /// `crit` names extensions the recipient **must** understand or reject the token. This
    /// relying party understands none, so the only conforming answer is to refuse — and a
    /// forger who could make the verifier skip a header it does not know would have exactly
    /// the seam `crit` exists to close.
    #[error("the ID token names critical header extensions this relying party does not implement")]
    CriticalHeader,
    /// The header names a key the set does not hold.
    ///
    /// The one rejection a caller acts on rather than logs: it is what triggers a JWKS refetch,
    /// because a provider that rotated its keys announces the fact this way.
    #[error("the ID token names key {kid:?}, which the key set does not hold")]
    UnknownKey {
        /// The header's `kid`, or `None` when the header carries none.
        kid: Option<String>,
    },
    /// The key the header named cannot be used to verify — a symmetric key, or one whose
    /// parameters do not parse.
    #[error("the key the ID token names cannot verify an asymmetric signature: {detail}")]
    UnusableKey {
        /// What was wrong with it.
        detail: String,
    },
    /// The signature does not verify under the named key.
    #[error("the ID token's signature does not verify")]
    Signature,
    /// The claims are not the shape OpenID Connect Core §2 requires.
    #[error("the ID token's claims are not usable: {detail}")]
    Claims {
        /// What was missing or malformed.
        detail: String,
    },
    /// `iss` is not the configured issuer.
    #[error("the ID token was issued by {found:?}, not by the configured issuer")]
    Issuer {
        /// The token's `iss`.
        found: String,
    },
    /// `aud` does not contain this relying party.
    #[error("the ID token is not addressed to this relying party")]
    Audience,
    /// `azp` is present and is not this relying party — or `aud` names several audiences and
    /// `azp` is absent, which OpenID Connect Core §3.1.3.7 rule 4 says it must not be.
    #[error("the ID token's authorized party is not this relying party")]
    AuthorizedParty,
    /// `exp` is in the past, beyond the skew.
    #[error("the ID token expired at {expired_at}")]
    Expired {
        /// The token's `exp`.
        expired_at: Timestamp,
    },
    /// `nbf` or `iat` is in the future, beyond the skew.
    #[error("the ID token is not valid before {valid_from}")]
    NotYetValid {
        /// The later of `nbf` and `iat`.
        valid_from: Timestamp,
    },
    /// `nonce` is absent or is not the one the authorization request carried — or, for a grant
    /// that sends none, is present at all.
    #[error("the ID token's nonce is not the one this ceremony issued")]
    Nonce,
    /// `sub` is empty or over [`MAX_SUBJECT_LENGTH`].
    #[error("the ID token's subject is not usable")]
    Subject,
}

/// The claims an ID token carries, as this relying party reads them.
///
/// `aud` is deserialized from either a string or an array, which OpenID Connect Core §2 allows.
#[derive(Debug, Deserialize)]
struct IdTokenClaims {
    iss: String,
    sub: String,
    #[serde(default, deserialize_with = "one_or_many")]
    aud: Vec<String>,
    exp: i64,
    #[serde(default)]
    iat: Option<i64>,
    #[serde(default)]
    nbf: Option<i64>,
    #[serde(default)]
    nonce: Option<String>,
    #[serde(default)]
    azp: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default, deserialize_with = "lenient_bool")]
    email_verified: Option<bool>,
}

/// `email_verified` as a JSON boolean or as the string `"true"` / `"false"`.
///
/// OpenID Connect Core §5.1 types the claim as a boolean, but some providers (Amazon Cognito
/// among them) send it as a string. Refusing those tokens fails the whole sign-in over a claim
/// that only ever *grants* trust, so the string spellings are read, ASCII case-insensitively,
/// and every other value — another string, a number, `null` — reads as absent, which
/// [`verify_id_token`] then treats as unverified. An unparseable claim never verifies an address.
fn lenient_bool<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::Bool(flag) => Some(flag),
        serde_json::Value::String(text) if text.eq_ignore_ascii_case("true") => Some(true),
        serde_json::Value::String(text) if text.eq_ignore_ascii_case("false") => Some(false),
        _ => None,
    })
}

/// `aud` as a single string or as an array of them.
fn one_or_many<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match OneOrMany::deserialize(deserializer)? {
        OneOrMany::One(one) => vec![one],
        OneOrMany::Many(many) => many,
    })
}

/// Verify `raw` under `keys` against `expect`, judging time against `now`.
///
/// The checks run in the order listed in the module docs, and the first failure is the answer:
/// header algorithm, key lookup, signature, then `iss`, `aud`, `azp`, the temporal claims,
/// `nonce`, and `sub`.
///
/// # Errors
///
/// Returns the first [`ClaimRejection`] the token trips.
pub fn verify_id_token(
    raw: &str,
    keys: &JwkSet,
    expect: &Expectations,
    now: Timestamp,
) -> Result<VerifiedIdentity, ClaimRejection> {
    let header = jsonwebtoken::decode_header(raw).map_err(|error| ClaimRejection::Malformed {
        detail: error.to_string(),
    })?;
    if !ALLOWED_ALGORITHMS.contains(&header.alg) {
        return Err(ClaimRejection::AlgorithmRefused {
            algorithm: format!("{:?}", header.alg),
        });
    }
    if header.crit.as_ref().is_some_and(|crit| !crit.is_empty()) {
        return Err(ClaimRejection::CriticalHeader);
    }

    // A header without `kid` resolves only when the set holds exactly one key: a provider that
    // publishes several and names none has given the verifier nothing to choose on, and trying
    // each in turn would let a forger pick the weakest.
    let jwk = match header.kid.as_deref() {
        Some(kid) => keys.find(kid),
        None if keys.keys.len() == 1 => keys.keys.first(),
        None => None,
    }
    .ok_or_else(|| ClaimRejection::UnknownKey {
        kid: header.kid.as_deref().map(bounded),
    })?;
    if matches!(jwk.algorithm, AlgorithmParameters::OctetKey(_)) {
        return Err(ClaimRejection::UnusableKey {
            detail: "the key is symmetric".to_owned(),
        });
    }
    if let Some(declared) = jwk.common.key_algorithm
        && format!("{declared:?}") != format!("{:?}", header.alg)
    {
        // A key published for one algorithm and used under another is the substitution attack
        // RFC 8725 §3.1 warns about; the header does not get to choose.
        return Err(ClaimRejection::UnusableKey {
            detail: format!(
                "the key is published for {declared:?} and the token claims {:?}",
                header.alg
            ),
        });
    }
    let key = DecodingKey::from_jwk(jwk).map_err(|error| ClaimRejection::UnusableKey {
        detail: error.to_string(),
    })?;

    // Signature only. Every claim below is checked here, against the caller's clock.
    let mut validation = Validation::new(header.alg);
    validation.validate_exp = false;
    validation.validate_nbf = false;
    validation.validate_aud = false;
    validation.required_spec_claims = HashSet::new();
    let decoded = jsonwebtoken::decode::<IdTokenClaims>(raw, &key, &validation).map_err(
        |error| match error.kind() {
            jsonwebtoken::errors::ErrorKind::InvalidSignature => ClaimRejection::Signature,
            jsonwebtoken::errors::ErrorKind::Json(_)
            | jsonwebtoken::errors::ErrorKind::MissingRequiredClaim(_)
            | jsonwebtoken::errors::ErrorKind::InvalidClaimFormat(_) => ClaimRejection::Claims {
                detail: error.to_string(),
            },
            jsonwebtoken::errors::ErrorKind::InvalidToken
            | jsonwebtoken::errors::ErrorKind::Base64(_)
            | jsonwebtoken::errors::ErrorKind::Utf8(_) => ClaimRejection::Malformed {
                detail: error.to_string(),
            },
            _ => ClaimRejection::UnusableKey {
                detail: error.to_string(),
            },
        },
    )?;
    let claims = decoded.claims;

    if claims.iss != expect.issuer {
        return Err(ClaimRejection::Issuer {
            found: bounded(&claims.iss),
        });
    }
    if !claims.aud.contains(&expect.client_id) {
        return Err(ClaimRejection::Audience);
    }
    // Core §3.1.3.7: `azp`, when present, must be us; and when `aud` names several parties it
    // must be present, or a token minted for a different client that merely lists us could be
    // presented here.
    match claims.azp.as_deref() {
        Some(azp) if azp != expect.client_id => return Err(ClaimRejection::AuthorizedParty),
        None if claims.aud.len() > 1 => return Err(ClaimRejection::AuthorizedParty),
        _ => {}
    }

    let skew = jiff::SignedDuration::from_secs(CLOCK_SKEW_SECONDS);
    let expired_at = instant(claims.exp)?;
    if expired_at.checked_add(skew).unwrap_or(Timestamp::MAX) <= now {
        return Err(ClaimRejection::Expired { expired_at });
    }
    let valid_from = [claims.nbf, claims.iat]
        .into_iter()
        .flatten()
        .map(instant)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .max();
    if let Some(valid_from) = valid_from
        && valid_from.checked_sub(skew).unwrap_or(Timestamp::MIN) > now
    {
        return Err(ClaimRejection::NotYetValid { valid_from });
    }

    // Compared as bytes rather than as strings with a short-circuiting `==` on purpose; the
    // nonce is high-entropy and single-use so the timing channel is academic, but the compare
    // costs nothing and the habit is worth keeping.
    let nonce_matches = match (claims.nonce.as_deref(), expect.nonce.as_deref()) {
        (Some(nonce), Some(expected)) => constant_time_equal(nonce.as_bytes(), expected.as_bytes()),
        (None, None) => true,
        (Some(_), None) | (None, Some(_)) => false,
    };
    if !nonce_matches {
        return Err(ClaimRejection::Nonce);
    }

    if claims.sub.is_empty() || claims.sub.len() > MAX_SUBJECT_LENGTH {
        return Err(ClaimRejection::Subject);
    }

    Ok(VerifiedIdentity {
        issuer: claims.iss,
        subject: claims.sub,
        email: claims
            .email
            .map(|address| address.trim().to_owned())
            .filter(|address| !address.is_empty()),
        email_verified: claims.email_verified.unwrap_or(false),
    })
}

/// A NumericDate claim as an instant, refusing one outside representable time.
fn instant(seconds: i64) -> Result<Timestamp, ClaimRejection> {
    Timestamp::from_second(seconds).map_err(|error| ClaimRejection::Claims {
        detail: format!("a NumericDate claim is out of range: {error}"),
    })
}

/// Byte equality that does not stop at the first difference.
fn constant_time_equal(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq as _;
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

#[cfg(test)]
mod tests;
