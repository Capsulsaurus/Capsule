//! The HTTP/3 transport adapter (networking doc, HTTP Transport): the half of opportunistic
//! HTTP/3 that dials, over quinn, h3 and h3-quinn. Compiled only with the off-by-default `http3`
//! cargo feature.
//!
//! [`H3Transport::send`] takes the [`H3Policy`] answer once, when a request starts, and carries
//! the request on exactly one connection:
//!
//! - **`Tcp`** — the request rides the [`TcpLeg`], the HTTP/1.1 client this transport races.
//! - **`Race`** — a pooled QUIC connection for the origin on the current network carries it;
//!   otherwise a QUIC dial to the advertised endpoint races a TCP dial ([`race_dial`]) and the
//!   request rides whichever connects first. A QUIC dial that loses the race keeps running in
//!   the background: it is pooled for the next request when it connects, and records UDP
//!   unusable on the network ([`UdpFailure::DialFailed`]) when it fails while TCP succeeded.
//! - **Progress.** While a request rides QUIC, a watchdog reads the connection's received
//!   datagrams. QUIC keep-alives make a live path answer at least every quarter of the stall
//!   bound, so a path silent for half of it has stopped making progress: the request fails with
//!   [`TransportError::PathStalled`], UDP is recorded unusable ([`UdpFailure::Stalled`]), and the
//!   retry engine's re-send rides TCP. Nothing moves mid-request.
//! - **Discovery.** Every response's `Alt-Svc` header, on either transport, feeds
//!   [`H3Policy::observe_alt_svc`] ([`observe_response`]); an origin with no live advertisement
//!   asks the [`HttpsResolver`] for HTTPS (SVCB) records first.
//! - **Windows.** The QUIC flow-control windows and the UDP socket buffers are sized by
//!   [`size_window`] ([`Windows`]).
//! - **No early data.** TLS 1.3 only, `enable_early_data` off, and a dial always waits for the
//!   full handshake: [`super::early_data_allowed`] refuses every method.

use std::collections::HashMap;
use std::future::{Future, poll_fn};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::{Pin, pin};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::Poll;
use std::time::Duration;

use bytes::{Buf, Bytes, BytesMut};
use tracing::instrument;

use super::{
    DEFAULT_WINDOW_BUDGET, H3_ALPN, H3Endpoint, H3Policy, HttpsRecord, TcpReason, TransportChoice,
    UdpFailure, WindowBudget, size_window,
};
use crate::net::path::PathGeneration;
use crate::net::{
    DIAL_CONNECT_TIMEOUT, ExchangeProgress, MonotonicClock, PACED_PIECE, RequestBounds, RequestCut,
    within_bounds,
};

/// A boxed error from a leg the caller supplies.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The bandwidth the default windows are sized for: 100 Mbit/s.
pub const DEFAULT_TARGET_BANDWIDTH: u64 = 12_500_000;

/// The round trip the default windows are sized for.
pub const DEFAULT_TARGET_RTT: Duration = Duration::from_millis(100);

/// The connections the default windows share [`DEFAULT_WINDOW_BUDGET`] among.
pub const DEFAULT_CONNECTIONS: u32 = 4;

/// How long a pooled QUIC connection may go unused before the next send closes it, so an idle
/// connection's keep-alives do not run on.
pub const POOL_IDLE: Duration = Duration::from_secs(30);

/// The shortest QUIC keep-alive interval, however short the stall bound.
const MIN_KEEP_ALIVE: Duration = Duration::from_millis(50);

// ─── Errors ──────────────────────────────────────────────────────────────────

/// Why [`H3Transport::send`] carried no response.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// The request has no absolute `http`/`https` URI to address.
    #[error("the request is not addressable: {0}")]
    InvalidRequest(String),
    /// The HTTP/3 client could not be configured.
    #[error("the HTTP/3 client could not be configured: {0}")]
    Config(String),
    /// No connection could be dialed.
    #[error(
        "no connection could be dialed (TCP: {tcp}; QUIC: {})",
        quic.as_deref().unwrap_or("not attempted")
    )]
    Dial {
        /// Why the QUIC dial failed, or `None` when none was attempted.
        quic: Option<String>,
        /// Why the TCP dial failed.
        tcp: String,
    },
    /// A QUIC dial failed (reported by [`H3Dialer::dial`] alone; [`H3Transport::send`] folds it
    /// into [`TransportError::Dial`]).
    #[error("the QUIC dial failed: {0}")]
    QuicDial(String),
    /// The request was cut under its [`RequestBounds`].
    #[error("{0}")]
    Cut(RequestCut),
    /// The QUIC path stopped making progress mid-request. UDP is recorded unusable on the
    /// network, so the re-send rides TCP.
    #[error("the QUIC path stopped making progress")]
    PathStalled,
    /// The exchange failed after its connection was dialed.
    #[error("the exchange failed: {0}")]
    Exchange(String),
}

