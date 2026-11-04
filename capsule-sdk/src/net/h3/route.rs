//! Routing the SDK's requests through the HTTP/3 transport (networking doc, HTTP Transport).
//! Compiled only with the off-by-default `http3` feature; a default build sends every request
//! straight on its `reqwest` client, exactly as before.
//!
//! [`execute`] is the one place a prepared `reqwest::Request` leaves the SDK under `http3`: the
//! hand-written data paths reach it through the protocol window's send
//! (`negotiation::send_negotiated`), and the generated operations through [`RoutedBackend`],
//! the generated client's transport seam. For each request it:
//!
//! - **Discovers.** Every response's `Alt-Svc`, on either transport, feeds the policy
//!   ([`super::transport::observe_response`]).
//! - **Never bypasses a proxy.** With a proxy configured ([`configured_proxy`]) there is no
//!   process transport, and every request rides its `reqwest` client through the proxy.
//! - **Chooses once.** A request rides QUIC only for an `https` origin the policy answers a race
//!   for ([`Carrier::races`]), and only when it is safe to send twice ([`resendable`]): an idempotent method, or a
//!   request its caller marked [`replayable`] (an upload chunk, addressed by offset and checked
//!   by checksum). Every other request rides its `reqwest` client unchanged.
//! - **Falls back at the request boundary.** A request that fails anywhere but on TCP —
//!   [`TransportError::PathStalled`] above all, which has already recorded UDP unusable on the
//!   network — is re-sent once on its `reqwest` client, so the re-send rides TCP and the retry
//!   engine above sees a genuine TCP outcome. The generated client's seam can carry only a
//!   `reqwest::Error`, so the re-send happens here rather than as a typed cut. A request whose
//!   TCP leg already failed in a race is never re-sent: its own `reqwest` error is returned.
//! - **Never cuts on its own.** The transport runs under [`UNBOUNDED`]: a caller that bounds its
//!   requests (the upload path's [`crate::net::within_bounds`]) keeps doing so over the same
//!   [`ExchangeProgress`], which the QUIC exchange reports to, and nothing gains a bound it did
//!   not have over TCP. The QUIC path watchdog is the transport's own, sized from the stall
//!   bound.

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use bytes::Bytes;
use reqwest::ResponseBuilderExt;
use tracing::instrument;

use super::tcp_leg::{LegContext, ReqwestLeg};
use super::transport::{
    Delivered, H3Config, H3Dialer, H3Transport, HttpsResolver, NoHttpsRecords, Origin, TcpLeg,
    TransportError, Via, observe_response,
};
use super::{H3Policy, TransportChoice};
use crate::net::path::PathGeneration;
use crate::net::{ExchangeProgress, MonotonicClock, RequestBounds, SystemClock};

/// The bounds the transport runs a routed request under: none worth the name, so the caller's
/// own bounds, if any, are the only ones that cut.
pub const UNBOUNDED: RequestBounds = RequestBounds::new(
    Duration::from_hours(365 * 24),
    Duration::from_hours(365 * 24),
);

// ─── Replayable bodies ───────────────────────────────────────────────────────

/// The request extension [`replayable`] attaches: the body's bytes, and the progress the
/// caller bounds the request by.
#[derive(Debug, Clone)]
pub struct Replay {
    bytes: Bytes,
    progress: ExchangeProgress,
}

/// Mark the request `builder` holds as safe to send twice, carrying `bytes` — the bytes of the
/// streamed body it already holds — and the `progress` its caller bounds it by, so it may ride
/// QUIC. A builder that holds an error is returned unchanged.
pub fn replayable(
    builder: reqwest::RequestBuilder,
    bytes: Bytes,
    progress: &ExchangeProgress,
) -> reqwest::RequestBuilder {
    let (client, request) = builder.build_split();
    let marked = request.and_then(|request| {
        let mut request = http::Request::<reqwest::Body>::try_from(request)?;
        request.extensions_mut().insert(Replay {
            bytes,
            progress: progress.clone(),
        });
        reqwest::Request::try_from(request)
    });
    match marked {
        Ok(request) => reqwest::RequestBuilder::from_parts(client, request),
        // A builder cannot be rebuilt around an error, so the one sent is a builder over an
        // empty URL: `send` fails with the same kind of error, before dialing.
        Err(error) => {
            tracing::info!(%error, "h3: the request did not build; sending its builder error");
            client.request(reqwest::Method::GET, "")
        }
    }
}

