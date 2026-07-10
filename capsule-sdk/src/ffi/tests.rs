//! Host-verifiable smoke for the S-D9 FFI surface.
//!
//! The native (Swift + Kotlin) harnesses drive a login → upload → status round-trip
//! against a dev server *through the generated bindings* — that is the platform CI's
//! half and needs native toolchains. This is the Rust-side half of the same
//! Done-when: it exercises the FFI-exposed flow functions directly (the exact
//! `async fn`s uniffi wraps) against the established in-process mock-HTTP-server
//! pattern (mirroring `auth.rs` / `upload/tests.rs`), so the surface is proven
//! wired end-to-end without a native toolchain.
//!
//! Sync (`sync_pull`) is gRPC and is exercised by the native harness against the
//! real server; its Rust-side shape is compiled here (the surface builds) but not
//! behaviorally driven — see the module docs.

use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::*;

const PROTOCOL: &str = "2026-07-10";

// ─── Minimal in-process mock HTTP server ─────────────────────────────────────

struct MockRequest {
    method: String,
    path: String,
}

struct MockResponse {
    status: u16,
    reason: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl MockResponse {
    fn new(status: u16, reason: &str) -> Self {
        Self {
            status,
            reason: reason.to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }
    fn header(mut self, key: &str, value: impl Into<String>) -> Self {
        self.headers.push((key.to_string(), value.into()));
        self
    }
    fn json(mut self, body: impl Into<String>) -> Self {
        self.headers
            .push(("Content-Type".into(), "application/json".into()));
        self.body = body.into().into_bytes();
        self
    }
}

struct MockServer {
    addr: std::net::SocketAddr,
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
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let handler = handler.clone();
                tokio::spawn(async move { handle_conn(stream, handler).await });
            }
        });
        MockServer { addr, handle }
    }

    fn base(&self) -> String {
        format!("http://{}", self.addr)
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

    let mut content_length = 0usize;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
    }

    // Drain the declared request body so the client's write completes before we
    // respond and close (we don't need to inspect it for these smokes).
    let mut drained = buf[header_end + 4..].len();
    while drained < content_length {
        match stream.read(&mut tmp).await {
            Ok(0) | Err(_) => break,
            Ok(n) => drained += n,
        }
    }

    let is_head = method.eq_ignore_ascii_case("HEAD");
    let resp = handler(&MockRequest { method, path });

    let mut out = format!("HTTP/1.1 {} {}\r\n", resp.status, resp.reason).into_bytes();
    for (k, v) in &resp.headers {
        out.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
    }
    let body_len = if is_head { 0 } else { resp.body.len() };
    out.extend_from_slice(
        format!("Content-Length: {body_len}\r\nConnection: close\r\n\r\n").as_bytes(),
    );
    if !is_head {
        out.extend_from_slice(&resp.body);
    }
    let _ = stream.write_all(&out).await;
    let _ = stream.flush().await;
}

// ─── Fixtures ────────────────────────────────────────────────────────────────

fn far_future() -> i64 {
    jiff::Timestamp::now().as_second() + 3600
}

fn token_json() -> String {
    serde_json::json!({
        "access_token": "access-1",
        "refresh_token": "refresh-1",
        "token_type": "Bearer",
        "expires_by": far_future(),
    })
    .to_string()
}

fn ffi_request(size: u64) -> FfiUploadRequest {
    FfiUploadRequest {
        size,
        hash: "0".repeat(64),
        content_type: "image/jpeg".into(),
        crypto_suite_id: 1,
        protocol_version: PROTOCOL.into(),
        blob_role: FfiBlobRole::Original,
        manifest_envelope: FfiManifestEnvelope {
            crypto_suite_id: 1,
            protocol_version: PROTOCOL.into(),
            album_id: Some("album-1".into()),
            file_id: "0192f000-0000-7000-8000-000000000001".into(),
            amk_version: 1,
            ciphertext_hash: "0".repeat(64),
            plaintext_size: size,
            chunk_size: 65536,
            key_mode: "derived".into(),
            metadata_blob_hash: None,
            created_by_user: "user-1".into(),
            created_by_device: "device-1".into(),
            client_version: "capsule-test/0".into(),
            timestamp: "2026-07-10T00:00:00Z".into(),
            action: "create".into(),
            prior_provenance_hash: None,
            retention_until: None,
        },
        album_id: Some("album-1".into()),
        owner_id: None,
        intent_id: None,
    }
}

// ─── The FFI-exposed flow: login → upload → status ───────────────────────────

