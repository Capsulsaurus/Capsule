//! The upload state machine under generated fault schedules (testing plan, `capsule-sdk` row:
//! "Upload, fetch and restore state machines reach the right end state under every fault
//! schedule; resume is driven by server truth; version refusals surface as one typed error").
//!
//! A model upload server runs over a real TCP socket, because the SDK's client is a genuine
//! `reqwest` client rather than an injectable transport. It keeps the authoritative state of
//! every session — the bytes received, whether the blob finalized — and answers the honest
//! protocol: a `PATCH` at the wrong offset is an `offset_mismatch` naming the right one, a
//! checksum that does not match its body is refused, a repeated create returns the active
//! session with its offset. Each request first draws the next fault from a proptest-generated
//! schedule: a server-asked wait, a dropped connection, a response lost after the server applied
//! the request (the duplicate a retry then sends), a short write, a rolled-back offset, a lost
//! session, a server error, a version refusal, a `duplicate_blob`, or the client process dying
//! mid-request.
//!
//! The driver plays the app around the SDK: an attempt that ends in a transport failure, an
//! exhausted retry budget or a kill is resumed — from the session it was told about when it has
//! one, from scratch otherwise — and every other end state is final. The properties:
//!
//! - **The right end state.** No refusal reached: the transfer completes, the session it names is
//!   the one the server finalized, and the server holds exactly the client's bytes. A version
//!   refusal reached: the run ends in [`UploadError::UpgradeRequired`], whichever request drew
//!   it. A server error: [`UploadError::Rejected`] carrying it. A `duplicate_blob`: a merge.
//! - **Server truth.** The client never sends a chunk at an offset the server did not give it,
//!   except straight after a fault that hid the server's answer from it (a lost response, a
//!   kill); every chunk's checksum matches its body.
//!
//! The clock is paused, so the retry engine's backoff costs nothing, and every case runs under a
//! fixed, printed seed (`PROPTEST_RNG_SEED` overrides it) that a failure replays from.

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use capsule_i18n::error_codes;
use capsule_sdk::upload::{
    BlobRole, CreateUploadRequest, ManifestEnvelope, StaticToken, UploadClient, UploadError,
    UploadOutcome, UploadTransport,
};
use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed, TestCaseError, TestRunner};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Notify;

/// The seed every case runs under when `PROPTEST_RNG_SEED` is unset.
const DEFAULT_SEED: u64 = 0x0CA9_5017;
/// Cases per run unless `PROPTEST_CASES` overrides it.
const CASES: u32 = 96;
/// The longest fault schedule. Short of the client's recovery budget (64 actions), so the
/// budget is never what ends a case; that ceiling has its own unit test.
const MAX_FAULTS: usize = 24;

// ── The fault schedule ───────────────────────────────────────────────────────

/// What the server does to one request before (or instead of) answering it honestly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    /// Answer honestly.
    Honest,
    /// `429 rate_limited`, `Retry-After: 0`; nothing applied.
    RateLimited,
    /// `503 unavailable`, `Retry-After: 0`; nothing applied.
    Unavailable,
    /// `507 storage_full`, `Retry-After: 0`; nothing applied.
    StorageFull,
    /// Close the connection without answering; nothing applied.
    Drop,
    /// Apply the request, then close the connection without answering: the client's retry is
    /// the duplicate delivery.
    LostResponse,
    /// A `PATCH`: keep only the first half of the chunk and acknowledge that.
    ShortWrite,
    /// A `PATCH`: refuse with `checksum_mismatch`; nothing applied.
    CorruptInTransit,
    /// A `PATCH`: the server lost its last half of the received bytes and says so with an
    /// `offset_mismatch` naming the rolled-back offset.
    Rollback,
    /// A `PATCH`: `offset_mismatch` without the offset header, so the client must ask `HEAD`.
    MismatchWithoutOffset,
    /// The session expires before this request is served.
    SessionLost,
    /// The client process dies while this request is in flight; the server applies it.
    Kill,
    /// `500` with no code: not recoverable at this layer.
    ServerError,
    /// `426 version_unsupported`.
    VersionRefused,
    /// A create: `409 duplicate_blob` — the server already holds this ciphertext.
    DuplicateBlob,
}