/// Whether a request with `method` may be sent twice: an idempotent method (RFC 9110 §9.2.2), or
/// one its caller marked [`replayable`].
#[must_use]
pub fn resendable(method: &http::Method, replay: bool) -> bool {
    replay
        || matches!(
            *method,
            http::Method::GET
                | http::Method::HEAD
                | http::Method::PUT
                | http::Method::DELETE
                | http::Method::OPTIONS
        )
}

// ─── The carrier seam ────────────────────────────────────────────────────────

/// What [`execute_via`] needs of the HTTP/3 transport: the policy's answer, the send, and
/// discovery. [`H3Transport`] is the production carrier; tests supply their own.
pub trait Carrier: Send + Sync {
    /// Whether a request to `origin` (the policy key) starting now on `network` would race QUIC.
    /// Never for an origin that is not `https`.
    fn races(&self, origin: &str, network: PathGeneration) -> bool;

    /// Carry `request` on `network` under `bounds` ([`H3Transport::send`]).
    fn send(
        &self,
        network: PathGeneration,
        request: http::Request<Bytes>,
        bounds: RequestBounds,
    ) -> impl Future<Output = Result<Delivered, TransportError>> + Send;

    /// Feed a response's headers from `origin` to the policy. An advertisement is believed only
    /// for an `https` origin.
    fn observe(&self, origin: &str, headers: &http::HeaderMap);
}

impl<C, T, R> Carrier for H3Transport<C, T, R>
where
    C: MonotonicClock + Send + Sync + 'static,
    T: TcpLeg,
    R: HttpsResolver,
{
    fn races(&self, origin: &str, network: PathGeneration) -> bool {
        is_https(origin)
            && matches!(
                self.policy().choose(origin, network),
                TransportChoice::Race { .. }
            )
    }

    fn send(
        &self,
        network: PathGeneration,
        request: http::Request<Bytes>,
        bounds: RequestBounds,
    ) -> impl Future<Output = Result<Delivered, TransportError>> + Send {
        H3Transport::send(self, network, request, bounds)
    }

    fn observe(&self, origin: &str, headers: &http::HeaderMap) {
        if is_https(origin) {
            observe_response(self.policy(), origin, headers);
        }
    }
}

// ─── Routing ─────────────────────────────────────────────────────────────────

/// Whether a policy key names an `https` origin.
fn is_https(origin: &str) -> bool {
    origin.starts_with("https://")
}

/// The policy key of an `http` or `https` URL ([`Origin::key`]), or `None` for any other.
fn origin_key(url: &reqwest::Url) -> Option<String> {
    let uri: http::Uri = url.as_str().parse().ok()?;
    Origin::from_uri(&uri).ok().map(|origin| origin.key())
}

/// The request half of the protocol handshake and `reqwest`'s own `Accept` default, added where
/// the request does not set them: a `reqwest` client adds its default headers only as it sends,
/// so a request carried by QUIC would otherwise go without them. Every SDK client starts from
/// [`crate::net::http_builder`], so these are the defaults it would have added. A body's length
/// is added too, as `reqwest` does for a buffered body.
fn complete_headers(headers: &mut http::HeaderMap, body_len: usize) {
    for (name, value) in &crate::net::protocol_headers() {
        if !headers.contains_key(name) {
            headers.insert(name.clone(), value.clone());
        }
    }
    if !headers.contains_key(http::header::ACCEPT) {
        headers.insert(http::header::ACCEPT, http::HeaderValue::from_static("*/*"));
    }
    if body_len > 0 && !headers.contains_key(http::header::CONTENT_LENGTH) {
        headers.insert(
            http::header::CONTENT_LENGTH,
            http::HeaderValue::from(body_len),
        );
    }
}

/// A request split into the one `reqwest` re-sends over TCP and, where it may ride QUIC, the
/// copy QUIC carries.
struct Split {
    original: reqwest::Request,
    quic: Option<http::Request<Bytes>>,
}

