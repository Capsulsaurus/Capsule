//! The window grammar, the verdict, and the per-origin latch.
//!
//! | test | proves |
//! | --- | --- |
//! | `headers_parse_into_a_window` | the three response headers are read verbatim, blanks as absent |
//! | `inside_the_window_is_no_verdict` | a client inside every bound is never refused |
//! | `each_bound_has_its_own_reason` | below Min, above Max and below the build floor each name their rule |
//! | `an_unparsable_bound_is_no_bound` | a malformed header never refuses (apps stay lenient) |
//! | `the_build_floor_compares_numerically` | `0.10.0` is above `0.9.0`; pre-release and build suffixes are ignored |
//! | `the_latch_refuses_writes_and_never_reads` | a latched window refuses a write before it leaves, and lets a read through |
//! | `the_latch_is_per_origin` | one server's floor never refuses a write to another |
//! | `the_read_side_signal_reports_without_refusing` | `upgrade_required_for` reports what a served read revealed |
//! | `the_problem_detail_is_the_message` | a `426` body's `detail` (or `error`, or text) becomes the message |

use reqwest::header::{HeaderMap, HeaderValue};

use super::*;

fn window(min: &str, max: &str, floor: &str) -> ServerWindow {
    ServerWindow {
        protocol_min: Some(min.to_owned()),
        protocol_max: Some(max.to_owned()),
        min_client_build: Some(floor.to_owned()),
    }
}

fn headers(min: &str, max: &str, floor: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in [
        (PROTOCOL_MIN_HEADER, min),
        (PROTOCOL_MAX_HEADER, max),
        (MIN_CLIENT_BUILD_HEADER, floor),
    ] {
        headers.insert(name, HeaderValue::from_str(value).expect("header value"));
    }
    headers
}

/// A fresh origin per test, so the process-wide latch never couples two tests.
fn origin(port: u16) -> Url {
    Url::parse(&format!("http://negotiation-test.invalid:{port}/v1/x")).expect("url")
}

#[test]
fn headers_parse_into_a_window() {
    let parsed = ServerWindow::from_headers(&headers("2026-01-01", "2026-12-31", "1.2.3"));
    assert_eq!(parsed, window("2026-01-01", "2026-12-31", "1.2.3"));
    assert!(parsed.is_advertised());

    let blank = ServerWindow::from_headers(&headers(" ", "", "  "));
    assert_eq!(blank, ServerWindow::default());
    assert!(!blank.is_advertised());
}

#[test]
fn inside_the_window_is_no_verdict() {
    let open = window("2026-01-01", "2026-12-31", "0.0.0");
    assert_eq!(open.verdict("2026-05-31", "0.1.0"), None);
    assert_eq!(
        open.verdict("2026-01-01", "0.0.0"),
        None,
        "bounds are inclusive"
    );
    assert_eq!(
        open.verdict("2026-12-31", "0.1.0"),
        None,
        "bounds are inclusive"
    );
    assert_eq!(ServerWindow::default().verdict("2026-05-31", "0.1.0"), None);
    assert_eq!(
        window("2026-01-01", "2026-12-31", "0.0.0").verdict(PROTOCOL_VERSION, CLIENT_BUILD),
        None,
        "this build is inside a window spanning its own protocol date"
    );
}

#[test]
fn each_bound_has_its_own_reason() {
    let w = window("2026-06-01", "2026-12-31", "2.0.0");
    assert_eq!(
        w.verdict("2026-05-31", "9.9.9"),
        Some(UpgradeReason::ProtocolBelowWindow)
    );
    assert_eq!(
        w.verdict("2027-01-01", "9.9.9"),
        Some(UpgradeReason::ProtocolAboveWindow)
    );
    assert_eq!(
        w.verdict("2026-07-01", "1.9.9"),
        Some(UpgradeReason::BuildBelowFloor)
    );
}