// ─── Origin ──────────────────────────────────────────────────────────────────

/// The scheme, host and port a request is addressed to: the key the [`H3Policy`] holds its
/// advertisements under.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Origin {
    secure: bool,
    host: String,
    port: u16,
}

impl Origin {
    /// An `https` origin. `host` carries no IPv6 brackets.
    #[must_use]
    pub fn https(host: impl Into<String>, port: u16) -> Self {
        Self {
            secure: true,
            host: host.into(),
            port,
        }
    }

    /// The origin of an absolute `http` or `https` URI, its port defaulted from the scheme.
    ///
    /// # Errors
    ///
    /// [`TransportError::InvalidRequest`] for a relative URI or another scheme.
    pub fn from_uri(uri: &http::Uri) -> Result<Self, TransportError> {
        let secure = match uri.scheme_str() {
            Some("https") => true,
            Some("http") => false,
            other => {
                return Err(TransportError::InvalidRequest(format!(
                    "scheme {other:?} is neither http nor https"
                )));
            }
        };
        let host = uri
            .host()
            .ok_or_else(|| TransportError::InvalidRequest("the URI names no host".to_owned()))?;
        let host = host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(host);
        Ok(Self {
            secure,
            host: host.to_owned(),
            port: uri.port_u16().unwrap_or(if secure { 443 } else { 80 }),
        })
    }

    /// Whether the scheme is `https`, the only one HTTP/3 carries.
    #[must_use]
    pub fn is_secure(&self) -> bool {
        self.secure
    }

    /// The host, without IPv6 brackets: the TLS server name.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The port.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// `scheme://host:port`, the policy's key.
    #[must_use]
    pub fn key(&self) -> String {
        let scheme = if self.secure { "https" } else { "http" };
        if self.host.contains(':') {
            format!("{scheme}://[{}]:{}", self.host, self.port)
        } else {
            format!("{scheme}://{}:{}", self.host, self.port)
        }
    }
}

// ─── Discovery ───────────────────────────────────────────────────────────────

/// Feed every `Alt-Svc` header of a response from `origin` to `policy`. Several headers read as
/// one comma-joined value (RFC 9110 §5.3); a response with none changes nothing, since an
/// advertisement holds until it expires or a `clear` withdraws it.
pub fn observe_response<C: MonotonicClock>(
    policy: &H3Policy<C>,
    origin: &str,
    headers: &http::HeaderMap,
) {
    let values: Vec<&str> = headers
        .get_all(http::header::ALT_SVC)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .collect();
    if !values.is_empty() {
        policy.observe_alt_svc(origin, &values.join(", "));
    }
}

/// A resolver that exposes HTTPS (SVCB) DNS records, the second discovery source.
///
/// The platform resolvers the SDK dials through expose none, so the production value is
/// [`NoHttpsRecords`]; a platform shell that can read them supplies its own.
pub trait HttpsResolver: Send + Sync + 'static {
    /// The HTTPS records published for `host`; empty when there are none or the lookup failed.
    /// Asked at every request whose origin has no live advertisement, so an implementation
    /// answers from its cache within the records' TTL.
    fn https_records(&self, host: &str) -> impl Future<Output = Vec<HttpsRecord>> + Send;
}

/// The resolver that knows no HTTPS records: `Alt-Svc` is the only discovery source.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoHttpsRecords;

impl HttpsResolver for NoHttpsRecords {
    async fn https_records(&self, _host: &str) -> Vec<HttpsRecord> {
        Vec::new()
    }
}

// ─── Windows ─────────────────────────────────────────────────────────────────

/// The flow-control windows and socket buffers of one QUIC connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Windows {
    /// The receive window of one stream: one request's response body.
    pub stream_receive: u64,
    /// The receive window of the whole connection.
    pub connection_receive: u64,
    /// The most unacknowledged bytes the connection sends.
    pub send: u64,
    /// `SO_RCVBUF` on the connection's UDP socket.
    pub socket_receive: usize,
    /// `SO_SNDBUF` on the connection's UDP socket.
    pub socket_send: usize,
}

impl Windows {
    /// Every window at `window` bytes: one request rides one stream, so a stream may use the
    /// whole connection's share.
    #[must_use]
    pub fn of(window: u64) -> Self {
        let socket = usize::try_from(window).unwrap_or(usize::MAX);
        Self {
            stream_receive: window,
            connection_receive: window,
            send: window,
            socket_receive: socket,
            socket_send: socket,
        }
    }