/// Split `request` for routing. The QUIC copy exists only for a [`resendable`] request whose
/// body's bytes are at hand.
fn split(request: reqwest::Request, context: &LegContext) -> reqwest::Result<Split> {
    let request = http::Request::<reqwest::Body>::try_from(request)?;
    let replay = request.extensions().get::<Replay>().cloned();
    let bytes = match &replay {
        Some(replay) => Some(replay.bytes.clone()),
        None => request.body().as_bytes().map(Bytes::copy_from_slice),
    };
    let quic = bytes
        .filter(|_| resendable(request.method(), replay.is_some()))
        .map(|bytes| {
            let mut headers = request.headers().clone();
            complete_headers(&mut headers, bytes.len());
            let mut quic = http::Request::new(bytes);
            *quic.method_mut() = request.method().clone();
            *quic.uri_mut() = request.uri().clone();
            *quic.headers_mut() = headers;
            quic.extensions_mut().insert(context.clone());
            if let Some(replay) = &replay {
                quic.extensions_mut().insert(replay.progress.clone());
            }
            quic
        });
    Ok(Split {
        original: reqwest::Request::try_from(request)?,
        quic,
    })
}

/// Send `request` on `client` over TCP, feeding its `Alt-Svc` to `carrier`.
async fn over_tcp<K: Carrier>(
    carrier: &K,
    origin: &str,
    client: &reqwest::Client,
    request: reqwest::Request,
) -> reqwest::Result<reqwest::Response> {
    let response = client.execute(request).await?;
    carrier.observe(origin, response.headers());
    Ok(response)
}

/// A carried response as `reqwest`'s, at `url`.
fn into_reqwest(delivered: Delivered, url: &reqwest::Url) -> reqwest::Response {
    let (parts, body) = delivered.response.into_parts();
    let mut builder = http::Response::builder()
        .status(parts.status)
        .version(parts.version)
        .url(url.clone());
    if let Some(headers) = builder.headers_mut() {
        *headers = parts.headers;
    }
    match builder.body(body) {
        Ok(response) => reqwest::Response::from(response),
        // Every part came from a response h3 or `reqwest` already accepted.
        Err(_) => unreachable!("a carried response's parts rebuild"),
    }
}

/// Route `request` on `network` through `carrier`, falling back to `client`.
///
/// # Errors
///
/// The `reqwest::Error` the request's TCP send ended with.
#[instrument(
    level = "debug",
    skip_all,
    fields(network = network.0, method = %request.method(), via = tracing::field::Empty)
)]
pub async fn execute_via<K: Carrier>(
    carrier: &K,
    network: PathGeneration,
    client: &reqwest::Client,
    request: reqwest::Request,
) -> reqwest::Result<reqwest::Response> {
    let Some(origin) = origin_key(request.url()) else {
        return client.execute(request).await;
    };
    // The policy first: a request it does not race leaves exactly as it came, uncopied.
    if !carrier.races(&origin, network) {
        tracing::Span::current().record("via", "tcp");
        return over_tcp(carrier, &origin, client, request).await;
    }
    let url = request.url().clone();
    let context = LegContext::new(client.clone());
    let Split { original, quic } = split(request, &context)?;
    let Some(quic) = quic else {
        tracing::debug!("h3: the request is not safe to send twice; it rides TCP");
        tracing::Span::current().record("via", "tcp");
        return over_tcp(carrier, &origin, client, original).await;
    };
    match carrier.send(network, quic, UNBOUNDED).await {
        Ok(delivered) => {
            let via = match delivered.via {
                Via::Quic => "quic",
                Via::Tcp => "tcp",
            };
            tracing::Span::current().record("via", via);
            Ok(into_reqwest(delivered, &url))
        }
        Err(error) => {
            if let Some(failed) = context.take_failure() {
                tracing::debug!(%failed, "h3: the request's TCP leg failed; not re-sent");
                return Err(failed);
            }
            tracing::info!(%error, %origin, "h3: request failed off TCP; re-sending over TCP");
            tracing::Span::current().record("via", "tcp-resend");
            over_tcp(carrier, &origin, client, original).await
        }
    }
}

// ─── The process transport ───────────────────────────────────────────────────

/// The process's HTTP/3 transport.
pub type SdkTransport = H3Transport<SystemClock, ReqwestLeg, NoHttpsRecords>;

