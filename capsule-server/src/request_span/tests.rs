use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use kynos::http::StatusCode;
use kynos::test::TestClient;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing_subscriber::layer::{Context, SubscriberExt as _};
use tracing_subscriber::registry::LookupSpan;

use super::*;
use crate::App;
use crate::config::{Config, Demands, Overrides};

// ===========================================================================================
// A captured subscriber
// ===========================================================================================

/// One span's fields, as their recorded values rendered to strings.
type Fields = BTreeMap<String, String>;

/// What the capturing layer saw: every closed `request` span, and every event emitted inside
/// one together with the request id of the span it was emitted in.
#[derive(Default)]
struct Captured {
    closed: Vec<Fields>,
    events: Vec<(String, String)>,
}

/// Records the fields of every `request` span and closes them into [`Captured`].
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Captured>>);

impl Capture {
    fn closed(&self) -> Vec<Fields> {
        self.0.lock().expect("the capture lock").closed.clone()
    }

    fn events(&self) -> Vec<(String, String)> {
        self.0.lock().expect("the capture lock").events.clone()
    }
}

struct Collect<'a>(&'a mut Fields);

impl Visit for Collect<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().to_owned(), value.to_owned());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().to_owned(), value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.insert(field.name().to_owned(), format!("{value:?}"));
    }
}

impl<S> tracing_subscriber::Layer<S> for Capture
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attributes: &Attributes<'_>, id: &Id, context: Context<'_, S>) {
        if attributes.metadata().name() != SPAN_NAME {
            return;
        }
        let mut fields = Fields::new();
        attributes.record(&mut Collect(&mut fields));
        if let Some(span) = context.span(id) {
            span.extensions_mut().insert(fields);
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, context: Context<'_, S>) {
        let Some(span) = context.span(id) else { return };
        let mut extensions = span.extensions_mut();
        if let Some(fields) = extensions.get_mut::<Fields>() {
            values.record(&mut Collect(fields));
        }
    }

    fn on_event(&self, event: &tracing::Event<'_>, context: Context<'_, S>) {
        let Some(scope) = context.event_scope(event) else {
            return;
        };
        for span in scope {
            if let Some(fields) = span.extensions().get::<Fields>() {
                let mut message = Fields::new();
                event.record(&mut Collect(&mut message));
                self.0.lock().expect("the capture lock").events.push((
                    fields.get("request_id").cloned().unwrap_or_default(),
                    message.get("message").cloned().unwrap_or_default(),
                ));
                return;
            }
        }
    }

    fn on_close(&self, id: Id, context: Context<'_, S>) {
        let Some(span) = context.span(&id) else {
            return;
        };
        if let Some(fields) = span.extensions_mut().remove::<Fields>() {
            self.0.lock().expect("the capture lock").closed.push(fields);
        }
    }
}

/// A subscriber capturing into the returned [`Capture`], installed for the current thread.
///
/// `#[tokio::test]` is a current-thread runtime, so every poll of the request runs under it.
fn capture() -> (Capture, tracing::subscriber::DefaultGuard) {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    let guard = tracing::subscriber::set_default(subscriber);
    (capture, guard)
}

// ===========================================================================================
// A server
// ===========================================================================================

/// A PKCS#8 v1 Ed25519 key, base64. Signs nothing; see `config`'s own tests.
const EXAMPLE_DER: &str = "MC4CAQAwBQYDK2VwBCIEIN6eTvXEL7xMZWHY8rTk7VbQSGSuRkle5MVfiiYUStLF";

/// The memory profile, assembled exactly as `serve` assembles it.
async fn client(root: &std::path::Path) -> TestClient<App> {
    let environment: BTreeMap<String, String> = [
        ("BLOB_ROOT".to_owned(), root.display().to_string()),
        ("JWT_ED25519_DER".to_owned(), EXAMPLE_DER.to_owned()),
    ]
    .into_iter()
    .collect();
    let overrides = Overrides {
        memory: true,
        ..Overrides::default()
    };
    let config =
        Config::load(&environment, &overrides, Demands::Serve).expect("the configuration loads");
    let assembled = crate::boot::assemble(&config)
        .await
        .expect("the memory profile assembles");
    TestClient::new(assembled.service().expect("the router builds"))
}

