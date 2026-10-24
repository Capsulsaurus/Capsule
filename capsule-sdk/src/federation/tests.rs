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

#[test]
fn the_debug_rendering_never_carries_the_token() {
    let pull = FederationPull::new(
        "https://home.test/v1",
        "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e60",
        "a.very.secret.token",
        "01937b7c-0000-7000-8000-0000000000aa",
    )
    .expect("the base url is a url");
    let rendered = format!("{pull:?}");
    assert!(!rendered.contains("a.very.secret.token"), "{rendered}");
    assert!(
        rendered.contains("01937b7c-0000-7000-8000-0000000000aa"),
        "{rendered}"
    );
}

/// A refresh refusal as the generated client hands it over.
fn refused(
    status: reqwest::StatusCode,
    wrap: fn(Box<rest::types::CodedProblem>) -> rest::RefreshCapabilityError,
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
    let cases: [(
        reqwest::StatusCode,
        fn(Box<rest::types::CodedProblem>) -> rest::RefreshCapabilityError,
        &str,
    ); 4] = [
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
