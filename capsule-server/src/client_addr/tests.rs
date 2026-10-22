use std::net::SocketAddr;

use kynos::http::body::Body;
use kynos::http::{HeaderMap, HeaderValue, Request};

use super::*;

fn ip(text: &str) -> IpAddr {
    text.parse().expect("a test address")
}

fn peer(text: &str) -> Option<SocketAddr> {
    Some(SocketAddr::new(ip(text), 40_000))
}

fn forwarded_for(value: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-forwarded-for",
        HeaderValue::from_str(value).expect("a header value"),
    );
    headers
}

/// What the router would resolve for these headers, peer and policy, as a [`ClientAddr`].
fn resolve(headers: &HeaderMap, peer: Option<SocketAddr>, trust: &ProxyTrust) -> ClientAddr {
    let forwarded = Forwarded::resolve(headers, peer, &trust.policy());
    let (mut parts, _body) = Request::new(Body::empty()).into_parts();
    // What dispatch inserts before any argument is built.
    let _ = parts.extensions.insert(forwarded);
    ClientAddr::of(&parts)
}

#[test]
fn the_default_trusts_nobody() {
    let trust = ProxyTrust::default();
    assert!(trust.trusts_nobody());
    assert!(trust.policy().trusts_nobody());
}

#[test]
fn a_spoofed_forwarded_for_from_an_untrusted_peer_is_ignored() {
    let headers = forwarded_for("198.51.100.7");
    let client = resolve(&headers, peer("203.0.113.9"), &ProxyTrust::default());
    assert_eq!(client.address(), Some(ip("203.0.113.9")));
    assert_eq!(client.source_key(), "203.0.113.9");
}

#[test]
fn a_peer_outside_the_trusted_networks_is_not_believed() {
    let trust = ProxyTrust {
        hops: 0,
        networks: ProxyTrust::parse_networks("10.0.0.0/8").expect("it parses"),
    };
    let headers = forwarded_for("198.51.100.7");
    let client = resolve(&headers, peer("203.0.113.9"), &trust);
    assert_eq!(client.address(), Some(ip("203.0.113.9")));
}

#[test]
fn a_trusted_network_hop_is_honoured() {
    let trust = ProxyTrust {
        hops: 0,
        networks: ProxyTrust::parse_networks("10.0.0.0/8").expect("it parses"),
    };
    let headers = forwarded_for("198.51.100.7");
    let client = resolve(&headers, peer("10.1.2.3"), &trust);
    assert_eq!(client.address(), Some(ip("198.51.100.7")));
}

#[test]
fn a_trusted_hop_count_stops_at_the_first_untrusted_element() {
    let trust = ProxyTrust {
        hops: 1,
        networks: Vec::new(),
    };
    // The client prepended a spoofed element; only the one the proxy appended is believed.
    let headers = forwarded_for("192.0.2.66, 198.51.100.7");
    let client = resolve(&headers, peer("10.1.2.3"), &trust);
    assert_eq!(client.address(), Some(ip("198.51.100.7")));
}

#[test]
fn no_socket_and_no_trusted_hop_is_the_unknown_source() {
    let client = resolve(&HeaderMap::new(), None, &ProxyTrust::default());
    assert_eq!(client.address(), None);
    assert_eq!(client.source_key(), UNKNOWN_SOURCE);
}

#[test]
fn a_request_with_no_resolution_recorded_is_the_unknown_source() {
    let (parts, _) = Request::new(Body::empty()).into_parts();
    assert_eq!(ClientAddr::of(&parts), ClientAddr::new(None));
}

#[test]
fn an_ipv6_client_is_keyed_by_its_slash_64() {
    let one = ClientAddr::new(Some(ip("2001:db8:1:2:aaaa::1")));
    let two = ClientAddr::new(Some(ip("2001:db8:1:2:bbbb::9")));
    let other = ClientAddr::new(Some(ip("2001:db8:1:3::1")));
    assert_eq!(one.source_key(), "2001:db8:1:2::/64");
    assert_eq!(one.source_key(), two.source_key());
    assert_ne!(one.source_key(), other.source_key());
}

#[test]
fn an_ipv4_mapped_client_is_keyed_as_ipv4() {
    let mapped = ClientAddr::new(Some(ip("::ffff:203.0.113.9")));
    let plain = ClientAddr::new(Some(ip("203.0.113.9")));
    assert_eq!(mapped.source_key(), plain.source_key());
}

#[test]
fn trusted_proxies_parse_addresses_and_networks() {
    let parsed = ProxyTrust::parse_networks(" 10.0.0.0/8, 192.168.1.5 ,fd00::/8,").expect("parses");
    assert_eq!(
        parsed,
        vec![
            (ip("10.0.0.0"), 8),
            (ip("192.168.1.5"), 32),
            (ip("fd00::"), 8),
        ]
    );
}

#[test]
fn a_malformed_trusted_proxy_entry_is_refused() {
    for entry in ["proxy.internal", "10.0.0.0/33", "fd00::/129", "10.0.0.0/x"] {
        let error = ProxyTrust::parse_networks(entry).expect_err(entry);
        assert!(error.contains(entry), "{entry}: {error}");
    }
}

#[test]
fn the_policy_is_displayed_for_the_startup_log() {
    assert_eq!(
        ProxyTrust::default().to_string(),
        "none (the socket peer is the client)"
    );
    let trust = ProxyTrust {
        hops: 1,
        networks: vec![(ip("10.0.0.0"), 8)],
    };
    assert_eq!(trust.to_string(), "1 hop(s), 10.0.0.0/8");
}
