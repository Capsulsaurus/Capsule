use kynos::security::SecurityScheme as _;

use super::*;

#[test]
fn the_two_schemes_describe_one_component() {
    // What keeps `components.securitySchemes` at one `bearer` entry and every operation's
    // `security` unchanged: Kynos accepts a second registration under a name exactly when
    // its description is byte-identical to the first.
    assert_eq!(ReadBearer::NAME, AccessToken::NAME);
    assert_eq!(ReadBearer::describe(), AccessToken::describe());
    assert_eq!(ReadBearer::challenge(), AccessToken::challenge());
}

/// The vector design/federation.md's format is pinned to, and `capsule-sdk` pins too.
const TOKEN: &str = "eyJhbGciOiJFZERTQSJ9.e30.c2lnbmF0dXJl";
const SIGNED_AT: i64 = 1_767_225_600;
const NONCE: &str = "AZQ3hW1ae6qB2vX9k0cR0w";
const PATH: &str = "/v1/blob/aaaa";

/// The peer's key, from a fixed seed so the signature is a fixed vector too.
fn peer_key() -> (ring::signature::Ed25519KeyPair, [u8; 32]) {
    use ring::signature::KeyPair as _;
    let pair = ring::signature::Ed25519KeyPair::from_seed_unchecked(&[7; 32]).expect("a seed");
    let public: [u8; 32] = pair.public_key().as_ref().try_into().expect("32 bytes");
    (pair, public)
}

fn claim() -> RequestClaim {
    RequestClaim::new("get", PATH, TOKEN, SIGNED_AT, NONCE)
}

fn signed(claim: &RequestClaim) -> RequestSignature {
    let (pair, _) = peer_key();
    let signature = pair.sign(&claim.signing_bytes().expect("it encodes"));
    RequestSignature {
        signed_at: claim.signed_at,
        nonce: claim.nonce.clone(),
        signature: signature.as_ref().try_into().expect("64 bytes"),
    }
}

#[test]
fn the_signing_bytes_are_stable() {
    // A peer implements the format from the design doc; these bytes are what it must produce.
    // Changing them is a breaking change to every peer, and `capsule-sdk`'s copy of this
    // vector fails alongside this one.
    let claim = claim();
    assert_eq!(claim.method, "GET", "the method is upper-cased");
    assert_eq!(
        claim.token_sha256,
        capsule_core::crypto::hash::hash_bytes(TOKEN.as_bytes()).to_hex()
    );
    assert_eq!(
        hex(&claim.signing_bytes().expect("it encodes")),
        SIGNING_BYTES_HEX
    );
    assert_eq!(signed(&claim).to_header_value(), HEADER);
}

/// The canonical CBOR of [`claim`].
const SIGNING_BYTES_HEX: &str = concat!(
    "a664706174686d2f76312f626c6f622f61616161656e6f6e636576415a51336857316165367142327658396b30",
    "63523077666d6574686f646347455467636f6e74657874781d63617073756c652d66656465726174696f6e2d72",
    "6571756573742d7631697369676e65645f61741a6955b9006c746f6b656e5f73686132353678403134643531",
    "3032326133653435316564396531366231333465393064653939393235653962306465313238666339643364",
    "303262363363356531646537346231",
);

/// The header [`claim`] signed under [`peer_key`] renders as.
const HEADER: &str = concat!(
    "t=1767225600, n=AZQ3hW1ae6qB2vX9k0cR0w, ",
    "s=fIbwDSvO5ktPJD2F-C8c9F2Kx38ZS5s0NrfCejPWJfZuprrjELdhdPxP3dBtgR9X8NqSUIN1Ww0qRK2dyfTgDg",
);

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

