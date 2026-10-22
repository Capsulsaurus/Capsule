//! Who is calling: the one client address every per-source limiter keys on (issue #704).
//!
//! # The address is the router's to resolve, once
//!
//! Behind a reverse proxy the socket peer is the proxy, so a limiter keyed on it is a global
//! limiter wearing a per-source label. The forwarding headers name the real client, but RFC 7239
//! §8.1 is explicit that every hop on the way — the client included — can write them, so a
//! limiter keyed on them unconditionally throttles by a value the attacker chooses. Neither is
//! acceptable, and the way out is a deployment stating which hops it trusts.
//!
//! [`ProxyTrust`] is that statement, read from the environment by [`crate::config`]. The router
//! is built with it ([`crate::service_behind`]), and Kynos resolves every request **once**, at
//! dispatch, into a `Forwarded` value: the socket peer, then each forwarding element right to
//! left for as long as the sender of that element is trusted. [`ClientAddr`] reads that value
//! back. There is exactly one resolution per request and one policy per process, so two limiters
//! cannot disagree about who a caller is.
//!
//! Unset, nothing is trusted: the socket peer is the client and every forwarding header is
//! ignored. That default is the one a spoofed header cannot move.
//!
//! # Which field is read
//!
//! Exactly one, the one the deployment names (`TRUSTED_PROXY_HEADER`, [`ProxyTrust::header`]). A
//! proxy appends to one forwarding field and passes the other through from the client untouched,
//! so believing both lets the client write the one the proxy does not and choose its own key.
//!
//! The default is `X-Forwarded-For` ([`DEFAULT_TRUSTED_PROXY_HEADER`]): the field the proxies a
//! deployment usually puts in front of this server write (nginx, an AWS ALB, Caddy, Traefik), and
//! the one the [self-hosting guide](../../capsule-docs/src/content/docs/guides/self-hosting.md)
//! configures. A deployment whose proxy writes only the RFC 7239 `Forwarded` field names that
//! instead. Whichever is named, the other is ignored whoever passes it on.
//!
//! # What a per-source key is
//!
//! [`ClientAddr::source_key`] is the string a [`CounterKey`](crate::counter::CounterKey) source
//! variant carries:
//!
//! - an IPv4 client is its address;
//! - an IPv6 client is its **`/64`**, because a single subscriber is routinely handed a whole
//!   `/64` and walks it for free — a per-address IPv6 key is a limiter one `ip addr add` defeats;
//! - an IPv4-mapped IPv6 address (`::ffff:a.b.c.d`) is the IPv4 address it maps, so a dual-stack
//!   listener does not hand one client two buckets;
//! - a request that arrived on no socket and named no trusted hop — the in-process test client —
//!   shares one `unknown` bucket rather than being exempt, because exemption is the thing a
//!   limiter must never grant by default.

use std::convert::Infallible;
use std::fmt;
use std::net::{IpAddr, Ipv6Addr};

use kynos::extract::FromRequestParts;
use kynos::extract::describe::Describe;
use kynos::http::Parts;
use kynos::http::forwarded::{Forwarded, Network, ProxyHeader, TrustedProxies};
use kynos::router::operation::OperationCx;

/// The source key a request with no resolvable address counts against.
pub const UNKNOWN_SOURCE: &str = "unknown";

/// The forwarding field a trusted hop is believed through when `TRUSTED_PROXY_HEADER` is unset.
/// See the module docs.
pub const DEFAULT_TRUSTED_PROXY_HEADER: ProxyHeader = ProxyHeader::XForwarded;

/// The IPv6 prefix length one source key covers.
const IPV6_SOURCE_PREFIX: u32 = 64;

/// Which forwarding hops a deployment believes, and through which field (`TRUSTED_PROXY_HOPS`,
/// `TRUSTED_PROXIES`, `TRUSTED_PROXY_HEADER`).
///
/// Two independent ways of naming a proxy, which compose exactly as Kynos's own policy does:
///
/// - **`hops`** trusts the N forwarding elements nearest the server, whoever wrote them. Right
///   for a managed load balancer whose addresses change under you.
/// - **`networks`** trusts any element written by a sender inside these networks. Right when the
///   proxies' addresses are known.
///
/// Both read the one field **`header`** names. The default trusts nobody, through
/// [`DEFAULT_TRUSTED_PROXY_HEADER`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyTrust {
    /// How many rightmost forwarding elements are believed regardless of their sender.
    pub hops: usize,
    /// Networks (address and prefix length) whose hops are believed.
    pub networks: Vec<(IpAddr, u8)>,
    /// The one forwarding field the trusted hops write, and so the only one read.
    pub header: ProxyHeader,
}

impl Default for ProxyTrust {
    fn default() -> Self {
        Self {
            hops: 0,
            networks: Vec::new(),
            header: DEFAULT_TRUSTED_PROXY_HEADER,
        }
    }
}

impl ProxyTrust {
    /// Whether this policy believes any forwarding header at all.
    pub fn trusts_nobody(&self) -> bool {
        self.hops == 0 && self.networks.is_empty()
    }

    /// The policy as the router takes it.
    ///
    /// A network Kynos refuses — a `/0`, which [`ProxyTrust::parse_networks`] already refuses —
    /// is left out and logged rather than widened to trusting everyone.
    pub fn policy(&self) -> TrustedProxies {
        let networks = self
            .networks
            .iter()
            .filter_map(|&(address, prefix)| match Network::new(address, prefix) {
                Ok(network) => Some(network),
                Err(error) => {
                    tracing::warn!(%address, prefix, %error, "a trusted proxy network was ignored");
                    None
                }
            })
            .collect::<Vec<_>>();
        TrustedProxies::hops(self.header, self.hops).and_networks(networks)
    }