    /// The windows [`size_window`] gives `bandwidth` bytes per second over `rtt`, for one of
    /// `connections` connections within `budget`.
    #[must_use]
    pub fn sized(bandwidth: u64, rtt: Duration, connections: u32, budget: WindowBudget) -> Self {
        Self::of(size_window(bandwidth, rtt, connections, budget))
    }
}

impl Default for Windows {
    /// [`DEFAULT_TARGET_BANDWIDTH`] over [`DEFAULT_TARGET_RTT`], one of [`DEFAULT_CONNECTIONS`]
    /// within [`DEFAULT_WINDOW_BUDGET`].
    fn default() -> Self {
        Self::sized(
            DEFAULT_TARGET_BANDWIDTH,
            DEFAULT_TARGET_RTT,
            DEFAULT_CONNECTIONS,
            DEFAULT_WINDOW_BUDGET,
        )
    }
}

// ─── Configuration ───────────────────────────────────────────────────────────

/// How the HTTP/3 client dials.
#[derive(Debug, Clone)]
pub struct H3Config {
    /// The trust anchors a server certificate must chain to.
    pub roots: Arc<rustls::RootCertStore>,
    /// The windows every connection opens with.
    pub windows: Windows,
    /// The stall bound sets the path watchdog and keep-alives; see [`H3Config::keep_alive`].
    pub bounds: RequestBounds,
    /// The longest a QUIC dial may take, handshake included.
    pub dial_timeout: Duration,
}

impl Default for H3Config {
    /// The webpki roots (the ones `reqwest`'s `rustls-tls` trusts), default [`Windows`] and
    /// [`RequestBounds`], and [`DIAL_CONNECT_TIMEOUT`].
    fn default() -> Self {
        Self {
            roots: Arc::new(rustls::RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            }),
            windows: Windows::default(),
            bounds: RequestBounds::default(),
            dial_timeout: DIAL_CONNECT_TIMEOUT,
        }
    }
}

impl H3Config {
    /// How long a request on a QUIC path may hear nothing before the path counts as stalled:
    /// half the stall bound, so the path is judged before the request's own stall cut.
    #[must_use]
    pub fn path_silence(&self) -> Duration {
        self.bounds.stall / 2
    }

    /// The QUIC keep-alive interval: a quarter of the stall bound, so a live path answers at
    /// least twice within [`H3Config::path_silence`].
    #[must_use]
    pub fn keep_alive(&self) -> Duration {
        (self.bounds.stall / 4).max(MIN_KEEP_ALIVE)
    }

    /// The QUIC idle timeout: the stall bound. Keep-alives keep a live connection open.
    #[must_use]
    pub fn idle_timeout(&self) -> Duration {
        self.bounds.stall.max(self.keep_alive() * 2)
    }
}

/// The rustls client config the QUIC handshake runs: the pinned `ring` provider, TLS 1.3 only,
/// ALPN `h3`, and no early data.
///
/// # Errors
///
/// [`TransportError::Config`] if the provider refuses TLS 1.3.
pub fn client_tls(
    roots: Arc<rustls::RootCertStore>,
) -> Result<rustls::ClientConfig, TransportError> {
    let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .map_err(|e| TransportError::Config(e.to_string()))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    tls.alpn_protocols = vec![H3_ALPN.as_bytes().to_vec()];
    tls.enable_early_data = false;
    Ok(tls)
}

/// The QUIC transport parameters for `config`: its [`Windows`], idle timeout and keep-alive.
///
/// # Errors
///
/// [`TransportError::Config`] for an idle timeout QUIC cannot encode.
pub fn transport_config(config: &H3Config) -> Result<quinn::TransportConfig, TransportError> {
    let varint = |value: u64| quinn::VarInt::from_u64(value).unwrap_or(quinn::VarInt::MAX);
    let idle = quinn::IdleTimeout::try_from(config.idle_timeout())
        .map_err(|e| TransportError::Config(e.to_string()))?;
    let mut transport = quinn::TransportConfig::default();
    transport
        .stream_receive_window(varint(config.windows.stream_receive))
        .receive_window(varint(config.windows.connection_receive))
        .send_window(config.windows.send)
        .max_idle_timeout(Some(idle))
        .keep_alive_interval(Some(config.keep_alive()));
    Ok(transport)
}

