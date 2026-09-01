//! The request body-size limit, and the two ways a body can be too large (slice `S-C33`).
//!
//! The gap this closes: Kynos 0.1.0 renders `#[schema(min_length / max_length)]` into the
//! emitted document but does not enforce it, so the auth surface removed those constraints
//! rather than publish validation the server does not perform. A body-size cap is the control
//! that belongs at this layer, and unlike a schema annotation it is *enforced* — which is what
//! these tests exist to state rather than assume.
//!
//! Two paths through [`kynos::middleware::limits::BodySize`] and both are covered here, because
//! only one of them can be reached by a well-behaved client and the other is the one an attacker
//! uses: a request that declares its length is refused from the head without reading the body,
//! and a request that declares none is counted frame by frame and abandoned when the count
//! passes the cap.
//!
//! Run with `cargo nextest run -p capsule-server`.

mod support;

use capsule_server::limits::MAX_REQUEST_BODY_BYTES;
use kynos::http::StatusCode;
use kynos::test::TestClient;
use support::Fixture;

fn client() -> TestClient<capsule_server::App> {
    TestClient::new(capsule_server::service(Fixture::working_app()).expect("router builds"))
}

/// A declared length over the cap is refused before the body is read.
#[tokio::test]
async fn an_over_declared_content_length_is_refused_without_reading_the_body() {
    let client = client();

    client
        .post("/v1/auth/login")
        .header("content-length", &(MAX_REQUEST_BODY_BYTES + 1).to_string())
        // A body nowhere near the declared length: the refusal is decided from the head, so the
        // bytes never matter. This is what keeps an oversized upload from costing bandwidth.
        .body("application/json", "{}")
        .send()
        .await
        .assert_status(StatusCode::PAYLOAD_TOO_LARGE);

    client.assert_conformance();
}

/// A body that declares no length is counted, and abandoned once it passes the cap.
///
/// The path a chunked request takes, and the only bound there is on one — a client that declares
/// nothing cannot be refused from the head.
#[tokio::test]
async fn an_oversized_body_with_no_declared_length_is_refused_by_the_running_count() {
    let client = client();
    let body = vec![b'x'; usize::try_from(MAX_REQUEST_BODY_BYTES).expect("the cap fits") + 1];

    client
        .post("/v1/auth/login")
        .body("application/json", body)
        .send()
        .await
        .assert_status(StatusCode::PAYLOAD_TOO_LARGE);

    client.assert_conformance();
}

/// The limit refuses only what is over it.
///
/// A cap that also refused legitimate traffic would be a worse failure than none, and a cap that
/// silently truncated would be worse still — so the body must arrive *whole* at the extractor,
/// which is what the `400` proves: the handler was reached and rejected the JSON on its merits.
#[tokio::test]
async fn a_large_body_within_the_limit_reaches_the_handler_whole() {
    let client = client();
    let filler = "a".repeat(1024 * 1024);
    let body = format!("{{ \"email\": \"{filler}\" ");

    assert!(
        body.len() as u64 <= MAX_REQUEST_BODY_BYTES,
        "the fixture must be inside the cap for this test to mean anything"
    );

    client
        .post("/v1/auth/login")
        .body("application/json", body)
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    client.assert_conformance();
}

/// The cap covers operations that take no body at all.
///
/// It is mounted on the router rather than on the operations that happen to accept a body today,
/// so a body sent where none is expected is refused rather than read into memory and discarded.
#[tokio::test]
async fn the_cap_covers_an_operation_that_takes_no_body() {
    let client = client();

    client
        .get("/v1/version")
        .header("content-length", &(MAX_REQUEST_BODY_BYTES + 1).to_string())
        .body("application/json", "{}")
        .send()
        .await
        .assert_status(StatusCode::PAYLOAD_TOO_LARGE);

    client.assert_conformance();
}

/// Every operation the server serves declares the `413` the limit can produce.
///
/// The half of `S-C28` that cannot be caught by producing responses: a status the server can
/// send and the document does not declare is a status a generated client has no case for. Here
/// the declaration comes from the interceptor's own type, so this asserts the wiring rather than
/// a hand-maintained list.
#[test]
fn every_operation_declares_the_limit_it_is_covered_by() {
    let document = capsule_server::openapi().expect("router describes itself");
    let json = document.to_json().expect("document serializes");
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("document is JSON");

    let paths = parsed
        .get("paths")
        .and_then(serde_json::Value::as_object)
        .expect("the document declares paths");
    assert!(!paths.is_empty(), "there is a surface to check");

    for (path, item) in paths {
        let operations = item.as_object().expect("a path item is an object");
        for (method, operation) in operations {
            assert!(
                operation.pointer("/responses/413").is_some(),
                "{method} {path} is covered by the body-size limit, so it must declare 413"
            );
        }
    }
}
