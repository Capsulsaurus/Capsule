use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::task::JoinHandle;

use super::*;
use crate::net::SystemClock;

// ─── Origin ──────────────────────────────────────────────────────────────────

#[test]
fn origin_defaults_the_port_from_the_scheme() {
    let https = Origin::from_uri(&"https://capsule.example/v1/x".parse().expect("uri"))
        .expect("https origin");
    assert_eq!(https, Origin::https("capsule.example", 443));
    assert_eq!(https.key(), "https://capsule.example:443");

    let http = Origin::from_uri(&"http://localhost:8080/".parse().expect("uri")).expect("origin");
    assert!(!http.is_secure());
    assert_eq!(http.port(), 8080);
    assert_eq!(http.key(), "http://localhost:8080");
}

#[test]
fn origin_strips_ipv6_brackets_for_the_server_name_and_keeps_them_in_the_key() {
    let origin = Origin::from_uri(&"https://[::1]:8443/".parse().expect("uri")).expect("origin");
    assert_eq!(origin.host(), "::1");
    assert_eq!(origin.key(), "https://[::1]:8443");
}

#[test]
fn origin_refuses_relative_and_foreign_uris() {
    for uri in ["/v1/x", "ftp://capsule.example/"] {
        let refused = Origin::from_uri(&uri.parse().expect("uri"));
        assert!(
            matches!(refused, Err(TransportError::InvalidRequest(_))),
            "{uri}: {refused:?}"
        );
    }
}

// ─── Discovery ───────────────────────────────────────────────────────────────

const KEY: &str = "https://capsule.example:443";
const NET: PathGeneration = PathGeneration(1);

fn capable_policy() -> H3Policy<SystemClock> {
    H3Policy::new(SystemClock::new(), true)
}

#[test]
fn every_alt_svc_header_on_a_response_is_observed() {
    let policy = capable_policy();
    let mut headers = http::HeaderMap::new();
    headers.append(http::header::ALT_SVC, "h2=\":443\"".parse().expect("value"));
    headers.append(
        http::header::ALT_SVC,
        "h3=\"alt.example:8443\"".parse().expect("value"),
    );
    observe_response(&policy, KEY, &headers);
    assert_eq!(
        policy.choose(KEY, NET),
        TransportChoice::Race {
            endpoint: H3Endpoint {
                host: Some("alt.example".to_owned()),
                port: 8443,
            }
        }
    );
}

#[test]
fn a_response_without_alt_svc_keeps_the_advertisement_and_clear_withdraws_it() {
    let policy = capable_policy();
    policy.observe_alt_svc(KEY, "h3=\":443\"");
    observe_response(&policy, KEY, &http::HeaderMap::new());
    assert!(matches!(
        policy.choose(KEY, NET),
        TransportChoice::Race { .. }
    ));

    let mut clear = http::HeaderMap::new();
    clear.insert(http::header::ALT_SVC, "clear".parse().expect("value"));
    observe_response(&policy, KEY, &clear);
    assert_eq!(
        policy.choose(KEY, NET),
        TransportChoice::Tcp(TcpReason::NotAdvertised)
    );
}

#[tokio::test]
async fn the_default_resolver_knows_no_https_records() {
    assert!(
        NoHttpsRecords
            .https_records("capsule.example")
            .await
            .is_empty()
    );
}

// ─── Windows and configuration ───────────────────────────────────────────────

#[test]
fn windows_are_the_bandwidth_delay_product_within_the_budget() {
    let windows = Windows::default();
    let expected = size_window(
        DEFAULT_TARGET_BANDWIDTH,
        DEFAULT_TARGET_RTT,
        DEFAULT_CONNECTIONS,
        DEFAULT_WINDOW_BUDGET,
    );
    // 100 Mbit/s over 100 ms is 1.25 MB, inside a quarter of 32 MiB.
    assert_eq!(expected, 1_250_000);
    assert_eq!(windows, Windows::of(expected));
    assert_eq!(windows.stream_receive, expected);
    assert_eq!(windows.connection_receive, expected);
    assert_eq!(windows.send, expected);
    assert_eq!(windows.socket_receive, 1_250_000);
    assert_eq!(windows.socket_send, 1_250_000);

    // A huge bandwidth-delay product is cut to the connection's share of the budget.
    let capped = Windows::sized(
        u64::MAX / 2,
        Duration::from_secs(1),
        8,
        DEFAULT_WINDOW_BUDGET,
    );
    assert_eq!(capped.send, DEFAULT_WINDOW_BUDGET.total / 8);
}

