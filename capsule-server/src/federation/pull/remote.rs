//! The pull's wire: the generated calls, and the transport that bounds what a peer can send.
//!
//! # Everything that parses is generated
//!
//! [`rest`] is `spargen`'s lowering of the committed contract, narrowed by `build.rs` to the three
//! operations a pull presents a capability to. [`PeerClient`] is orchestration over it: it signs
//! each presentation ([`super::signer`]), picks the operation, and sorts what came back into a
//! [`RemoteError`]. It holds no parser.
//!
//! # A peer is hostile until proven otherwise
//!
//! Every response comes through [`PeerTransport`], which wraps whatever executes the request:
//!
//! - **One origin.** A request whose URL is not the peer's pinned origin (`https://{peer}`) is
//!   never executed. Nothing a peer sends names a URL this server then follows: the generated
//!   calls build every URL from that origin and an operation's path, the production client
//!   follows no redirect ([`egress_client`]), and a `3xx` is a status the contract does not
//!   declare, so it is [`RemoteError::Malformed`].
//! - **Bounded bodies.** Each call names the most bytes it will read. A declared
//!   `Content-Length` past it is refused before a byte is read, and a body that grows past it is
//!   abandoned at the byte that crossed. The generated runtime reads a success body whole to
//!   decode it, so this is the only place the bound can sit.
//! - **Bounded time.** The production client cuts a connect at [`CONNECT_TIMEOUT`] and a
//!   request at [`REQUEST_TIMEOUT`]; the worker abandons the whole pull at its deadline.
//!
//! A refusal by the transport is reported to the generated code as a status no operation
//! declares, and to the caller as the [`Violation`] it was, so it is never mistaken for
//! something the peer said.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use reqwest::Url;
use tracing::instrument;

use super::signer::{RequestSigner, fresh_nonce, signature_header};
use crate::federation::PeerId;
use crate::store::{AlbumId, Clock};

/// The generated client: the three operations a federated pull calls, and their types.
///
/// Do not edit: `build.rs` regenerates it from `openapi.json` on every build. The allows are the
/// generated text's, as they are on `capsule_sdk::rest`: its doc comments carry this crate's
/// handler rustdoc, whose intra-doc links do not resolve from inside the module.
#[allow(
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    missing_docs,
    unreachable_pub,
    dead_code,
    rustdoc::broken_intra_doc_links,
    rustdoc::private_intra_doc_links,
    rustdoc::invalid_html_tags,
    rustdoc::bare_urls
)]
pub mod rest {
    include!(concat!(env!("OUT_DIR"), "/peer_client.rs"));
}

/// The security scheme the contract registers both bearer kinds under.
const BEARER_SCHEME: &str = "bearer";

/// The longest a connection to a peer may take to open.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The longest one request to a peer may take, body included.
///
/// A minute: a sync page or a metadata blob is at most a few mebibytes, and a peer that cannot
/// deliver one in a minute is not one a pull waits on.
pub const REQUEST_TIMEOUT: Duration = Duration::from_mins(1);

/// The status a refusal by [`PeerTransport`] is reported to the generated code under.
///
/// No operation declares it, so the generated call answers [`rest::Error::UnexpectedStatus`] and
/// [`PeerClient`] reads the [`Violation`] the transport recorded instead.
const VIOLATION_STATUS: u16 = 599;

/// The origin every request of a pull from `peer` goes to: `https://{peer}`.
///
/// # Errors
///
/// A description when the peer id is not a host a URL can name, which [`PeerId::parse`] already
/// refuses at every door a peer id enters through.
pub fn origin_of(peer: &PeerId) -> Result<Url, String> {
    Url::parse(&format!("https://{peer}/")).map_err(|error| error.to_string())
}

/// The production HTTP client a pull runs over: rustls, no redirects, HTTPS only, bounded time.
///
/// # Errors
///
/// Whatever reqwest reports when it cannot build a TLS stack.
pub fn egress_client() -> Result<reqwest::Client, reqwest::Error> {
    let builder = reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .user_agent(concat!("capsule-server/", env!("CARGO_PKG_VERSION")));
    #[cfg(feature = "test-egress")]
    let builder = test_egress::apply(builder)?;
    builder.build()
}