/// A nonblocking UDP socket for a connection to `peer`, its buffers sized to `windows`. A
/// platform that refuses a buffer size keeps its default; the connection still dials.
fn bind_udp(peer: SocketAddr, windows: &Windows) -> std::io::Result<std::net::UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};

    let (domain, local): (Domain, SocketAddr) = if peer.is_ipv4() {
        (Domain::IPV4, (Ipv4Addr::UNSPECIFIED, 0).into())
    } else {
        (Domain::IPV6, (Ipv6Addr::UNSPECIFIED, 0).into())
    };
    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    if let Err(error) = socket.set_recv_buffer_size(windows.socket_receive) {
        tracing::debug!(%error, size = windows.socket_receive, "h3: SO_RCVBUF refused");
    }
    if let Err(error) = socket.set_send_buffer_size(windows.socket_send) {
        tracing::debug!(%error, size = windows.socket_send, "h3: SO_SNDBUF refused");
    }
    socket.set_nonblocking(true)?;
    socket.bind(&local.into())?;
    Ok(socket.into())
}

// ─── Dialing ─────────────────────────────────────────────────────────────────

/// Dials QUIC connections and opens HTTP/3 on them. Cheap to clone.
#[derive(Debug, Clone)]
pub struct H3Dialer {
    inner: Arc<DialerInner>,
}

#[derive(Debug)]
struct DialerInner {
    client: quinn::ClientConfig,
    windows: Windows,
    dial_timeout: Duration,
    path_silence: Duration,
}

impl H3Dialer {
    /// A dialer for `config`.
    ///
    /// # Errors
    ///
    /// [`TransportError::Config`] if the TLS or transport configuration is refused.
    pub fn new(config: &H3Config) -> Result<Self, TransportError> {
        let tls = client_tls(config.roots.clone())?;
        let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls)
            .map_err(|e| TransportError::Config(e.to_string()))?;
        let mut client = quinn::ClientConfig::new(Arc::new(crypto));
        client.transport_config(Arc::new(transport_config(config)?));
        Ok(Self {
            inner: Arc::new(DialerInner {
                client,
                windows: config.windows,
                dial_timeout: config.dial_timeout,
                path_silence: config.path_silence(),
            }),
        })
    }

    /// Dial `addr` as `server_name` and open HTTP/3 on it. Each connection has its own UDP
    /// socket, so its buffers are its own and it dies with its path. The full handshake
    /// completes before any request: no early data.
    ///
    /// # Errors
    ///
    /// [`TransportError::QuicDial`] when the socket, handshake or HTTP/3 setup fails or the dial
    /// outlasts its timeout.
    #[instrument(level = "debug", skip(self), err(Display))]
    pub async fn dial(
        &self,
        addr: SocketAddr,
        server_name: &str,
    ) -> Result<H3Connection, TransportError> {
        let dial = |e: &dyn std::fmt::Display| TransportError::QuicDial(e.to_string());
        let socket = bind_udp(addr, &self.inner.windows).map_err(|e| dial(&e))?;
        let endpoint = quinn::Endpoint::new(
            quinn::EndpointConfig::default(),
            None,
            socket,
            Arc::new(quinn::TokioRuntime),
        )
        .map_err(|e| dial(&e))?;
        let connecting = endpoint
            .connect_with(self.inner.client.clone(), addr, server_name)
            .map_err(|e| dial(&e))?;
        let handshake = async {
            let quic = connecting.await.map_err(|e| dial(&e))?;
            let (driver, send) = h3::client::new(h3_quinn::Connection::new(quic.clone()))
                .await
                .map_err(|e| dial(&e))?;
            Ok::<_, TransportError>((quic, send, driver))
        };
        let (quic, send, driver) = tokio::time::timeout(self.inner.dial_timeout, handshake)
            .await
            .map_err(|_| TransportError::QuicDial("the dial timed out".to_owned()))??;
        tokio::spawn(drive(driver));
        tracing::debug!(%addr, rtt_ms = quic.rtt().as_millis() as u64, "h3: QUIC connected");
        Ok(H3Connection {
            quic,
            send,
            _endpoint: endpoint,
            path_silence: self.inner.path_silence,
        })
    }
}

type Driver = h3::client::Connection<h3_quinn::Connection, Bytes>;
type Sender = h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>;

/// Drive the HTTP/3 control streams until the connection closes.
async fn drive(mut driver: Driver) {
    let reason = driver.wait_idle().await;
    tracing::debug!(%reason, "h3: connection closed");
}

/// One open HTTP/3 connection. Cheap to clone: every clone is the same connection.
#[derive(Clone)]
pub struct H3Connection {
    quic: quinn::Connection,
    send: Sender,
    _endpoint: quinn::Endpoint,
    path_silence: Duration,
}

impl std::fmt::Debug for H3Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("H3Connection")
            .field("remote", &self.quic.remote_address())
            .field("closed", &self.quic.close_reason())
            .finish_non_exhaustive()
    }
}

