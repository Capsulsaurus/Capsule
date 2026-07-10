//! Tests for the S-D4 verify-before-destroy client.
//!
//! The gate is driven against an in-process mock HTTP server that replays the real wire of
//! `POST /storage/verify` and `GET /upload/{id}/receipt` (the same shape the S-C3 / S-C15
//! handlers produce). Custody receipts are signed with a real `capsule-core` hybrid attestation
//! key so the client verifies the true signature under the pinned key — no crypto is faked.
//!
//! These mock-level smokes are deterministic and hermetic. The cross-module case — this client
//! driving the REAL media server (testcontainer Postgres, real attestation key, real disk) —
//! lives in `capsule-api/media`'s test suite, which dev-depends on `capsule-sdk`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

use capsule_core::crypto::hash::Hash32;
use capsule_core::crypto::keys::{HybridSigningKey, HybridVerifyingKey};
use capsule_core::library::{
    BlobRole, CustodyReceipt, CustodyReceiptCore, ReceiptExpectations, ReleaseDecision,
    RetainReason,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use uuid::Uuid;

use super::*;

// ─── In-process mock HTTP server (JSON only) ─────────────────────────────────

#[derive(Debug, Clone)]
struct MockRequest {
    method: String,
    path: String,
}

#[derive(Debug, Clone)]
struct MockResponse {
    status: u16,
    reason: String,
    body: Vec<u8>,
}

impl MockResponse {
    fn json(status: u16, reason: &str, body: impl Into<String>) -> Self {
        Self {
            status,
            reason: reason.to_string(),
            body: body.into().into_bytes(),
        }
    }
}

struct MockServer {
    addr: SocketAddr,
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

impl MockServer {
    async fn start<F>(handler: F) -> Self
    where
        F: Fn(&MockRequest) -> MockResponse + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handler = Arc::new(handler);
        let handle = tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(pair) => pair,
                    Err(_) => break,
                };
                let handler = handler.clone();
                tokio::spawn(async move { handle_conn(stream, handler).await });
            }
        });
        MockServer { addr, handle }
    }

    fn base_url(&self) -> String {
        let addr = self.addr;
        format!("http://{addr}")
    }

    fn client(&self) -> StorageVerifyClient {
        StorageVerifyClient::new(VerifyTransport::with_static_token(
            reqwest::Client::new(),
            self.base_url(),
            StaticToken("test-token".into()),
        ))
    }
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

async fn handle_conn<F>(mut stream: tokio::net::TcpStream, handler: Arc<F>)
where
    F: Fn(&MockRequest) -> MockResponse + Send + Sync + 'static,
{
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    let header_end = loop {
        if let Some(pos) = find_subsequence(&buf, b"\r\n\r\n") {
            break pos;
        }
        match stream.read(&mut tmp).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
        }
    };
    let header_text = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let raw_path = parts.next().unwrap_or_default();
    let path = raw_path.split('?').next().unwrap_or(raw_path).to_string();

    // Drain the declared body so the client's write completes cleanly.
    let mut content_length = 0usize;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < content_length {
        match stream.read(&mut tmp).await {
            Ok(0) | Err(_) => break,
            Ok(n) => body.extend_from_slice(&tmp[..n]),
        }
    }

    let resp = handler(&MockRequest { method, path });
    let mut out = format!("HTTP/1.1 {} {}\r\n", resp.status, resp.reason).into_bytes();
    out.extend_from_slice(
        format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            resp.body.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(&resp.body);
    let _ = stream.write_all(&out).await;
    let _ = stream.flush().await;
}

// ─── Fixtures ────────────────────────────────────────────────────────────────

const RECEIVED_AT: &str = "2026-07-10T00:00:00Z";

fn now_base() -> i64 {
    RECEIVED_AT.parse::<jiff::Timestamp>().unwrap().as_second()
}

/// A test clock the smokes advance to drive the 60 s re-verify window.
struct TestClock(AtomicI64);

impl TestClock {
    fn at(base: i64) -> Self {
        Self(AtomicI64::new(base))
    }
    fn advance(&self, secs: i64) {
        self.0.fetch_add(secs, Ordering::SeqCst);
    }
}

