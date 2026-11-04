//! The production [`TcpLeg`]: the SDK's HTTP/1.1 `reqwest` client, as the leg the QUIC dial
//! races (networking doc, HTTP Transport). Compiled only with the off-by-default `http3` feature.
//!
//! `reqwest` dials inside `send` and exposes no connect-only step, so the race observes a
//! **reachability dial** instead: a bounded TCP connect to the origin's host and port
//! ([`probe`]), closed as soon as it connects. It proves the TCP path answers, which is all the
//! race asks of it; the exchange that follows rides `reqwest`'s own pooled connection. Nothing
//! here speaks HTTP over the probe's socket, so the SDK still does not reimplement the HTTP
//! connector (networking doc, Packet Marking). The dial connects directly, as QUIC does, so it
//! never runs while a proxy is configured: the router then builds no transport
//! ([`super::route::configured_proxy`]).
//!
//! An exchange runs on the `reqwest::Client` its request names in a [`LegContext`] extension —
//! the caller's, so its pool and default headers are the ones the request would have ridden —
//! and reports a `reqwest` failure back through it, so the router can hand the genuine error to
//! the retry engine instead of re-sending a request that already rode TCP.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use tracing::instrument;

use super::transport::{BoxError, Origin, TcpLeg, TransportError};
use crate::net::{DIAL_CONNECT_TIMEOUT, ExchangeProgress, PacedBody, RequestBounds, within_bounds};

/// The request extension an exchange reads its client from, and reports a `reqwest` failure
/// to. Cheap to clone: every clone shares the one failure slot.
#[derive(Debug, Clone)]
pub struct LegContext {
    client: reqwest::Client,
    failure: Arc<Mutex<Option<reqwest::Error>>>,
}

impl LegContext {
    /// A context executing on `client`, with no failure recorded.
    #[must_use]
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            failure: Arc::new(Mutex::new(None)),
        }
    }

    /// The `reqwest` failure an exchange under this context recorded, taken out of it.
    #[must_use]
    pub fn take_failure(&self) -> Option<reqwest::Error> {
        self.failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    pub(crate) fn record(&self, error: reqwest::Error) {
        *self.failure.lock().unwrap_or_else(PoisonError::into_inner) = Some(error);
    }
}

/// The HTTP/1.1 leg over `reqwest`.
#[derive(Debug, Clone)]
pub struct ReqwestLeg {
    /// The client an exchange runs on when its request carries no [`LegContext`].
    client: reqwest::Client,
    dial_timeout: Duration,
}

impl ReqwestLeg {
    /// A leg falling back to `client`, dialing under [`DIAL_CONNECT_TIMEOUT`].
    #[must_use]
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            dial_timeout: DIAL_CONNECT_TIMEOUT,
        }
    }

    /// The same leg with its reachability dial bounded by `timeout`.
    #[must_use]
    pub fn with_dial_timeout(mut self, timeout: Duration) -> Self {
        self.dial_timeout = timeout;
        self
    }
}

/// Connect to `host`:`port` over TCP within `timeout`, then close: the reachability dial.
///
/// # Errors
///
/// The resolver's or the connect's error, or a timeout.
#[instrument(level = "debug", err(Display))]
pub async fn probe(host: &str, port: u16, timeout: Duration) -> Result<(), BoxError> {
    let connect = tokio::net::TcpStream::connect((host, port));
    match tokio::time::timeout(timeout, connect).await {
        Ok(Ok(stream)) => {
            tracing::trace!(peer = ?stream.peer_addr().ok(), "h3: TCP reachable");
            Ok(())
        }
        Ok(Err(error)) => Err(Box::new(error)),
        Err(_) => Err("the TCP dial timed out".into()),
    }
}

/// Why an exchange over `reqwest` ended without a response.
enum Failure {
    Http(reqwest::Error),
    Cut(crate::net::RequestCut),
    Malformed(String),
}

impl From<reqwest::Error> for Failure {
    fn from(error: reqwest::Error) -> Self {
        Self::Http(error)
    }
}

/// Execute `request` on `client` under `bounds`, reading the whole response body, every step
/// reported to `progress`.
async fn read_whole(
    client: &reqwest::Client,
    request: reqwest::Request,
    bounds: RequestBounds,
    progress: &ExchangeProgress,
) -> Result<http::Response<Bytes>, Failure> {
    let mut response = within_bounds(bounds, progress, client.execute(request))
        .await
        .map_err(Failure::Cut)??;
    progress.on_received();
    let mut head = http::Response::builder()
        .status(response.status())
        .version(response.version());
    if let Some(headers) = head.headers_mut() {
        *headers = response.headers().clone();
    }
    let mut body = BytesMut::new();
    while let Some(piece) = within_bounds(bounds, progress, response.chunk())
        .await
        .map_err(Failure::Cut)??
    {
        progress.on_received();
        body.extend_from_slice(&piece);
    }
    head.body(body.freeze())
        .map_err(|e| Failure::Malformed(e.to_string()))
}

impl TcpLeg for ReqwestLeg {
    type Conn = ();

    async fn dial(&self, origin: &Origin) -> Result<(), BoxError> {
        probe(origin.host(), origin.port(), self.dial_timeout).await
    }

    /// Rebuild `request` as a `reqwest` request — its body paced into the request's
    /// [`ExchangeProgress`] where it carries one — and run it on the [`LegContext`]'s client.
    #[instrument(level = "debug", skip_all, fields(method = %request.method()))]
    async fn exchange(
        &self,
        (): (),
        request: http::Request<Bytes>,
        bounds: RequestBounds,
    ) -> Result<http::Response<Bytes>, TransportError> {
        let context = request.extensions().get::<LegContext>().cloned();
        let paced = request.extensions().get::<ExchangeProgress>().cloned();
        let client = context
            .as_ref()
            .map_or_else(|| self.client.clone(), |c| c.client.clone());
        let (parts, bytes) = request.into_parts();
        let body: reqwest::Body = match &paced {
            Some(progress) => PacedBody::new(bytes, progress.clone()).into(),
            None => bytes.into(),
        };
        let request = reqwest::Request::try_from(http::Request::from_parts(parts, body))
            .map_err(|e| TransportError::InvalidRequest(e.to_string()))?;
        let progress = paced.unwrap_or_else(ExchangeProgress::waiting);
        match read_whole(&client, request, bounds, &progress).await {
            Ok(response) => Ok(response),
            Err(Failure::Cut(cut)) => Err(TransportError::Cut(cut)),
            Err(Failure::Malformed(detail)) => Err(TransportError::Exchange(detail)),
            Err(Failure::Http(error)) => {
                let detail = error.to_string();
                if let Some(context) = context {
                    context.record(error);
                }
                Err(TransportError::Exchange(detail))
            }
        }
    }
}

#[cfg(test)]
mod tests;