#[test]
fn the_path_is_judged_before_the_request_stalls() {
    let config = H3Config {
        bounds: RequestBounds::new(Duration::from_secs(20), Duration::from_mins(1)),
        ..H3Config::default()
    };
    assert_eq!(config.keep_alive(), Duration::from_secs(5));
    assert_eq!(config.path_silence(), Duration::from_secs(10));
    assert_eq!(config.idle_timeout(), Duration::from_secs(20));
    assert!(config.keep_alive() * 2 <= config.path_silence());
    assert!(config.path_silence() < config.bounds.stall);
    transport_config(&config).expect("the transport parameters encode");
}

#[test]
fn the_tls_config_offers_alpn_h3_without_early_data() {
    let tls = client_tls(H3Config::default().roots).expect("tls config");
    assert_eq!(tls.alpn_protocols, vec![b"h3".to_vec()]);
    assert!(!tls.enable_early_data);
    assert!(!H3Config::default().roots.is_empty());
    for method in [http::Method::GET, http::Method::HEAD, http::Method::PUT] {
        assert!(!super::super::early_data_allowed(&method));
    }
}

// ─── The dial race ───────────────────────────────────────────────────────────

type Dial = Result<&'static str, &'static str>;

async fn after(delay: Duration, result: Dial) -> Dial {
    tokio::time::sleep(delay).await;
    result
}

const SOON: Duration = Duration::from_millis(10);
const LATER: Duration = Duration::from_millis(50);

#[tokio::test(start_paused = true)]
async fn quic_connecting_first_wins_the_race() {
    let raced = race_dial(after(SOON, Ok("quic")), after(LATER, Ok("tcp"))).await;
    assert!(matches!(raced, Ok(Raced::Quic("quic"))));
}

#[tokio::test(start_paused = true)]
async fn tcp_connecting_first_hands_back_the_running_quic_dial() {
    let raced = race_dial(after(LATER, Ok("quic")), after(SOON, Ok("tcp"))).await;
    let Ok(Raced::Tcp {
        conn: "tcp",
        quic: QuicLeg::Pending(dial),
    }) = raced
    else {
        panic!("TCP should win with QUIC still dialing: {raced:?}");
    };
    // The lost dial is still live: finishing it is how a late connection gets pooled.
    assert_eq!(dial.await, Ok("quic"));
}

#[tokio::test(start_paused = true)]
async fn a_failed_quic_dial_waits_for_tcp_and_is_reported() {
    let raced = race_dial(after(SOON, Err("udp blocked")), after(LATER, Ok("tcp"))).await;
    assert!(matches!(
        raced,
        Ok(Raced::Tcp {
            conn: "tcp",
            quic: QuicLeg::Failed("udp blocked"),
        })
    ));
}

#[tokio::test(start_paused = true)]
async fn a_failed_tcp_dial_waits_for_quic() {
    let raced = race_dial(after(LATER, Ok("quic")), after(SOON, Err("refused"))).await;
    assert!(matches!(raced, Ok(Raced::Quic("quic"))));
}

#[tokio::test(start_paused = true)]
async fn both_dials_failing_reports_both() {
    let raced = race_dial(after(SOON, Err("udp")), after(LATER, Err("tcp"))).await;
    let Err(BothFailed { quic, tcp }) = raced else {
        panic!("both legs failed: {raced:?}");
    };
    assert_eq!((quic, tcp), ("udp", "tcp"));
}

// ─── Loopback harness ────────────────────────────────────────────────────────
//
// One origin served twice from one store: HTTP/3 over QUIC, and a framed request/response
// protocol over TCP standing in for the HTTP/1.1 client. Every chunk is a `PUT /chunks/N`; the
// store refuses a chunk re-sent with different bytes, so "no duplicate or corrupt state" is
// "every chunk held once, byte-identical, and no conflict".

const SERVER_NAME: &str = "localhost";
const CHUNK: usize = 64 * 1024;

#[derive(Debug, Default)]
struct Store {
    chunks: Mutex<BTreeMap<u32, Bytes>>,
    conflicts: AtomicU64,
}

