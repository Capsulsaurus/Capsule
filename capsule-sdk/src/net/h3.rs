//! The opportunistic HTTP/3 policy (networking doc, HTTP Transport).
//!
//! HTTP/3 is an optimization the client may take, never a property the protocol depends on. This
//! module is the whole of the decision the SDK makes about it, as plain values over an injected
//! [`MonotonicClock`] so every rule is a deterministic unit test:
//!
//! - **Discovery.** The client learns of an HTTP/3 endpoint only from an `Alt-Svc` response header
//!   ([`H3Policy::observe_alt_svc`], parsed by [`parse_alt_svc`]) or an HTTPS DNS record
//!   ([`H3Policy::observe_https_record`]). Each advertisement expires; a bounded table holds them.
//! - **Race at dial.** [`H3Policy::choose`] answers [`TransportChoice::Race`] for an origin with a
//!   live advertisement on a network whose UDP is not known to be unusable. The race is between
//!   connection attempts, never between requests: a request rides exactly one dialed connection.
//! - **Per-network "UDP unusable" record.** A QUIC dial that fails while TCP succeeds, or a QUIC
//!   path that stops making progress, records UDP as unusable on that [`PathGeneration`] for
//!   [`UDP_UNUSABLE_TTL`] ([`H3Policy::record_udp_unusable`]). A new path starts with no record.
//! - **Fallback at a request boundary.** The choice is taken once, when a request starts. A
//!   request already riding QUIC is never moved mid-flight: it fails, the retry engine re-sends it,
//!   and the re-send's [`H3Policy::choose`] answers TCP. Resume reads server truth, so the switch
//!   costs at most the bytes in flight.
//! - **No connection migration.** Nothing here keys on a QUIC connection; a new path generation
//!   drops pooled connections ([`super::path::GenerationPool`]) exactly as it does for TCP.
//! - **No early data.** [`early_data_allowed`] refuses every method: the contract permits `GET` and
//!   `HEAD` in TLS 1.3 early data, and this build takes none of it.
//! - **Windows cover the bandwidth-delay product** within a fixed memory budget ([`size_window`]).
//!
//! Default builds never race: [`H3Policy::for_build`] is capable only with the off-by-default
//! `http3` cargo feature, and even then no transport adapter consumes the choice yet, because the
//! SDK's `reqwest` 0.12 exposes HTTP/3 only under `--cfg reqwest_unstable`. Every transport stays
//! pinned to HTTP/1.1 ([`super::http_builder`]).

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use tracing::instrument;

use super::MonotonicClock;
use super::path::PathGeneration;

/// The ALPN protocol id an advertisement must carry. Draft ids (`h3-29` and earlier) are ignored.
pub const H3_ALPN: &str = "h3";

/// How long an `Alt-Svc` entry holds when it names no `ma` (RFC 7838 §3.1: 24 hours).
pub const ALT_SVC_DEFAULT_MAX_AGE: Duration = Duration::from_hours(24);

/// The longest any advertisement is believed, whatever it claims, so a stale endpoint cannot be
/// pinned for days.
pub const ALT_SVC_MAX_AGE_CAP: Duration = Duration::from_hours(24);

/// The most origins whose advertisement is held at once; the soonest to expire is evicted first.
pub const MAX_ADVERTISED_ORIGINS: usize = 64;

/// How long a network keeps its "UDP unusable" record before QUIC is tried on it again.
pub const UDP_UNUSABLE_TTL: Duration = Duration::from_mins(10);

/// The most networks whose "UDP unusable" record is held at once; the oldest path is evicted first.
pub const MAX_UDP_RECORDS: usize = 8;

// ─── Alt-Svc ─────────────────────────────────────────────────────────────────

/// Where an advertised HTTP/3 endpoint listens. `host` is `None` when the advertisement names
/// only a port (`":443"`), meaning the origin's own host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct H3Endpoint {
    /// The alternative host, or `None` for the origin's host.
    pub host: Option<String>,
    /// The UDP port.
    pub port: u16,
}