/// Where a test's peers answer, and the private CA their TLS fronts present: the one seam
/// between a pull and a peer on loopback. Compiled only with the off-by-default `test-egress`
/// feature, which `capsule-e2e` turns on for its test targets alone.
///
/// A peer id is a bare host name and every pull dials `https://{peer}/` ([`origin_of`]), so a
/// peer standing on `127.0.0.1` behind an ephemeral port cannot be reached as a deployment
/// reaches one. This seam changes the two facts a test cannot supply through DNS and the public
/// roots, and nothing else:
///
/// - **Name resolution.** [`route`](test_egress::route) resolves one host to one socket address
///   (`reqwest::ClientBuilder::resolve`), so `https://alice.test/` connects to the TLS front a
///   case bound. The URL, its scheme and its host are unchanged, so [`PeerTransport`]'s origin
///   pin reads the same values production does.
/// - **Trust anchors.** [`trust_root`](test_egress::trust_root) adds a DER certificate to the roots the client already
///   trusts. The handshake still verifies the front's chain and its name against that host.
///
/// HTTPS-only, no redirects and the time bounds stay as [`egress_client`] sets them. Nothing is
/// registered unless a test registers it, so a build with the feature on and an empty registry
/// builds exactly the production client. The registry is process-wide because the client is
/// built inside [`crate::boot::assemble`], which takes no test collaborator; register before
/// assembling the server that should dial the route.
#[cfg(feature = "test-egress")]
pub mod test_egress {
    use std::net::SocketAddr;
    use std::sync::{Mutex, PoisonError};

    /// Host → address overrides, in registration order.
    static ROUTES: Mutex<Vec<(String, SocketAddr)>> = Mutex::new(Vec::new());
    /// DER certificates trusted beside the client's built-in roots.
    static ROOTS: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());

    /// Resolve `host` to `to` in every egress client built after this call.
    pub fn route(host: &str, to: SocketAddr) {
        tracing::warn!(host, %to, "test egress: a peer host is routed to a fixed address");
        ROUTES
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((host.to_owned(), to));
    }

    /// Trust the DER certificate `der` as a root in every egress client built after this call.
    pub fn trust_root(der: Vec<u8>) {
        tracing::warn!(bytes = der.len(), "test egress: a private root is trusted");
        ROOTS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(der);
    }

    /// `builder` with every registered route and root applied.
    pub(super) fn apply(
        mut builder: reqwest::ClientBuilder,
    ) -> Result<reqwest::ClientBuilder, reqwest::Error> {
        for (host, to) in ROUTES.lock().unwrap_or_else(PoisonError::into_inner).iter() {
            builder = builder.resolve(host, *to);
        }
        for der in ROOTS.lock().unwrap_or_else(PoisonError::into_inner).iter() {
            builder = builder.add_root_certificate(reqwest::Certificate::from_der(der)?);
        }
        Ok(builder)
    }
}

/// What [`PeerTransport`] refused, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    /// A request named a URL off the peer's pinned origin. Never executed.
    OffOrigin {
        /// The URL it named.
        url: String,
    },
    /// The response was larger than the call allows.
    TooLarge {
        /// The most the call reads.
        limit: u64,
    },
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OffOrigin { url } => write!(f, "a request named {url}, off the peer's origin"),
            Self::TooLarge { limit } => {
                write!(f, "the response was larger than the {limit} bytes allowed")
            }
        }
    }
}

/// One call's bound, and the violation the transport recorded for it.
#[derive(Debug, Default)]
struct CallState {
    limit: u64,
    violation: Option<Violation>,
}

/// The pull's transport: pins every request to one origin and bounds every body.
///
/// Wraps the backend that executes the request — the production [`rest::ReqwestBackend`] over
/// [`egress_client`], or a test's scripted peer — so the bounds are the same whichever runs.
#[derive(Debug)]
pub struct PeerTransport {
    inner: Arc<dyn rest::HttpBackend>,
    origin: Url,
    state: Mutex<CallState>,
}

impl PeerTransport {
    /// A transport to `origin`, executing on `inner`.
    pub fn new(inner: Arc<dyn rest::HttpBackend>, origin: Url) -> Self {
        Self {
            inner,
            origin,
            state: Mutex::new(CallState::default()),
        }
    }