/// Registers an account, signs in, and returns its access token.
async fn access_token(client: &TestClient<App>) -> String {
    let credentials = serde_json::json!({ "email": "somebody@example.test", "password": "correct horse battery" });
    for path in ["/v1/auth/register", "/v1/auth/login"] {
        let response = client
            .post(path)
            .header(
                "x-capsule-protocol",
                capsule_core::crypto::primitives::PROTOCOL_VERSION,
            )
            .header("accept", "application/json")
            .json(&credentials)
            .send()
            .await;
        response.assert_status(StatusCode::OK);
        if path.ends_with("login") {
            let body: serde_json::Value = response.json();
            return body["access_token"]
                .as_str()
                .expect("a sign-in returns an access token")
                .to_owned();
        }
    }
    unreachable!("the loop returns at the sign-in")
}

/// The one closed span carrying `request_id`.
fn span_named(closed: &[Fields], request_id: &str) -> Fields {
    let matching: Vec<&Fields> = closed
        .iter()
        .filter(|fields| fields.get("request_id").map(String::as_str) == Some(request_id))
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "exactly one request span carries {request_id}: {closed:?}"
    );
    matching[0].clone()
}

/// The response's `X-Request-Id`, parsed as the UUIDv7 it must be.
fn request_id_of(header: Option<&str>) -> String {
    let value = header
        .expect("every response carries X-Request-Id")
        .to_owned();
    let parsed = uuid::Uuid::parse_str(&value).expect("the request id is a UUID");
    assert_eq!(
        parsed.get_version(),
        Some(uuid::Version::SortRand),
        "and a v7 one"
    );
    value
}

// ===========================================================================================
// The cases
// ===========================================================================================

/// An authenticated request's span names the request, the route, the session and the outcome.
#[tokio::test]
async fn an_authenticated_request_records_its_session_status_and_latency() {
    let root = tempfile::tempdir().expect("a scratch directory");
    let client = client(root.path()).await;
    let token = access_token(&client).await;
    let (capture, _guard) = capture();

    let response = client
        .get("/v1/quota")
        .header(
            "x-capsule-protocol",
            capsule_core::crypto::primitives::PROTOCOL_VERSION,
        )
        .header("accept", "application/json")
        .header("authorization", &format!("Bearer {token}"))
        .send()
        .await;
    response.assert_status(StatusCode::OK);
    let request_id = request_id_of(response.header("x-request-id"));

    let span = span_named(&capture.closed(), &request_id);
    assert_eq!(span["method"], "GET");
    assert_eq!(span["route"], "/v1/quota", "the template, never the path");
    assert_eq!(span["client"], crate::client_addr::UNKNOWN_SOURCE);
    assert_eq!(span["status"], "200");
    assert!(
        span["latency_ms"].parse::<u64>().is_ok(),
        "latency is recorded"
    );
    assert!(
        !span["user_id"].is_empty(),
        "the bearer scheme recorded the account: {span:?}"
    );
    assert!(!span["session_id"].is_empty(), "and the session: {span:?}");
    assert!(
        span.values().all(|value| !value.contains(&token)),
        "the token is never recorded"
    );
    assert!(
        capture
            .events()
            .iter()
            .any(|(id, message)| id == &request_id && message == "request completed"),
        "the completion event is emitted inside the span"
    );
}