/// One `h3` alternative from an `Alt-Svc` header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AltService {
    /// Where it listens.
    pub endpoint: H3Endpoint,
    /// How long it holds, already clamped to [`ALT_SVC_MAX_AGE_CAP`].
    pub max_age: Duration,
}

/// A parsed `Alt-Svc` header value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AltSvc {
    /// `clear`: every alternative for the origin is withdrawn.
    Clear,
    /// The `h3` alternatives in the order the server preferred them. Empty when the header named
    /// none this client can use.
    Services(Vec<AltService>),
}

/// Parse an `Alt-Svc` header value (RFC 7838 §3), keeping only `h3` alternatives.
///
/// Lenient as the RFC asks: an entry that does not parse, or names another protocol, is skipped
/// rather than failing the header; unknown parameters, and an `ma` that is not a number, are
/// ignored.
#[must_use]
pub fn parse_alt_svc(value: &str) -> AltSvc {
    let value = value.trim();
    if value.eq_ignore_ascii_case("clear") {
        return AltSvc::Clear;
    }
    let services = split_outside_quotes(value, ',')
        .into_iter()
        .filter_map(parse_alt_value)
        .collect();
    AltSvc::Services(services)
}

/// One `protocol-id="alt-authority"; param=value; ...` entry, or `None` if it is not a usable
/// `h3` alternative.
fn parse_alt_value(entry: &str) -> Option<AltService> {
    let mut parts = split_outside_quotes(entry, ';').into_iter();
    let (protocol, authority) = parts.next()?.split_once('=')?;
    if protocol.trim() != H3_ALPN {
        return None;
    }
    let authority = unquote(authority.trim())?;
    let (host, port) = authority.rsplit_once(':')?;
    let port: u16 = port.parse().ok().filter(|p| *p != 0)?;
    let host = (!host.is_empty()).then(|| host.to_owned());

    let mut max_age = ALT_SVC_DEFAULT_MAX_AGE;
    for param in parts {
        let Some((name, raw)) = param.split_once('=') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("ma") {
            let raw = raw.trim();
            let raw = unquote(raw).unwrap_or(raw);
            if let Ok(secs) = raw.parse() {
                max_age = Duration::from_secs(secs);
            }
        }
    }
    Some(AltService {
        endpoint: H3Endpoint { host, port },
        max_age: max_age.min(ALT_SVC_MAX_AGE_CAP),
    })
}

/// Split on `sep` wherever it is not inside a quoted string, trimming each piece and dropping
/// empty ones.
fn split_outside_quotes(value: &str, sep: char) -> Vec<&str> {
    let mut pieces = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut escaped = false;
    for (index, ch) in value.char_indices() {
        match ch {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            c if c == sep && !quoted => {
                pieces.push(&value[start..index]);
                start = index + c.len_utf8();
            }
            _ => {}
        }
    }
    pieces.push(&value[start..]);
    pieces
        .into_iter()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect()
}

/// The inside of a `"..."` quoted string with no escapes, or `None` if it is not one.
fn unquote(value: &str) -> Option<&str> {
    let inner = value.strip_prefix('"')?.strip_suffix('"')?;
    (!inner.contains(['"', '\\'])).then_some(inner)
}

// ─── HTTPS DNS record ────────────────────────────────────────────────────────

/// The fields of an HTTPS (SVCB) DNS record the policy reads, as a resolver reports them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpsRecord {
    /// The record's `alpn` values.
    pub alpn: Vec<String>,
    /// The record's `port`, or `None` for the origin's port.
    pub port: Option<u16>,
    /// The record's target name, or `None` for the origin's host (target `.`).
    pub target: Option<String>,
    /// The record's TTL.
    pub ttl: Duration,
}

// ─── The policy ──────────────────────────────────────────────────────────────