fn fault() -> impl Strategy<Value = Fault> {
    prop_oneof![
        8 => Just(Fault::Honest),
        2 => Just(Fault::RateLimited),
        1 => Just(Fault::Unavailable),
        1 => Just(Fault::StorageFull),
        2 => Just(Fault::Drop),
        2 => Just(Fault::LostResponse),
        2 => Just(Fault::ShortWrite),
        1 => Just(Fault::CorruptInTransit),
        1 => Just(Fault::Rollback),
        1 => Just(Fault::MismatchWithoutOffset),
        1 => Just(Fault::SessionLost),
        1 => Just(Fault::Kill),
        1 => Just(Fault::ServerError),
        1 => Just(Fault::VersionRefused),
        1 => Just(Fault::DuplicateBlob),
    ]
}

/// The run-ending answer a case drew, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    Version,
    ServerError,
    Duplicate,
}

// ── The model server ─────────────────────────────────────────────────────────

#[derive(Debug)]
struct Session {
    id: String,
    received: Vec<u8>,
    finalized: bool,
}

#[derive(Debug, Default)]
struct Model {
    size: usize,
    faults: Vec<Fault>,
    next_fault: usize,
    sessions: Vec<Session>,
    /// The last session id whose create response reached the client.
    told_session: Option<String>,
    /// The client may be acting on a stale offset: a response that would have corrected it
    /// was lost.
    stale: bool,
    refusal: Option<Refusal>,
    violations: Vec<String>,
    requests: usize,
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Reply {
    fn new(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: String::new(),
        }
    }

    fn header(mut self, name: &str, value: impl ToString) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    fn error(status: u16, code: &str) -> Self {
        let mut reply = Self::new(status).header("Content-Type", "application/json");
        reply.body = format!(r#"{{"error":"refused","code":"{code}"}}"#);
        reply
    }

    fn wait(status: u16, code: &str) -> Self {
        Self::error(status, code).header("Retry-After", 0)
    }

    fn offset(offset: usize) -> Self {
        Self::new(204).header("X-Capsule-Offset", offset)
    }
}

/// What the server does with a request: answer it, or close the connection on it.
enum Answer {
    Reply(Reply),
    Close,
}

impl Model {
    fn active(&mut self) -> Option<&mut Session> {
        self.sessions.iter_mut().rev().find(|s| !s.finalized)
    }

    fn session(&mut self, id: &str) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    fn handle(&mut self, req: &Request, kill: &Notify) -> Answer {
        self.requests += 1;
        let fault = self
            .faults
            .get(self.next_fault)
            .copied()
            .unwrap_or(Fault::Honest);
        self.next_fault += 1;
        let id = req.path.trim_start_matches('/').to_string();
        let is_create = req.method == "POST";
        let is_patch = req.method == "PATCH";

        match fault {
            Fault::RateLimited => {
                return Answer::Reply(Reply::wait(429, error_codes::UPLOAD_RATE_LIMITED));
            }
            Fault::Unavailable => {
                return Answer::Reply(Reply::wait(503, error_codes::UPLOAD_UNAVAILABLE));
            }
            Fault::StorageFull => {
                return Answer::Reply(Reply::wait(507, error_codes::UPLOAD_STORAGE_FULL));
            }
            Fault::Drop => return Answer::Close,
            Fault::ServerError => {
                self.refusal = Some(Refusal::ServerError);
                return Answer::Reply(Reply::new(500));
            }
            Fault::VersionRefused => {
                self.refusal = Some(Refusal::Version);
                return Answer::Reply(
                    Reply::error(426, error_codes::PROTOCOL_VERSION_UNSUPPORTED)
                        .header("X-Capsule-Protocol-Min", "2099-01-01")
                        .header("X-Capsule-Protocol-Max", "2099-12-31"),
                );
            }
            Fault::DuplicateBlob if is_create => {
                self.refusal = Some(Refusal::Duplicate);
                let mut reply = Reply::error(409, error_codes::UPLOAD_DUPLICATE_BLOB);
                reply.body = format!(
                    r#"{{"error":"This content is already stored as asset existing-asset","code":"{}"}}"#,
                    error_codes::UPLOAD_DUPLICATE_BLOB
                );
                return Answer::Reply(reply);
            }
            Fault::SessionLost => {
                self.sessions.retain(|s| s.finalized);
            }
            _ => {}
        }

        let reply = if is_create {
            self.create()
        } else if is_patch {
            self.patch(&id, req, fault)
        } else if req.method == "HEAD" {
            self.head(&id)
        } else {
            Reply::new(405)
        };

        match fault {
            Fault::LostResponse => {
                self.stale = true;
                Answer::Close
            }
            Fault::Kill => {
                self.stale = true;
                kill.notify_one();
                Answer::Reply(reply)
            }
            _ => {
                if is_create && reply.status < 300 {
                    self.told_session = reply.body.split('"').nth(3).map(str::to_string);
                }
                Answer::Reply(reply)
            }
        }
    }

