//! **E2E case 9** — the cross-version protocol gate, end to end — and the body-less `413`.
//!
//! Case 9's wording: a client whose `protocol_version` falls outside the server's range
//! attempts an upload, receives `426`, and the UI surfaces an actionable error. The UI leg is
//! out of scope here; what the SDK hands the UI is the typed error with the server's window,
//! asserted from three angles:
//!
//! 1. a per-transport pin outside the default window is refused at `POST /v1/upload` with the
//!    window the server advertises (`UploadError::UpgradeRequired { min, max }`);
//! 2. a server booted with `PROTOCOL_MIN = PROTOCOL_MAX = 2000-01-01` — the whole
//!    `Config` → `boot` → `Negotiation` path — refuses this build's *writes* with `426` and
//!    `error.protocol.version_unsupported`, and stamps `X-Capsule-Protocol-Min/Max` on every
//!    response, the exempt ones included;
//! 3. a *read* at an out-of-window date succeeds and carries the window (issue #404's
//!    decision: reads of any grammatical protocol date are admitted), while a malformed
//!    handshake is `400 error.request.malformed`.
//!
//! The `413` contract: Kynos's body-size backstop answers with no problem body, so the SDK
//! reports it as `code: None` rather than minting a code the server never sent.

use capsule_core::crypto::pwkdf::WrappedSecret;
use capsule_e2e::push::ensure_album;
use capsule_e2e::{Device, PASSWORD, PROTOCOL_VERSION, Server};
use capsule_sdk::auth::{AuthClient, AuthError};
use capsule_sdk::push::{bundle_blobs, create_request};
use capsule_sdk::recovery::{RecoveryClient, RecoveryError};
use capsule_sdk::rest;
use capsule_sdk::upload::{UploadClient, UploadError, UploadTransport};

const DEFAULT_MIN: &str = "2026-01-01";
const DEFAULT_MAX: &str = "2026-12-31";
const STALE: &str = "1999-01-01";
const VERSION_UNSUPPORTED: &str = "error.protocol.version_unsupported";

/// **E2E case 9**, leg 1: a stale transport pin against the default window.
#[tokio::test]
async fn e2e_case_9_a_stale_pin_is_refused_with_the_servers_window() {
    let server = Server::boot().await;
    let mut device = Device::register(&server, "stale").await;
    let asset = device.import_jpeg("stale.jpg");
    let bundle = device
        .workspace
        .upload_bundle(&asset)
        .expect("a bundle for the asset");
    let blobs = bundle_blobs(&bundle);
    let (blob, hash) = blobs.first().expect("a bundle has a T0 blob");
    let request = create_request(&bundle, blob, hash);

    // The pin wins over the transport's default header (`net.rs`): this is the one
    // hand-written place the SDK lets a caller speak an older protocol.
    let stale = UploadClient::new(UploadTransport::with_session(
        device.session.clone(),
        server.upload_base(),
        STALE,
    ));
    let refused = stale
        .create_session(&request)
        .await
        .expect_err("a protocol date before the window is refused");
    match refused {
        UploadError::UpgradeRequired { min, max, .. } => {
            assert_eq!(min.as_deref(), Some(DEFAULT_MIN));
            assert_eq!(max.as_deref(), Some(DEFAULT_MAX));
        }
        other => panic!("expected UpgradeRequired, got {other:?}"),
    }

    // The same request at this build's date succeeds — the pin was the only difference.
    device
        .upload_client(&server)
        .create_session(&request)
        .await
        .expect("this build's protocol date is inside the window");
}