/// Why a request rides TCP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpReason {
    /// This build has no HTTP/3 transport.
    NotBuilt,
    /// The origin advertised no live HTTP/3 endpoint.
    NotAdvertised,
    /// UDP is recorded unusable on the current network.
    UdpUnusable,
}

/// How the next request to an origin is dialed. Taken once, when the request starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportChoice {
    /// Dial TCP only.
    Tcp(TcpReason),
    /// Race a QUIC dial to `endpoint` against a TCP dial; the request rides whichever connects
    /// first.
    Race {
        /// The advertised HTTP/3 endpoint.
        endpoint: H3Endpoint,
    },
}

/// Why UDP was recorded unusable on a network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UdpFailure {
    /// The QUIC dial failed while the TCP dial it raced connected.
    DialFailed,
    /// A QUIC path stopped making progress mid-request.
    Stalled,
}

#[derive(Debug, Default)]
struct State {
    /// Origin → (endpoint, expiry in clock millis).
    advertised: HashMap<String, (H3Endpoint, u64)>,
    /// Network → expiry in clock millis.
    udp_unusable: HashMap<PathGeneration, u64>,
}

/// The opportunistic HTTP/3 decision: what has been advertised, which networks have unusable UDP,
/// and what the next request should dial.
#[derive(Debug)]
pub struct H3Policy<C> {
    clock: C,
    capable: bool,
    state: Mutex<State>,
}

impl<C: MonotonicClock> H3Policy<C> {
    /// A policy over `clock`. `capable` says whether the build has an HTTP/3 transport; an
    /// incapable policy still records what it observes and always answers TCP.
    pub fn new(clock: C, capable: bool) -> Self {
        Self {
            clock,
            capable,
            state: Mutex::new(State::default()),
        }
    }