    /// `POST /upload`: the active session for this blob when there is one (`200` with its
    /// offset), a fresh session otherwise.
    fn create(&mut self) -> Reply {
        if self.sessions.iter().any(|s| s.finalized) {
            // The blob is already stored: an honest server merges rather than re-receives it.
            self.refusal = Some(Refusal::Duplicate);
            return Reply::error(409, error_codes::UPLOAD_DUPLICATE_BLOB);
        }
        let (status, id, offset) = if let Some(active) = self.active() {
            (200, active.id.clone(), active.received.len())
        } else {
            let id = format!("sess-{}", self.sessions.len());
            self.sessions.push(Session {
                id: id.clone(),
                received: Vec::new(),
                finalized: false,
            });
            (201, id, 0)
        };
        let mut reply = Reply::new(status)
            .header("Content-Type", "application/json")
            .header("X-Capsule-Offset", offset);
        reply.body =
            format!(r#"{{"id":"{id}","upload_url":"/upload/{id}","suggested_chunk_size":262144}}"#);
        reply
    }

    fn head(&mut self, id: &str) -> Reply {
        let size = self.size;
        match self.session(id) {
            Some(session) => Reply::new(200)
                .header("X-Capsule-Offset", session.received.len())
                .header("X-Capsule-Content-Length", size)
                .header(
                    "X-Capsule-Upload-Status",
                    if session.finalized {
                        "completed"
                    } else {
                        "receiving"
                    },
                ),
            None => Reply::new(404),
        }
    }

    fn patch(&mut self, id: &str, req: &Request, fault: Fault) -> Reply {
        let size = self.size;
        let stale = self.stale;
        let offset: usize = req
            .header("X-Capsule-Offset")
            .and_then(|v| v.parse().ok())
            .unwrap_or(usize::MAX);
        let checksum = hex::encode(Sha256::digest(&req.body));
        let mut violation = None;
        let reply = match self.session(id) {
            None => Reply::error(404, error_codes::UPLOAD_SESSION_NOT_FOUND),
            Some(session) if session.finalized => Reply::offset(session.received.len()),
            Some(session) => {
                if req.header("X-Capsule-Checksum") != Some(checksum.as_str()) {
                    violation = Some(format!("chunk at {offset} carried a wrong checksum"));
                    Reply::error(400, error_codes::UPLOAD_CHECKSUM_MISMATCH)
                } else if offset != session.received.len() {
                    if !stale {
                        violation = Some(format!(
                            "client sent offset {offset} while the server had told it {}",
                            session.received.len()
                        ));
                    }
                    Reply::error(409, error_codes::UPLOAD_OFFSET_MISMATCH)
                        .header("X-Capsule-Offset", session.received.len())
                } else {
                    match fault {
                        Fault::CorruptInTransit => {
                            Reply::error(400, error_codes::UPLOAD_CHECKSUM_MISMATCH)
                        }
                        Fault::MismatchWithoutOffset => {
                            Reply::error(409, error_codes::UPLOAD_OFFSET_MISMATCH)
                        }
                        Fault::Rollback => {
                            let kept = session.received.len() / 2;
                            session.received.truncate(kept);
                            Reply::error(409, error_codes::UPLOAD_OFFSET_MISMATCH)
                                .header("X-Capsule-Offset", kept)
                        }
                        _ => {
                            let take = if fault == Fault::ShortWrite {
                                req.body.len() / 2
                            } else {
                                req.body.len()
                            };
                            session.received.extend_from_slice(&req.body[..take]);
                            if session.received.len() > size {
                                violation = Some("client sent past the declared size".into());
                            }
                            if session.received.len() == size {
                                session.finalized = true;
                            }
                            Reply::offset(session.received.len())
                        }
                    }
                }
            }
        };
        if let Some(v) = violation {
            self.violations.push(v);
        }
        if reply.status < 300 || reply.status == 409 {
            // The client now holds (or is about to learn) the server's offset.
            self.stale = false;
        }
        reply
    }
}

// ── The socket around the model ──────────────────────────────────────────────

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> Option<Request> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 16 * 1024];
    let head_end = loop {
        if let Some(at) = find(&buf, b"\r\n\r\n") {
            break at;
        }
        let n = stream.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut start = lines.next()?.split_whitespace();
    let method = start.next()?.to_string();
    let path = start.next()?.to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    let length: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < length {
        let n = stream.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(length);
    Some(Request {
        method,
        path: path.trim_start_matches("/upload").to_string(),
        headers,
        body,
    })
}

async fn serve(listener: TcpListener, model: Arc<Mutex<Model>>, kill: Arc<Notify>) {
    let mut connections = tokio::task::JoinSet::new();
    while let Ok((mut stream, _)) = listener.accept().await {
        while connections.try_join_next().is_some() {}
        let model = model.clone();
        let kill = kill.clone();
        connections.spawn(async move {
            let Some(req) = Box::pin(read_request(&mut stream)).await else {
                return;
            };
            let answer = model.lock().expect("model lock").handle(&req, &kill);
            let Answer::Reply(reply) = answer else {
                return;
            };
            let body = if req.method == "HEAD" {
                String::new()
            } else {
                reply.body
            };
            let mut out = format!("HTTP/1.1 {} X\r\n", reply.status);
            for (k, v) in &reply.headers {
                let _ = write!(out, "{k}: {v}\r\n");
            }
            let _ = write!(
                out,
                "Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(out.as_bytes()).await;
            let _ = stream.flush().await;
        });
    }
}

// ── The driver ───────────────────────────────────────────────────────────────

fn blob(size: usize, salt: u8) -> Vec<u8> {
    (0..size)
        .map(|i| (i.wrapping_mul(31) as u8) ^ salt)
        .collect()
}

fn request(data: &[u8]) -> CreateUploadRequest {
    let hash = hex::encode(Sha256::digest(data));
    CreateUploadRequest {
        size: data.len() as u64,
        hash: hash.clone(),
        content_type: "application/octet-stream".into(),
        crypto_suite_id: 1,
        protocol_version: "2026-01-01".into(),
        blob_role: BlobRole::Original,
        manifest_envelope: ManifestEnvelope {
            crypto_suite_id: 1,
            protocol_version: "2026-01-01".into(),
            album_id: None,
            file_id: "0190f0f0-0000-7000-8000-000000000001".into(),
            amk_version: 1,
            ciphertext_hash: hash,
            plaintext_size: data.len() as u64,
            chunk_size: 65536,
            key_mode: "derived".into(),
            metadata_blob_hash: None,
            derivative_role: None,
            derivative_format: None,
            created_by_user: "user".into(),
            created_by_device: "device".into(),
            client_version: "test".into(),
            timestamp: "2026-05-31T10:00:00Z".into(),
            action: "create".into(),
            prior_provenance_hash: None,
            retention_until: None,
        },
        album_id: None,
        owner_id: None,
        intent_id: None,
    }
}

/// How one attempt ended.
enum Attempt {
    Finished(Result<UploadOutcome, UploadError>),
    Killed,
}

/// Run one schedule to its end state, resuming after every interruption as an app would.
async fn run_schedule(
    size: usize,
    salt: u8,
    faults: Vec<Fault>,
) -> Result<(Model, Result<UploadOutcome, UploadError>), TestCaseError> {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("addr");
    let model = Arc::new(Mutex::new(Model {
        size,
        faults: faults.clone(),
        ..Model::default()
    }));
    let kill = Arc::new(Notify::new());
    let server = tokio::spawn(serve(listener, model.clone(), kill.clone()));

    let data = blob(size, salt);
    let req = request(&data);
    let interruptions = faults
        .iter()
        .filter(|f| matches!(f, Fault::Drop | Fault::LostResponse | Fault::Kill))
        .count();

    let mut attempts = 0;
    let outcome = loop {
        attempts += 1;
        prop_assert!(
            attempts <= interruptions + 1,
            "{attempts} attempts for {interruptions} interruptions"
        );
        // Every attempt is a fresh process: a new client, no state but the session id it was
        // told about.
        let client = UploadClient::new(UploadTransport::with_static_token(
            capsule_sdk::net::http_client().expect("client"),
            format!("http://{addr}/upload"),
            "2026-01-01",
            StaticToken("token".into()),
        ));
        let told = model.lock().expect("model lock").told_session.clone();
        let upload = async {
            match told {
                Some(id) => client.upload_resuming(&id, &req, &data).await,
                None => client.upload(&req, &data).await,
            }
        };
        let attempt = tokio::select! {
            biased;
            () = kill.notified() => Attempt::Killed,
            result = upload => Attempt::Finished(result),
        };
        match attempt {
            Attempt::Killed
            | Attempt::Finished(Err(
                UploadError::Transport(_) | UploadError::RetriesExhausted(_),
            )) => {}
            Attempt::Finished(result) => break result,
        }
    };
    server.abort();
    let model = std::mem::take(&mut *model.lock().expect("model lock"));
    Ok((model, outcome))
}

fn check(
    size: usize,
    salt: u8,
    model: &Model,
    outcome: &Result<UploadOutcome, UploadError>,
) -> Result<(), TestCaseError> {
    prop_assert!(
        model.violations.is_empty(),
        "server-truth violations: {:?}",
        model.violations
    );
    match (model.refusal, outcome) {
        (None, Ok(UploadOutcome::Completed { session_id })) => {
            let finalized: Vec<&Session> = model.sessions.iter().filter(|s| s.finalized).collect();
            prop_assert_eq!(finalized.len(), 1, "exactly one session finalizes");
            prop_assert_eq!(&finalized[0].id, session_id);
            prop_assert!(
                finalized[0].received == blob(size, salt),
                "the server holds other bytes than the client's"
            );
        }
        (Some(Refusal::Version), Err(UploadError::UpgradeRequired { min, max, .. })) => {
            prop_assert_eq!(min.as_deref(), Some("2099-01-01"));
            prop_assert_eq!(max.as_deref(), Some("2099-12-31"));
        }
        (Some(Refusal::ServerError), Err(UploadError::Rejected { status: 500, .. })) => {}
        (Some(Refusal::Duplicate), Ok(UploadOutcome::AlreadyStored { asset_ref })) => {
            prop_assert_eq!(asset_ref, "existing-asset");
        }
        // A duplicate met while re-creating a lost session surfaces as the typed rejection the
        // caller resolves as the same merge one level up.
        (
            Some(Refusal::Duplicate),
            Err(UploadError::Rejected {
                code: Some(code), ..
            }),
        ) if code == error_codes::UPLOAD_DUPLICATE_BLOB => {}
        (refusal, outcome) => {
            return Err(TestCaseError::fail(format!(
                "refusal {refusal:?} ended as {outcome:?}"
            )));
        }
    }
    Ok(())
}

/// The seeded configuration: a fixed seed unless `PROPTEST_RNG_SEED` names another, printed so
/// a failure replays from it.
fn config() -> Config {
    let seed = std::env::var("PROPTEST_RNG_SEED")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(DEFAULT_SEED);
    eprintln!("proptest seed {seed}; replay with PROPTEST_RNG_SEED={seed}");
    let base = Config::default();
    let cases = if std::env::var_os("PROPTEST_CASES").is_some() {
        base.cases
    } else {
        CASES
    };
    Config {
        cases,
        rng_seed: RngSeed::Fixed(seed),
        failure_persistence: None,
        ..base
    }
}

#[test]
fn every_fault_schedule_reaches_the_right_end_state() {
    let schedule = (
        // From one byte to a little over three default chunks (256 KiB each).
        1usize..(3 * 256 * 1024 + 4096),
        any::<u8>(),
        proptest::collection::vec(fault(), 0..MAX_FAULTS),
    );
    let mut runner = TestRunner::new(config());
    runner
        .run(&schedule, |(size, salt, faults)| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .start_paused(true)
                .build()
                .expect("runtime");
            let (model, outcome) = runtime.block_on(run_schedule(size, salt, faults))?;
            check(size, salt, &model, &outcome)
        })
        .expect("every fault schedule reaches the right end state");
}