/// An unauthenticated refusal carries the id on the header and in the problem, and no session.
#[tokio::test]
async fn a_problem_carries_the_request_id_its_header_does() {
    let root = tempfile::tempdir().expect("a scratch directory");
    let client = client(root.path()).await;
    let (capture, _guard) = capture();

    let response = client
        .get("/v1/quota")
        .header(
            "x-capsule-protocol",
            capsule_core::crypto::primitives::PROTOCOL_VERSION,
        )
        .header("accept", "application/problem+json")
        .header("authorization", "Bearer not-a-token")
        .send()
        .await;
    response.assert_status(StatusCode::UNAUTHORIZED);
    let request_id = request_id_of(response.header("x-request-id"));
    let body: serde_json::Value = response.json();
    assert_eq!(body[crate::problem::REQUEST_ID_MEMBER], request_id.as_str());
    assert_eq!(
        body[crate::problem::CODE_MEMBER],
        "error.request.unauthenticated",
        "and the coded-problem rewrite still ran beneath it"
    );

    let span = span_named(&capture.closed(), &request_id);
    assert_eq!(span["status"], "401");
    assert!(
        !span.contains_key("user_id") && !span.contains_key("session_id"),
        "a refused request names no session: {span:?}"
    );
    assert!(
        span.values().all(|value| !value.contains("not-a-token")),
        "the presented credential is never recorded"
    );
}

/// A request no operation answers still gets a span, an id, and a stamped problem.
#[tokio::test]
async fn an_unrouted_request_is_spanned_as_unmatched() {
    let root = tempfile::tempdir().expect("a scratch directory");
    let client = client(root.path()).await;
    let (capture, _guard) = capture();

    let response = client
        .get("/v1/no-such-surface/secret-in-the-path")
        .header("accept", "application/problem+json")
        .send()
        .await;
    response.assert_status(StatusCode::NOT_FOUND);
    let request_id = request_id_of(response.header("x-request-id"));
    let body: serde_json::Value = response.json();
    assert_eq!(body[crate::problem::REQUEST_ID_MEMBER], request_id.as_str());

    let span = span_named(&capture.closed(), &request_id);
    assert_eq!(span["route"], UNMATCHED);
    assert_eq!(span["status"], "404");
    assert!(
        span.values()
            .all(|value| !value.contains("secret-in-the-path")),
        "the request's own path is never recorded"
    );
}

/// A client's own `X-Request-Id` is ignored, and two requests never share an id.
#[tokio::test]
async fn the_request_id_is_the_servers_and_unique() {
    let root = tempfile::tempdir().expect("a scratch directory");
    let client = client(root.path()).await;

    let mut seen = Vec::new();
    for _ in 0..2 {
        let response = client
            .get("/v1/version")
            .header("accept", "application/json")
            .header("x-request-id", "chosen-by-the-client")
            .send()
            .await;
        response.assert_status(StatusCode::OK);
        seen.push(request_id_of(response.header("x-request-id")));
    }
    assert_ne!(seen[0], seen[1]);
}

/// The stamp adds the member, keeps one already present, and leaves a non-object alone.
#[test]
fn the_stamp_adds_only_what_is_missing() {
    let stamped_body: serde_json::Value = serde_json::from_slice(&stamped(
        Bytes::from_static(br#"{"status":401,"code":"error.request.unauthenticated"}"#),
        "0190",
    ))
    .expect("json");
    assert_eq!(stamped_body["request_id"], "0190");
    assert_eq!(
        stamped_body["code"], "error.request.unauthenticated",
        "and nothing else moved"
    );

    let present = Bytes::from_static(br#"{"status":401,"request_id":"earlier"}"#);
    assert_eq!(stamped(present.clone(), "0190"), present);

    for body in [Bytes::from_static(b"not json"), Bytes::from_static(b"[]")] {
        assert_eq!(stamped(body.clone(), "0190"), body);
    }
}

/// Outside a request span, recording a session is a no-op rather than a panic.
#[test]
fn recording_a_session_outside_a_request_is_harmless() {
    record_session(
        &crate::store::UserId::new("u".to_owned()),
        &crate::store::SessionId::new("s".to_owned()),
    );
}