impl ReleaseClock for TestClock {
    fn now_unix(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

fn ct_hash() -> Hash32 {
    Hash32::from_bytes([0xAB; 32])
}

fn asset_id() -> Uuid {
    Uuid::from_u128(0x0192_f000_0000_7000_8000_0000_0000_0042)
}

fn upload_id() -> Uuid {
    Uuid::from_u128(0x0192_f000_0000_7000_8000_0000_0000_0099)
}

fn expectations() -> ReceiptExpectations {
    ReceiptExpectations {
        ciphertext_hash: ct_hash(),
        size: 4096,
        role: BlobRole::Original,
        envelope_hash: None,
    }
}

/// A durable verdict JSON for `asset_id()` over the one required blob.
fn durable_verdict_json(durable: bool) -> String {
    format!(
        r#"{{"verdicts":[{{"asset_id":{aid:?},"durable":{durable},"blobs":[{{"hash":{hash:?},"role":"original","stored":{durable},"indexed":{durable},"retrievable":{durable}}}],"checked_at":{ts:?}}}]}}"#,
        aid = asset_id().to_string(),
        durable = durable,
        hash = ct_hash().to_hex(),
        ts = RECEIVED_AT,
    )
}

/// Sign a valid custody receipt for the write and return `(attestation_key, receipt_cbor_b64)`.
fn signed_receipt() -> (HybridVerifyingKey, String) {
    let key = HybridSigningKey::from_seed64(&[9u8; 64]);
    let core = CustodyReceiptCore {
        version: "custody-receipt/v1".into(),
        crypto_suite_id: 1,
        protocol_version: "2026-07-10".into(),
        server_id: "capsule.example".into(),
        server_key_id: Hash32::from_bytes([0x11; 32]),
        receipt_seq: 1,
        prior_receipt_hash: None,
        upload_id: upload_id().to_string(),
        asset_id: asset_id().to_string(),
        blob_role: "original".into(),
        ciphertext_hash: ct_hash(),
        size: 4096,
        envelope_hash: None,
        uploaded_by_user: Uuid::from_u128(7).to_string(),
        uploaded_by_device: None,
        received_at: RECEIVED_AT.into(),
    };
    let server_sig = key.sign(&core.signing_bytes());
    let receipt = CustodyReceipt { core, server_sig };
    let b64 = base64::engine::general_purpose::STANDARD.encode(receipt.to_canonical_cbor());
    (key.verifying_key(), b64)
}

fn request() -> ReleaseRequest {
    ReleaseRequest {
        asset_id: asset_id(),
        upload_id: upload_id(),
        blob_hashes: vec![ct_hash()],
        expectations: expectations(),
        verify_asset_accepted: true,
    }
}

// ─── Smokes ──────────────────────────────────────────────────────────────────

/// The storage-verification doc's "Verify-before-destroy gate (smoke)": a non-`durable` verdict
/// refuses release and surfaces the unconfirmed state; flipped to `durable` (with a verified
/// receipt), the release proceeds.
#[tokio::test]
async fn verify_before_release_gate_smoke() {
    let (att_key, receipt_b64) = signed_receipt();
    let durable = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let d = durable.clone();
    let rb = receipt_b64.clone();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/storage/verify") => {
            MockResponse::json(200, "OK", durable_verdict_json(d.load(Ordering::SeqCst)))
        }
        ("GET", p) if p.ends_with("/receipt") => {
            MockResponse::json(200, "OK", format!(r#"{{"receipt_cbor":{rb:?}}}"#))
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let coord = ReleaseCoordinator::with_clock(
        server.client(),
        vec![att_key],
        DEFAULT_REVERIFY_WINDOW_SECS,
        TestClock::at(now_base()),
    );

    // Non-durable: retain, do not release.
    assert_eq!(
        coord.evaluate(&request()).await,
        ReleaseDecision::Retain(RetainReason::NotDurable)
    );

    // The server now durably holds the bytes → the next attempt re-queries (non-durable verdicts
    // are never cached) and the release proceeds.
    durable.store(true, Ordering::SeqCst);
    assert_eq!(coord.evaluate(&request()).await, ReleaseDecision::Release);
}

/// The storage-verification doc's "Receipt-gated release (smoke)": a device-owned-original
/// release with the receipt fetch failing is refused **even on `durable = true`**; once the
/// receipt endpoint is restored, the release proceeds.
#[tokio::test]
async fn receipt_gated_release_smoke() {
    let (att_key, receipt_b64) = signed_receipt();
    let receipt_ok = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ro = receipt_ok.clone();
    let rb = receipt_b64.clone();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/storage/verify") => MockResponse::json(200, "OK", durable_verdict_json(true)),
        ("GET", p) if p.ends_with("/receipt") => {
            if ro.load(Ordering::SeqCst) {
                MockResponse::json(200, "OK", format!(r#"{{"receipt_cbor":{rb:?}}}"#))
            } else {
                // Session Completed but the receipt is withheld — 409 (invariant 33 shape).
                MockResponse::json(
                    409,
                    "Conflict",
                    r#"{"code":"error.upload.receipt_not_available","error":"withheld"}"#,
                )
            }
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let coord = ReleaseCoordinator::with_clock(
        server.client(),
        vec![att_key],
        DEFAULT_REVERIFY_WINDOW_SECS,
        TestClock::at(now_base()),
    );

    // Durable, but the receipt is withheld → refuse to release the only copy.
    assert_eq!(
        coord.evaluate(&request()).await,
        ReleaseDecision::Retain(RetainReason::ReceiptUnavailable)
    );

    // Receipt endpoint restored → release proceeds.
    receipt_ok.store(true, Ordering::SeqCst);
    assert_eq!(coord.evaluate(&request()).await, ReleaseDecision::Release);
}

/// A receipt that verifies structurally but was signed by a **different** attestation key (or
/// carries mismatched fields) never releases — the pin is load-bearing.
#[tokio::test]
async fn unpinned_receipt_key_refuses_release() {
    let (_real_key, receipt_b64) = signed_receipt();
    let rb = receipt_b64.clone();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/storage/verify") => MockResponse::json(200, "OK", durable_verdict_json(true)),
        ("GET", p) if p.ends_with("/receipt") => {
            MockResponse::json(200, "OK", format!(r#"{{"receipt_cbor":{rb:?}}}"#))
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    // Pin an unrelated key: the real receipt's signature will not verify under it.
    let wrong_key = HybridSigningKey::from_seed64(&[3u8; 64]).verifying_key();
    let coord = ReleaseCoordinator::with_clock(
        server.client(),
        vec![wrong_key],
        DEFAULT_REVERIFY_WINDOW_SECS,
        TestClock::at(now_base()),
    );
    assert_eq!(
        coord.evaluate(&request()).await,
        ReleaseDecision::Retain(RetainReason::ReceiptMissing)
    );
}

/// The 60 s re-verify window: a second release attempt inside the window reuses the cached
/// verdict (no extra `/storage/verify` round-trip); past the window it re-fetches.
#[tokio::test]
async fn reverify_window_reuses_then_refetches() {
    let (att_key, receipt_b64) = signed_receipt();
    let verify_hits = Arc::new(AtomicUsize::new(0));
    let vh = verify_hits.clone();
    let rb = receipt_b64.clone();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/storage/verify") => {
            vh.fetch_add(1, Ordering::SeqCst);
            MockResponse::json(200, "OK", durable_verdict_json(true))
        }
        ("GET", p) if p.ends_with("/receipt") => {
            MockResponse::json(200, "OK", format!(r#"{{"receipt_cbor":{rb:?}}}"#))
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let clock = TestClock::at(now_base());
    // Take a handle to advance the shared clock after construction.
    let coord = ReleaseCoordinator::with_clock(
        server.client(),
        vec![att_key],
        DEFAULT_REVERIFY_WINDOW_SECS,
        clock,
    );

    assert_eq!(coord.evaluate(&request()).await, ReleaseDecision::Release);
    assert_eq!(
        verify_hits.load(Ordering::SeqCst),
        1,
        "first verdict fetched"
    );

    // A second attempt within the window reuses the cached verdict.
    assert_eq!(coord.evaluate(&request()).await, ReleaseDecision::Release);
    assert_eq!(
        verify_hits.load(Ordering::SeqCst),
        1,
        "verdict reused within the 60 s window"
    );

    // Advance past the window: the stale verdict is re-fetched.
    coord.clock.advance(DEFAULT_REVERIFY_WINDOW_SECS + 1);
    assert_eq!(coord.evaluate(&request()).await, ReleaseDecision::Release);
    assert_eq!(
        verify_hits.load(Ordering::SeqCst),
        2,
        "verdict re-fetched past the window"
    );
}

/// Wire mapping: a `/storage/verify` response decodes into the core `StorageVerdict` (hex →
/// `Hash32`, role string → closed enum) so `release_is_safe` can re-check it.
#[tokio::test]
async fn verify_response_maps_into_core_verdict() {
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/storage/verify") => MockResponse::json(200, "OK", durable_verdict_json(true)),
        other => panic!("unexpected {other:?}"),
    })
    .await;
    let client = server.client();
    let verdicts = client
        .verify(
            &[AssetQuery {
                asset_id: asset_id(),
                blob_hashes: vec![ct_hash()],
            }],
            false,
        )
        .await
        .unwrap();
    assert_eq!(verdicts.len(), 1);
    let v = &verdicts[0];
    assert_eq!(v.asset_id, asset_id());
    assert!(v.durable);
    assert_eq!(v.blobs.len(), 1);
    assert_eq!(v.blobs[0].hash, ct_hash());
    assert_eq!(v.blobs[0].role, BlobRole::Original);
    assert!(capsule_core::library::release_is_safe(v, true));
}