impl H3Connection {
    /// Whether the connection has closed.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.quic.close_reason().is_some()
    }

    /// Carry `request` on its own stream under `bounds`, and read the whole response.
    ///
    /// # Errors
    ///
    /// [`TransportError::PathStalled`] when the path stops making progress (or the connection
    /// idles out), [`TransportError::Cut`] when the request stalls on a live path, and
    /// [`TransportError::Exchange`] for any other failure.
    // The URI is not logged: a path or query may carry a capability.
    #[instrument(level = "debug", skip_all, fields(method = %request.method()))]
    pub async fn send(
        &self,
        request: http::Request<Bytes>,
        bounds: RequestBounds,
    ) -> Result<http::Response<Bytes>, TransportError> {
        let progress = ExchangeProgress::waiting();
        let exchange = within_bounds(
            bounds,
            &progress,
            exchange(self.send.clone(), request, progress.clone()),
        );
        let outcome = match first(exchange, path_watchdog(&self.quic, self.path_silence)).await {
            Either::Left(Ok(result)) => result,
            Either::Left(Err(cut)) => Err(TransportError::Cut(cut)),
            Either::Right(()) => Err(TransportError::PathStalled),
        };
        match outcome {
            Err(TransportError::Exchange(_))
                if matches!(
                    self.quic.close_reason(),
                    Some(quinn::ConnectionError::TimedOut)
                ) =>
            {
                Err(TransportError::PathStalled)
            }
            other => other,
        }
    }
}

/// One request on its own stream: the head, the body in [`PACED_PIECE`] pieces, then the whole
/// response, each step reported to `progress`.
async fn exchange(
    mut send: Sender,
    request: http::Request<Bytes>,
    progress: ExchangeProgress,
) -> Result<http::Response<Bytes>, TransportError> {
    let failed = |e: &dyn std::fmt::Display| TransportError::Exchange(e.to_string());
    let (parts, mut body) = request.into_parts();
    progress.on_sent();
    let mut stream = send
        .send_request(http::Request::from_parts(parts, ()))
        .await
        .map_err(|e| failed(&e))?;
    while !body.is_empty() {
        let piece = body.split_to(body.len().min(PACED_PIECE));
        stream.send_data(piece).await.map_err(|e| failed(&e))?;
        progress.on_sent();
    }
    stream.finish().await.map_err(|e| failed(&e))?;
    progress.on_sent_all();
    let head = stream.recv_response().await.map_err(|e| failed(&e))?;
    progress.on_received();
    let mut received = BytesMut::new();
    while let Some(mut chunk) = stream.recv_data().await.map_err(|e| failed(&e))? {
        progress.on_received();
        received.extend_from_slice(&chunk.copy_to_bytes(chunk.remaining()));
    }
    Ok(head.map(|()| received.freeze()))
}

/// Resolves once `quic` has received no datagram for `silence`.
async fn path_watchdog(quic: &quinn::Connection, silence: Duration) {
    let tick = (silence / 4).max(Duration::from_millis(10));
    let mut seen = quic.stats().udp_rx.datagrams;
    let mut heard = tokio::time::Instant::now();
    loop {
        tokio::time::sleep(tick).await;
        let now = tokio::time::Instant::now();
        let received = quic.stats().udp_rx.datagrams;
        if received != seen {
            seen = received;
            heard = now;
        } else if now.duration_since(heard) >= silence {
            tracing::debug!(
                silent_ms = now.duration_since(heard).as_millis() as u64,
                "h3: QUIC path silent"
            );
            return;
        }
    }
}

// ─── The dial race ───────────────────────────────────────────────────────────

/// One of two outcomes.
#[derive(Debug)]
enum Either<A, B> {
    Left(A),
    Right(B),
}

/// Whichever of `a` and `b` finishes first; the other is dropped.
async fn first<A: Future, B: Future>(a: A, b: B) -> Either<A::Output, B::Output> {
    let mut a = pin!(a);
    let mut b = pin!(b);
    poll_fn(|cx| {
        if let Poll::Ready(output) = a.as_mut().poll(cx) {
            return Poll::Ready(Either::Left(output));
        }
        if let Poll::Ready(output) = b.as_mut().poll(cx) {
            return Poll::Ready(Either::Right(output));
        }
        Poll::Pending
    })
    .await
}

/// The QUIC side of a race TCP won.
pub enum QuicLeg<F, E> {
    /// The QUIC dial is still running; finish it to pool or record it.
    Pending(Pin<Box<F>>),
    /// The QUIC dial had already failed.
    Failed(E),
}

impl<F, E: std::fmt::Debug> std::fmt::Debug for QuicLeg<F, E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending(_) => f.write_str("Pending"),
            Self::Failed(error) => f.debug_tuple("Failed").field(error).finish(),
        }
    }
}

/// Which dial connected first.
pub enum Raced<Q, T, F, EQ> {
    /// QUIC connected first; the TCP dial was dropped.
    Quic(Q),
    /// TCP connected first.
    Tcp {
        /// The TCP connection the request rides.
        conn: T,
        /// What became of the QUIC dial.
        quic: QuicLeg<F, EQ>,
    },
}

