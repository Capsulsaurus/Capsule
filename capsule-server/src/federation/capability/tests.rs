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
        BlobRole::DerivativeManifest,
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
