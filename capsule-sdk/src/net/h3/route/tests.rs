#![allow(clippy::unwrap_used)]

use std::sync::Mutex;

use super::*;
use crate::testmock::{MockRequest, MockResponse, MockServer};

/// What a [`ScriptedCarrier`] does with a request it is handed.
#[derive(Debug, Clone, Copy)]
enum Script {
    /// Answer it over QUIC with a `200` carrying `quic`.
    Quic,
    /// Fail it as a stalled QUIC path.
    Stall,
    /// Fail it as a TCP leg that rode the request and failed, recording a genuine `reqwest`
    /// error in its [`LegContext`].
    TcpFailed,
}

/// A carrier with a fixed policy answer and a scripted send, keeping what it saw.
#[derive(Debug)]
struct ScriptedCarrier {
    races: bool,
    script: Script,
    sent: Mutex<Vec<http::Request<Bytes>>>,
    observed: Mutex<Vec<(String, http::HeaderMap)>>,
}

impl ScriptedCarrier {
    fn new(races: bool, script: Script) -> Self {
        Self {
            races,
            script,
            sent: Mutex::new(Vec::new()),
            observed: Mutex::new(Vec::new()),
        }
    }

    fn sent(&self) -> usize {
        self.sent.lock().unwrap().len()
    }
}

impl Carrier for ScriptedCarrier {
    fn races(&self, _origin: &str, _network: PathGeneration) -> bool {
        self.races
    }

    async fn send(
        &self,
        _network: PathGeneration,
        request: http::Request<Bytes>,
        bounds: RequestBounds,
    ) -> Result<Delivered, TransportError> {
        assert_eq!(
            bounds, UNBOUNDED,
            "the router never bounds a request itself"
        );
        let context = request.extensions().get::<LegContext>().cloned().unwrap();
        self.sent.lock().unwrap().push(request);
        match self.script {
            Script::Quic => Ok(Delivered {
                via: Via::Quic,
                response: http::Response::builder()
                    .status(200)
                    .header("x-carried", "quic")
                    .body(Bytes::from_static(b"quic"))
                    .unwrap(),
            }),
            Script::Stall => Err(TransportError::PathStalled),
            Script::TcpFailed => {
                let port = closed_port().await;
                let error = bare_client()
                    .get(format!("http://127.0.0.1:{port}/"))
                    .send()
                    .await
                    .unwrap_err();
                context.record(error);
                Err(TransportError::Exchange("the TCP leg failed".to_owned()))
            }
        }
    }

    fn observe(&self, origin: &str, headers: &http::HeaderMap) {
        self.observed
            .lock()
            .unwrap()
            .push((origin.to_owned(), headers.clone()));
    }
}

async fn closed_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}

/// A mock answering `200` with an `Alt-Svc`, keeping each request it read.
async fn advertising() -> (MockServer, Arc<Mutex<Vec<MockRequest>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let server = MockServer::start(move |request| {
        log.lock().unwrap().push(request.clone());
        MockResponse::new(200, "OK")
            .header("Alt-Svc", "h3=\":443\"; ma=60")
            .json_body("{}")
    })
    .await;
    (server, seen)
}

const NETWORK: PathGeneration = PathGeneration(3);

#[test]
fn only_idempotent_or_replayable_requests_are_resendable() {
    for method in [
        http::Method::GET,
        http::Method::HEAD,
        http::Method::PUT,
        http::Method::DELETE,
        http::Method::OPTIONS,
    ] {
        assert!(resendable(&method, false), "{method}");
    }
    for method in [http::Method::POST, http::Method::PATCH] {
        assert!(!resendable(&method, false), "{method}");
        assert!(resendable(&method, true), "{method} marked replayable");
    }
}

#[test]
fn a_quic_request_gains_the_defaults_a_client_adds_as_it_sends() {
    let mut headers = http::HeaderMap::new();
    complete_headers(&mut headers, 12);
    for (name, value) in &crate::net::protocol_headers() {
        assert_eq!(headers.get(name), Some(value), "{name}");
    }
    assert_eq!(headers[http::header::ACCEPT], "*/*");
    assert_eq!(headers[http::header::CONTENT_LENGTH], "12");

    let mut set = http::HeaderMap::new();
    set.insert(
        crate::net::negotiation::PROTOCOL_HEADER,
        http::HeaderValue::from_static("2020-01-01"),
    );
    set.insert(
        http::header::ACCEPT,
        http::HeaderValue::from_static("application/json"),
    );
    complete_headers(&mut set, 0);
    assert_eq!(set[crate::net::negotiation::PROTOCOL_HEADER], "2020-01-01");
    assert_eq!(set[http::header::ACCEPT], "application/json");
    assert!(!set.contains_key(http::header::CONTENT_LENGTH));
}