impl Store {
    fn handle(&self, method: &http::Method, path: &str, body: Bytes) -> (http::StatusCode, Bytes) {
        let Some(index) = path
            .strip_prefix("/chunks/")
            .and_then(|index| index.parse::<u32>().ok())
        else {
            return (http::StatusCode::NOT_FOUND, Bytes::new());
        };
        let mut chunks = self.chunks.lock().unwrap_or_else(PoisonError::into_inner);
        match *method {
            http::Method::PUT => match chunks.get(&index) {
                Some(held) if *held != body => {
                    self.conflicts.fetch_add(1, Ordering::SeqCst);
                    (http::StatusCode::CONFLICT, Bytes::new())
                }
                Some(_) => (http::StatusCode::OK, Bytes::new()),
                None => {
                    chunks.insert(index, body);
                    (http::StatusCode::OK, Bytes::new())
                }
            },
            http::Method::GET => chunks
                .get(&index)
                .map_or((http::StatusCode::NOT_FOUND, Bytes::new()), |held| {
                    (http::StatusCode::OK, held.clone())
                }),
            _ => (http::StatusCode::METHOD_NOT_ALLOWED, Bytes::new()),
        }
    }

    fn assert_holds_exactly(&self, source: &[Bytes]) {
        let chunks = self.chunks.lock().unwrap_or_else(PoisonError::into_inner);
        assert_eq!(chunks.len(), source.len(), "every chunk is held once");
        for (index, bytes) in source.iter().enumerate() {
            assert_eq!(
                chunks.get(&(index as u32)),
                Some(bytes),
                "chunk {index} is byte-identical"
            );
        }
        assert_eq!(
            self.conflicts.load(Ordering::SeqCst),
            0,
            "no corrupt re-send"
        );
    }
}

struct Server {
    store: Arc<Store>,
    quic_addr: SocketAddr,
    tcp_addr: SocketAddr,
    alt_svc: Arc<Mutex<String>>,
    roots: Arc<rustls::RootCertStore>,
    quic_connections: Arc<AtomicU32>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl Server {
    fn advertise(&self, value: String) {
        *self.alt_svc.lock().unwrap_or_else(PoisonError::into_inner) = value;
    }

    fn origin_uri(&self, path: &str) -> String {
        format!("https://{SERVER_NAME}:{}{path}", self.tcp_addr.port())
    }
}

fn alt_svc_to(addr: SocketAddr) -> String {
    format!("h3=\"{addr}\"; ma=60")
}

async fn server() -> Server {
    let certified = rcgen::generate_simple_self_signed(vec![SERVER_NAME.to_owned()]).expect("cert");
    let cert = CertificateDer::from(certified.cert.der().to_vec());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(certified.key_pair.serialize_der()));
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.clone()).expect("root");

    let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .expect("tls 1.3")
    .with_no_client_auth()
    .with_single_cert(vec![cert], key)
    .expect("server cert");
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls).expect("quic tls");
    let endpoint = quinn::Endpoint::server(
        quinn::ServerConfig::with_crypto(Arc::new(crypto)),
        (Ipv4Addr::LOCALHOST, 0).into(),
    )
    .expect("quic endpoint");
    let quic_addr = endpoint.local_addr().expect("quic addr");
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("tcp listener");
    let tcp_addr = listener.local_addr().expect("tcp addr");

    let store = Arc::new(Store::default());
    let alt_svc = Arc::new(Mutex::new(alt_svc_to(quic_addr)));
    let quic_connections = Arc::new(AtomicU32::new(0));
    let tasks = vec![
        tokio::spawn(serve_h3(
            endpoint,
            store.clone(),
            alt_svc.clone(),
            quic_connections.clone(),
        )),
        tokio::spawn(serve_tcp(listener, store.clone(), alt_svc.clone())),
    ];
    Server {
        store,
        quic_addr,
        tcp_addr,
        alt_svc,
        roots: Arc::new(roots),
        quic_connections,
        tasks,
    }
}

async fn serve_h3(
    endpoint: quinn::Endpoint,
    store: Arc<Store>,
    alt_svc: Arc<Mutex<String>>,
    connections: Arc<AtomicU32>,
) {
    while let Some(incoming) = endpoint.accept().await {
        let (store, alt_svc, connections) = (store.clone(), alt_svc.clone(), connections.clone());
        tokio::spawn(async move {
            let Ok(conn) = incoming.await else { return };
            connections.fetch_add(1, Ordering::SeqCst);
            let Ok(mut h3) = h3::server::builder()
                .build::<_, Bytes>(h3_quinn::Connection::new(conn))
                .await
            else {
                return;
            };
            while let Ok(Some(resolver)) = h3.accept().await {
                let (store, alt_svc) = (store.clone(), alt_svc.clone());
                tokio::spawn(async move {
                    let Ok((request, mut stream)) = resolver.resolve_request().await else {
                        return;
                    };
                    let mut body = BytesMut::new();
                    while let Ok(Some(mut chunk)) = stream.recv_data().await {
                        body.extend_from_slice(&chunk.copy_to_bytes(chunk.remaining()));
                    }
                    let (status, out) =
                        store.handle(request.method(), request.uri().path(), body.freeze());
                    let advertised = alt_svc
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clone();
                    let head = http::Response::builder()
                        .status(status)
                        .header(http::header::ALT_SVC, advertised)
                        .body(())
                        .expect("response head");
                    if stream.send_response(head).await.is_ok()
                        && stream.send_data(out).await.is_ok()
                    {
                        let _ = stream.finish().await;
                    }
                });
            }
        });
    }
}

