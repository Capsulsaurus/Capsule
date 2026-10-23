use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::jwk::{
    CommonParameters, EllipticCurve, Jwk, KeyAlgorithm, OctetKeyPairParameters, OctetKeyPairType,
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
        let der = ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
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
        nonce: Some(NONCE.to_owned()),
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
    set.keys[0].algorithm = AlgorithmParameters::OctetKey(jsonwebtoken::jwk::OctetKeyParameters {
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
fn several_audiences_without_an_authorized_party_are_refused() {
    // Core §3.1.3.7 rule 4: a multi-audience token names who it was issued to, or it is
    // a token for somebody else that merely lists us.
    let signer = Signer::generate("k1");
    let mut claims = good_claims();
    claims["aud"] = json!([CLIENT, "another-client"]);
    assert_eq!(
        verify(&signer.sign(&claims), &keys(&[&signer])),
        Err(ClaimRejection::AuthorizedParty)
    );
}

#[test]
fn a_critical_header_extension_is_refused() {
    let signer = Signer::generate("k1");
    let mut header = Header::new(Algorithm::EdDSA);
    header.kid = Some("k1".to_owned());
    header.crit = Some(vec!["b64".to_owned()]);
    let token = jsonwebtoken::encode(&header, &good_claims(), &signer.encoding).expect("signs");
    assert_eq!(
        verify(&token, &keys(&[&signer])),
        Err(ClaimRejection::CriticalHeader)
    );
    // An empty `crit` is a malformed-but-harmless header and is not what the rule is about.
    header.crit = Some(Vec::new());
    let token = jsonwebtoken::encode(&header, &good_claims(), &signer.encoding).expect("signs");
    assert!(verify(&token, &keys(&[&signer])).is_ok());
}

#[test]
fn provider_supplied_strings_are_bounded_before_they_reach_a_log() {
    let signer = Signer::generate("k1");
    let mut claims = good_claims();
    let long = format!("https://{}.test", "x".repeat(2_000));
    claims["iss"] = json!(long);
    match verify(&signer.sign(&claims), &keys(&[&signer])) {
        Err(ClaimRejection::Issuer { found }) => {
            assert!(
                found.len() <= MAX_QUOTED_BYTES + '…'.len_utf8(),
                "{}",
                found.len()
            );
            assert!(found.ends_with('…'));
        }
        other => panic!("expected an issuer refusal, got {other:?}"),
    }
    let mut header = Header::new(Algorithm::EdDSA);
    header.kid = Some("k".repeat(2_000));
    let token = jsonwebtoken::encode(&header, &good_claims(), &signer.encoding).expect("signs");
    match verify(&token, &keys(&[&signer])) {
        Err(ClaimRejection::UnknownKey { kid: Some(kid) }) => {
            assert!(kid.len() <= MAX_QUOTED_BYTES + '…'.len_utf8());
        }
        other => panic!("expected an unknown key, got {other:?}"),
    }
    assert_eq!(bounded("short"), "short");
    // Cut on a character boundary: a multi-byte character straddling the limit is dropped
    // whole rather than split.
    let cut = bounded(&"é".repeat(200));
    assert!(cut.len() <= MAX_QUOTED_BYTES + '…'.len_utf8());
    assert!(std::str::from_utf8(cut.as_bytes()).is_ok());
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
fn a_grant_that_sends_no_nonce_admits_only_a_token_without_one() {
    let signer = Signer::generate("k1");
    let keys = keys(&[&signer]);
    let device = Expectations {
        nonce: None,
        ..expectations()
    };
    let mut claims = good_claims();
    assert_eq!(
        verify_id_token(&signer.sign(&claims), &keys, &device, now()),
        Err(ClaimRejection::Nonce),
        "a token carrying a nonce was minted for an authorization request, not this grant"
    );
    claims.as_object_mut().expect("object").remove("nonce");
    let identity = verify_id_token(&signer.sign(&claims), &keys, &device, now())
        .expect("a token with no nonce answers a grant that sent none");
    assert_eq!(identity.issuer, ISSUER);
    // Every other check still runs.
    claims["aud"] = json!("someone-else");
    assert_eq!(
        verify_id_token(&signer.sign(&claims), &keys, &device, now()),
        Err(ClaimRejection::Audience)
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

#[test]
fn a_string_email_verified_is_read_and_anything_else_is_unverified() {
    let signer = Signer::generate("k1");
    for (claim, expected) in [
        (json!("true"), true),
        (json!("TRUE"), true),
        (json!("false"), false),
        (json!("yes"), false),
        (json!(1), false),
        (serde_json::Value::Null, false),
    ] {
        let mut claims = good_claims();
        claims["email_verified"] = claim.clone();
        let identity = verify(&signer.sign(&claims), &keys(&[&signer]))
            .unwrap_or_else(|error| panic!("{claim} must not fail sign-in: {error:?}"));
        assert_eq!(
            identity.email_verified, expected,
            "email_verified = {claim}"
        );
    }
}