impl<Q: std::fmt::Debug, T: std::fmt::Debug, F, EQ: std::fmt::Debug> std::fmt::Debug
    for Raced<Q, T, F, EQ>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Quic(conn) => f.debug_tuple("Quic").field(conn).finish(),
            Self::Tcp { conn, quic } => f
                .debug_struct("Tcp")
                .field("conn", conn)
                .field("quic", quic)
                .finish(),
        }
    }
}

/// Both dials failed.
#[derive(Debug)]
pub struct BothFailed<EQ, ET> {
    /// Why QUIC failed.
    pub quic: EQ,
    /// Why TCP failed.
    pub tcp: ET,
}

enum Leg<F, E> {
    Dialing(Pin<Box<F>>),
    Failed(E),
    Taken,
}

/// Race a QUIC dial against a TCP dial; the first to connect wins.
///
/// A failed dial waits for the other. TCP winning hands back the QUIC dial, still running or
/// already failed, so the caller can pool it or record UDP unusable: a QUIC dial that merely
/// lost is not a failure. Racing is of connection attempts only; no request is sent until one
/// has won.
///
/// # Errors
///
/// [`BothFailed`] when neither connects.
pub async fn race_dial<FQ, FT, Q, T, EQ, ET>(
    quic: FQ,
    tcp: FT,
) -> Result<Raced<Q, T, FQ, EQ>, BothFailed<EQ, ET>>
where
    FQ: Future<Output = Result<Q, EQ>>,
    FT: Future<Output = Result<T, ET>>,
{
    let mut quic = Leg::Dialing(Box::pin(quic));
    let mut tcp = Leg::Dialing(Box::pin(tcp));
    poll_fn(move |cx| {
        if let Leg::Dialing(dial) = &mut quic
            && let Poll::Ready(result) = dial.as_mut().poll(cx)
        {
            match result {
                Ok(conn) => return Poll::Ready(Ok(Raced::Quic(conn))),
                Err(error) => quic = Leg::Failed(error),
            }
        }
        if let Leg::Dialing(dial) = &mut tcp
            && let Poll::Ready(result) = dial.as_mut().poll(cx)
        {
            match result {
                Ok(conn) => {
                    let quic = match std::mem::replace(&mut quic, Leg::Taken) {
                        Leg::Dialing(dial) => QuicLeg::Pending(dial),
                        Leg::Failed(error) => QuicLeg::Failed(error),
                        Leg::Taken => unreachable!("the race resolves once"),
                    };
                    return Poll::Ready(Ok(Raced::Tcp { conn, quic }));
                }
                Err(error) => tcp = Leg::Failed(error),
            }
        }
        if matches!(quic, Leg::Failed(_)) && matches!(tcp, Leg::Failed(_)) {
            let (Leg::Failed(quic), Leg::Failed(tcp)) = (
                std::mem::replace(&mut quic, Leg::Taken),
                std::mem::replace(&mut tcp, Leg::Taken),
            ) else {
                unreachable!("both legs were just seen failed");
            };
            return Poll::Ready(Err(BothFailed { quic, tcp }));
        }
        Poll::Pending
    })
    .await
}

// ─── The transport ───────────────────────────────────────────────────────────

/// The HTTP/1.1 TCP client the QUIC dial races. Its dial is a connection attempt, and its
/// exchange carries one request on the connection it dialed; pooling across exchanges is the
/// leg's own business.
pub trait TcpLeg: Send + Sync + 'static {
    /// A dialed connection.
    type Conn: Send + 'static;

    /// Dial `origin`.
    fn dial(&self, origin: &Origin) -> impl Future<Output = Result<Self::Conn, BoxError>> + Send;

    /// Carry `request` on `conn` under `bounds`.
    fn exchange(
        &self,
        conn: Self::Conn,
        request: http::Request<Bytes>,
        bounds: RequestBounds,
    ) -> impl Future<Output = Result<http::Response<Bytes>, TransportError>> + Send;
}

/// The transport a response rode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// HTTP/3 over QUIC.
    Quic,
    /// The [`TcpLeg`].
    Tcp,
}

/// A response and the transport it rode.
#[derive(Debug)]
pub struct Delivered {
    /// Which transport carried the request.
    pub via: Via,
    /// The whole response.
    pub response: http::Response<Bytes>,
}

#[derive(Debug)]
struct Pooled {
    network: PathGeneration,
    conn: H3Connection,
    used: tokio::time::Instant,
}

type Pool = Arc<Mutex<HashMap<Origin, Pooled>>>;