// The framed TCP stand-in: `method u8 (0 GET, 1 PUT) | path u16+bytes | body u32+bytes`, then
// `status u16 | alt-svc u16+bytes | body u32+bytes`. One exchange per connection.

async fn serve_tcp(listener: TcpListener, store: Arc<Store>, alt_svc: Arc<Mutex<String>>) {
    while let Ok((mut socket, _)) = listener.accept().await {
        let (store, alt_svc) = (store.clone(), alt_svc.clone());
        tokio::spawn(async move {
            let Ok(method) = socket.read_u8().await else {
                return;
            };
            let method = if method == 1 {
                http::Method::PUT
            } else {
                http::Method::GET
            };
            let Ok(path) = read_frame_u16(&mut socket).await else {
                return;
            };
            let Ok(body) = read_frame_u32(&mut socket).await else {
                return;
            };
            let path = String::from_utf8_lossy(&path).into_owned();
            let (status, out) = store.handle(&method, &path, body);
            let advertised = alt_svc
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            let mut reply = Vec::new();
            reply.extend_from_slice(&status.as_u16().to_be_bytes());
            reply.extend_from_slice(&(advertised.len() as u16).to_be_bytes());
            reply.extend_from_slice(advertised.as_bytes());
            reply.extend_from_slice(&(out.len() as u32).to_be_bytes());
            reply.extend_from_slice(&out);
            let _ = socket.write_all(&reply).await;
        });
    }
}

async fn read_frame_u16(socket: &mut TcpStream) -> std::io::Result<Bytes> {
    let len = socket.read_u16().await?;
    let mut buf = vec![0; usize::from(len)];
    socket.read_exact(&mut buf).await?;
    Ok(buf.into())
}

async fn read_frame_u32(socket: &mut TcpStream) -> std::io::Result<Bytes> {
    let len = socket.read_u32().await?;
    let mut buf = vec![0; len as usize];
    socket.read_exact(&mut buf).await?;
    Ok(buf.into())
}

/// The TCP leg the harness races: dials the server's TCP port after `delay`.
#[derive(Debug)]
struct FramedTcp {
    addr: SocketAddr,
    delay: Duration,
    dials: AtomicU32,
}

impl FramedTcp {
    fn new(addr: SocketAddr, delay: Duration) -> Self {
        Self {
            addr,
            delay,
            dials: AtomicU32::new(0),
        }
    }
}

impl TcpLeg for FramedTcp {
    type Conn = TcpStream;

    async fn dial(&self, _origin: &Origin) -> Result<TcpStream, BoxError> {
        self.dials.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(self.delay).await;
        Ok(TcpStream::connect(self.addr).await?)
    }

    async fn exchange(
        &self,
        mut conn: TcpStream,
        request: http::Request<Bytes>,
        _bounds: RequestBounds,
    ) -> Result<http::Response<Bytes>, TransportError> {
        let failed = |e: std::io::Error| TransportError::Exchange(e.to_string());
        let path = request.uri().path().as_bytes().to_vec();
        let mut frame = vec![u8::from(request.method() == http::Method::PUT)];
        frame.extend_from_slice(&(path.len() as u16).to_be_bytes());
        frame.extend_from_slice(&path);
        frame.extend_from_slice(&(request.body().len() as u32).to_be_bytes());
        frame.extend_from_slice(request.body());
        conn.write_all(&frame).await.map_err(failed)?;
        let status = conn.read_u16().await.map_err(failed)?;
        let alt_svc = read_frame_u16(&mut conn).await.map_err(failed)?;
        let body = read_frame_u32(&mut conn).await.map_err(failed)?;
        let mut response = http::Response::builder().status(status);
        if !alt_svc.is_empty() {
            response = response.header(http::header::ALT_SVC, alt_svc.as_ref());
        }
        response
            .body(body)
            .map_err(|e| TransportError::Exchange(e.to_string()))
    }
}

/// A UDP relay in front of the QUIC port that can start dropping every datagram, both ways:
/// the unprivileged stand-in for netem.
struct UdpRelay {
    addr: SocketAddr,
    dropping: Arc<AtomicBool>,
    task: JoinHandle<()>,
}