/// The whole Done-when host-verifiable half: build the FFI client, `login` through
/// the exported entry point to get a session **handle**, `upload` a blob through the
/// handle, then `upload_status` (HEAD) — all against one mock server replaying the
/// real wire. No token ever leaves the handle.
#[tokio::test]
async fn ffi_login_upload_status_round_trip() {
    const SIZE: u64 = 4096;
    let server =
        MockServer::start(
            |req: &MockRequest| match (req.method.as_str(), req.path.as_str()) {
                ("POST", "/auth/login") => MockResponse::new(200, "OK").json(token_json()),
                ("POST", "/auth/logout") => MockResponse::new(200, "OK").json(r#"{"error":"ok"}"#),
                // Fresh upload session (offset 0).
                ("POST", "/upload") => MockResponse::new(201, "Created")
                    .header("X-Capsule-Offset", "0")
                    .json(
                        serde_json::json!({
                            "id": "sess-1",
                            "upload_url": "/upload/sess-1",
                            "suggested_chunk_size": 4096,
                        })
                        .to_string(),
                    ),
                // Chunk accepted; no offset header → the client advances by chunk length.
                ("PATCH", "/upload/sess-1") => MockResponse::new(200, "OK"),
                // Status (HEAD): fully received, finalizing.
                ("HEAD", "/upload/sess-1") => MockResponse::new(200, "OK")
                    .header("X-Capsule-Offset", SIZE.to_string())
                    .header("X-Capsule-Content-Length", SIZE.to_string())
                    .header("X-Capsule-Upload-Status", "finalizing"),
                _ => MockResponse::new(404, "Not Found").json(r#"{"error":"no route"}"#),
            },
        )
        .await;

    let client = FfiCapsuleClient::new(
        format!("{}/auth", server.base()),
        format!("{}/upload", server.base()),
        PROTOCOL.to_string(),
        None,
    )
    .unwrap();

    // login → session handle (tokens stay inside the handle).
    let session = client
        .login("a@example.com".into(), "pw".into())
        .await
        .unwrap();
    assert!(session.is_authenticated().await);

    // upload file through the handle.
    let outcome = session
        .upload(ffi_request(SIZE), vec![7u8; SIZE as usize])
        .await
        .unwrap();
    let session_id = match outcome {
        FfiUploadOutcome::Completed { session_id } => session_id,
        FfiUploadOutcome::AlreadyStored { asset_ref } => panic!("unexpected merge: {asset_ref}"),
    };
    assert_eq!(session_id, "sess-1");

    // upload status (HEAD) through the handle.
    let status = session
        .upload_status(session_id)
        .await
        .unwrap()
        .expect("session present");
    assert_eq!(status.offset, SIZE);
    assert_eq!(status.total_size, Some(SIZE));
    assert_eq!(status.status, "finalizing");

    // logout clears the handle.
    session.logout().await.unwrap();
    assert!(!session.is_authenticated().await);
}

/// A `duplicate_blob` on create resolves through the FFI as the merge outcome, not a
/// transfer — the exact `error.*`-code-switched path the upload client owns.
#[tokio::test]
async fn ffi_upload_surfaces_duplicate_blob_merge() {
    let server = MockServer::start(|req: &MockRequest| match (req.method.as_str(), req.path.as_str())
    {
        ("POST", "/auth/login") => MockResponse::new(200, "OK").json(token_json()),
        ("POST", "/upload") => MockResponse::new(409, "Conflict").json(
            r#"{"error":"This content is already stored as asset asset-xyz","code":"error.upload.duplicate_blob"}"#,
        ),
        _ => MockResponse::new(404, "Not Found").json(r#"{"error":"no route"}"#),
    })
    .await;

    let client = FfiCapsuleClient::new(
        format!("{}/auth", server.base()),
        format!("{}/upload", server.base()),
        PROTOCOL.to_string(),
        None,
    )
    .unwrap();
    let session = client
        .login("a@example.com".into(), "pw".into())
        .await
        .unwrap();

    let outcome = session
        .upload(ffi_request(4096), vec![7u8; 4096])
        .await
        .unwrap();
    match outcome {
        FfiUploadOutcome::AlreadyStored { asset_ref } => assert_eq!(asset_ref, "asset-xyz"),
        FfiUploadOutcome::Completed { .. } => panic!("expected a merge, not a transfer"),
    }
}

/// Login failure maps to a typed `FfiError::Auth` carrying the stable catalog code
/// (which clients localize) — never a bare status.
#[tokio::test]
async fn ffi_login_invalid_credentials_carries_catalog_code() {
    let server =
        MockServer::start(
            |req: &MockRequest| match (req.method.as_str(), req.path.as_str()) {
                ("POST", "/auth/login") => MockResponse::new(401, "Unauthorized")
                    .json(r#"{"error":"Invalid credentials"}"#),
                _ => MockResponse::new(404, "Not Found").json(r#"{"error":"no route"}"#),
            },
        )
        .await;

    let client = FfiCapsuleClient::new(
        format!("{}/auth", server.base()),
        format!("{}/upload", server.base()),
        PROTOCOL.to_string(),
        None,
    )
    .unwrap();

    // The Ok type (`Arc<FfiSession>`) is not Debug, so match rather than `unwrap_err`.
    match client.login("a@example.com".into(), "bad".into()).await {
        Err(FfiError::Auth { code, .. }) => assert_eq!(
            code.as_deref(),
            Some(capsule_i18n::error_codes::AUTH_INVALID_CREDENTIALS)
        ),
        Err(other) => panic!("expected FfiError::Auth, got {other:?}"),
        Ok(_) => panic!("expected invalid-credentials to fail"),
    }
}

/// A bad base URL is rejected at construction as `InvalidArgument`, before any call.
#[test]
fn ffi_client_rejects_invalid_base_url() {
    // `Arc<FfiCapsuleClient>` (the Ok type) is not Debug, so match rather than
    // `unwrap_err`.
    match FfiCapsuleClient::new(
        "not a url".into(),
        "also bad".into(),
        PROTOCOL.to_string(),
        None,
    ) {
        Err(FfiError::InvalidArgument { .. }) => {}
        Err(other) => panic!("expected InvalidArgument, got {other:?}"),
        Ok(_) => panic!("expected an invalid-base-url rejection"),
    }
}
