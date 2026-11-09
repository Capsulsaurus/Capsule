use super::*;

fn snapshot(revoked: &[&str], max_staleness: Duration) -> RevocationSnapshot {
    RevocationSnapshot {
        revoked: revoked.iter().map(|jti| (*jti).to_owned()).collect(),
        max_staleness,
        fetched_at: Instant::now(),
    }
}

#[test]
fn a_snapshot_refuses_the_jtis_it_carries_and_no_others() {
    let held = snapshot(&["one", "two"], Duration::from_mins(15));
    assert!(held.refuses("one"));
    assert!(held.refuses("two"));
    assert!(!held.refuses("three"));
}

#[test]
fn a_snapshot_is_fresh_only_inside_the_issuers_own_bound() {
    // The bound is the issuer's, carried on the list itself, and it is measured on the
    // peer's own clock — the party being checked does not get to say how old its list is.
    let held = snapshot(&[], Duration::from_mins(15));
    assert!(held.is_fresh(held.fetched_at));
    assert!(held.is_fresh(held.fetched_at + Duration::from_mins(15)));
    assert!(!held.is_fresh(held.fetched_at + Duration::from_mins(15) + Duration::from_secs(1)));

    // A bound of zero is a list that is stale the instant after it is read, which is what a
    // server publishing `max_staleness_seconds: 0` is asking for.
    let strict = snapshot(&[], Duration::ZERO);
    assert!(!strict.is_fresh(strict.fetched_at + Duration::from_millis(1)));
}

/// A puller's key from a fixed seed, the way a peer server would hold its operational key.
struct SeedSigner(ed25519_dalek::SigningKey);

impl RequestSigner for SeedSigner {
    fn sign(&self, message: &[u8]) -> [u8; 64] {
        use ed25519_dalek::Signer as _;
        self.0.sign(message).to_bytes()
    }
}

fn signer(seed: u8) -> SeedSigner {
    SeedSigner(ed25519_dalek::SigningKey::from_bytes(&[seed; 32]))
}

#[test]
fn the_signing_bytes_match_the_servers_vector() {
    // The same inputs, the same canonical CBOR and the same header the server's
    // `federation::scheme::tests::the_signing_bytes_are_stable` pins: the two implementations of
    // design/federation.md's format cannot drift apart without one of them failing.
    const TOKEN: &str = "eyJhbGciOiJFZERTQSJ9.e30.c2lnbmF0dXJl";
    const NONCE: &str = "AZQ3hW1ae6qB2vX9k0cR0w";
    let claim = RequestClaim {
        context: REQUEST_CONTEXT,
        method: "GET",
        path: "/v1/blob/aaaa",
        token_sha256: capsule_core::crypto::hash::hash_bytes(TOKEN.as_bytes()).to_hex(),
        signed_at: 1_767_225_600,
        nonce: NONCE,
    };
    let bytes = capsule_core::cbor::to_canonical_vec(&claim).expect("it encodes");
    assert_eq!(
        hex::encode(bytes),
        concat!(
            "a664706174686d2f76312f626c6f622f61616161656e6f6e636576415a51336857316165367142327658",
            "396b3063523077666d6574686f646347455467636f6e74657874781d63617073756c652d6665646572",
            "6174696f6e2d726571756573742d7631697369676e65645f61741a6955b9006c746f6b656e5f736861",
            "32353678403134643531303232613365343531656439653136623133346539306465393939323565",
            "3962306465313238666339643364303262363363356531646537346231",
        )
    );
    assert_eq!(
        signature_header(
            &signer(7),
            "get",
            "/v1/blob/aaaa",
            TOKEN,
            1_767_225_600,
            NONCE
        ),
        Ok(concat!(
            "t=1767225600, n=AZQ3hW1ae6qB2vX9k0cR0w, ",
            "s=fIbwDSvO5ktPJD2F-C8c9F2Kx38ZS5s0NrfCejPWJfZuprrjELdhdPxP3dBtgR9X8NqSUIN1Ww0qRK2dyfTgDg",
        )
        .to_owned())
    );
}

#[test]
fn every_signature_carries_a_fresh_nonce_and_the_current_time() {
    let first = request_signature(&signer(7), "GET", "/v1/sync", "token").expect("it signs");
    let second = request_signature(&signer(7), "GET", "/v1/sync", "token").expect("it signs");
    let member = |header: &str, key: &str| {
        header
            .split(", ")
            .find_map(|member| member.strip_prefix(key))
            .expect("the member is present")
            .to_owned()
    };
    assert_ne!(member(&first, "n="), member(&second, "n="));
    assert!(member(&first, "n=").len() >= 22, "at least sixteen bytes");
    let signed_at: i64 = member(&first, "t=").parse().expect("Unix seconds");
    assert!((signed_at - jiff::Timestamp::now().as_second()).abs() <= 5);
}

#[test]
fn the_debug_rendering_never_carries_the_token() {
    let pull = FederationPull::new(
        "https://home.test/v1",
        "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e60",
        "a.very.secret.token",
        "01937b7c-0000-7000-8000-0000000000aa",
        Arc::new(signer(7)),
    )
    .expect("the base url is a url");
    let rendered = format!("{pull:?}");
    assert!(!rendered.contains("a.very.secret.token"), "{rendered}");
    assert!(
        rendered.contains("01937b7c-0000-7000-8000-0000000000aa"),
        "{rendered}"
    );
}

/// One of the generated refresh refusal's variant constructors.
type Wrap = fn(Box<rest::types::CodedProblem>) -> rest::RefreshCapabilityError;

/// A refresh refusal as the generated client hands it over.
fn refused(
    status: reqwest::StatusCode,
    wrap: Wrap,
    code: &str,
) -> rest::Error<rest::RefreshCapabilityError> {
    let problem: rest::types::CodedProblem = serde_json::from_value(serde_json::json!({
        "type": "about:blank",
        "status": status.as_u16(),
        "detail": "refused",
        "code": code,
    }))
    .expect("a coded problem");
    rest::Error::Api(rest::ResponseValue::new(
        status,
        reqwest::header::HeaderMap::new(),
        wrap(Box::new(problem)),
    ))
}

#[test]
fn every_refresh_refusal_keeps_the_servers_code() {
    // #529: the `409` a peer acts on — the member it was granted for has left the roster, so the
    // next move is to ask the owner — reached the caller with its code dropped.
    let cases: [(reqwest::StatusCode, Wrap, &str); 4] = [
        (
            reqwest::StatusCode::CONFLICT,
            rest::RefreshCapabilityError::Status409,
            "error.federation.member_not_on_roster",
        ),
        (
            reqwest::StatusCode::NOT_FOUND,
            rest::RefreshCapabilityError::Status404,
            "error.federation.not_configured",
        ),
        (
            reqwest::StatusCode::FORBIDDEN,
            rest::RefreshCapabilityError::Status403,
            "error.federation.capability_revoked",
        ),
        (
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            rest::RefreshCapabilityError::Status429,
            "error.federation.rate_budget_exceeded",
        ),
    ];
    for (status, wrap, code) in cases {
        match refresh_refusal(refused(status, wrap, code)) {
            FederationError::Refused { code: got, detail } => {
                assert_eq!(got, code, "{status}");
                assert_eq!(detail, "refused", "{status}");
            }
            other => panic!("{status} mapped to {other:?}"),
        }
    }
}