impl Drop for UdpRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl UdpRelay {
    async fn to(upstream: SocketAddr) -> Self {
        Self::holding_first(upstream, Duration::ZERO).await
    }

    /// A relay that holds the client's first datagram for `hold` before forwarding it, so a
    /// QUIC dial through it connects, but only after a TCP dial with no delay has won.
    async fn holding_first(upstream: SocketAddr, hold: Duration) -> Self {
        let front = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("front");
        let back = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("back");
        back.connect(upstream).await.expect("connect upstream");
        let addr = front.local_addr().expect("relay addr");
        let dropping = Arc::new(AtomicBool::new(false));
        let drops = dropping.clone();
        let task = tokio::spawn(async move {
            let mut client = None;
            let mut hold = Some(hold).filter(|hold| !hold.is_zero());
            let (mut up, mut down) = (vec![0; 65_536], vec![0; 65_536]);
            loop {
                tokio::select! {
                    Ok((len, from)) = front.recv_from(&mut up) => {
                        client = Some(from);
                        if let Some(hold) = hold.take() {
                            tokio::time::sleep(hold).await;
                        }
                        if !drops.load(Ordering::SeqCst) {
                            let _ = back.send(&up[..len]).await;
                        }
                    }
                    Ok(len) = back.recv(&mut down) => {
                        if let Some(client) = client
                            && !drops.load(Ordering::SeqCst)
                        {
                            let _ = front.send_to(&down[..len], client).await;
                        }
                    }
                }
            }
        });
        Self {
            addr,
            dropping,
            task,
        }
    }

    fn drop_everything(&self) {
        self.dropping.store(true, Ordering::SeqCst);
    }
}

fn config(roots: Arc<rustls::RootCertStore>, stall: Duration) -> H3Config {
    H3Config {
        roots,
        bounds: RequestBounds::new(stall, Duration::from_secs(5)),
        dial_timeout: Duration::from_millis(500),
        ..H3Config::default()
    }
}

fn transport(
    server: &Server,
    tcp_delay: Duration,
    stall: Duration,
) -> H3Transport<SystemClock, FramedTcp> {
    let config = config(server.roots.clone(), stall);
    H3Transport::new(
        Arc::new(capable_policy()),
        H3Dialer::new(&config).expect("dialer"),
        FramedTcp::new(server.tcp_addr, tcp_delay),
        NoHttpsRecords,
    )
}

fn put(server: &Server, index: usize, body: Bytes) -> http::Request<Bytes> {
    http::Request::put(server.origin_uri(&format!("/chunks/{index}")))
        .body(body)
        .expect("request")
}

fn source(chunks: usize) -> Vec<Bytes> {
    (0..chunks)
        .map(|index| Bytes::from(vec![(index % 251) as u8; CHUNK]))
        .collect()
}

/// How one chunk got through, and what the attempts before it met.
#[derive(Debug)]
struct Sent {
    via: Via,
    failures: Vec<String>,
}

/// Send one chunk the way the retry engine would: each failure re-sends, and each re-send asks
/// the policy again.
async fn send_chunk<R: HttpsResolver>(
    transport: &H3Transport<SystemClock, FramedTcp, R>,
    request: impl Fn() -> http::Request<Bytes>,
    bounds: RequestBounds,
) -> Sent {
    let mut failures = Vec::new();
    for _ in 0..4 {
        match transport.send(NET, request(), bounds).await {
            Ok(delivered) => {
                assert_eq!(delivered.response.status(), http::StatusCode::OK);
                return Sent {
                    via: delivered.via,
                    failures,
                };
            }
            Err(error) => failures.push(error.to_string()),
        }
    }
    panic!("the chunk never got through: {failures:?}");
}

// ─── Loopback tests ──────────────────────────────────────────────────────────

