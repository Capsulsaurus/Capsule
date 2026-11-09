//! The request body-size limit, and the two ways a body can be too large.
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

use std::time::{Duration, Instant};

use capsule_server::limits::{
    MAX_BOUNDED_BODY_BYTES, MAX_FEDERATION_BODY_BYTES, MAX_REQUEST_BODY_BYTES,
};
use capsule_server::upload::chunk::MAX_CHUNK_BYTES;
use kynos::http::StatusCode;
use kynos::test::TestClient;
use support::{Fixture, PROTOCOL_VERSION};
use tokio::io::AsyncWriteExt as _;
use tokio::net::TcpStream;

fn client() -> TestClient<capsule_server::App> {
    TestClient::new(capsule_server::service(Fixture::working_app()).expect("router builds"))
}

/// A declared length over the backstop is refused before the body is read.
///
/// `create_drop` keeps the backstop (#576), so it is where the backstop itself is exercised.
#[tokio::test]
async fn an_over_declared_content_length_is_refused_without_reading_the_body() {
    let client = client();

    client
        .post("/d/anything")
        .header("content-length", &(MAX_REQUEST_BODY_BYTES + 1).to_string())
        // A body nowhere near the declared length: the refusal is decided from the head, so the
        // bytes never matter. This is what keeps an oversized upload from costing bandwidth.
        .body("application/json", "{}")
        .send()
        .await
        .assert_status(StatusCode::PAYLOAD_TOO_LARGE);

    client.assert_conformance();
}

/// A body that declares no length is counted, and abandoned once it passes the backstop.
///
/// The path a chunked request takes, and the only bound there is on one — a client that declares
/// nothing cannot be refused from the head.
#[tokio::test]
async fn an_oversized_body_with_no_declared_length_is_refused_by_the_running_count() {
    let client = client();
    let body = vec![b'x'; usize::try_from(MAX_REQUEST_BODY_BYTES).expect("the cap fits") + 1];

    client
        .post("/d/anything")
        .body("application/json", body)
        .send()
        .await
        .assert_status(StatusCode::PAYLOAD_TOO_LARGE);

    client.assert_conformance();
}

/// The backstop refuses only what is over it.
///
/// A cap that also refused legitimate traffic would be a worse failure than none, and a cap that
/// silently truncated would be worse still — so the body must arrive *whole* at the extractor,
/// which is what the `400` proves: the handler was reached and rejected the JSON on its merits.
/// The body is past the bounded cap, so this also proves `create_drop` is not under it.
#[tokio::test]
async fn a_large_body_within_the_limit_reaches_the_handler_whole() {
    let client = client();
    let filler = "a".repeat(usize::try_from(MAX_BOUNDED_BODY_BYTES).expect("the cap fits"));
    let body = format!("{{ \"kem_ct\": \"{filler}\" ");

    assert!(
        body.len() as u64 <= MAX_REQUEST_BODY_BYTES,
        "the fixture must be inside the backstop for this test to mean anything"
    );

    client
        .post("/d/anything")
        .body("application/json", body)
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    client.assert_conformance();
}

/// A bounded write's declared length over the bounded cap is refused from the head (#576).
///
/// `login` is an unauthenticated write: under the backstop it could be handed 32 MiB to parse.
#[tokio::test]
async fn a_bounded_write_over_the_bounded_cap_is_refused() {
    let client = client();

    client
        .post("/v1/auth/login")
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .header("content-length", &(MAX_BOUNDED_BODY_BYTES + 1).to_string())
        .body("application/json", "{}")
        .send()
        .await
        .assert_status(StatusCode::PAYLOAD_TOO_LARGE);

    client.assert_conformance();
}

/// The bounded cap holds for a body that declares no length, by the running count.
#[tokio::test]
async fn an_undeclared_bounded_body_over_its_cap_is_refused_by_the_running_count() {
    let client = client();
    let body = vec![b'x'; usize::try_from(MAX_BOUNDED_BODY_BYTES).expect("the cap fits") + 1];

    client
        .post("/v1/auth/login")
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .body("application/json", body)
        .send()
        .await
        .assert_status(StatusCode::PAYLOAD_TOO_LARGE);

    client.assert_conformance();
}

/// A bounded write's body at exactly the bounded cap reaches the handler whole.
///
/// The `400` is the extractor refusing the bytes on their merits, which it can only do once the
/// cap has let them through.
#[tokio::test]
async fn a_bounded_body_at_its_cap_reaches_the_handler() {
    let client = client();
    let body = vec![b'x'; usize::try_from(MAX_BOUNDED_BODY_BYTES).expect("the cap fits")];

    client
        .post("/v1/auth/login")
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .body("application/json", body)
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    client.assert_conformance();
}

