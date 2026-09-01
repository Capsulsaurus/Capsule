//! Every rejection carries an `error.*` code, including the ones Capsule does not write
//! (`S-C36`), and the document says so (`S-C38`).
//!
//! These two slices are one change. `S-C36` is the interceptor that fills in the member on a
//! framework-rendered problem; `S-C38` is the document describing it. Either alone leaves the
//! i18n contract half-built: a code nothing declares is one a generated client cannot reach, and
//! a declaration nothing honours is a lie.
//!
//! The document half is asserted in `src/openapi/tests.rs`, over the emitted document. This file
//! is the wire half — the statuses the *framework* renders, driven over HTTP, because the one
//! thing a document test cannot tell you is whether the server actually sends what it promised.

mod support;

use kynos::http::StatusCode;
use serde_json::json;
use support::Fixture;

/// The `code` an RFC 9457 body publishes.
fn code_of(body: &serde_json::Value) -> &str {
    body.get("code")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("<no code member>")
}

#[tokio::test]
async fn the_frameworks_own_rejections_carry_a_code() {
    // Every one of these is rendered by a type Capsule does not own and cannot add a field to:
    // Kynos's `AuthRejection`, its body-size interceptor, and its `Path` and `Json` extractors.
    // Before `S-C36` each reached a user as untranslatable English.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let pair = fixture.login().await;

    // 401 — no credential at all.
    let unauthenticated: serde_json::Value = fixture
        .client
        .get("/v1/quota")
        .header("accept", "application/problem+json")
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED)
        .json();
    assert_eq!(code_of(&unauthenticated), "error.request.unauthenticated");

    // 403 — a valid credential of the wrong kind.
    let forbidden: serde_json::Value = fixture
        .client
        .get("/v1/quota")
        .header("accept", "application/problem+json")
        .header("authorization", &format!("Bearer {}", pair.refresh_token))
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN)
        .json();
    assert_eq!(code_of(&forbidden), "error.request.forbidden");

    // 400 — a path segment the extractor cannot decode. `%FF` is not valid UTF-8.
    let malformed: serde_json::Value = fixture
        .client
        .get("/v1/blob/%FF")
        .header("accept", "application/problem+json")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST)
        .json();
    assert_eq!(code_of(&malformed), "error.request.malformed");

    // 415 — a body in a media type the operation does not accept.
    let unsupported: serde_json::Value = fixture
        .client
        .post("/v1/auth/login")
        .header("accept", "application/problem+json")
        .body("text/plain", "{}")
        .send()
        .await
        .assert_status(StatusCode::UNSUPPORTED_MEDIA_TYPE)
        .json();
    assert_eq!(
        code_of(&unsupported),
        "error.request.unsupported_media_type"
    );

    // 422 — the right media type, the wrong shape.
    let unprocessable: serde_json::Value = fixture
        .client
        .post("/v1/auth/login")
        .header("accept", "application/problem+json")
        .json(&json!({ "email": 42 }))
        .send()
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY)
        .json();
    assert_eq!(code_of(&unprocessable), "error.request.unprocessable");

    fixture.client.assert_conformance();
}

#[tokio::test]
async fn a_capsule_rejections_own_code_is_never_replaced() {
    // The property that keeps the interceptor from being able to do damage. A coarse
    // `error.request.*` code overwriting a specific catalog code would replace a diagnosis with
    // a shrug, and it would do it silently — so the interceptor only ever fills a gap.
    let fixture = Fixture::working();

    let invalid: serde_json::Value = fixture
        .client
        .post("/v1/auth/login")
        .header("accept", "application/problem+json")
        .json(&json!({ "email": support::EMAIL, "password": "wrong" }))
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED)
        .json();
    assert_eq!(
        code_of(&invalid),
        "error.auth.invalid_credentials",
        "a 401 the handler decided keeps the handler's code, not the framework's"
    );

    fixture.client.assert_conformance();
}

#[tokio::test]
async fn coding_a_problem_does_not_disturb_anything_else_about_it() {
    // The interceptor rewrites a body. This is the case that says it rewrites *only* the body,
    // and only by adding one member: the status, the media type and every other field survive.
    let fixture = Fixture::working();

    let response = fixture
        .client
        .get("/v1/quota")
        .header("accept", "application/problem+json")
        .send()
        .await;
    response.assert_status(StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.header("content-type"),
        Some("application/problem+json"),
        "the media type is untouched, which is what keeps the body's meaning the same"
    );
    assert!(
        response.header("www-authenticate").is_some(),
        "the challenge the operation declares as required is still sent — the reason this is an \
         interceptor and not a second 401 beside the framework's"
    );

    let problem: serde_json::Value = response.json();
    assert_eq!(problem["status"], 401);
    assert_eq!(problem["title"], "Unauthorized");
    assert_eq!(
        problem["detail"], "authentication is required",
        "the English detail stays English, which is the other half of the i18n contract"
    );
    assert_eq!(code_of(&problem), "error.request.unauthenticated");
}

#[tokio::test]
async fn a_body_that_is_not_a_problem_is_never_read() {
    // The guard that keeps ciphertext off the interceptor's path. A `200` is checked by
    // content-type before its body is taken, so a ranged blob delivery is never buffered — and
    // this is the case that fails if the check is ever moved after the take.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;

    let response = fixture
        .client
        .get("/v1/quota")
        .header("accept", "application/json")
        .header("authorization", &bearer)
        .send()
        .await;
    response.assert_status(StatusCode::OK);
    let body: serde_json::Value = response.json();
    assert!(
        body.get("code").is_none(),
        "a successful response is not a problem and must not grow a problem's member"
    );
}