/// The network generation requests start on; see [`observe_generation`].
static NETWORK: AtomicU64 = AtomicU64::new(0);

/// Record `generation` as the network new requests start on. A path that changes drops the
/// QUIC connections pooled on the old one, and starts with no "UDP unusable" record.
pub fn observe_generation(generation: PathGeneration) {
    let old = NETWORK.swap(generation.0, Ordering::Relaxed);
    if old != generation.0 {
        tracing::debug!(
            old,
            new = generation.0,
            "h3: requests now start on a new network"
        );
    }
}

/// The network generation new requests start on.
#[must_use]
pub fn current_generation() -> PathGeneration {
    PathGeneration(NETWORK.load(Ordering::Relaxed))
}

/// The proxy variables a `reqwest` client reads from the environment (curl's names, either case).
pub const PROXY_VARIABLES: [&str; 6] = [
    "ALL_PROXY",
    "all_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
];

/// The first of [`PROXY_VARIABLES`] `var` reads a non-empty value for: a proxy the SDK's
/// `reqwest` clients send through. Every SDK client starts from [`crate::net::http_builder`],
/// which keeps `reqwest`'s environment proxies, so this is the proxy its requests ride. QUIC and
/// the TCP leg's reachability dial both connect directly, so a configured proxy means no request
/// races QUIC: it would bypass the proxy. `NO_PROXY` is not consulted, so a host it exempts
/// rides TCP too rather than the SDK re-deciding `reqwest`'s exemptions.
pub fn configured_proxy(var: impl Fn(&str) -> Option<String>) -> Option<&'static str> {
    PROXY_VARIABLES
        .into_iter()
        .find(|name| var(name).is_some_and(|value| !value.trim().is_empty()))
}

/// The process's transport, built once: webpki roots, default windows and bounds. `None` when a
/// proxy is configured ([`configured_proxy`]) or the transport cannot be configured, and every
/// request then rides TCP.
pub fn transport() -> Option<&'static SdkTransport> {
    static TRANSPORT: OnceLock<Option<SdkTransport>> = OnceLock::new();
    TRANSPORT
        .get_or_init(|| {
            if let Some(variable) = configured_proxy(|name| std::env::var(name).ok()) {
                tracing::info!(
                    variable,
                    "h3: a proxy is configured; QUIC would bypass it, so every request rides TCP"
                );
                return None;
            }
            let built = H3Dialer::new(&H3Config::default())
                .map_err(|e| e.to_string())
                .and_then(|dialer| {
                    let client = crate::net::http_client().map_err(|e| e.to_string())?;
                    Ok(H3Transport::new(
                        Arc::new(H3Policy::for_build(SystemClock::new())),
                        dialer,
                        ReqwestLeg::new(client),
                        NoHttpsRecords,
                    ))
                });
            match built {
                Ok(transport) => Some(transport),
                Err(error) => {
                    tracing::warn!(%error, "h3: no HTTP/3 transport; every request rides TCP");
                    None
                }
            }
        })
        .as_ref()
}

/// Send `request` on `client`, routed through the process's transport on the current network.
///
/// # Errors
///
/// The `reqwest::Error` the request's TCP send ended with.
pub async fn execute(
    client: &reqwest::Client,
    request: reqwest::Request,
) -> reqwest::Result<reqwest::Response> {
    match transport() {
        Some(transport) => execute_via(transport, current_generation(), client, request).await,
        None => client.execute(request).await,
    }
}

/// The generated client's transport under `http3`: every operation goes through [`execute`].
#[derive(Debug, Clone)]
pub struct RoutedBackend {
    client: reqwest::Client,
}

impl RoutedBackend {
    /// A backend sending on `client`.
    #[must_use]
    pub fn new(client: reqwest::Client) -> Self {
        Self { client }
    }
}

impl crate::rest::HttpBackend for RoutedBackend {
    fn execute(&self, request: reqwest::Request) -> crate::rest::ExecuteFuture<'_> {
        let client = self.client.clone();
        Box::pin(async move {
            execute(&client, request)
                .await
                .map_err(crate::rest::TransportError::new)
        })
    }
}

#[cfg(test)]
mod tests;