fn lock(pool: &Pool) -> std::sync::MutexGuard<'_, HashMap<Origin, Pooled>> {
    pool.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Opportunistic HTTP/3: the [`H3Policy`], the QUIC dialer, a pool of one QUIC connection per
/// origin and network, and the [`TcpLeg`] it races.
pub struct H3Transport<C, T, R = NoHttpsRecords> {
    policy: Arc<H3Policy<C>>,
    dialer: H3Dialer,
    tcp: T,
    resolver: R,
    pool: Pool,
}

impl<C, T: std::fmt::Debug, R> std::fmt::Debug for H3Transport<C, T, R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("H3Transport")
            .field("tcp", &self.tcp)
            .field("pooled", &lock(&self.pool).len())
            .finish_non_exhaustive()
    }
}

impl<C, T, R> H3Transport<C, T, R>
where
    C: MonotonicClock + Send + Sync + 'static,
    T: TcpLeg,
    R: HttpsResolver,
{
    /// A transport deciding with `policy`, dialing QUIC with `dialer`, racing `tcp`, and
    /// discovering through `resolver`.
    pub fn new(policy: Arc<H3Policy<C>>, dialer: H3Dialer, tcp: T, resolver: R) -> Self {
        Self {
            policy,
            dialer,
            tcp,
            resolver,
            pool: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The policy this transport decides with.
    pub fn policy(&self) -> &Arc<H3Policy<C>> {
        &self.policy
    }

    /// Carry `request`, addressed by its absolute URI, on the current `network`, and read the
    /// whole response. The transport is chosen once, here; a request is never moved
    /// mid-flight and never retried: a failure goes back to the retry engine, whose re-send
    /// asks again.
    ///
    /// # Errors
    ///
    /// [`TransportError`]; [`TransportError::PathStalled`] has already recorded UDP unusable on
    /// `network`.
    // Only the origin is logged: a path or query may carry a capability.
    #[instrument(
        level = "debug",
        skip_all,
        fields(network = network.0, origin = tracing::field::Empty, via)
    )]
    pub async fn send(
        &self,
        network: PathGeneration,
        request: http::Request<Bytes>,
        bounds: RequestBounds,
    ) -> Result<Delivered, TransportError> {
        let origin = Origin::from_uri(request.uri())?;
        let key = origin.key();
        tracing::Span::current().record("origin", key.as_str());
        let choice = if origin.is_secure() {
            self.choose(&origin, &key, network).await
        } else {
            TransportChoice::Tcp(TcpReason::NotAdvertised)
        };
        tracing::debug!(?choice, "h3: transport chosen");
        let delivered = match choice {
            TransportChoice::Tcp(_) => {
                self.evict(&origin);
                self.send_tcp(&origin, request, bounds).await
            }
            TransportChoice::Race { endpoint } => match self.pooled(&origin, network) {
                Some(conn) => {
                    self.send_quic(&origin, network, &conn, request, bounds)
                        .await
                }
                None => {
                    self.race(&origin, &endpoint, network, request, bounds)
                        .await
                }
            },
        }?;
        tracing::Span::current().record("via", tracing::field::debug(delivered.via));
        observe_response(&self.policy, &key, delivered.response.headers());
        Ok(delivered)
    }

    /// The policy's answer, asking the resolver for HTTPS records first when the origin has no
    /// live advertisement.
    async fn choose(&self, origin: &Origin, key: &str, network: PathGeneration) -> TransportChoice {
        let choice = self.policy.choose(key, network);
        if choice != TransportChoice::Tcp(TcpReason::NotAdvertised) {
            return choice;
        }
        let records = self.resolver.https_records(origin.host()).await;
        if records.is_empty() {
            return choice;
        }
        for record in &records {
            self.policy.observe_https_record(key, origin.port(), record);
        }
        self.policy.choose(key, network)
    }

    /// The pooled connection for `origin` on `network`, if it is still open. Closes every
    /// connection idle past [`POOL_IDLE`] or left from another network.
    fn pooled(&self, origin: &Origin, network: PathGeneration) -> Option<H3Connection> {
        let now = tokio::time::Instant::now();
        let mut pool = lock(&self.pool);
        pool.retain(|_, pooled| {
            pooled.network == network
                && !pooled.conn.is_closed()
                && now.duration_since(pooled.used) < POOL_IDLE
        });
        let pooled = pool.get_mut(origin)?;
        pooled.used = now;
        Some(pooled.conn.clone())
    }

    fn evict(&self, origin: &Origin) {
        if lock(&self.pool).remove(origin).is_some() {
            tracing::debug!(origin = %origin.key(), "h3: pooled QUIC connection dropped");
        }
    }

    async fn send_tcp(
        &self,
        origin: &Origin,
        request: http::Request<Bytes>,
        bounds: RequestBounds,
    ) -> Result<Delivered, TransportError> {
        let conn = self
            .tcp
            .dial(origin)
            .await
            .map_err(|e| TransportError::Dial {
                quic: None,
                tcp: e.to_string(),
            })?;
        self.exchange_tcp(conn, request, bounds).await
    }

    async fn exchange_tcp(
        &self,
        conn: T::Conn,
        request: http::Request<Bytes>,
        bounds: RequestBounds,
    ) -> Result<Delivered, TransportError> {
        let response = self.tcp.exchange(conn, request, bounds).await?;
        Ok(Delivered {
            via: Via::Tcp,
            response,
        })
    }

    async fn send_quic(
        &self,
        origin: &Origin,
        network: PathGeneration,
        conn: &H3Connection,
        request: http::Request<Bytes>,
        bounds: RequestBounds,
    ) -> Result<Delivered, TransportError> {
        match conn.send(request, bounds).await {
            Ok(response) => Ok(Delivered {
                via: Via::Quic,
                response,
            }),
            Err(error) => {
                self.evict(origin);
                if matches!(error, TransportError::PathStalled) {
                    self.policy
                        .record_udp_unusable(network, UdpFailure::Stalled);
                }
                Err(error)
            }
        }
    }

    async fn race(
        &self,
        origin: &Origin,
        endpoint: &H3Endpoint,
        network: PathGeneration,
        request: http::Request<Bytes>,
        bounds: RequestBounds,
    ) -> Result<Delivered, TransportError> {
        let quic = dial_quic(self.dialer.clone(), origin.clone(), endpoint.clone());
        match race_dial(quic, self.tcp.dial(origin)).await {
            Ok(Raced::Quic(conn)) => {
                tracing::debug!(origin = %origin.key(), "h3: QUIC won the dial race");
                self.pool_conn(origin, network, conn.clone());
                self.send_quic(origin, network, &conn, request, bounds)
                    .await
            }
            Ok(Raced::Tcp { conn, quic }) => {
                tracing::debug!(origin = %origin.key(), ?quic, "h3: TCP won the dial race");
                match quic {
                    QuicLeg::Failed(error) => {
                        tracing::debug!(%error, "h3: QUIC dial failed");
                        self.policy
                            .record_udp_unusable(network, UdpFailure::DialFailed);
                    }
                    QuicLeg::Pending(dial) => self.finish_dial(origin, network, dial),
                }
                self.exchange_tcp(conn, request, bounds).await
            }
            Err(BothFailed { quic, tcp }) => Err(TransportError::Dial {
                quic: Some(quic.to_string()),
                tcp: tcp.to_string(),
            }),
        }
    }

    fn pool_conn(&self, origin: &Origin, network: PathGeneration, conn: H3Connection) {
        lock(&self.pool).insert(
            origin.clone(),
            Pooled {
                network,
                conn,
                used: tokio::time::Instant::now(),
            },
        );
    }

    /// Let a QUIC dial that lost the race finish in the background: pooled if it connects,
    /// UDP recorded unusable if it fails, since TCP connected while it did.
    fn finish_dial<F>(&self, origin: &Origin, network: PathGeneration, dial: Pin<Box<F>>)
    where
        F: Future<Output = Result<H3Connection, TransportError>> + Send + 'static,
    {
        let policy = self.policy.clone();
        let pool = self.pool.clone();
        let origin = origin.clone();
        tokio::spawn(async move {
            match dial.await {
                Ok(conn) => {
                    tracing::debug!(origin = %origin.key(), "h3: late QUIC dial pooled");
                    lock(&pool).entry(origin).or_insert(Pooled {
                        network,
                        conn,
                        used: tokio::time::Instant::now(),
                    });
                }
                Err(error) => {
                    tracing::debug!(%error, "h3: QUIC dial failed after TCP won");
                    policy.record_udp_unusable(network, UdpFailure::DialFailed);
                }
            }
        });
    }
}

/// Resolve `endpoint` (the origin's host when it names none) and dial it as the origin: the
/// certificate must be valid for the origin, whatever host the advertisement names (RFC 7838
/// §2.1).
async fn dial_quic(
    dialer: H3Dialer,
    origin: Origin,
    endpoint: H3Endpoint,
) -> Result<H3Connection, TransportError> {
    let host = endpoint.host.as_deref().unwrap_or(origin.host());
    let addr = tokio::net::lookup_host((host, endpoint.port))
        .await
        .map_err(|e| TransportError::QuicDial(e.to_string()))?
        .next()
        .ok_or_else(|| TransportError::QuicDial(format!("{host} resolved to no address")))?;
    dialer.dial(addr, origin.host()).await
}

#[cfg(test)]
mod tests;
