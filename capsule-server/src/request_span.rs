//! [`RequestSpan`] — one `tracing` span per request, and the request id that names it.
//!
//! # What a request leaves behind
//!
//! Every request the router receives — a routed operation, an unrouted `404`, a `405` — runs
//! inside one `request` span carrying:
//!
//! | Field | When | What |
//! | --- | --- | --- |
//! | `request_id` | on entry | A UUIDv7 this server minted for the request |
//! | `method` | on entry | The request method |
//! | `route` | on entry | The matched `paths` key, or `<unmatched>`: bounded cardinality |
//! | `client` | on entry | The client address, resolved through the deployment's proxy trust |
//! | `user_id`, `session_id` | once the bearer scheme admits the request | See [`record_session`] |
//! | `status`, `latency_ms` | on completion | What the response said and how long it took |
//!
//! Every event the chain emits beneath it — a handler's, a port's, the bearer scheme's — inherits
//! the span, so a subscriber that renders spans (the binary's JSON layer does) puts the request
//! id and the session on every line a request caused. That is the whole point: a refusal in the
//! log can be traced to the session that made it, and a client quoting a request id can be
//! matched to its log lines.
//!
//! # The id is the server's, never the client's
//!
//! An inbound `X-Request-Id` is ignored. A client-chosen id would let a caller collide its own
//! requests with somebody else's in the log, or forge a correlation an operator then trusts.
//! The id is minted here, returned on the `X-Request-Id` response header of **every** response,
//! and stamped into every problem document as a `request_id` member, so a user reporting a
//! refusal has the one string an operator needs to find it.
//!
//! # What is never recorded
//!
//! No header value, no body, no query string, and no credential. The route field is the
//! template (`/v1/blob/{hash}`), never the request's own path, so nothing a client puts in a URL
//! reaches the span. The bearer scheme records the account and the session it resolved — ids,
//! not the token that named them.
//!
//! # Ordering
//!
//! Mounted **outermost**, on the router, so the span encloses everything — the coded-problem
//! rewrite, the protocol window, every group's body cap and gate, the extractors and the
//! handler — and so the problem body it stamps is the final one.

use std::convert::Infallible;
use std::time::Instant;

use bytes::Bytes;
use kynos::extract::FromRequestParts as _;
use kynos::extract::params::header::{EncodeHeaders, HeaderParams};
use kynos::http::{HeaderName, HeaderValue, Request};
use kynos::middleware::{Continued, Interceptor, Next};
use kynos::openapi::{Header, Map, Parameter, RefOr, Schema};
use kynos::schema::registry::Registry;
use tracing::Instrument as _;
use tracing::field::Empty;

use crate::client_addr::ClientAddr;

/// The response header the request id rides, as the document spells it.
pub const REQUEST_ID_HEADER: &str = "X-Request-Id";

/// The span's name, and therefore the key a subscriber files its fields under.
pub const SPAN_NAME: &str = "request";

/// What the `route` field says for a request no operation matched.
pub const UNMATCHED: &str = "<unmatched>";

/// The `X-Request-Id` response header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestIdHeader(HeaderValue);

impl RequestIdHeader {
    /// The header for `id`.
    fn of(id: uuid::Uuid) -> Self {
        Self(HeaderValue::from_str(&id.to_string()).expect("a hyphenated UUID is a header value"))
    }
}

/// The header's declaration: `(name, header)`.
///
/// The one source for both of the document's spellings of it — the response header Kynos files
/// under an operation's success responses through [`HeaderParams::response_headers`], and the
/// one the post-emit walk in [`crate::openapi`](fn@crate::openapi) files under every other
/// response — so the two cannot disagree about what it carries.
#[must_use]
pub fn response_header_declaration() -> (&'static str, Header) {
    (
        REQUEST_ID_HEADER,
        Header::new(uuid_schema()).required(true).with_description(
            "The UUIDv7 this server minted for the request. Quote it when reporting a \
             failure: it names the request's log lines. Never taken from the client.",
        ),
    )
}

/// A UUID, as the header carries it.
///
/// A plain string, **without** `format: uuid`: spargen maps that format to a `uuid::Uuid` and
/// then demands a `uuid` requirement the workspace does not pin, and a correlation handle a
/// client only quotes back gains nothing from being parsed.
fn uuid_schema() -> Schema {
    serde_json::from_value(serde_json::json!({ "type": "string" }))
        .expect("a literal string schema is a schema")
}

impl HeaderParams for RequestIdHeader {
    const NAMES: &'static [&'static str] = &["x-request-id"];

    fn parameters(registry: &mut Registry) -> Vec<Parameter> {
        let _ = registry;
        vec![
            Parameter::header(REQUEST_ID_HEADER, uuid_schema())
                .required(true)
                .with_description("The UUIDv7 this server minted for the request."),
        ]
    }

    fn response_headers(registry: &mut Registry) -> Map<RefOr<Header>> {
        let _ = registry;
        let (name, header) = response_header_declaration();
        let mut headers = Map::new();
        headers.insert(name.to_owned(), RefOr::Item(header));
        headers
    }
}

