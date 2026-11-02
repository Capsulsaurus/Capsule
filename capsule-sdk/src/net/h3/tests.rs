use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

/// A clock the test advances by hand.
#[derive(Debug, Clone, Default)]
struct HandClock(Arc<AtomicU64>);

impl HandClock {
    fn advance(&self, by: Duration) {
        self.0.fetch_add(by.as_millis() as u64, Ordering::SeqCst);
    }
}

impl MonotonicClock for HandClock {
    fn now_millis(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

const ORIGIN: &str = "https://capsule.example:443";
const NET: PathGeneration = PathGeneration(1);

fn capable() -> (HandClock, H3Policy<HandClock>) {
    let clock = HandClock::default();
    (clock.clone(), H3Policy::new(clock, true))
}

fn port_only(port: u16) -> H3Endpoint {
    H3Endpoint { host: None, port }
}

// ─── parse_alt_svc ───────────────────────────────────────────────────────────

#[test]
fn alt_svc_clear_withdraws() {
    assert_eq!(parse_alt_svc(" clear "), AltSvc::Clear);
    assert_eq!(parse_alt_svc("Clear"), AltSvc::Clear);
}

#[test]
fn alt_svc_keeps_only_h3_in_server_order() {
    let parsed =
        parse_alt_svc(r#"h3-29=":443", h2=":443", h3="alt.example:8443"; ma=60, h3=":443""#);
    assert_eq!(
        parsed,
        AltSvc::Services(vec![
            AltService {
                endpoint: H3Endpoint {
                    host: Some("alt.example".to_owned()),
                    port: 8443,
                },
                max_age: Duration::from_secs(60),
            },
            AltService {
                endpoint: port_only(443),
                max_age: ALT_SVC_DEFAULT_MAX_AGE,
            },
        ])
    );
}

#[test]
fn alt_svc_max_age_is_capped_and_unknown_params_ignored() {
    let parsed = parse_alt_svc(r#"h3=":443"; persist=1; ma=99999999; foo="a,b;c""#);
    assert_eq!(
        parsed,
        AltSvc::Services(vec![AltService {
            endpoint: port_only(443),
            max_age: ALT_SVC_MAX_AGE_CAP,
        }])
    );
}

#[test]
fn alt_svc_unparseable_ma_falls_back_to_default() {
    let AltSvc::Services(services) = parse_alt_svc(r#"h3=":443"; ma=soon"#) else {
        panic!("expected services");
    };
    assert_eq!(services[0].max_age, ALT_SVC_DEFAULT_MAX_AGE);
}

#[test]
fn alt_svc_skips_malformed_entries() {
    for header in [
        "h3",
        "h3=:443",
        r#"h3="""#,
        r#"h3="host""#,
        r#"h3=":0""#,
        r#"h3=":99999""#,
        r#"h3="a\"b:443""#,
        "",
    ] {
        assert_eq!(
            parse_alt_svc(header),
            AltSvc::Services(vec![]),
            "{header:?}"
        );
    }
}

#[test]
fn alt_svc_bracketed_ipv6_authority() {
    let AltSvc::Services(services) = parse_alt_svc(r#"h3="[2001:db8::1]:443""#) else {
        panic!("expected services");
    };
    assert_eq!(
        services[0].endpoint,
        H3Endpoint {
            host: Some("[2001:db8::1]".to_owned()),
            port: 443,
        }
    );
}

// ─── Discovery and the race ──────────────────────────────────────────────────

#[test]
fn nothing_advertised_rides_tcp() {
    let (_, policy) = capable();
    assert_eq!(
        policy.choose(ORIGIN, NET),
        TransportChoice::Tcp(TcpReason::NotAdvertised)
    );
}

#[test]
fn alt_svc_advertisement_races_until_it_expires() {
    let (clock, policy) = capable();
    policy.observe_alt_svc(ORIGIN, r#"h3=":443"; ma=60"#);
    assert_eq!(
        policy.choose(ORIGIN, NET),
        TransportChoice::Race {
            endpoint: port_only(443)
        }
    );
    // Another origin learns nothing from this one's header.
    assert_eq!(
        policy.choose("https://other.example:443", NET),
        TransportChoice::Tcp(TcpReason::NotAdvertised)
    );
    clock.advance(Duration::from_secs(60));
    assert_eq!(
        policy.choose(ORIGIN, NET),
        TransportChoice::Tcp(TcpReason::NotAdvertised)
    );
}

#[test]
fn alt_svc_clear_or_no_h3_withdraws_the_advertisement() {
    for withdrawal in ["clear", r#"h2=":443""#] {
        let (_, policy) = capable();
        policy.observe_alt_svc(ORIGIN, r#"h3=":443""#);
        policy.observe_alt_svc(ORIGIN, withdrawal);
        assert_eq!(
            policy.choose(ORIGIN, NET),
            TransportChoice::Tcp(TcpReason::NotAdvertised),
            "{withdrawal:?}"
        );
    }
}

#[test]
fn https_record_with_h3_advertises() {
    let (clock, policy) = capable();
    policy.observe_https_record(
        ORIGIN,
        443,
        &HttpsRecord {
            alpn: vec!["h2".to_owned(), "h3".to_owned()],
            port: None,
            target: Some("edge.example".to_owned()),
            ttl: Duration::from_secs(300),
        },
    );
    assert_eq!(
        policy.choose(ORIGIN, NET),
        TransportChoice::Race {
            endpoint: H3Endpoint {
                host: Some("edge.example".to_owned()),
                port: 443,
            }
        }
    );
    clock.advance(Duration::from_secs(300));
    assert_eq!(
        policy.choose(ORIGIN, NET),
        TransportChoice::Tcp(TcpReason::NotAdvertised)
    );
}

#[test]
fn https_record_without_h3_does_not_withdraw_alt_svc() {
    let (_, policy) = capable();
    policy.observe_alt_svc(ORIGIN, r#"h3=":8443""#);
    policy.observe_https_record(
        ORIGIN,
        443,
        &HttpsRecord {
            alpn: vec!["h2".to_owned()],
            port: None,
            target: None,
            ttl: Duration::from_secs(300),
        },
    );
    assert_eq!(
        policy.choose(ORIGIN, NET),
        TransportChoice::Race {
            endpoint: port_only(8443)
        }
    );
}

#[test]
fn advertisement_table_is_bounded() {
    let (clock, policy) = capable();
    // The first origin expires soonest, so it is the one a full table evicts.
    policy.observe_alt_svc("https://o0", r#"h3=":443"; ma=10"#);
    clock.advance(Duration::from_millis(1));
    for index in 1..=MAX_ADVERTISED_ORIGINS {
        policy.observe_alt_svc(&format!("https://o{index}"), r#"h3=":443"; ma=60"#);
    }
    assert_eq!(policy.state().advertised.len(), MAX_ADVERTISED_ORIGINS);
    assert_eq!(
        policy.choose("https://o0", NET),
        TransportChoice::Tcp(TcpReason::NotAdvertised)
    );
    assert!(matches!(
        policy.choose(&format!("https://o{MAX_ADVERTISED_ORIGINS}"), NET),
        TransportChoice::Race { .. }
    ));
}

#[test]
fn incapable_build_always_rides_tcp() {
    let policy = H3Policy::new(HandClock::default(), false);
    policy.observe_alt_svc(ORIGIN, r#"h3=":443""#);
    assert_eq!(
        policy.choose(ORIGIN, NET),
        TransportChoice::Tcp(TcpReason::NotBuilt)
    );
}

#[test]
fn for_build_is_incapable_until_a_transport_exists() {
    let policy = H3Policy::for_build(HandClock::default());
    policy.observe_alt_svc(ORIGIN, r#"h3=":443""#);
    let raced = matches!(policy.choose(ORIGIN, NET), TransportChoice::Race { .. });
    assert!(!raced);
}

// ─── UDP unusable and fallback at the request boundary ───────────────────────

#[test]
fn udp_unusable_falls_back_to_tcp_at_the_next_request_until_expiry() {
    for failure in [UdpFailure::DialFailed, UdpFailure::Stalled] {
        let (clock, policy) = capable();
        policy.observe_alt_svc(ORIGIN, r#"h3=":443""#);
        // The request in flight already chose; the failure only moves the next one.
        let in_flight = policy.choose(ORIGIN, NET);
        assert!(matches!(in_flight, TransportChoice::Race { .. }));
        policy.record_udp_unusable(NET, failure);
        assert_eq!(
            policy.choose(ORIGIN, NET),
            TransportChoice::Tcp(TcpReason::UdpUnusable),
            "{failure:?}"
        );
        clock.advance(UDP_UNUSABLE_TTL.saturating_sub(Duration::from_millis(1)));
        assert_eq!(
            policy.choose(ORIGIN, NET),
            TransportChoice::Tcp(TcpReason::UdpUnusable)
        );
        clock.advance(Duration::from_millis(1));
        assert!(matches!(
            policy.choose(ORIGIN, NET),
            TransportChoice::Race { .. }
        ));
        assert!(policy.state().udp_unusable.is_empty());
    }
}

#[test]
fn udp_unusable_is_per_network() {
    let (_, policy) = capable();
    policy.observe_alt_svc(ORIGIN, r#"h3=":443""#);
    policy.record_udp_unusable(NET, UdpFailure::Stalled);
    assert!(matches!(
        policy.choose(ORIGIN, PathGeneration(2)),
        TransportChoice::Race { .. }
    ));
}

#[test]
fn udp_records_are_bounded_oldest_path_first() {
    let (_, policy) = capable();
    for generation in 0..=MAX_UDP_RECORDS as u64 {
        policy.record_udp_unusable(PathGeneration(generation), UdpFailure::DialFailed);
    }
    let state = policy.state();
    assert_eq!(state.udp_unusable.len(), MAX_UDP_RECORDS);
    assert!(!state.udp_unusable.contains_key(&PathGeneration(0)));
}

// ─── Early data ──────────────────────────────────────────────────────────────

#[test]
fn early_data_is_never_taken() {
    use http::Method;
    for method in [
        Method::GET,
        Method::HEAD,
        Method::POST,
        Method::PUT,
        Method::PATCH,
        Method::DELETE,
        Method::OPTIONS,
    ] {
        assert!(!early_data_allowed(&method), "{method}");
    }
}

// ─── Bandwidth-delay product ─────────────────────────────────────────────────

#[test]
fn window_is_the_bdp_within_the_budget() {
    let budget = DEFAULT_WINDOW_BUDGET;
    // 10 MB/s × 100 ms = 1 MB, under one connection's share.
    assert_eq!(
        size_window(10_000_000, Duration::from_millis(100), 1, budget),
        1_000_000
    );
    // A tiny estimate is raised to the floor.
    assert_eq!(
        size_window(1_000, Duration::from_millis(10), 1, budget),
        budget.floor
    );
    // A huge product is cut to the connection's share of the total.
    assert_eq!(
        size_window(u64::MAX, Duration::from_secs(10), 4, budget),
        budget.total / 4
    );
    // Zero connections is treated as one.
    assert_eq!(
        size_window(u64::MAX, Duration::from_secs(10), 0, budget),
        budget.total
    );
}

#[test]
fn windows_never_sum_past_the_budget_even_against_the_floor() {
    let budget = WindowBudget {
        floor: 64 * 1024,
        total: 256 * 1024,
    };
    for connections in 1..=16_u32 {
        let window = size_window(1, Duration::from_millis(1), connections, budget);
        assert!(
            window * u64::from(connections) <= budget.total,
            "{connections}"
        );
    }
}