/// **E2E case 9**, leg 2: a server whose window excludes this build refuses its writes and
/// advertises the window on every response.
#[tokio::test]
async fn e2e_case_9_a_server_outside_this_builds_window_refuses_writes_with_426() {
    let server = Server::boot_with_window("2000-01-01", "2000-01-01").await;

    // The first write any client makes — registration — through the SDK's own auth client.
    let Err(refused) = AuthClient::new(&server.auth_base())
        .expect("the auth base parses")
        .register("nobody@e2e.capsule.test", PASSWORD)
        .await
    else {
        panic!("a write from outside the window is refused");
    };
    match &refused {
        AuthError::Unexpected { status, code, .. } => {
            assert_eq!(*status, 426);
            assert_eq!(code.as_deref(), Some(VERSION_UNSUPPORTED));
        }
        other => panic!("expected a 426 with the gate's code, got {other:?}"),
    }
    assert_eq!(refused.error_code(), Some(VERSION_UNSUPPORTED));

    // The window rides every response, an exempt operation's included (`GET /v1/version` is
    // one of the ten the design exempts, and this client sends no handshake at all).
    let exempt = rest::Client::new(server.base_url()).expect("the API root parses");
    let version = exempt.get_version().await.expect("the version is public");
    let header = |name: &str| {
        version
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    assert_eq!(
        header("x-capsule-protocol-min").as_deref(),
        Some("2000-01-01")
    );
    assert_eq!(
        header("x-capsule-protocol-max").as_deref(),
        Some("2000-01-01")
    );
}

/// **E2E case 9**, leg 3: reads are admitted at any grammatical date and carry the window;
/// a handshake that does not parse is a `400` everywhere the gate stands.
#[tokio::test]
async fn e2e_case_9_reads_at_an_old_protocol_succeed_and_carry_the_window() {
    let server = Server::boot().await;
    let device = Device::register(&server, "reader").await;
    let feed = format!("{}/v1/sync", server.base_url());

    // An explicit header wins over the transport's default: the request leaves at `1999-01-01`.
    let response = device
        .session
        .execute(|http| http.get(&feed).header("x-capsule-protocol", STALE))
        .await
        .expect("the feed answers");
    assert_eq!(
        response.status().as_u16(),
        200,
        "a read at an old date is admitted"
    );
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    assert_eq!(
        header("x-capsule-protocol-min").as_deref(),
        Some(DEFAULT_MIN)
    );
    assert_eq!(
        header("x-capsule-protocol-max").as_deref(),
        Some(DEFAULT_MAX)
    );

    let malformed = device
        .session
        .execute(|http| http.get(&feed).header("x-capsule-protocol", "yesterday"))
        .await
        .expect("the gate answers");
    assert_eq!(malformed.status().as_u16(), 400);
    let problem: serde_json::Value = malformed.json().await.expect("a problem body");
    assert_eq!(problem["code"], "error.request.malformed");
}

/// The body-less `413`: the transport backstop carries no problem body, so the SDK reports
/// `code: None` rather than a code the server never sent.
#[tokio::test]
async fn a_body_past_the_transport_limit_reaches_the_sdk_as_a_codeless_413() {
    let server = Server::boot().await;
    let device = Device::register(&server, "escrow").await;
    let recovery = RecoveryClient::new(device.session.clone(), server.base_url())
        .expect("the API root parses");

    // 33 MiB: one past the 32 MiB `BodySize` backstop. Fast Argon2id parameters, because
    // nothing here derives — the bytes never reach the escrow route's own checks.
    let oversized = WrappedSecret {
        mem_kib: 64,
        t_cost: 1,
        p_cost: 1,
        salt: [0; 32],
        nonce: [0; 12],
        ciphertext: vec![0; 33 * 1024 * 1024],
    };
    let refused = recovery
        .store_escrow(&oversized)
        .await
        .expect_err("a body past the transport limit is refused");
    match &refused {
        RecoveryError::Malformed { code, .. } => assert!(
            code.is_none(),
            "a body-less 413 carries no code for the SDK to relay, got {code:?}"
        ),
        other => panic!("expected Malformed, got {other:?}"),
    }
    assert_eq!(refused.error_code(), None);

    // The account is otherwise healthy: the same client provisions and reads as before.
    let albums =
        capsule_sdk::albums::AlbumClient::new(capsule_sdk::albums::AlbumTransport::with_session(
            device.session.clone(),
            server.albums_base(),
        ));
    ensure_album(&albums, device.workspace.default_album_id())
        .await
        .expect("the session survives the refusal");
    let _ = PROTOCOL_VERSION;
}