#[test]
fn only_an_https_origin_races_or_is_believed() {
    let transport = H3Transport::new(
        Arc::new(H3Policy::for_build(SystemClock::new())),
        H3Dialer::new(&H3Config::default()).unwrap(),
        ReqwestLeg::new(bare_client()),
        NoHttpsRecords,
    );
    let mut advertised = http::HeaderMap::new();
    advertised.insert(
        http::header::ALT_SVC,
        http::HeaderValue::from_static("h3=\":443\""),
    );
    let secure = "https://example.test:443";
    let plain = "http://example.test:80";

    assert!(!Carrier::races(&transport, secure, NETWORK));
    Carrier::observe(&transport, secure, &advertised);
    Carrier::observe(&transport, plain, &advertised);
    assert!(Carrier::races(&transport, secure, NETWORK));
    assert!(!Carrier::races(&transport, plain, NETWORK));

    transport.policy().observe_alt_svc(plain, "h3=\":443\"");
    assert!(
        !Carrier::races(&transport, plain, NETWORK),
        "an advertisement for a plain origin never races"
    );
}

#[tokio::test]
async fn a_request_the_policy_does_not_race_rides_tcp_and_feeds_discovery() {
    let (server, seen) = advertising().await;
    let carrier = ScriptedCarrier::new(false, Script::Quic);
    let client = crate::net::http_client().unwrap();
    let request = client
        .get(format!("{}/v1/thing", server.base_url()))
        .build()
        .unwrap();

    let response = execute_via(&carrier, NETWORK, &client, request)
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert_eq!(carrier.sent(), 0);
    let observed = carrier.observed.lock().unwrap();
    assert_eq!(observed.len(), 1);
    assert_eq!(
        observed[0].0,
        Origin::from_uri(&server.base_url().parse().unwrap())
            .unwrap()
            .key()
    );
    assert_eq!(observed[0].1[http::header::ALT_SVC], "h3=\":443\"; ma=60");
}