/// The exempt group's non-drop operations carry the bounded cap too.
///
/// `refresh_token` is exempt from the gate and takes a token, never a document.
#[tokio::test]
async fn an_exempt_write_over_the_bounded_cap_is_refused() {
    let client = client();

    client
        .post("/v1/auth/refresh")
        .header("content-length", &(MAX_BOUNDED_BODY_BYTES + 1).to_string())
        .body("application/json", "{}")
        .send()
        .await
        .assert_status(StatusCode::PAYLOAD_TOO_LARGE);

    client.assert_conformance();
}

/// The two writes the protocol leaves unbounded keep the backstop (#576).
///
/// A `metadata-update` op carries the whole encrypted sidecar inline and a whole-album share
/// enumerates every blob it serves; neither has a size the protocol bounds. Neither request
/// carries a bearer, so the proof is that each is answered by the route's own `401` rather than
/// the body-size `413` a bounded cap would give.
#[tokio::test]
async fn the_unbounded_writes_keep_the_backstop() {
    let client = client();
    let body = vec![b'x'; usize::try_from(MAX_BOUNDED_BODY_BYTES).expect("the cap fits") + 1];

    for path in ["/v1/albums/anything/ops", "/v1/shares"] {
        client
            .post(path)
            .header("x-capsule-protocol", PROTOCOL_VERSION)
            // Declared, so a bounded cap would refuse it from the head whether or not the route
            // ever reads the body.
            .header("content-length", &body.len().to_string())
            .body("application/json", body.clone())
            .send()
            .await
            .assert_status(StatusCode::UNAUTHORIZED);
    }

    client.assert_conformance();
}

/// The cap covers operations that take no body at all.
///
/// Every group carries one, the exempt thirteen's included, rather than only the operations that
/// happen to accept a body today, so a body sent where none is expected is refused rather than
/// read into memory and discarded. `GET /v1/version` carries the bounded cap.
#[tokio::test]
async fn the_cap_covers_an_operation_that_takes_no_body() {
    let client = client();

    client
        .get("/v1/version")
        .header("content-length", &(MAX_BOUNDED_BODY_BYTES + 1).to_string())
        .body("application/json", "{}")
        .send()
        .await
        .assert_status(StatusCode::PAYLOAD_TOO_LARGE);

    client.assert_conformance();
}

/// Every operation the server serves declares the `413` the limit can produce.
///
/// The half of conformance that cannot be caught by producing responses: a status the server can
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

/// The federation group carries its own, tighter cap (#478), refused from the declared length.
///
/// Far below the backstop: a body the auth surface would read whole is refused here before a
/// byte of it is read, and before the protocol gate looks at anything.
#[tokio::test]
async fn a_federation_report_over_its_own_cap_is_refused() {
    let client = client();

    client
        .post("/v1/federation/reports")
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .header(
            "content-length",
            &(MAX_FEDERATION_BODY_BYTES + 1).to_string(),
        )
        .body("application/json", "{}")
        .send()
        .await
        .assert_status(StatusCode::PAYLOAD_TOO_LARGE);

    client.assert_conformance();
}

/// The federation cap holds for a body that declares no length, by the running count.
#[tokio::test]
async fn an_undeclared_federation_body_over_its_cap_is_refused_by_the_running_count() {
    let client = client();
    let body = vec![b'x'; usize::try_from(MAX_FEDERATION_BODY_BYTES).expect("the cap fits") + 1];

    client
        .post("/v1/federation/reports")
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .body("application/json", body)
        .send()
        .await
        .assert_status(StatusCode::PAYLOAD_TOO_LARGE);

    client.assert_conformance();
}

/// A report at exactly the federation cap reaches the handler whole.
///
/// The `400` is the extractor refusing the bytes on their merits, which it can only do once the
/// cap has let them through.
#[tokio::test]
async fn a_federation_body_at_its_cap_reaches_the_handler() {
    let client = client();
    let body = vec![b'x'; usize::try_from(MAX_FEDERATION_BODY_BYTES).expect("the cap fits")];

    client
        .post("/v1/federation/reports")
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .body("application/json", body)
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    client.assert_conformance();
}