    /// Arm the next call: at most `limit` bytes of body, and no violation recorded yet.
    fn arm(&self, limit: u64) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.limit = limit;
        state.violation = None;
    }

    /// The violation the last call recorded, if any.
    fn take_violation(&self) -> Option<Violation> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .violation
            .take()
    }

    fn limit(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .limit
    }

    /// Record `violation` and answer the status no operation declares.
    fn refuse(&self, violation: Violation) -> reqwest::Response {
        tracing::warn!(%violation, origin = %self.origin, "the pull transport refused a response");
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .violation = Some(violation);
        let mut response = http::Response::new(Vec::<u8>::new());
        *response.status_mut() = reqwest::StatusCode::from_u16(VIOLATION_STATUS)
            .unwrap_or(reqwest::StatusCode::BAD_GATEWAY);
        reqwest::Response::from(response)
    }

    /// Whether `url` is on the pinned origin: same scheme, host and port.
    fn on_origin(&self, url: &Url) -> bool {
        url.scheme() == self.origin.scheme()
            && url.host_str() == self.origin.host_str()
            && url.port_or_known_default() == self.origin.port_or_known_default()
    }
}

impl rest::HttpBackend for PeerTransport {
    fn execute(&self, request: reqwest::Request) -> rest::ExecuteFuture<'_> {
        Box::pin(async move {
            if !self.on_origin(request.url()) {
                return Ok(self.refuse(Violation::OffOrigin {
                    url: request.url().to_string(),
                }));
            }
            let limit = self.limit();
            let mut response = self.inner.execute(request).await?;
            if response
                .content_length()
                .is_some_and(|length| length > limit)
            {
                return Ok(self.refuse(Violation::TooLarge { limit }));
            }
            let status = response.status();
            let version = response.version();
            let headers = response.headers().clone();
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(rest::TransportError::new)? {
                if (body.len() as u64).saturating_add(chunk.len() as u64) > limit {
                    return Ok(self.refuse(Violation::TooLarge { limit }));
                }
                body.extend_from_slice(&chunk);
            }
            let mut rebuilt = http::Response::new(body);
            *rebuilt.status_mut() = status;
            *rebuilt.version_mut() = version;
            *rebuilt.headers_mut() = headers;
            Ok(reqwest::Response::from(rebuilt))
        })
    }
}

/// Why a call to a peer did not answer what was asked.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RemoteError {
    /// The peer could not be reached, timed out, or failed on its side (`5xx`). Not the peer's
    /// input being wrong: a pull that meets it backs off and nothing more.
    #[error("the peer could not be reached: {0}")]
    Unreachable(String),
    /// The peer answered a refusal the contract declares.
    #[error("the peer refused with {status}{}", code.as_deref().map(|code| format!(" {code}")).unwrap_or_default())]
    Refused {
        /// The status.
        status: u16,
        /// The stable `error.*` code, where the answer carried one.
        code: Option<String>,
    },
    /// The peer answered something the contract does not allow: an undeclared status (a
    /// redirect among them), a body that does not decode, a body past the call's bound, or a
    /// request the transport would not send. The peer's input failed validation.
    #[error("the peer's answer is malformed: {0}")]
    Malformed(String),
}

/// One peer, as a pull talks to it: the generated calls, signed and bounded.
///
/// One call at a time: the transport's bound is armed per call, and a pull's calls are
/// sequential, so the worker builds one client per pull.
#[derive(Debug)]
pub struct PeerClient {
    transport: Arc<PeerTransport>,
    base: String,
    signer: Arc<dyn RequestSigner>,
    clock: Arc<dyn Clock>,
    gate: tokio::sync::Mutex<()>,
}

impl PeerClient {
    /// A client for the peer at `origin`, executing on `backend`, signing with `signer`, dating
    /// each signature by `clock`.
    pub fn new(
        backend: Arc<dyn rest::HttpBackend>,
        origin: Url,
        signer: Arc<dyn RequestSigner>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let base = origin.as_str().trim_end_matches('/').to_owned();
        Self {
            transport: Arc::new(PeerTransport::new(backend, origin)),
            base,
            signer,
            clock,
            gate: tokio::sync::Mutex::new(()),
        }
    }

    /// A generated client presenting `token`.
    fn client(&self, token: &str) -> Result<rest::Client, RemoteError> {
        let backend: Arc<dyn rest::HttpBackend> = self.transport.clone();
        rest::Client::with_backend(backend, &self.base)
            .map(|client| {
                client.with_credential(
                    BEARER_SCHEME,
                    rest::Credential::Bearer(token.to_owned().into()),
                )
            })
            .map_err(|error| RemoteError::Malformed(format!("no client for the origin: {error}")))
    }