#[test]
fn a_signature_verifies_under_the_peers_key_and_no_other() {
    let claim = claim();
    let signature = signed(&claim);
    let (_, public) = peer_key();
    assert_eq!(signature.verify(&claim, &public), Ok(()));

    let other = ring::signature::Ed25519KeyPair::from_seed_unchecked(&[8; 32]).expect("a seed");
    let other: [u8; 32] = ring::signature::KeyPair::public_key(&other)
        .as_ref()
        .try_into()
        .expect("32 bytes");
    assert_eq!(
        signature.verify(&claim, &other),
        Err(RequestSignatureError::NotAuthentic)
    );
}

#[test]
fn every_component_is_covered_by_the_signature() {
    // One mutation per field of the claim, so a field added without one here is a field this
    // case silently stops covering.
    let original = claim();
    let signature = signed(&original);
    let (_, public) = peer_key();
    let mutations: [fn(&mut RequestClaim); 6] = [
        |claim| claim.context = "capsule-federation-report-v1".to_owned(),
        |claim| claim.method = "POST".to_owned(),
        |claim| claim.path = "/v1/blob/bbbb".to_owned(),
        |claim| claim.token_sha256 = "0".repeat(64),
        |claim| claim.signed_at += 1,
        |claim| claim.nonce = "AZQ3hW1ae6qB2vX9k0cR0x".to_owned(),
    ];
    for (index, mutate) in mutations.iter().enumerate() {
        let mut claim = original.clone();
        mutate(&mut claim);
        assert_eq!(
            signature.verify(&claim, &public),
            Err(RequestSignatureError::NotAuthentic),
            "mutation {index} still verified"
        );
    }
}

#[test]
fn a_header_parses_in_any_order_and_round_trips() {
    let signature = signed(&claim());
    let rendered = signature.to_header_value();
    assert_eq!(RequestSignature::parse(&rendered), Ok(signature.clone()));

    let encoded = BASE64URL.encode(signature.signature);
    let reordered = format!("s={encoded},t={SIGNED_AT} ,  n={NONCE}");
    assert_eq!(RequestSignature::parse(&reordered), Ok(signature));
}

#[test]
fn a_malformed_header_is_refused_naming_nothing_it_carried() {
    let encoded = BASE64URL.encode([0; 64]);
    let short = BASE64URL.encode([0; 63]);
    let cases = [
        String::new(),
        format!("t={SIGNED_AT}, n={NONCE}"),
        format!("t={SIGNED_AT}, n={NONCE}, s={encoded}, x=1"),
        format!("t={SIGNED_AT}, t={SIGNED_AT}, n={NONCE}, s={encoded}"),
        format!("t={SIGNED_AT} n={NONCE}, s={encoded}"),
        format!("t=-1, n={NONCE}, s={encoded}"),
        format!("t=+1, n={NONCE}, s={encoded}"),
        format!("t=, n={NONCE}, s={encoded}"),
        format!("t=99999999999999999999, n={NONCE}, s={encoded}"),
        format!("t={SIGNED_AT}, n=short, s={encoded}"),
        format!(
            "t={SIGNED_AT}, n={}, s={encoded}",
            "a".repeat(MAX_NONCE_CHARS + 1)
        ),
        format!("t={SIGNED_AT}, n=AZQ3hW1ae6qB2vX9k0cR0w==, s={encoded}"),
        format!("t={SIGNED_AT}, n=AZQ3hW1ae6qB2vX9k0cR/w, s={encoded}"),
        format!("t={SIGNED_AT}, n={NONCE}, s={short}"),
        format!("t={SIGNED_AT}, n={NONCE}, s={encoded}=="),
        format!("T={SIGNED_AT}, n={NONCE}, s={encoded}"),
    ];
    for case in &cases {
        assert!(
            matches!(
                RequestSignature::parse(case),
                Err(RequestSignatureError::Malformed(_))
            ),
            "{case:?} parsed"
        );
    }
    // The bounds themselves are accepted.
    for nonce in ["a".repeat(MIN_NONCE_CHARS), "a".repeat(MAX_NONCE_CHARS)] {
        assert!(
            RequestSignature::parse(&format!("t=0, n={nonce}, s={encoded}")).is_ok(),
            "a {}-character nonce was refused",
            nonce.len()
        );
    }
}