/// The chunk routes keep the backstop, so a body past the protocol's 16 MiB chunk bound is not
/// refused by the transport cap.
///
/// That is what leaves `413 error.upload.chunk_too_large` reachable: the route that understands
/// the bound is the one that answers for it. Neither request here carries what the route needs
/// to get that far — no bearer on the upload, no live link on the drop — so the proof is that
/// each is answered by the route's own refusal instead of the body-size `413`.
#[tokio::test]
async fn a_body_past_the_chunk_bound_still_reaches_the_chunk_routes() {
    let client = client();
    let body = vec![b'x'; usize::try_from(MAX_CHUNK_BYTES).expect("the bound fits") + 1];
    assert!(
        body.len() as u64 <= MAX_REQUEST_BODY_BYTES,
        "the fixture must be inside the backstop for this test to mean anything"
    );

    client
        .patch("/v1/upload/anything")
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .body("application/offset+octet-stream", body.clone())
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    client
        .patch("/d/anything/anything")
        .header("x-capsule-offset", "0")
        .body("application/octet-stream", body)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND);

    client.assert_conformance();
}

// ===========================================================================================
// The header-read and idle bound, on a socket (#717)
// ===========================================================================================

/// The header-read bound every socket case below serves with.
const HEADER_READ_TIMEOUT: Duration = Duration::from_millis(300);

/// A client that connects and never sends a byte is disconnected at the header bound (#795).
///
/// The read that tells HTTP/1 from the HTTP/2 preface precedes hyper's own header timer, so
/// this is the case that timer alone never reached: Kynos holds a connection's first head to
/// the header-read timeout from accept, sniff included.
#[tokio::test]
async fn a_client_that_never_sends_a_byte_is_disconnected() {
    let address = support::serve_on_socket(Fixture::working_app(), HEADER_READ_TIMEOUT).await;
    let mut stream = TcpStream::connect(address).await.expect("it connects");
    let started = Instant::now();

    let trailing = support::read_until_closed(&mut stream).await;
    assert!(
        trailing.is_empty(),
        "nothing was asked for, so nothing is answered: {trailing}"
    );
    assert!(
        started.elapsed() >= HEADER_READ_TIMEOUT,
        "closed before the header bound passed"
    );
}

/// A client that sends part of a request head and then stalls is disconnected.
#[tokio::test]
async fn a_client_that_stalls_inside_its_request_head_is_disconnected() {
    let address = support::serve_on_socket(Fixture::working_app(), HEADER_READ_TIMEOUT).await;
    let mut stream = TcpStream::connect(address).await.expect("it connects");
    let started = Instant::now();
    // A request line and one header, and never the blank line that ends the head.
    stream
        .write_all(b"GET /v1/version HTTP/1.1\r\nHost: capsule.test\r\n")
        .await
        .expect("it writes");

    support::read_until_closed(&mut stream).await;
    assert!(
        started.elapsed() >= HEADER_READ_TIMEOUT,
        "closed before the header bound passed"
    );
}

/// A kept-alive connection that sits idle after its response is closed at the same bound.
///
/// The first request proves the connection is kept alive at all — the response is read and the
/// socket is still open — and the close that follows, with no further byte sent, is the idle
/// bound.
#[tokio::test]
async fn a_kept_alive_connection_left_idle_is_closed() {
    let address = support::serve_on_socket(Fixture::working_app(), HEADER_READ_TIMEOUT).await;
    let mut stream = TcpStream::connect(address).await.expect("it connects");
    stream
        .write_all(
            b"GET /v1/version HTTP/1.1\r\nHost: capsule.test\r\nAccept: application/json\r\n\r\n",
        )
        .await
        .expect("it writes");
    let answer = support::read_response(&mut stream).await;
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");

    let idle_from = Instant::now();
    let trailing = support::read_until_closed(&mut stream).await;
    assert!(
        trailing.is_empty(),
        "nothing more was asked for: {trailing}"
    );
    assert!(
        idle_from.elapsed() >= HEADER_READ_TIMEOUT,
        "closed before the idle bound passed"
    );
}

/// A client that keeps its connection busy is not cut off by the bound: a second request sent
/// inside it is answered on the same connection.
#[tokio::test]
async fn a_kept_alive_connection_used_inside_the_bound_stays_open() {
    let address = support::serve_on_socket(Fixture::working_app(), Duration::from_secs(5)).await;
    let mut stream = TcpStream::connect(address).await.expect("it connects");
    for _ in 0..2 {
        stream
            .write_all(
                b"GET /v1/version HTTP/1.1\r\nHost: capsule.test\r\nAccept: application/json\r\n\r\n",
            )
            .await
            .expect("it writes");
        let answer = support::read_response(&mut stream).await;
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