impl EncodeHeaders for RequestIdHeader {
    fn encode(&self) -> Vec<(HeaderName, HeaderValue)> {
        vec![(HeaderName::from_static("x-request-id"), self.0.clone())]
    }
}

/// Opens the request span, mints the request id, and returns it on every response.
///
/// See the module docs. Holds nothing: the id is drawn per request and the clock it times with
/// is the process's monotonic one, which is a measurement rather than a decision.
#[derive(Debug, Clone, Copy, Default)]
pub struct RequestSpan;

impl RequestSpan {
    /// The interceptor.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl<C: Sync + 'static> Interceptor<C> for RequestSpan {
    type Reads = ();
    type Adds = RequestIdHeader;
    /// Never answers on its own: a span observes a decision, it does not make one.
    type Short = Infallible;

    async fn intercept(
        &self,
        request: Request,
        (): (),
        context: &C,
        next: Next<'_, C>,
    ) -> Result<Continued<RequestIdHeader>, Infallible> {
        let started = Instant::now();
        let id = uuid::Uuid::now_v7();
        let route = next.route().map_or(UNMATCHED, |route| route.path());

        // The one resolution dispatch made, read back the way every limiter reads it, so the
        // span and the limiters cannot disagree about who a caller is.
        let (mut parts, body) = request.into_parts();
        let Ok(client) = ClientAddr::from_request_parts(&mut parts, context).await;
        let request = Request::from_parts(parts, body);
        let client = client.address().map_or_else(
            || crate::client_addr::UNKNOWN_SOURCE.to_owned(),
            |address| address.to_string(),
        );

        let span = tracing::info_span!(
            "request",
            request_id = %id,
            method = %request.method(),
            route,
            client = %client,
            user_id = Empty,
            session_id = Empty,
            status = Empty,
            latency_ms = Empty,
        );
        tracing::debug!(parent: &span, "request received");

        let mut continued = REQUEST_ID
            .scope(id, next.run(request).instrument(span.clone()))
            .await;

        if crate::problem::is_problem(continued.headers()) {
            let status = continued.status().as_u16();
            match crate::problem::collect(continued.take_body()).await {
                Ok(bytes) => continued.set_body(kynos::http::body::Body::from_bytes(stamped(
                    bytes,
                    &id.to_string(),
                ))),
                Err(error) => {
                    // As in `CodedProblems`: an in-memory problem body that cannot be read is
                    // not a state a request causes, and the body is already gone.
                    tracing::error!(
                        parent: &span,
                        %error,
                        status,
                        "a problem body could not be read to stamp its request id"
                    );
                }
            }
        }

        let status = continued.status().as_u16();
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        span.record("status", status);
        span.record("latency_ms", latency_ms);
        tracing::info!(parent: &span, status, latency_ms, "request completed");

        Ok(continued.with_headers(RequestIdHeader::of(id)))
    }
}

tokio::task_local! {
    /// The id [`RequestSpan`] minted for the request the current task is serving.
    static REQUEST_ID: uuid::Uuid;
}

/// The id of the request the current task is serving, or `None` outside one.
///
/// What a durable record of the request's effect — an [activity
/// event](crate::activity::ActivityEvent) — names so it can be matched to the request's log
/// lines. Read from a task-local the interceptor scopes around the rest of the chain, not from
/// the span: a span's fields are write-only to the code beneath it.
pub fn current_request_id() -> Option<uuid::Uuid> {
    REQUEST_ID.try_with(|id| *id).ok()
}

/// Records the account and session the bearer scheme admitted on the current request's span.
///
/// Called from [`crate::auth::scheme`] once a token verifies **and** its session is live, so a
/// span carrying a `session_id` names a session that was open when the request was admitted.
/// Ids only: the token never reaches this function. A no-op outside a request span, which is
/// what a test calling the authenticator directly gets.
pub fn record_session(user: &crate::store::UserId, session: &crate::store::SessionId) {
    let span = tracing::Span::current();
    span.record("user_id", tracing::field::display(user));
    span.record("session_id", tracing::field::display(session));
}

/// The problem body with a `request_id` member, or unchanged when it cannot carry one.
///
/// A body that already names a `request_id` keeps it, and anything that is not a JSON object is
/// left alone — the reasons [`crate::problem`]'s `coded` gives for the same choices.
fn stamped(bytes: Bytes, id: &str) -> Bytes {
    let Ok(serde_json::Value::Object(mut members)) =
        serde_json::from_slice::<serde_json::Value>(&bytes)
    else {
        return bytes;
    };
    if members.contains_key(crate::problem::REQUEST_ID_MEMBER) {
        return bytes;
    }
    members.insert(
        crate::problem::REQUEST_ID_MEMBER.to_owned(),
        serde_json::Value::String(id.to_owned()),
    );
    serde_json::to_vec(&serde_json::Value::Object(members)).map_or(bytes, Bytes::from)
}

#[cfg(test)]
mod tests;