const STALL: Duration = Duration::from_millis(600);
const TCP_SLOWER: Duration = Duration::from_millis(300);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dialed_quic_connection_carries_requests_both_ways() {
    let server = server().await;
    let config = config(server.roots.clone(), STALL);
    let conn = H3Dialer::new(&config)
        .expect("dialer")
        .dial(server.quic_addr, SERVER_NAME)
        .await
        .expect("dial");
    let body = Bytes::from(vec![7; 3 * PACED_PIECE + 5]);
    let response = conn
        .send(put(&server, 0, body.clone()), config.bounds)
        .await
        .expect("put");
    assert_eq!(response.status(), http::StatusCode::OK);
    assert_eq!(
        response.headers().get(http::header::ALT_SVC),
        Some(&alt_svc_to(server.quic_addr).parse().expect("value"))
    );
    let got = conn
        .send(
            http::Request::get(server.origin_uri("/chunks/0"))
                .body(Bytes::new())
                .expect("get"),
            config.bounds,
        )
        .await
        .expect("get");
    assert_eq!(got.into_body(), body);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_quic_exchange_reports_to_the_progress_its_request_carries() {
    let server = server().await;
    let config = config(server.roots.clone(), STALL);
    let conn = H3Dialer::new(&config)
        .expect("dialer")
        .dial(server.quic_addr, SERVER_NAME)
        .await
        .expect("dial");
    let progress = ExchangeProgress::waiting();
    let mut request = put(&server, 0, Bytes::from(vec![7; 2 * PACED_PIECE]));
    request.extensions_mut().insert(progress.clone());

    let response = conn.send(request, config.bounds).await.expect("put");

    assert_eq!(response.status(), http::StatusCode::OK);
    // The caller's own bounds read the same progress: the response came back on it.
    assert!(
        matches!(progress.phase(), crate::net::Phase::Receiving { .. }),
        "{:?}",
        progress.phase()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_certificate_the_roots_do_not_hold_fails_the_dial() {
    let server = server().await;
    let stranger = config(Arc::new(H3Config::default().roots.as_ref().clone()), STALL);
    let refused = H3Dialer::new(&stranger)
        .expect("dialer")
        .dial(server.quic_addr, SERVER_NAME)
        .await;
    assert!(
        matches!(refused, Err(TransportError::QuicDial(_))),
        "{refused:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn alt_svc_on_a_tcp_response_moves_the_next_request_to_quic() {
    let server = server().await;
    let transport = transport(&server, TCP_SLOWER, STALL);
    let bounds = config(server.roots.clone(), STALL).bounds;
    let chunks = source(3);

    let vias: Vec<Via> = {
        let mut vias = Vec::new();
        for (index, chunk) in chunks.iter().enumerate() {
            let sent = send_chunk(&transport, || put(&server, index, chunk.clone()), bounds).await;
            assert!(sent.failures.is_empty(), "{:?}", sent.failures);
            vias.push(sent.via);
        }
        vias
    };
    // Unadvertised, then the race QUIC wins, then the pooled connection.
    assert_eq!(vias, [Via::Tcp, Via::Quic, Via::Quic]);
    assert_eq!(server.quic_connections.load(Ordering::SeqCst), 1);
    server.store.assert_holds_exactly(&chunks);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plain_http_origin_never_races() {
    let server = server().await;
    let transport = transport(&server, Duration::ZERO, STALL);
    let uri = format!("http://{SERVER_NAME}:{}/chunks/0", server.tcp_addr.port());
    let key = Origin::from_uri(&uri.parse().expect("uri"))
        .expect("origin")
        .key();
    transport
        .policy()
        .observe_alt_svc(&key, &alt_svc_to(server.quic_addr));
    let delivered = transport
        .send(
            NET,
            http::Request::put(uri)
                .body(Bytes::from_static(b"x"))
                .expect("request"),
            config(server.roots.clone(), STALL).bounds,
        )
        .await
        .expect("send");
    assert_eq!(delivered.via, Via::Tcp);
    assert_eq!(server.quic_connections.load(Ordering::SeqCst), 0);
}

/// A resolver that publishes one HTTPS record.
#[derive(Debug)]
struct OneRecord(HttpsRecord);

impl HttpsResolver for OneRecord {
    async fn https_records(&self, host: &str) -> Vec<HttpsRecord> {
        assert_eq!(host, SERVER_NAME);
        vec![self.0.clone()]
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_https_record_lets_the_first_request_race() {
    let server = server().await;
    let config = config(server.roots.clone(), STALL);
    let transport = H3Transport::new(
        Arc::new(capable_policy()),
        H3Dialer::new(&config).expect("dialer"),
        FramedTcp::new(server.tcp_addr, TCP_SLOWER),
        OneRecord(HttpsRecord {
            alpn: vec!["h3".to_owned()],
            port: Some(server.quic_addr.port()),
            target: Some(server.quic_addr.ip().to_string()),
            ttl: Duration::from_mins(5),
        }),
    );
    let sent = send_chunk(
        &transport,
        || put(&server, 0, Bytes::from_static(b"first")),
        config.bounds,
    )
    .await;
    assert_eq!(sent.via, Via::Quic);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_quic_dial_that_fails_while_tcp_connects_records_udp_unusable() {
    let server = server().await;
    let blackhole = UdpRelay::to(server.quic_addr).await;
    blackhole.drop_everything();
    let transport = transport(&server, Duration::ZERO, STALL);
    let key = Origin::from_uri(&server.origin_uri("/").parse().expect("uri"))
        .expect("origin")
        .key();
    transport
        .policy()
        .observe_alt_svc(&key, &alt_svc_to(blackhole.addr));
    server.advertise(alt_svc_to(blackhole.addr));

    let bounds = config(server.roots.clone(), STALL).bounds;
    let sent = send_chunk(
        &transport,
        || put(&server, 0, Bytes::from_static(b"x")),
        bounds,
    )
    .await;
    assert_eq!(sent.via, Via::Tcp);

    // The lost QUIC dial runs out its timeout in the background, then records the network.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while transport.policy().choose(&key, NET) != TransportChoice::Tcp(TcpReason::UdpUnusable) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the failed dial was never recorded"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        transport.policy().choose(&key, PathGeneration(2)),
        TransportChoice::Race {
            endpoint: H3Endpoint {
                host: Some(blackhole.addr.ip().to_string()),
                port: blackhole.addr.port(),
            }
        },
        "a new network starts with no record"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_quic_dial_that_connects_after_tcp_won_is_pooled_for_the_next_request() {
    let server = server().await;
    let slow = UdpRelay::holding_first(server.quic_addr, Duration::from_millis(150)).await;
    let transport = transport(&server, Duration::ZERO, STALL);
    let origin = Origin::from_uri(&server.origin_uri("/").parse().expect("uri")).expect("origin");
    let key = origin.key();
    transport
        .policy()
        .observe_alt_svc(&key, &alt_svc_to(slow.addr));
    server.advertise(alt_svc_to(slow.addr));
    let bounds = config(server.roots.clone(), STALL).bounds;

    let first = send_chunk(
        &transport,
        || put(&server, 0, Bytes::from_static(b"first")),
        bounds,
    )
    .await;
    assert_eq!(first.via, Via::Tcp, "TCP had no delay and should win");

    // The lost QUIC dial finishes in the background and is pooled.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !lock(&transport.pool).contains_key(&origin) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the late QUIC dial was never pooled"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let second = send_chunk(
        &transport,
        || put(&server, 1, Bytes::from_static(b"second")),
        bounds,
    )
    .await;
    assert_eq!(second.via, Via::Quic);
    assert!(second.failures.is_empty(), "{:?}", second.failures);
    assert_eq!(server.quic_connections.load(Ordering::SeqCst), 1);
    assert_eq!(transport.tcp.dials.load(Ordering::SeqCst), 1, "no new dial");
    assert!(matches!(
        transport.policy().choose(&key, NET),
        TransportChoice::Race { .. }
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_dials_failing_is_a_dial_error_that_records_udp_unusable() {
    let server = server().await;
    let blackhole = UdpRelay::to(server.quic_addr).await;
    blackhole.drop_everything();
    let closed = {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("listener");
        listener.local_addr().expect("addr")
    };
    let config = config(server.roots.clone(), STALL);
    let transport = H3Transport::new(
        Arc::new(capable_policy()),
        H3Dialer::new(&config).expect("dialer"),
        FramedTcp::new(closed, Duration::ZERO),
        NoHttpsRecords,
    );
    let key = Origin::from_uri(&server.origin_uri("/").parse().expect("uri"))
        .expect("origin")
        .key();
    transport
        .policy()
        .observe_alt_svc(&key, &alt_svc_to(blackhole.addr));
    let failed = transport
        .send(
            NET,
            put(&server, 0, Bytes::from_static(b"x")),
            config.bounds,
        )
        .await;
    assert!(
        matches!(failed, Err(TransportError::Dial { quic: Some(_), .. })),
        "{failed:?}"
    );
    // The next request on this network rides TCP alone instead of waiting out the QUIC dial.
    assert_eq!(
        transport.policy().choose(&key, NET),
        TransportChoice::Tcp(TcpReason::UdpUnusable)
    );
}

/// The acceptance scenario without privileges: UDP starts dropping mid-transfer, and the
/// transfer finishes over TCP with every chunk held once and intact.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_quic_path_that_stops_making_progress_finishes_the_transfer_over_tcp() {
    let server = server().await;
    let relay = UdpRelay::to(server.quic_addr).await;
    server.advertise(alt_svc_to(relay.addr));
    let transport = transport(&server, TCP_SLOWER, STALL);
    let bounds = config(server.roots.clone(), STALL).bounds;
    let chunks = source(10);
    let mut sent = Vec::new();
    for (index, chunk) in chunks.iter().enumerate() {
        if index == 4 {
            relay.drop_everything();
        }
        sent.push(send_chunk(&transport, || put(&server, index, chunk.clone()), bounds).await);
    }

    let vias: Vec<Via> = sent.iter().map(|sent| sent.via).collect();
    assert_eq!(vias[..4], [Via::Tcp, Via::Quic, Via::Quic, Via::Quic]);
    assert!(vias[4..].iter().all(|via| *via == Via::Tcp), "{vias:?}");
    // The chunk in flight when UDP died failed once as a stalled path, and its re-send rode TCP.
    assert_eq!(sent[4].failures.len(), 1, "{:?}", sent[4].failures);
    assert!(
        sent[4].failures[0].contains("stopped making progress"),
        "{:?}",
        sent[4].failures
    );
    assert!(sent[5..].iter().all(|sent| sent.failures.is_empty()));
    let key = Origin::from_uri(&server.origin_uri("/").parse().expect("uri"))
        .expect("origin")
        .key();
    assert_eq!(
        transport.policy().choose(&key, NET),
        TransportChoice::Tcp(TcpReason::UdpUnusable)
    );
    server.store.assert_holds_exactly(&chunks);
}

// ─── netem acceptance harness ────────────────────────────────────────────────

/// The acceptance harness on a real kernel path: `tc netem` drops every UDP datagram to the
/// server's QUIC port a fixed time into the transfer, and the transfer must finish over TCP with
/// no duplicate or corrupt state. Needs `tc` and `CAP_NET_ADMIN` over `lo`, which it reshapes,
/// so it compiles only with `--features http3-netem` and runs only when asked by name. An
/// unprivileged user and network namespace gives it a private `lo` to reshape: build the test
/// binary first, then run it inside one.
///
/// ```text
/// cargo test -p capsule-sdk --features http3-netem --lib --no-run
/// unshare -rn sh -c 'ip link set lo up && target/debug/deps/capsule_sdk-<hash> --ignored netem'
/// ```
#[cfg(feature = "http3-netem")]
mod netem {
    use std::process::Command;

    use super::*;

    /// How long into the transfer UDP starts dropping.
    const DROP_AFTER: Duration = Duration::from_secs(2);

    /// A `tc` rule set on `lo` dropping UDP to one port, removed on drop.
    struct UdpDrop;

    fn tc(args: &[&str]) {
        let status = Command::new("tc").args(args).status().expect("run tc");
        assert!(status.success(), "tc {args:?} failed: {status}");
    }

    impl UdpDrop {
        /// Four prio bands with the default priomap, so nothing lands in band 4 but what the
        /// filter sends there, and band 4 drops everything.
        fn port(port: u16) -> Self {
            let port = port.to_string();
            tc(&[
                "qdisc", "add", "dev", "lo", "root", "handle", "1:", "prio", "bands", "4",
                "priomap", "1", "2", "2", "2", "1", "2", "0", "0", "1", "1", "1", "1", "1", "1",
                "1", "1",
            ]);
            let guard = Self;
            tc(&[
                "qdisc", "add", "dev", "lo", "parent", "1:4", "handle", "40:", "netem", "loss",
                "100%",
            ]);
            tc(&[
                "filter", "add", "dev", "lo", "protocol", "ip", "parent", "1:0", "prio", "1",
                "u32", "match", "ip", "protocol", "17", "0xff", "match", "ip", "dport", &port,
                "0xffff", "flowid", "1:4",
            ]);
            guard
        }
    }

    impl Drop for UdpDrop {
        fn drop(&mut self) {
            let _ = Command::new("tc")
                .args(["qdisc", "del", "dev", "lo", "root"])
                .status();
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "needs tc and CAP_NET_ADMIN; run with --features http3-netem -- --ignored netem"]
    async fn netem_dropping_udp_mid_transfer_finishes_over_tcp() {
        let server = server().await;
        let transport = transport(&server, TCP_SLOWER, STALL);
        let bounds = config(server.roots.clone(), STALL).bounds;
        let chunks = source(48);
        let started = tokio::time::Instant::now();
        let mut rule = None;
        let mut vias = Vec::new();
        for (index, chunk) in chunks.iter().enumerate() {
            if rule.is_none() && started.elapsed() >= DROP_AFTER {
                rule = Some(UdpDrop::port(server.quic_addr.port()));
            }
            let sent = send_chunk(&transport, || put(&server, index, chunk.clone()), bounds).await;
            vias.push(sent.via);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(rule.is_some(), "the transfer outran the drop: {vias:?}");
        assert!(vias.contains(&Via::Quic), "{vias:?}");
        assert_eq!(vias.last(), Some(&Via::Tcp), "{vias:?}");
        server.store.assert_holds_exactly(&chunks);
    }
}
