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
//! | `a_refusal_names_the_rule_its_window_gives` | a `426` names the bound it broke, else `Refused` |
//! | `route_classes::a_read_is_served_and_reports_the_floor` | read (typed client): served, and the floor it revealed is reported |
//! | `route_classes::a_read_426_is_the_typed_refusal` | read (typed client): a `426` is `UpgradeRequired` |
//! | `route_classes::a_typed_write_is_refused_before_it_leaves` | write (typed client): refused locally with a `426` that maps to `UpgradeRequired` |
//! | `route_classes::a_session_write_is_refused_before_it_leaves` | write (session path): refused before sending, as `AuthError::UpgradeRequired` |
//! | `route_classes::a_session_write_426_is_the_typed_refusal` | write (session path): the server's `426` is the same refusal |
//! | `route_classes::session_upkeep_is_never_refused_locally` | refresh and sign-out still leave under a floor this build is below |
//!
//! The upload route class is covered in `upload/tests.rs`.

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

#[test]
fn a_refusal_names_the_rule_its_window_gives() {
    let below = UpgradeRequired::refused(
        &headers("2099-01-01", "2099-12-31", "0.0.0"),
        PROTOCOL_VERSION,
        "old",
    );
    assert_eq!(below.reason, UpgradeReason::ProtocolBelowWindow);
    assert_eq!(below.message, "old");

    let bare = UpgradeRequired::refused(&HeaderMap::new(), PROTOCOL_VERSION, "no window");
    assert_eq!(bare.reason, UpgradeReason::Refused);
    assert_eq!(*bare.window, ServerWindow::default());
}

// ─── Route classes over a mock server ───────────────────────────────────────
//
// The upload route class is covered beside its client (`upload/tests.rs`); these are the read
// and write classes, over the typed (generated) client and the session's hand-written path.

mod route_classes {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::auth::{AuthClient, AuthError, PersistedSession, Session};
    use crate::client::{AuthenticatedClient, Negotiate, upgrade_required_of};
    use crate::rest::{self, HttpBackend as _};
    use crate::testmock::{MockRequest, MockResponse, MockServer};

    fn session_for(base: &str) -> Session {
        AuthClient::new(base)
            .expect("a base url")
            .resume(PersistedSession {
                access_token: "test-access".to_string().into(),
                refresh_token: "test-refresh".to_string().into(),
                access_expires_at_unix: jiff::Timestamp::now().as_second() + 3_600,
            })
            .expect("a session resumes from any pair")
    }