#[test]
fn freshness_is_symmetric_and_inclusive_of_its_bound() {
    let signature = signed(&claim());
    let at = Timestamp::from_second(SIGNED_AT).expect("in range");
    assert!(signature.is_fresh(at));
    assert!(signature.is_fresh(at + REQUEST_FRESHNESS));
    assert!(signature.is_fresh(at - REQUEST_FRESHNESS));
    assert!(!signature.is_fresh(at + REQUEST_FRESHNESS + SignedDuration::from_secs(1)));
    assert!(!signature.is_fresh(at - REQUEST_FRESHNESS - SignedDuration::from_secs(1)));

    // A `t` no timestamp can hold is never fresh.
    let absurd = RequestSignature {
        signed_at: i64::MAX,
        ..signature
    };
    assert!(!absurd.is_fresh(at));
}

#[test]
fn the_nonce_is_remembered_for_as_long_as_a_signature_can_be_fresh() {
    assert_eq!(REQUEST_NONCE_WINDOW, REQUEST_FRESHNESS * 2);
    assert_eq!(budgets::FEDERATED_REQUEST_NONCE.limit, 1);
    assert_eq!(
        budgets::FEDERATED_REQUEST_NONCE.window,
        REQUEST_NONCE_WINDOW
    );
}

#[test]
fn a_presentation_never_prints_its_bearer() {
    let presentation = ReadPresentation {
        bearer: carrier::bearer(&request(&[("authorization", "Bearer secret-token")]))
            .expect("it reads")
            .expect("a bearer"),
        signature: Some("t=1".to_owned()),
        method: "GET".to_owned(),
        path: PATH.to_owned(),
    };
    let printed = format!("{presentation:?}");
    assert!(!printed.contains("secret-token"), "{printed}");
}

/// A request head carrying `headers`.
fn request(headers: &[(&'static str, &'static str)]) -> Parts {
    let (mut parts, _) = kynos::http::Request::new(kynos::http::body::Body::empty()).into_parts();
    parts.uri = format!("{PATH}?ignored=1").parse().expect("a target");
    for (name, value) in headers {
        parts.headers.append(
            kynos::http::HeaderName::from_static(name),
            kynos::http::HeaderValue::from_static(value),
        );
    }
    parts
}

#[test]
fn the_carrier_reads_the_bearer_the_signature_and_the_path_without_its_query() {
    let presented = ReadBearer::present(&request(&[
        ("authorization", "Bearer token"),
        (REQUEST_SIGNATURE_HEADER, "t=1, n=x, s=y"),
    ]))
    .expect("it reads")
    .expect("a credential");
    assert_eq!(presented.bearer.as_str(), "token");
    assert_eq!(presented.signature.as_deref(), Some("t=1, n=x, s=y"));
    assert_eq!(presented.method, "GET");
    assert_eq!(presented.path, PATH);

    // No bearer is anonymity, whatever else the request carries.
    assert!(
        ReadBearer::present(&request(&[(REQUEST_SIGNATURE_HEADER, "t=1")]))
            .expect("it reads")
            .is_none()
    );

    // No signature is absent, and two are one that fails parsing rather than a choice.
    let unsigned = ReadBearer::present(&request(&[("authorization", "Bearer token")]))
        .expect("it reads")
        .expect("a credential");
    assert_eq!(unsigned.signature, None);
    let doubled = ReadBearer::present(&request(&[
        ("authorization", "Bearer token"),
        (REQUEST_SIGNATURE_HEADER, "t=1"),
        (REQUEST_SIGNATURE_HEADER, "t=2"),
    ]))
    .expect("it reads")
    .expect("a credential");
    assert_eq!(doubled.signature.as_deref(), Some(""));
}