    /// The signature header for presenting `token` on `method path`, now.
    fn sign(&self, method: &str, path: &str, token: &str) -> Result<String, RemoteError> {
        signature_header(
            self.signer.as_ref(),
            method,
            path,
            token,
            self.clock.now(),
            &fresh_nonce(),
        )
        .map_err(|error| {
            RemoteError::Malformed(format!("the request could not be signed: {error}"))
        })
    }

    /// One page of `album`'s feed after `cursor` (from the start when `None`), reading at most
    /// `limit` bytes.
    ///
    /// # Errors
    ///
    /// [`RemoteError`], by what went wrong.
    #[instrument(skip(self, token, cursor), fields(base = %self.base, album = %album))]
    pub async fn sync_page(
        &self,
        token: &str,
        album: &AlbumId,
        cursor: Option<&str>,
        page_size: u32,
        limit: u64,
    ) -> Result<rest::types::SyncPageResponse, RemoteError> {
        let _gate = self.gate.lock().await;
        let client = self.client(token)?;
        let params = rest::SyncFeedParams {
            x_capsule_request_signature: Some(self.sign("GET", "/v1/sync", token)?),
            album_id: Some(album.as_str().to_owned()),
            cursor: cursor
                .filter(|cursor| !cursor.is_empty())
                .map(str::to_owned),
            page_size: Some(i64::from(page_size)),
            ..rest::SyncFeedParams::default()
        };
        self.transport.arm(limit);
        let result = client
            .sync_feed(capsule_core::crypto::primitives::PROTOCOL_VERSION, params)
            .await;
        match result {
            Ok(page) => Ok(page.into_inner()),
            Err(error) => Err(self.classify(error, |refusal| {
                use rest::SyncFeedError as E;
                match refusal {
                    E::Status400(problem) => (400, Some(problem.code.clone())),
                    E::Status401(problem) => (401, Some(problem.code.clone())),
                    E::Status403(problem) => (403, Some(problem.code.clone())),
                    E::Status409(problem) => (409, Some(problem.code.clone())),
                    E::Status413(problem) => (413, Some(problem.code.clone())),
                    E::Status429(problem) => (429, Some(problem.code.clone())),
                    E::Status500(problem) => (500, Some(problem.code.clone())),
                }
            })),
        }
    }