    /// Every answer advertises a window this build's protocol is inside and a client-build
    /// floor it is below; every request's method is recorded.
    async fn floored_server() -> (MockServer, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let server = MockServer::start(move |req: &MockRequest| {
            log.lock().unwrap().push(req.method.clone());
            MockResponse::new(200, "OK")
                .header(PROTOCOL_MIN_HEADER, "2026-01-01")
                .header(PROTOCOL_MAX_HEADER, "2099-12-31")
                .header(MIN_CLIENT_BUILD_HEADER, "999.0.0")
                .json_body(r#"{"state":"ok","used":0}"#)
        })
        .await;
        (server, seen)
    }

    fn refusing_server(status: u16) -> impl Fn(&MockRequest) -> MockResponse + Send + Sync {
        move |_req: &MockRequest| {
            MockResponse::new(status, "Upgrade Required")
                .header(PROTOCOL_MIN_HEADER, "2099-01-01")
                .header(PROTOCOL_MAX_HEADER, "2099-12-31")
                .header("Content-Type", "application/problem+json")
                .json_body(
                    r#"{"type":"about:blank","status":426,"detail":"too old","code":"error.protocol.version_unsupported"}"#,
                )
        }
    }

    /// **Read, typed client.** A read is served whatever the window says ("reads of any past
    /// version succeed"), and the floor it revealed is what the client then reports.
    #[tokio::test]
    async fn a_read_is_served_and_reports_the_floor() {
        let (server, seen) = floored_server().await;
        let client =
            AuthenticatedClient::new(&server.base_url(), session_for(&server.base_url())).unwrap();
        assert!(
            client.upgrade_required().is_none(),
            "nothing advertised yet"
        );

        client
            .get_quota(PROTOCOL_VERSION, None)
            .await
            .expect("the read is served");

        let signal = client.upgrade_required().expect("the floor was latched");
        assert_eq!(signal.reason, UpgradeReason::BuildBelowFloor);
        assert_eq!(signal.window.min_client_build.as_deref(), Some("999.0.0"));
        assert_eq!(*seen.lock().unwrap(), vec!["GET".to_owned()]);
    }

    /// **Read, typed client, server refusal.** A `426` on a read, documented or not, is the
    /// one typed refusal too.
    #[tokio::test]
    async fn a_read_426_is_the_typed_refusal() {
        let server = MockServer::start(refusing_server(426)).await;
        let client =
            AuthenticatedClient::new(&server.base_url(), session_for(&server.base_url())).unwrap();
        let error = client
            .get_quota(PROTOCOL_VERSION, None)
            .await
            .expect_err("refused");
        let refusal = upgrade_required_of(&error).expect("a 426 is UpgradeRequired");
        assert_eq!(refusal.reason, UpgradeReason::ProtocolBelowWindow);
        assert_eq!(refusal.window.protocol_min.as_deref(), Some("2099-01-01"));
    }

    /// **Write, typed client.** Once a response latched the floor, a write through the typed
    /// client's transport is answered locally with a `426` carrying that window, and never
    /// reaches the server.
    #[tokio::test]
    async fn a_typed_write_is_refused_before_it_leaves() {
        let (server, seen) = floored_server().await;
        let backend = Negotiate::over(Arc::new(rest::ReqwestBackend::new(
            crate::net::http_client().unwrap(),
        )));
        let url = Url::parse(&server.base_url()).unwrap();

        let read = reqwest::Request::new(Method::GET, url.join("/v1/quota").unwrap());
        assert_eq!(
            backend.execute(read).await.unwrap().status(),
            StatusCode::OK
        );

        let write = reqwest::Request::new(Method::DELETE, url.join("/v1/sessions/x").unwrap());
        let answered = backend.execute(write).await.unwrap();
        assert_eq!(answered.status(), StatusCode::UPGRADE_REQUIRED);
        assert_eq!(
            answered
                .headers()
                .get(MIN_CLIENT_BUILD_HEADER)
                .and_then(|v| v.to_str().ok()),
            Some("999.0.0")
        );
        let body = answered.text().await.unwrap();
        assert!(
            body.contains(error_codes::PROTOCOL_VERSION_UNSUPPORTED),
            "{body}"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["GET".to_owned()],
            "the write never left"
        );

        // And the shape the generated operation maps that answer to is the typed refusal.
        let mut headers = HeaderMap::new();
        headers.insert(MIN_CLIENT_BUILD_HEADER, HeaderValue::from_static("999.0.0"));
        let error: rest::Error<std::io::Error> = rest::Error::UnexpectedStatus {
            status: StatusCode::UPGRADE_REQUIRED,
            headers,
            body: body.into_bytes().into(),
        };
        let refusal = upgrade_required_of(&error).expect("typed");
        assert_eq!(refusal.reason, UpgradeReason::BuildBelowFloor);
    }

    /// **Write, session path.** The hand-written surfaces (albums, verify, upgrade, upload)
    /// send through `Session::execute`: a write after a floored read is refused before it
    /// leaves, as [`AuthError::UpgradeRequired`].
    #[tokio::test]
    async fn a_session_write_is_refused_before_it_leaves() {
        let (server, seen) = floored_server().await;
        let session = session_for(&server.base_url());
        let base = server.base_url();

        session
            .execute(|http| http.get(format!("{base}/v1/anything")))
            .await
            .expect("the read is served");
        let error = session
            .execute(|http| http.post(format!("{base}/v1/anything")))
            .await
            .expect_err("the write is refused");
        let AuthError::UpgradeRequired(refusal) = error else {
            panic!("expected UpgradeRequired, got {error:?}");
        };
        assert_eq!(refusal.reason, UpgradeReason::BuildBelowFloor);
        assert_eq!(
            error_codes::PROTOCOL_VERSION_UNSUPPORTED,
            AuthError::UpgradeRequired(refusal).error_code().unwrap()
        );
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["GET".to_owned()],
            "the write never left"
        );
    }

    /// **Write, session path, server refusal.** The server's own `426` is the same typed
    /// refusal, with its detail as the message.
    #[tokio::test]
    async fn a_session_write_426_is_the_typed_refusal() {
        let server = MockServer::start(refusing_server(426)).await;
        let session = session_for(&server.base_url());
        let base = server.base_url();
        let error = session
            .execute(|http| http.post(format!("{base}/v1/anything")))
            .await
            .expect_err("refused");
        let AuthError::UpgradeRequired(refusal) = error else {
            panic!("expected UpgradeRequired, got {error:?}");
        };
        assert_eq!(refusal.reason, UpgradeReason::ProtocolBelowWindow);
        assert_eq!(refusal.message, "too old");
    }

    /// **Session upkeep.** Refresh and sign-out are never refused locally: a read needs a live
    /// session, and reads keep working against any server — only the server's own `426`
    /// refuses them.
    #[tokio::test]
    async fn session_upkeep_is_never_refused_locally() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let server = MockServer::start(move |req: &MockRequest| {
            log.lock()
                .unwrap()
                .push(format!("{} {}", req.method, req.path));
            let response = match req.path.as_str() {
                "/refresh" => MockResponse::new(200, "OK").json_body(
                    serde_json::json!({
                        "access_token": "fresh-access",
                        "refresh_token": "fresh-refresh",
                        "expires_by": jiff::Timestamp::now().as_second() + 3_600,
                    })
                    .to_string(),
                ),
                "/logout" => MockResponse::new(204, "No Content"),
                _ => MockResponse::new(200, "OK").json_body("{}"),
            };
            response
                .header(PROTOCOL_MIN_HEADER, "2026-01-01")
                .header(PROTOCOL_MAX_HEADER, "2099-12-31")
                .header(MIN_CLIENT_BUILD_HEADER, "999.0.0")
        })
        .await;
        let base = server.base_url();
        let session = session_for(&base);

        session
            .execute(|http| http.get(format!("{base}/v1/anything")))
            .await
            .expect("the read is served and latches the floor");
        session
            .refresh()
            .await
            .expect("a refresh is upkeep, not a data write");
        session.logout().await.expect("and so is sign-out");
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                "GET /v1/anything".to_owned(),
                "POST /refresh".to_owned(),
                "POST /logout".to_owned()
            ]
        );
    }
}
