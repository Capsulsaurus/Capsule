//! An in-process mock HTTP server for the SDK's hand-written wire clients (test-only).
//!
//! Replays the real wire — statuses, headers, `ApiError` JSON with the stable `error.*` codes
//! straight from the S-C1 handlers — over a real TCP socket, because the SDK's clients are
//! genuine `reqwest` clients rather than an injectable transport. Shared by the
//! [`upload`](crate::upload) client's recovery-matrix tests and the [`push`](crate::push)
//! bundle tests so there is one mock, not one per module.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::upload::{StaticToken, UploadClient, UploadTransport};

#[derive(Debug, Clone)]
pub(crate) struct MockRequest {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
}

impl MockRequest {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct MockResponse {
    pub(crate) status: u16,
    pub(crate) reason: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
}

impl MockResponse {
    pub(crate) fn new(status: u16, reason: &str) -> Self {
        Self {
            status,
            reason: reason.to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }
    pub(crate) fn header(mut self, k: &str, v: impl Into<String>) -> Self {
        self.headers.push((k.to_string(), v.into()));
        self
    }
    pub(crate) fn json_body(mut self, body: impl Into<String>) -> Self {
        self.headers
            .push(("Content-Type".into(), "application/json".into()));
        self.body = body.into().into_bytes();
        self
    }
    /// An `ApiError` rejection carrying the stable `error.*` code — the exact wire
    /// the S-C1 `UploadError::write` produces.
    pub(crate) fn api_error(status: u16, reason: &str, code: &str, message: &str) -> Self {
        MockResponse::new(status, reason)
            .json_body(format!(r#"{{"error":{message:?},"code":{code:?}}}"#))
    }
}

/// A dropped mock server aborts its accept loop.
pub(crate) struct MockServer {
    addr: SocketAddr,
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

impl MockServer {
    pub(crate) async fn start<F>(handler: F) -> Self
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
                tokio::spawn(async move {
                    handle_conn(stream, handler).await;
                });
            }
        });
        MockServer { addr, handle }
    }

    pub(crate) fn base_url(&self) -> String {
        let addr = self.addr;
        format!("http://{addr}")
    }

    pub(crate) fn client(&self, protocol: &str) -> UploadClient {
        let transport = UploadTransport::with_static_token(
            reqwest::Client::new(),
            self.base_url(),
            protocol,
            StaticToken("test-token".into()),
        );
        UploadClient::new(transport)
    }
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

async fn handle_conn<F>(mut stream: tokio::net::TcpStream, handler: Arc<F>)
where
    F: Fn(&MockRequest) -> MockResponse + Send + Sync + 'static,
{
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];

    // Read until the end of the header block.
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

    let mut headers = Vec::new();
    let mut content_length = 0usize;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim().to_string();
            let v = v.trim().to_string();
            if k.eq_ignore_ascii_case("content-length") {
                content_length = v.parse().unwrap_or(0);
            }
            headers.push((k, v));
        }
    }

    // Read the declared body.
    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < content_length {
        match stream.read(&mut tmp).await {
            Ok(0) | Err(_) => break,
            Ok(n) => body.extend_from_slice(&tmp[..n]),
        }
    }
    body.truncate(content_length);

    let is_head = method.eq_ignore_ascii_case("HEAD");
    let req = MockRequest {
        method,
        path,
        headers,
        body,
    };
    let resp = handler(&req);

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