#[test]
fn an_unparsable_bound_is_no_bound() {
    let junk = window("soon", "2026/12/31", "latest");
    assert_eq!(junk.verdict("2026-05-31", "0.1.0"), None);
    // A malformed client protocol is not judged against the dates either.
    assert_eq!(
        window("2026-06-01", "2026-12-31", "0.0.0").verdict("garbage", "0.1.0"),
        None
    );
}

#[test]
fn the_build_floor_compares_numerically() {
    assert_eq!(parse_build("0.10.0"), Some((0, 10, 0)));
    assert!(parse_build("0.10.0") > parse_build("0.9.0"));
    assert_eq!(parse_build("1.2.3-rc.1+abc"), Some((1, 2, 3)));
    assert_eq!(parse_build("1.2"), None);
    assert_eq!(parse_build("1.2.3.4"), None);
    let w = window("2026-01-01", "2026-12-31", "0.10.0");
    assert_eq!(
        w.verdict("2026-05-31", "0.9.0"),
        Some(UpgradeReason::BuildBelowFloor)
    );
    assert_eq!(w.verdict("2026-05-31", "0.10.0"), None);
}

#[test]
fn the_latch_refuses_writes_and_never_reads() {
    let url = origin(1);
    // Nothing latched: nothing refused.
    assert!(preflight(&Method::POST, &url, PROTOCOL_VERSION).is_ok());

    observe(&url, &headers("2099-01-01", "2099-12-31", "0.0.0"));
    for read in [Method::GET, Method::HEAD, Method::OPTIONS] {
        assert!(preflight(&read, &url, PROTOCOL_VERSION).is_ok(), "{read}");
    }
    for write in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
        let refusal = preflight(&write, &url, PROTOCOL_VERSION).expect_err("write refused");
        assert_eq!(
            refusal.reason,
            UpgradeReason::ProtocolBelowWindow,
            "{write}"
        );
        assert_eq!(refusal.protocol, PROTOCOL_VERSION);
        assert_eq!(refusal.build, CLIENT_BUILD);
        assert_eq!(refusal.window.protocol_min.as_deref(), Some("2099-01-01"));
        assert_eq!(
            refusal.error_code(),
            error_codes::PROTOCOL_VERSION_UNSUPPORTED
        );
    }

    // A later response that admits this build clears the refusal; one that advertises nothing
    // leaves the latch as it was.
    observe(&url, &HeaderMap::new());
    assert!(preflight(&Method::POST, &url, PROTOCOL_VERSION).is_err());
    observe(&url, &headers("2026-01-01", "2099-12-31", "0.0.0"));
    assert!(preflight(&Method::POST, &url, PROTOCOL_VERSION).is_ok());
}

#[test]
fn the_latch_is_per_origin() {
    let floored = origin(2);
    let other = origin(3);
    observe(&floored, &headers("2026-01-01", "2099-12-31", "999.0.0"));
    assert_eq!(
        preflight(&Method::POST, &floored, PROTOCOL_VERSION)
            .expect_err("floored")
            .reason,
        UpgradeReason::BuildBelowFloor
    );
    assert!(preflight(&Method::POST, &other, PROTOCOL_VERSION).is_ok());
}

#[test]
fn the_read_side_signal_reports_without_refusing() {
    let url = origin(4);
    assert_eq!(upgrade_required_for(&url), None);
    observe(&url, &headers("2026-01-01", "2099-12-31", "999.0.0"));
    let signal = upgrade_required_for(&url).expect("signalled");
    assert_eq!(signal.reason, UpgradeReason::BuildBelowFloor);
    assert_eq!(
        advertised(&url).and_then(|w| w.min_client_build),
        Some("999.0.0".to_owned())
    );
}

#[test]
fn the_problem_detail_is_the_message() {
    assert_eq!(
        problem_detail(r#"{"type":"about:blank","status":426,"detail":"too old","code":"c"}"#),
        "too old"
    );
    assert_eq!(problem_detail(r#"{"error":"legacy","code":"c"}"#), "legacy");
    assert_eq!(problem_detail("plain text"), "plain text");
}
