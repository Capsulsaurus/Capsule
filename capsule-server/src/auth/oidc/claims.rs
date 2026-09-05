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

/// What the relying party expects the token to say about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expectations {
    /// The configured issuer, compared for exact string equality with `iss`.
    pub issuer: String,
    /// This relying party's `client_id`; `aud` must contain it and `azp`, if present, must be it.
    pub client_id: String,
    /// The nonce the authorization request carried; the token must echo it exactly.
    pub nonce: String,
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
    /// `azp` is present and is not this relying party.
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
    /// `nonce` is absent or is not the one the authorization request carried.
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
    #[serde(default)]
    email_verified: Option<bool>,
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

    // A header without `kid` resolves only when the set holds exactly one key: a provider that
    // publishes several and names none has given the verifier nothing to choose on, and trying
    // each in turn would let a forger pick the weakest.
    let jwk = match header.kid.as_deref() {
        Some(kid) => keys.find(kid),
        None if keys.keys.len() == 1 => keys.keys.first(),
        None => None,
    }
    .ok_or_else(|| ClaimRejection::UnknownKey {
        kid: header.kid.clone(),
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
        return Err(ClaimRejection::Issuer { found: claims.iss });
    }
    if !claims.aud.contains(&expect.client_id) {
        return Err(ClaimRejection::Audience);
    }
    if claims
        .azp
        .as_deref()
        .is_some_and(|azp| azp != expect.client_id)
    {
        return Err(ClaimRejection::AuthorizedParty);
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
    let nonce_matches = claims
        .nonce
        .as_deref()
        .is_some_and(|nonce| constant_time_equal(nonce.as_bytes(), expect.nonce.as_bytes()));
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
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use jsonwebtoken::jwk::{
        CommonParameters, EllipticCurve, Jwk, KeyAlgorithm, OctetKeyPairParameters,
        OctetKeyPairType,
    };
    use jsonwebtoken::{EncodingKey, Header};
    use ring::signature::KeyPair as _;
    use serde_json::json;

    use super::*;

    const ISSUER: &str = "https://idp.example.test";
    const CLIENT: &str = "capsule";
    const NONCE: &str = "nonce-1";

    /// A signing key and the JWK a provider would publish for it.
    struct Signer {
        kid: &'static str,
        encoding: EncodingKey,
        public: Vec<u8>,
    }

    impl Signer {
        fn generate(kid: &'static str) -> Self {
            let der =
                ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
                    .expect("the platform generates keys");
            let pair = ring::signature::Ed25519KeyPair::from_pkcs8(der.as_ref())
                .expect("a key just generated parses");
            Self {
                kid,
                encoding: EncodingKey::from_ed_der(der.as_ref()),
                public: pair.public_key().as_ref().to_vec(),
            }
        }

        fn jwk(&self) -> Jwk {
            Jwk {
                common: CommonParameters {
                    key_id: Some(self.kid.to_owned()),
                    key_algorithm: Some(KeyAlgorithm::EdDSA),
                    ..CommonParameters::default()
                },
                algorithm: AlgorithmParameters::OctetKeyPair(OctetKeyPairParameters {
                    key_type: OctetKeyPairType::OctetKeyPair,
                    curve: EllipticCurve::Ed25519,
                    x: URL_SAFE_NO_PAD.encode(&self.public),
                }),
            }
        }

        fn sign(&self, claims: &serde_json::Value) -> String {
            let mut header = Header::new(Algorithm::EdDSA);
            header.kid = Some(self.kid.to_owned());
            jsonwebtoken::encode(&header, claims, &self.encoding).expect("the key signs")
        }
    }

    fn keys(signers: &[&Signer]) -> JwkSet {
        JwkSet {
            keys: signers.iter().map(|signer| signer.jwk()).collect(),
        }
    }

    fn expectations() -> Expectations {
        Expectations {
            issuer: ISSUER.to_owned(),
            client_id: CLIENT.to_owned(),
            nonce: NONCE.to_owned(),
        }
    }

    fn now() -> Timestamp {
        Timestamp::from_second(1_700_000_000).expect("in range")
    }

    /// Claims a conforming provider would mint for a sign-in that began a minute ago.
    fn good_claims() -> serde_json::Value {
        json!({
            "iss": ISSUER,
            "sub": "subject-1",
            "aud": CLIENT,
            "exp": now().as_second() + 300,
            "iat": now().as_second() - 60,
            "nonce": NONCE,
            "email": "  somebody@example.test ",
            "email_verified": true,
        })
    }

    fn verify(token: &str, keys: &JwkSet) -> Result<VerifiedIdentity, ClaimRejection> {
        verify_id_token(token, keys, &expectations(), now())
    }

    #[test]
    fn a_conforming_token_yields_the_identity() {
        let signer = Signer::generate("k1");
        let identity = verify(&signer.sign(&good_claims()), &keys(&[&signer])).expect("verifies");
        assert_eq!(
            identity,
            VerifiedIdentity {
                issuer: ISSUER.to_owned(),
                subject: "subject-1".to_owned(),
                email: Some("somebody@example.test".to_owned()),
                email_verified: true,
            }
        );
    }

    #[test]
    fn an_audience_array_containing_the_client_is_accepted() {
        let signer = Signer::generate("k1");
        let mut claims = good_claims();
        claims["aud"] = json!(["somebody-else", CLIENT]);
        claims["azp"] = json!(CLIENT);
        assert!(verify(&signer.sign(&claims), &keys(&[&signer])).is_ok());
    }

    #[test]
    fn a_token_signed_by_a_foreign_key_is_refused() {
        let ours = Signer::generate("k1");
        let theirs = Signer::generate("k1");
        assert_eq!(
            verify(&theirs.sign(&good_claims()), &keys(&[&ours])),
            Err(ClaimRejection::Signature)
        );
    }

    #[test]
    fn an_unknown_kid_is_the_one_rejection_that_asks_for_a_refetch() {
        let old = Signer::generate("k1");
        let rotated = Signer::generate("k2");
        assert_eq!(
            verify(&rotated.sign(&good_claims()), &keys(&[&old])),
            Err(ClaimRejection::UnknownKey {
                kid: Some("k2".to_owned())
            })
        );
        // And the same token verifies once the set has caught up.
        assert!(verify(&rotated.sign(&good_claims()), &keys(&[&old, &rotated])).is_ok());
    }

    #[test]
    fn a_header_without_kid_resolves_only_against_a_single_key() {
        let signer = Signer::generate("k1");
        let header = Header::new(Algorithm::EdDSA);
        let token = jsonwebtoken::encode(&header, &good_claims(), &signer.encoding).expect("signs");
        assert!(verify(&token, &keys(&[&signer])).is_ok());

        let other = Signer::generate("k2");
        assert_eq!(
            verify(&token, &keys(&[&signer, &other])),
            Err(ClaimRejection::UnknownKey { kid: None }),
            "two keys and no kid is a choice the verifier must not make"
        );
    }

    #[test]
    fn a_symmetric_key_in_the_set_cannot_verify_anything() {
        let signer = Signer::generate("k1");
        let mut set = keys(&[&signer]);
        set.keys[0].algorithm =
            AlgorithmParameters::OctetKey(jsonwebtoken::jwk::OctetKeyParameters {
                key_type: jsonwebtoken::jwk::OctetKeyType::Octet,
                value: URL_SAFE_NO_PAD.encode(b"shared secret"),
            });
        assert!(matches!(
            verify(&signer.sign(&good_claims()), &set),
            Err(ClaimRejection::UnusableKey { .. })
        ));
    }

    #[test]
    fn an_hmac_token_is_refused_before_any_key_is_consulted() {
        let signer = Signer::generate("k1");
        let mut header = Header::new(Algorithm::HS256);
        header.kid = Some("k1".to_owned());
        let token = jsonwebtoken::encode(
            &header,
            &good_claims(),
            &EncodingKey::from_secret(b"anything"),
        )
        .expect("signs");
        assert_eq!(
            verify(&token, &keys(&[&signer])),
            Err(ClaimRejection::AlgorithmRefused {
                algorithm: "HS256".to_owned()
            })
        );
    }

    #[test]
    fn a_key_published_for_another_algorithm_is_not_used_under_this_one() {
        let signer = Signer::generate("k1");
        let mut set = keys(&[&signer]);
        set.keys[0].common.key_algorithm = Some(KeyAlgorithm::RS256);
        assert!(matches!(
            verify(&signer.sign(&good_claims()), &set),
            Err(ClaimRejection::UnusableKey { .. })
        ));
    }

    #[test]
    fn garbage_is_malformed() {
        let signer = Signer::generate("k1");
        assert!(matches!(
            verify("not.a.jws", &keys(&[&signer])),
            Err(ClaimRejection::Malformed { .. })
        ));
    }

    #[test]
    fn the_issuer_must_match_exactly() {
        let signer = Signer::generate("k1");
        let mut claims = good_claims();
        claims["iss"] = json!("https://idp.example.test/");
        assert_eq!(
            verify(&signer.sign(&claims), &keys(&[&signer])),
            Err(ClaimRejection::Issuer {
                found: "https://idp.example.test/".to_owned()
            }),
            "a trailing slash is a different issuer; the mix-up defence is exact equality"
        );
    }

    #[test]
    fn a_token_for_another_client_is_refused() {
        let signer = Signer::generate("k1");
        let mut claims = good_claims();
        claims["aud"] = json!("another-client");
        assert_eq!(
            verify(&signer.sign(&claims), &keys(&[&signer])),
            Err(ClaimRejection::Audience)
        );
    }

    #[test]
    fn an_authorized_party_that_is_not_us_is_refused() {
        let signer = Signer::generate("k1");
        let mut claims = good_claims();
        claims["aud"] = json!([CLIENT, "another-client"]);
        claims["azp"] = json!("another-client");
        assert_eq!(
            verify(&signer.sign(&claims), &keys(&[&signer])),
            Err(ClaimRejection::AuthorizedParty)
        );
    }

    #[test]
    fn expiry_is_judged_against_the_callers_clock_with_skew() {
        let signer = Signer::generate("k1");
        let mut claims = good_claims();
        claims["exp"] = json!(now().as_second() - CLOCK_SKEW_SECONDS + 1);
        assert!(
            verify(&signer.sign(&claims), &keys(&[&signer])).is_ok(),
            "inside the skew it is still honoured"
        );
        claims["exp"] = json!(now().as_second() - CLOCK_SKEW_SECONDS);
        assert!(matches!(
            verify(&signer.sign(&claims), &keys(&[&signer])),
            Err(ClaimRejection::Expired { .. })
        ));
    }

    #[test]
    fn a_token_from_the_future_is_refused() {
        let signer = Signer::generate("k1");
        let mut claims = good_claims();
        claims["nbf"] = json!(now().as_second() + CLOCK_SKEW_SECONDS + 1);
        assert!(matches!(
            verify(&signer.sign(&claims), &keys(&[&signer])),
            Err(ClaimRejection::NotYetValid { .. })
        ));
        let mut claims = good_claims();
        claims["iat"] = json!(now().as_second() + CLOCK_SKEW_SECONDS + 1);
        assert!(matches!(
            verify(&signer.sign(&claims), &keys(&[&signer])),
            Err(ClaimRejection::NotYetValid { .. })
        ));
    }

    #[test]
    fn a_missing_expiry_is_a_claims_failure() {
        let signer = Signer::generate("k1");
        let mut claims = good_claims();
        claims.as_object_mut().expect("object").remove("exp");
        assert!(matches!(
            verify(&signer.sign(&claims), &keys(&[&signer])),
            Err(ClaimRejection::Claims { .. })
        ));
    }

    #[test]
    fn the_nonce_must_be_the_one_this_ceremony_issued() {
        let signer = Signer::generate("k1");
        let mut claims = good_claims();
        claims["nonce"] = json!("a-nonce-from-another-ceremony");
        assert_eq!(
            verify(&signer.sign(&claims), &keys(&[&signer])),
            Err(ClaimRejection::Nonce)
        );
        claims.as_object_mut().expect("object").remove("nonce");
        assert_eq!(
            verify(&signer.sign(&claims), &keys(&[&signer])),
            Err(ClaimRejection::Nonce),
            "an absent nonce is a replayable token"
        );
    }

    #[test]
    fn the_subject_is_bounded() {
        let signer = Signer::generate("k1");
        let mut claims = good_claims();
        claims["sub"] = json!("");
        assert_eq!(
            verify(&signer.sign(&claims), &keys(&[&signer])),
            Err(ClaimRejection::Subject)
        );
        claims["sub"] = json!("x".repeat(MAX_SUBJECT_LENGTH + 1));
        assert_eq!(
            verify(&signer.sign(&claims), &keys(&[&signer])),
            Err(ClaimRejection::Subject)
        );
    }

    #[test]
    fn an_absent_or_blank_address_is_none_and_unverified_by_default() {
        let signer = Signer::generate("k1");
        let mut claims = good_claims();
        claims["email"] = json!("   ");
        claims
            .as_object_mut()
            .expect("object")
            .remove("email_verified");
        let identity = verify(&signer.sign(&claims), &keys(&[&signer])).expect("verifies");
        assert_eq!(identity.email, None);
        assert!(!identity.email_verified);
    }
}
