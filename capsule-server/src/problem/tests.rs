use super::*;

/// Every status the mapping names is a real catalog key, and the fallback is too.
///
/// Cheap, and it is the check that stops a typo shipping: `code_for` returns a `&'static
/// str` from `capsule_i18n::error_codes`, so a wrong *name* is a compile error, but a
/// mapping that quietly falls through to `UNMAPPED` for a status the surface renders is not.
#[test]
fn every_status_the_surface_renders_has_its_own_code() {
    for status in [400_u16, 401, 403, 404, 405, 406, 413, 415, 416, 422, 501] {
        assert_ne!(
            code_for(status),
            UNMAPPED,
            "status {status} is declared somewhere on this surface and has no code of its own"
        );
    }
    assert_eq!(code_for(418), UNMAPPED, "and anything else is the fallback");
}

/// A framework problem gains a code.
#[test]
fn a_codeless_problem_is_coded() {
    let body = Bytes::from_static(br#"{"type":"about:blank","title":"Unauthorized","status":401}"#);
    let coded: serde_json::Value = serde_json::from_slice(&coded(401, body)).expect("json");
    assert_eq!(coded["code"], "error.request.unauthenticated");
    assert_eq!(coded["title"], "Unauthorized", "and nothing else moved");
}

/// A Capsule problem is passed through untouched, byte for byte.
///
/// The property that keeps this interceptor from being able to reach a rejection this crate
/// owns: a coarse framework code overwriting a specific catalog code would replace a
/// diagnosis with a shrug, and it would do it silently.
#[test]
fn a_problem_that_already_carries_a_code_is_untouched() {
    let body = Bytes::from_static(
        br#"{"type":"about:blank","status":401,"code":"error.auth.session_expired"}"#,
    );
    assert_eq!(
        coded(401, body.clone()),
        body,
        "an existing code is authoritative, and so is the exact encoding around it"
    );
}

/// Anything that is not a JSON object is left alone rather than replaced.
#[test]
fn a_body_this_module_cannot_parse_is_left_alone() {
    for body in [
        Bytes::from_static(b"not json at all"),
        Bytes::from_static(b"[]"),
        Bytes::from_static(b""),
    ] {
        assert_eq!(coded(500, body.clone()), body);
    }
}

/// An unmapped status still gets a code, because a codeless problem is the defect.
#[test]
fn an_unmapped_status_still_gets_a_code() {
    let body = Bytes::from_static(br#"{"status":418}"#);
    let coded: serde_json::Value = serde_json::from_slice(&coded(418, body)).expect("json");
    assert_eq!(coded["code"], UNMAPPED);
}

/// The retry instant is read from the member every throttled answer carries, and from
/// nothing else.
#[test]
fn the_retry_instant_is_the_problems_own_member() {
    assert_eq!(
        retry_instant(
            br#"{"status":429,"code":"error.upload.rate_limited","retry_after":1700000060}"#
        ),
        Some(1_700_000_060)
    );
    for body in [
        &br#"{"status":503,"code":"error.request.failed"}"#[..],
        br#"{"retry_after":"soon"}"#,
        br#"{"retry_after":-5}"#,
        b"not json",
    ] {
        assert_eq!(retry_instant(body), None);
    }
}

/// The media-type test is the guard that keeps ciphertext off this path.
#[test]
fn only_a_problem_body_is_read() {
    let mut headers = http::HeaderMap::new();
    assert!(!is_problem(&headers), "no content type is not a problem");
    for (value, expected) in [
        ("application/problem+json", true),
        ("application/problem+json; charset=utf-8", true),
        ("APPLICATION/PROBLEM+JSON", true),
        ("application/json", false),
        ("application/octet-stream", false),
    ] {
        headers.insert(
            header::CONTENT_TYPE,
            http::HeaderValue::from_str(value).expect("a header value"),
        );
        assert_eq!(is_problem(&headers), expected, "for {value}");
    }
}