#[tokio::test]
async fn a_request_not_safe_to_send_twice_never_rides_quic() {
    let (server, seen) = advertising().await;
    let carrier = ScriptedCarrier::new(true, Script::Quic);
    let client = crate::net::http_client().unwrap();
    let request = client
        .post(format!("{}/v1/thing", server.base_url()))
        .body("{}")
        .build()
        .unwrap();

    let response = execute_via(&carrier, NETWORK, &client, request)
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    assert_eq!(carrier.sent(), 0);
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_raced_request_rides_quic_whole_and_reads_as_a_reqwest_response() {
    let carrier = ScriptedCarrier::new(true, Script::Quic);
    let client = crate::net::http_client().unwrap();
    let url = "http://127.0.0.1:9/v1/things/1";
    let request = client
        .put(url)
        .bearer_auth("t")
        .body("payload")
        .build()
        .unwrap();

    let response = execute_via(&carrier, NETWORK, &client, request)
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    assert_eq!(response.url().as_str(), url);
    assert_eq!(response.headers()["x-carried"], "quic");
    assert_eq!(response.text().await.unwrap(), "quic");
    let sent = carrier.sent.lock().unwrap();
    let carried = &sent[0];
    assert_eq!(carried.method(), http::Method::PUT);
    assert_eq!(carried.uri(), url);
    assert_eq!(carried.body().as_ref(), b"payload");
    assert_eq!(carried.headers()[http::header::AUTHORIZATION], "Bearer t");
    assert!(
        carried
            .headers()
            .contains_key(crate::net::negotiation::PROTOCOL_HEADER)
    );
    assert_eq!(carried.headers()[http::header::CONTENT_LENGTH], "7");
}

#[tokio::test]
async fn a_stalled_quic_path_re_sends_the_request_over_tcp() {
    let (server, seen) = advertising().await;
    let carrier = ScriptedCarrier::new(true, Script::Stall);
    let client = crate::net::http_client().unwrap();
    let request = client
        .get(format!("{}/v1/thing", server.base_url()))
        .build()
        .unwrap();

    let response = execute_via(&carrier, NETWORK, &client, request)
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    assert_eq!(carrier.sent(), 1);
    assert_eq!(seen.lock().unwrap().len(), 1, "the re-send rode TCP once");
    assert_eq!(carrier.observed.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_request_whose_tcp_leg_failed_returns_that_error_without_a_re_send() {
    let (server, seen) = advertising().await;
    let carrier = ScriptedCarrier::new(true, Script::TcpFailed);
    let client = crate::net::http_client().unwrap();
    let request = client
        .get(format!("{}/v1/thing", server.base_url()))
        .build()
        .unwrap();

    let error = execute_via(&carrier, NETWORK, &client, request)
        .await
        .unwrap_err();

    assert!(error.is_connect());
    assert!(seen.lock().unwrap().is_empty(), "nothing was re-sent");
}

#[tokio::test]
async fn a_replayable_chunk_rides_quic_with_its_progress() {
    let carrier = ScriptedCarrier::new(true, Script::Quic);
    let client = crate::net::http_client().unwrap();
    let progress = ExchangeProgress::waiting();
    let bytes = Bytes::from_static(b"chunk-bytes");
    let builder = client
        .patch("http://127.0.0.1:9/upload/s")
        .body(crate::net::PacedBody::new(bytes.clone(), progress.clone()));
    let request = replayable(builder, bytes.clone(), &progress)
        .build()
        .unwrap();

    execute_via(&carrier, NETWORK, &client, request)
        .await
        .unwrap();

    let sent = carrier.sent.lock().unwrap();
    assert_eq!(sent[0].method(), http::Method::PATCH);
    assert_eq!(sent[0].body(), &bytes);
    assert!(sent[0].extensions().get::<ExchangeProgress>().is_some());
}

#[tokio::test]
async fn a_replayable_chunk_off_quic_sends_its_paced_body() {
    let (server, seen) = advertising().await;
    let carrier = ScriptedCarrier::new(true, Script::Stall);
    let client = crate::net::http_client().unwrap();
    let progress = ExchangeProgress::waiting();
    let bytes = Bytes::from(vec![9u8; 2 * crate::net::PACED_PIECE + 1]);
    let builder = client
        .patch(format!("{}/upload/s", server.base_url()))
        .body(crate::net::PacedBody::new(bytes.clone(), progress.clone()));
    let request = replayable(builder, bytes.clone(), &progress)
        .build()
        .unwrap();

    let response = execute_via(&carrier, NETWORK, &client, request)
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    assert_eq!(carrier.sent(), 1);
    assert_eq!(seen.lock().unwrap()[0].body, bytes.as_ref());
}

#[tokio::test]
async fn a_builder_that_does_not_build_still_fails_without_dialing() {
    let client = crate::net::http_client().unwrap();
    let progress = ExchangeProgress::waiting();
    let builder = client.get("not a url");

    let error = replayable(builder, Bytes::new(), &progress)
        .send()
        .await
        .unwrap_err();

    assert!(error.is_builder());
}

#[tokio::test]
async fn the_generated_clients_backend_routes_through_the_process_transport() {
    let (server, seen) = advertising().await;
    let backend = RoutedBackend::new(crate::net::http_client().unwrap());
    let request = http::Request::get(format!("{}/v1/thing", server.base_url()))
        .body(crate::rest::HttpBody::empty())
        .unwrap();

    let response = crate::rest::HttpBackend::execute(&backend, request)
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    assert!(
        seen.lock().unwrap()[0]
            .header(crate::net::negotiation::PROTOCOL_HEADER)
            .is_some(),
        "the request rode the backend's client"
    );
}

#[test]
fn any_https_proxy_variable_reqwest_reads_keeps_requests_off_quic() {
    assert_eq!(configured_proxy(|_| None), None);
    assert_eq!(
        configured_proxy(|_| Some("  ".to_owned())),
        None,
        "an empty variable configures no proxy"
    );
    for variable in PROXY_VARIABLES {
        let found = configured_proxy(|name| {
            (name == variable).then(|| "http://proxy.example:3128".to_owned())
        });
        assert_eq!(found, Some(variable), "{variable}");
    }
    assert_eq!(
        configured_proxy(|name| (name == "NO_PROXY").then(|| "*".to_owned())),
        None,
        "an exemption alone configures no proxy"
    );
    for variable in ["HTTP_PROXY", "http_proxy"] {
        assert_eq!(
            configured_proxy(|name| {
                (name == variable).then(|| "http://proxy.example:3128".to_owned())
            }),
            None,
            "{variable} proxies only http:// destinations, which never race QUIC"
        );
    }
}

#[test]
fn the_network_follows_the_reported_generation() {
    observe_generation(PathGeneration(41));
    assert_eq!(current_generation(), PathGeneration(41));
    observe_generation(PathGeneration(42));
    assert_eq!(current_generation(), PathGeneration(42));
}

/// A plain reqwest client (no SDK handshake), after the process crypto provider the SDK's own
/// builders install.
fn bare_client() -> reqwest::Client {
    crate::net::ensure_crypto_provider();
    reqwest::Client::new()
}
