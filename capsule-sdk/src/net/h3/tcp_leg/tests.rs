#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use super::*;
use crate::net::RequestCut;
use crate::testmock::{MockRequest, MockResponse, MockServer};

/// A mock that answers every request with `response`, keeping each request it read.
async fn recording(response: MockResponse) -> (MockServer, Arc<Mutex<Vec<MockRequest>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let server = MockServer::start(move |request| {
        log.lock().unwrap().push(request.clone());
        response.clone()
    })
    .await;
    (server, seen)
}

/// A port nothing listens on.
async fn closed_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}

fn request(method: http::Method, url: &str, body: Bytes) -> http::Request<Bytes> {
    let mut request = http::Request::new(body);
    *request.method_mut() = method;
    *request.uri_mut() = url.parse().unwrap();
    request
}

#[tokio::test]
async fn the_dial_connects_to_a_listening_origin() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let leg = ReqwestLeg::new(reqwest::Client::new());
    let origin = Origin::from_uri(&format!("http://127.0.0.1:{port}/").parse().unwrap()).unwrap();
    leg.dial(&origin).await.unwrap();
}

#[tokio::test]
async fn the_dial_fails_where_nothing_listens() {
    let port = closed_port().await;
    let leg = ReqwestLeg::new(reqwest::Client::new());
    let origin = Origin::from_uri(&format!("http://127.0.0.1:{port}/").parse().unwrap()).unwrap();
    assert!(leg.dial(&origin).await.is_err());
}

#[tokio::test]
async fn the_dial_is_bounded_by_its_timeout() {
    // TEST-NET-1 (RFC 5737) never answers: the connect either hangs until the bound cuts it or
    // is refused at once by a host with no route; neither outlasts the bound by much.
    let started = std::time::Instant::now();
    let outcome = probe("192.0.2.1", 9, Duration::from_millis(100)).await;
    assert!(outcome.is_err());
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn an_exchange_rides_the_context_client_and_reads_the_whole_response() {
    let (server, seen) = recording(
        MockResponse::new(200, "OK")
            .header("Alt-Svc", "h3=\":443\"")
            .json_body("{\"ok\":true}"),
    )
    .await;
    let leg = ReqwestLeg::new(reqwest::Client::new());
    let context = LegContext::new(crate::net::http_client().unwrap());
    let mut request = request(
        http::Method::GET,
        &format!("{}/v1/thing", server.base_url()),
        Bytes::new(),
    );
    request.extensions_mut().insert(context.clone());

    let response = leg
        .exchange((), request, RequestBounds::default())
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()[http::header::ALT_SVC], "h3=\":443\"");
    assert_eq!(response.body().as_ref(), b"{\"ok\":true}");
    assert!(context.take_failure().is_none());
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].path, "/v1/thing");
    // The context's client is the SDK's, so its handshake rode the request.
    assert!(
        seen[0]
            .header(crate::net::negotiation::PROTOCOL_HEADER)
            .is_some()
    );
}

#[tokio::test]
async fn a_paced_exchange_sends_the_whole_body_and_reports_progress() {
    let (server, seen) = recording(MockResponse::new(204, "No Content")).await;
    let leg = ReqwestLeg::new(reqwest::Client::new());
    let progress = ExchangeProgress::waiting();
    let body = Bytes::from(vec![7u8; 3 * crate::net::PACED_PIECE + 5]);
    let mut request = request(
        http::Method::PATCH,
        &format!("{}/upload/s", server.base_url()),
        body.clone(),
    );
    request.extensions_mut().insert(progress.clone());

    let response = leg
        .exchange((), request, RequestBounds::default())
        .await
        .unwrap();

    assert_eq!(response.status(), 204);
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].method, "PATCH");
    assert_eq!(seen[0].body, body.as_ref());
    assert_eq!(
        seen[0].header("content-length"),
        Some(body.len().to_string().as_str())
    );
}

#[tokio::test]
async fn a_failed_exchange_records_its_reqwest_error_in_the_context() {
    let port = closed_port().await;
    let leg = ReqwestLeg::new(reqwest::Client::new());
    let context = LegContext::new(reqwest::Client::new());
    let mut request = request(
        http::Method::GET,
        &format!("http://127.0.0.1:{port}/"),
        Bytes::new(),
    );
    request.extensions_mut().insert(context.clone());

    let error = leg
        .exchange((), request, RequestBounds::default())
        .await
        .unwrap_err();

    assert!(matches!(error, TransportError::Exchange(_)));
    let failure = context.take_failure().unwrap();
    assert!(failure.is_connect());
    assert!(
        context.take_failure().is_none(),
        "the failure is taken once"
    );
}

#[tokio::test]
async fn an_exchange_is_cut_under_its_bounds_without_recording_a_failure() {
    let (server, _seen) = recording(MockResponse::hold_open()).await;
    let leg = ReqwestLeg::new(reqwest::Client::new());
    let context = LegContext::new(reqwest::Client::new());
    let mut request = request(
        http::Method::GET,
        &format!("{}/held", server.base_url()),
        Bytes::new(),
    );
    request.extensions_mut().insert(context.clone());
    let bounds = RequestBounds::new(Duration::from_millis(50), Duration::from_millis(50));

    let error = leg.exchange((), request, bounds).await.unwrap_err();

    assert!(matches!(error, TransportError::Cut(RequestCut::NoResponse)));
    assert!(context.take_failure().is_none());
}

#[tokio::test]
async fn an_exchange_without_a_context_rides_the_leg_client() {
    let (server, seen) = recording(MockResponse::new(200, "OK")).await;
    let leg = ReqwestLeg::new(crate::net::http_client().unwrap());
    let request = request(
        http::Method::GET,
        &format!("{}/plain", server.base_url()),
        Bytes::new(),
    );

    let response = leg
        .exchange((), request, RequestBounds::default())
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    assert!(
        seen.lock().unwrap()[0]
            .header(crate::net::negotiation::PROTOCOL_HEADER)
            .is_some()
    );
}