    /// A policy capable exactly when the build enables the `http3` cargo feature.
    pub fn for_build(clock: C) -> Self {
        Self::new(clock, cfg!(feature = "http3"))
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Record an `Alt-Svc` header `origin` answered with. `clear`, or a header naming no usable
    /// `h3` alternative, withdraws the origin's advertisement; otherwise the first `h3`
    /// alternative replaces it.
    #[instrument(level = "debug", skip(self))]
    pub fn observe_alt_svc(&self, origin: &str, header: &str) {
        let first = match parse_alt_svc(header) {
            AltSvc::Clear => None,
            AltSvc::Services(services) => services.into_iter().next(),
        };
        match first {
            Some(service) => self.advertise(origin, service.endpoint, service.max_age),
            None => {
                if self.state().advertised.remove(origin).is_some() {
                    tracing::debug!(origin, "h3: advertisement withdrawn");
                }
            }
        }
    }

    /// Record an HTTPS DNS record for `origin` whose port is `origin_port`. A record without `h3`
    /// in its `alpn` changes nothing: DNS is one of two discovery sources, so its silence does
    /// not withdraw what `Alt-Svc` advertised.
    #[instrument(level = "debug", skip(self))]
    pub fn observe_https_record(&self, origin: &str, origin_port: u16, record: &HttpsRecord) {
        if !record.alpn.iter().any(|alpn| alpn == H3_ALPN) {
            return;
        }
        let endpoint = H3Endpoint {
            host: record.target.clone(),
            port: record.port.unwrap_or(origin_port),
        };
        self.advertise(origin, endpoint, record.ttl.min(ALT_SVC_MAX_AGE_CAP));
    }

    fn advertise(&self, origin: &str, endpoint: H3Endpoint, max_age: Duration) {
        let now = self.clock.now_millis();
        let expires = now.saturating_add(max_age.as_millis() as u64);
        let mut state = self.state();
        state.advertised.retain(|_, (_, until)| *until > now);
        if !state.advertised.contains_key(origin)
            && state.advertised.len() >= MAX_ADVERTISED_ORIGINS
        {
            let soonest = state
                .advertised
                .iter()
                .min_by_key(|(_, (_, until))| *until)
                .map(|(key, _)| key.clone());
            if let Some(key) = soonest {
                state.advertised.remove(&key);
            }
        }
        tracing::debug!(
            origin,
            ?endpoint,
            max_age_s = max_age.as_secs(),
            "h3: advertised"
        );
        state
            .advertised
            .insert(origin.to_owned(), (endpoint, expires));
    }

    /// Record UDP as unusable on `network` for [`UDP_UNUSABLE_TTL`]. Requests already started
    /// keep their transport; the next [`Self::choose`] on this network answers TCP.
    #[instrument(level = "info", skip(self))]
    pub fn record_udp_unusable(&self, network: PathGeneration, failure: UdpFailure) {
        let until = self
            .clock
            .now_millis()
            .saturating_add(UDP_UNUSABLE_TTL.as_millis() as u64);
        let mut state = self.state();
        state.udp_unusable.insert(network, until);
        while state.udp_unusable.len() > MAX_UDP_RECORDS {
            let Some(oldest) = state.udp_unusable.keys().min().copied() else {
                break;
            };
            state.udp_unusable.remove(&oldest);
        }
        tracing::info!(
            network = network.0,
            ?failure,
            "h3: UDP recorded unusable; TCP until expiry"
        );
    }

    /// How a request to `origin` starting now on `network` is dialed.
    #[instrument(level = "trace", skip(self), ret)]
    pub fn choose(&self, origin: &str, network: PathGeneration) -> TransportChoice {
        if !self.capable {
            return TransportChoice::Tcp(TcpReason::NotBuilt);
        }
        let now = self.clock.now_millis();
        let mut state = self.state();
        if let Some(until) = state.udp_unusable.get(&network).copied() {
            if until > now {
                return TransportChoice::Tcp(TcpReason::UdpUnusable);
            }
            state.udp_unusable.remove(&network);
            tracing::debug!(network = network.0, "h3: UDP-unusable record expired");
        }
        match state.advertised.get(origin) {
            Some((endpoint, until)) if *until > now => TransportChoice::Race {
                endpoint: endpoint.clone(),
            },
            Some(_) => {
                state.advertised.remove(origin);
                TransportChoice::Tcp(TcpReason::NotAdvertised)
            }
            None => TransportChoice::Tcp(TcpReason::NotAdvertised),
        }
    }
}

// ─── Early data ──────────────────────────────────────────────────────────────

/// Whether a request with `method` may ride TLS 1.3 early data. Never, in this build: the
/// contract forbids it for every non-safe method and only permits it for `GET` and `HEAD`, which
/// this client does not take.
#[must_use]
pub fn early_data_allowed(_method: &http::Method) -> bool {
    false
}

// ─── Bandwidth-delay product ─────────────────────────────────────────────────

/// The fixed memory a transport may spend on flow-control and socket windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowBudget {
    /// The smallest window worth opening, so a slow first estimate cannot starve a connection.
    pub floor: u64,
    /// The most bytes all connections' windows may hold together.
    pub total: u64,
}

/// 64 KiB floor, 32 MiB across every connection.
pub const DEFAULT_WINDOW_BUDGET: WindowBudget = WindowBudget {
    floor: 64 * 1024,
    total: 32 * 1024 * 1024,
};

/// The window for one of `connections` connections carrying `bandwidth` bytes per second over a
/// round trip of `rtt`: the bandwidth-delay product, cut to the connection's equal share of the
/// budget, and raised to the floor only where that share allows it. The windows of all
/// `connections` never sum past [`WindowBudget::total`].
#[must_use]
pub fn size_window(bandwidth: u64, rtt: Duration, connections: u32, budget: WindowBudget) -> u64 {
    let share = budget.total / u64::from(connections.max(1));
    let bdp =
        (u128::from(bandwidth) * rtt.as_micros() / 1_000_000).min(u128::from(u64::MAX)) as u64;
    bdp.max(budget.floor).min(share)
}

#[cfg(test)]
mod tests;