    /// The blob at `hash`, whole, reading at most `limit` bytes.
    ///
    /// The bytes are not checked against `hash` here: that is validation, and it is timed and
    /// charged as validation is.
    ///
    /// # Errors
    ///
    /// [`RemoteError`], by what went wrong.
    #[instrument(skip(self, token), fields(base = %self.base))]
    pub async fn blob(&self, token: &str, hash: &str, limit: u64) -> Result<Vec<u8>, RemoteError> {
        let _gate = self.gate.lock().await;
        // The address is the one place a peer's value enters a URL. It goes into the path only
        // once it is 64 lowercase hex digits, so it names a resource on the pinned origin and
        // nothing else.
        if hash.len() != 64
            || !hash
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(RemoteError::Malformed(
                "a blob address is not 64 lowercase hex digits".into(),
            ));
        }
        let client = self.client(token)?;
        let params = rest::GetBlobParams {
            x_capsule_request_signature: Some(self.sign(
                "GET",
                &format!("/v1/blob/{hash}"),
                token,
            )?),
            ..rest::GetBlobParams::default()
        };
        self.transport.arm(limit);
        let result = client
            .get_blob(
                hash.to_owned(),
                capsule_core::crypto::primitives::PROTOCOL_VERSION,
                params,
            )
            .await;
        match result {
            Ok(response) => Ok(match response.into_inner() {
                rest::GetBlobResponse::Status200(bytes)
                | rest::GetBlobResponse::Status206(bytes) => bytes.to_vec(),
            }),
            Err(error) => Err(self.classify(error, |refusal| {
                use rest::GetBlobError as E;
                match refusal {
                    E::Status304 => (304, None),
                    E::Status412 => (412, None),
                    E::Status400(problem) => (400, Some(problem.code.clone())),
                    E::Status401(problem) => (401, Some(problem.code.clone())),
                    E::Status403(problem) => (403, Some(problem.code.clone())),
                    E::Status404(problem) => (404, Some(problem.code.clone())),
                    E::Status409(problem) => (409, Some(problem.code.clone())),
                    E::Status410(problem) => (410, Some(problem.code.clone())),
                    E::Status413(problem) => (413, Some(problem.code.clone())),
                    E::Status416(problem) => (416, Some(problem.code.clone())),
                    E::Status429(problem) => (429, Some(problem.code.clone())),
                    E::Status500(problem) => (500, Some(problem.code.clone())),
                }
            })),
        }
    }

    /// Exchange `token` for its successor, reading at most `limit` bytes.
    ///
    /// The successor is **not** trusted here: the caller verifies it against the issuer's
    /// pinned key before keeping it.
    ///
    /// # Errors
    ///
    /// [`RemoteError`], by what went wrong.
    #[instrument(skip(self, token), fields(base = %self.base))]
    pub async fn refresh(
        &self,
        token: &str,
        limit: u64,
    ) -> Result<rest::types::RefreshedCapabilityResponse, RemoteError> {
        let _gate = self.gate.lock().await;
        let client = self.client(token)?;
        let params = rest::RefreshCapabilityParams {
            x_capsule_request_signature: Some(self.sign(
                "POST",
                "/v1/federation/capabilities/refresh",
                token,
            )?),
            ..rest::RefreshCapabilityParams::default()
        };
        self.transport.arm(limit);
        let result = client
            .refresh_capability(capsule_core::crypto::primitives::PROTOCOL_VERSION, params)
            .await;
        match result {
            Ok(refreshed) => Ok(refreshed.into_inner()),
            Err(error) => Err(self.classify(error, |refusal| {
                use rest::RefreshCapabilityError as E;
                match refusal {
                    E::Status400(problem) => (400, Some(problem.code.clone())),
                    E::Status401(problem) => (401, Some(problem.code.clone())),
                    E::Status403(problem) => (403, Some(problem.code.clone())),
                    E::Status404(problem) => (404, Some(problem.code.clone())),
                    E::Status409(problem) => (409, Some(problem.code.clone())),
                    E::Status413(problem) => (413, Some(problem.code.clone())),
                    // Refused as TLS early data: a retryable refusal, as 429 is, so the pull
                    // backs off and keeps its current capability.
                    E::Status425(problem) => (425, Some(problem.code.clone())),
                    E::Status426(problem) => (426, Some(problem.code.clone())),
                    E::Status429(problem) => (429, Some(problem.code.clone())),
                    E::Status500(problem) => (500, Some(problem.code.clone())),
                }
            })),
        }
    }

    /// Sort a generated error into a [`RemoteError`], preferring what the transport recorded.
    ///
    /// Exhaustive over the generated taxonomy on purpose: a class spargen adds later is a compile
    /// error here rather than a silent default.
    fn classify<E>(
        &self,
        error: rest::Error<E>,
        declared: impl FnOnce(E) -> (u16, Option<String>),
    ) -> RemoteError {
        if let Some(violation) = self.transport.take_violation() {
            return RemoteError::Malformed(violation.to_string());
        }
        match error {
            rest::Error::Api(response) => {
                let (status, code) = declared(response.into_inner());
                if status >= 500 {
                    RemoteError::Unreachable(format!(
                        "the peer failed with {status}{}",
                        code.map(|code| format!(" {code}")).unwrap_or_default()
                    ))
                } else {
                    RemoteError::Refused { status, code }
                }
            }
            rest::Error::UnexpectedStatus { status, .. } if status.is_server_error() => {
                RemoteError::Unreachable(format!("the peer failed with {status}"))
            }
            rest::Error::UnexpectedStatus { status, .. } => RemoteError::Malformed(format!(
                "the peer answered {status}, which the contract does not declare"
            )),
            rest::Error::Decode { path, .. } => {
                RemoteError::Malformed(format!("the body does not decode at {path}"))
            }
            rest::Error::Redirect(error) => {
                RemoteError::Malformed(format!("the peer redirected: {error}"))
            }
            rest::Error::RequestConstruction(error) => {
                RemoteError::Malformed(format!("the request could not be built: {error}"))
            }
            rest::Error::Transport(error) | rest::Error::InterruptedBody(error) => {
                RemoteError::Unreachable(error.to_string())
            }
            rest::Error::Timeout(kind) => RemoteError::Unreachable(format!("timed out: {kind:?}")),
            rest::Error::Protocol(error) => RemoteError::Unreachable(error.to_string()),
        }
    }
}

#[cfg(test)]
mod tests;