    /// The configured field's name as an operator spells it in a proxy's configuration.
    pub fn header_name(&self) -> &'static str {
        match self.header {
            ProxyHeader::XForwarded => "X-Forwarded-For",
            ProxyHeader::Forwarded => "Forwarded",
        }
    }

    /// Parse `TRUSTED_PROXY_HEADER`: `x-forwarded-for` or `forwarded`, in any case.
    ///
    /// Surrounding whitespace is ignored.
    ///
    /// # Errors
    ///
    /// Any other value, as a sentence naming the two that are accepted.
    pub fn parse_header(text: &str) -> Result<ProxyHeader, String> {
        let name = text.trim();
        if name.eq_ignore_ascii_case("x-forwarded-for") {
            Ok(ProxyHeader::XForwarded)
        } else if name.eq_ignore_ascii_case("forwarded") {
            Ok(ProxyHeader::Forwarded)
        } else {
            Err(format!(
                "`{name}` is not a forwarding field this server reads (use `x-forwarded-for` or `forwarded`)"
            ))
        }
    }

    /// Parse `TRUSTED_PROXIES`: a comma-separated list of addresses and CIDR networks.
    ///
    /// A bare address is its own single-address network (`/32` or `/128`). Whitespace around an
    /// entry is ignored and empty entries are skipped, so a trailing comma is harmless.
    ///
    /// # Errors
    ///
    /// The first entry that is not an address, or whose prefix is longer than its family allows,
    /// as a sentence an operator can act on.
    pub fn parse_networks(text: &str) -> Result<Vec<(IpAddr, u8)>, String> {
        text.split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(parse_network)
            .collect()
    }
}

impl fmt::Display for ProxyTrust {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.trusts_nobody() {
            return f.write_str("none (the socket peer is the client)");
        }
        write!(f, "{} hop(s)", self.hops)?;
        for (address, prefix) in &self.networks {
            write!(f, ", {address}/{prefix}")?;
        }
        write!(f, " through {}", self.header_name())
    }
}

/// One `TRUSTED_PROXIES` entry.
fn parse_network(entry: &str) -> Result<(IpAddr, u8), String> {
    let (address, prefix) = match entry.split_once('/') {
        Some((address, prefix)) => (address, Some(prefix)),
        None => (entry, None),
    };
    let address: IpAddr = address
        .parse()
        .map_err(|error| format!("`{entry}` is not an IP address or CIDR network ({error})"))?;
    let longest = if address.is_ipv4() { 32 } else { 128 };
    let prefix = match prefix {
        None => longest,
        Some(prefix) => prefix
            .parse::<u8>()
            .ok()
            .filter(|prefix| *prefix <= longest)
            .ok_or_else(|| {
                format!("`{entry}` has a prefix length that is not between 0 and {longest}")
            })?,
    };
    // A `/0` (or its IPv4-mapped spelling) trusts every sender, which lets any client choose
    // its own key; Kynos refuses it as a network, and so does this.
    Network::new(address, prefix).map_err(|error| format!("`{entry}` is refused: {error}"))?;
    Ok((address, prefix))
}

/// The client address of a request, resolved through the deployment's [`ProxyTrust`].
///
/// A handler argument that contributes nothing to the description, like Kynos's own
/// `ConnectInfo`: who connected is a property of the transport, not of the contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientAddr(Option<IpAddr>);

impl ClientAddr {
    /// A client address, for a caller that already knows it.
    pub fn new(address: Option<IpAddr>) -> Self {
        Self(address)
    }

    /// The resolved address, or `None` where no socket carried the request and no trusted hop
    /// named one.
    pub fn address(self) -> Option<IpAddr> {
        self.0
    }

    /// The key a per-source limiter counts this client against. See the module docs.
    pub fn source_key(self) -> String {
        match self.0.map(canonical) {
            None => UNKNOWN_SOURCE.to_owned(),
            Some(IpAddr::V4(address)) => address.to_string(),
            Some(IpAddr::V6(address)) => {
                let mask = u128::MAX << (128 - IPV6_SOURCE_PREFIX);
                let network = Ipv6Addr::from(u128::from(address) & mask);
                format!("{network}/{IPV6_SOURCE_PREFIX}")
            }
        }
    }

    /// The client the router's resolution names, or none where there was no resolution.
    fn of(forwarded: Option<&Forwarded>) -> Self {
        Self(forwarded.and_then(Forwarded::client))
    }
}

/// An IPv4-mapped IPv6 address as the IPv4 address it maps; anything else unchanged.
fn canonical(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(address, IpAddr::V4),
        IpAddr::V4(_) => address,
    }
}

/// Infallible for the reason Kynos's `ConnectInfo` is: the transport answers this, and no request
/// a client can send fails to produce an answer.
impl<C: Sync> FromRequestParts<C> for ClientAddr {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, context: &C) -> Result<Self, Self::Rejection> {
        // Kynos's own argument for the one resolution dispatch made before any interceptor ran;
        // it keeps that resolution in a request extension of its own rather than a `Forwarded`.
        let Ok(forwarded) = Forwarded::from_request_parts(parts, context).await;
        let client = Self::of(Some(&forwarded));
        tracing::trace!(client = ?client.0, "resolved the client address");
        Ok(client)
    }
}

impl Describe for ClientAddr {
    fn describe(operation: &mut OperationCx<'_>) {
        let _ = operation;
    }
}

#[cfg(test)]
mod tests;
