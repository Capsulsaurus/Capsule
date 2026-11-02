//! The upload state machine under generated fault schedules (testing plan, `capsule-sdk` row:
//! "Upload, fetch and restore state machines reach the right end state under every fault
//! schedule; resume is driven by server truth; version refusals surface as one typed error").
//!
//! A model upload server runs over a real TCP socket, because the SDK's client is a genuine
//! `reqwest` client rather than an injectable transport. It keeps the authoritative state of
//! every session — which bytes of the blob it has committed, on the protocol's 4 KiB grid, and
//! whether the blob finalized — and answers the block-grid protocol honestly: a range disjoint
//! from the committed set is written, one wholly inside it is a duplicate answered `204`, one
//! partly inside it is a `409 range_conflict` carrying the set, a checksum that does not match
//! its body is refused, and a repeated create returns the active session with its committed
//! set. Each request first draws the next fault from a proptest-generated schedule: a
//! server-asked wait, a dropped connection, a response lost after the server applied the request
//! (the lost ACK), a refused overlap with a range in flight, a full in-flight window, a fragment
//! bound, a lost record of a committed range, a lost session, a server error (before or after the
//! server applied the request), a version refusal, a `duplicate_blob`, or the client process
//! dying mid-request.
//!
//! The client runs with a ranges window drawn from 1 to 4, so the schedule cancels and reorders
//! ranges in flight. The driver plays the app around the SDK: an attempt that ends in a transport
//! failure, an exhausted retry budget or a kill is resumed — from the session it was told about
//! when it has one, from scratch otherwise — and every other end state is final. The properties:
//!
//! - **The declared hash.** No refusal reached: the transfer completes, the session it names is
//!   the one the server finalized, and the server holds exactly the client's bytes, so the blob
//!   hashes to what the client declared. A version refusal reached: the run ends in
//!   [`UploadError::UpgradeRequired`]. A `duplicate_blob`: a merge naming the structured
//!   `existing_asset`.
//! - **Server truth.** Every range sits on the grid, inside the declared size, with a checksum
//!   matching its body. The client re-plans from the committed set the server reports, so it never
//!   sends a range overlapping bytes it was told are committed, except after a fault that hid an
//!   answer from it (a lost response, a kill).
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
/// The protocol's block grid.
const GRID: usize = 4096;

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
    /// Apply the request, then close the connection without answering: the lost ACK.
    LostResponse,
    /// A `PATCH`: refuse with `checksum_mismatch`; nothing applied.
    CorruptInTransit,
    /// A `PATCH`: `409 range_conflict` with the committed set and `Retry-After: 0`, as if the
    /// range overlapped one still in flight; nothing applied.
    OverlapInFlight,
    /// A `PATCH`: `409 too_many_in_flight`, `Retry-After: 0`; nothing applied.
    WindowFull,
    /// A `PATCH`: `409 too_fragmented`, `Retry-After: 0`; nothing applied.
    Fragmented,
    /// The session store loses the record of the highest committed range before this request:
    /// those bytes are outside the committed set again and must be sent again.
    LostRecord,
    /// The session expires before this request is served.
    SessionLost,
    /// The client process dies while this request is in flight; the server applies it.
    Kill,
    /// `500` with no code and no `Retry-After`; nothing applied. A wait on the retry engine's
    /// backoff.
    ServerError,
    /// Apply the request, then answer `500 storage_inconsistent`: the client must learn from
    /// `HEAD` what the server kept (a completing chunk: that the blob finalized).
    InconsistentAfterApply,
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
        1 => Just(Fault::CorruptInTransit),
        1 => Just(Fault::OverlapInFlight),
        1 => Just(Fault::WindowFull),
        1 => Just(Fault::Fragmented),
        1 => Just(Fault::LostRecord),
        1 => Just(Fault::SessionLost),
        1 => Just(Fault::Kill),
        1 => Just(Fault::ServerError),
        1 => Just(Fault::InconsistentAfterApply),
        1 => Just(Fault::VersionRefused),
        1 => Just(Fault::DuplicateBlob),
    ]
}

/// The run-ending answer a case drew, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    Version,
    Duplicate,
}

// ── The model server ─────────────────────────────────────────────────────────

#[derive(Debug)]
struct Session {
    id: String,
    /// The staged bytes; only the committed ones mean anything.
    staged: Vec<u8>,
    /// Which bytes are committed.
    held: Vec<bool>,
    finalized: bool,
}

impl Session {
    fn new(id: String, size: usize) -> Self {
        Self {
            id,
            staged: vec![0; size],
            held: vec![false; size],
            finalized: false,
        }
    }

    fn full(&self) -> bool {
        self.held.iter().all(|&h| h)
    }

    fn prefix(&self) -> usize {
        self.held
            .iter()
            .position(|&h| !h)
            .unwrap_or(self.held.len())
    }

    /// The committed set's ranges, half-open.
    fn ranges(&self) -> Vec<(usize, usize)> {
        ranges_of(&self.held)
    }

    /// `X-Capsule-Committed`.
    fn committed(&self) -> String {
        self.ranges()
            .iter()
            .map(|(start, end)| format!("{start}-{}", end - 1))
            .collect::<Vec<_>>()
            .join(",")
    }

    fn status(&self) -> &'static str {
        if self.finalized {
            "completed"
        } else {
            "uploading"
        }
    }
}

fn ranges_of(held: &[bool]) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut at = 0;
    while at < held.len() {
        if held[at] {
            let start = at;
            while at < held.len() && held[at] {
                at += 1;
            }
            ranges.push((start, at));
        } else {
            at += 1;
        }
    }
    ranges
}

#[derive(Debug, Default)]
struct Model {
    size: usize,
    faults: Vec<Fault>,
    next_fault: usize,
    sessions: Vec<Session>,
    /// The last session id whose create response reached the client.
    told_session: Option<String>,
    /// What the client was told is committed, per byte, on the session it was told about.
    told: Vec<bool>,
    /// The client may not have heard of something the server applied: a response was lost or the
    /// process died, so a duplicate or an overlap is not a violation.
    stale: bool,
    /// A client process died with requests in flight; they may land at any later point.
    killed: bool,
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

    /// The state a session reports on a `204`, a `HEAD` or a resumed create.
    fn progress(self, session: &Session) -> Self {
        self.header("X-Capsule-Offset", session.prefix())
            .header("X-Capsule-Committed", session.committed())
            .header("X-Capsule-Upload-Status", session.status())
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
            Fault::ServerError => return Answer::Reply(Reply::new(500)),
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
                return Answer::Reply(duplicate());
            }
            Fault::SessionLost => {
                self.sessions.retain(|s| s.finalized);
            }
            Fault::LostRecord => {
                if let Some(session) = self.session(&id).filter(|s| !s.finalized)
                    && let Some(&(start, end)) = session.ranges().last()
                {
                    session.held[start..end].fill(false);
                }
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
                // The dying process's other requests may still land after its successor reads
                // the session, so no overlap after this is a violation.
                self.killed = true;
                self.stale = true;
                kill.notify_one();
                Answer::Reply(reply)
            }
            Fault::InconsistentAfterApply => {
                Answer::Reply(Reply::error(500, error_codes::UPLOAD_STORAGE_INCONSISTENT))
            }
            _ => {
                self.learn(req, &id, &reply, is_create);
                Answer::Reply(reply)
            }
        }
    }

    /// Record what an answer that reaches the client tells it: a create or a `HEAD` states the
    /// committed set, and the client re-plans from it; a chunk's answer adds to it.
    fn learn(&mut self, req: &Request, id: &str, reply: &Reply, is_create: bool) {
        if reply.status >= 300 {
            return;
        }
        if is_create {
            self.told_session = reply.body.split('"').nth(3).map(str::to_string);
        } else if self.told_session.as_deref() != Some(id) {
            return;
        }
        let Some(committed) = reply
            .headers
            .iter()
            .find(|(k, _)| k == "X-Capsule-Committed")
            .map(|(_, v)| v.clone())
        else {
            return;
        };
        let mut told = vec![false; self.size];
        for part in committed.split(',').filter(|p| !p.is_empty()) {
            let (start, last) = part.split_once('-').expect("a range");
            let (start, last): (usize, usize) = (start.parse().unwrap(), last.parse().unwrap());
            told[start..=last].fill(true);
        }
        if req.method == "PATCH" {
            for (byte, held) in told.iter_mut().enumerate() {
                *held |= self.told.get(byte).copied().unwrap_or(false);
            }
        } else {
            // The client now re-plans from server truth again.
            self.stale = self.killed;
        }
        self.told = told;
    }

    /// `POST /upload`: the active session for this blob when there is one (`200` with its
    /// committed set), a fresh session otherwise.
    fn create(&mut self) -> Reply {
        if self.sessions.iter().any(|s| s.finalized) {
            // The blob is already stored: an honest server merges rather than re-receives it.
            self.refusal = Some(Refusal::Duplicate);
            return duplicate();
        }
        let size = self.size;
        let resumed = self.active().is_some();
        if !resumed {
            let id = format!("sess-{}", self.sessions.len());
            self.sessions.push(Session::new(id, size));
        }
        let session = self.active().expect("an active session");
        let id = session.id.clone();
        let mut reply = Reply::new(if resumed { 200 } else { 201 })
            .header("Content-Type", "application/json")
            .header(
                "X-Capsule-Advice",
                "chunk=262144, sessions=4, ranges=4, ttl=300",
            );
        if resumed {
            reply = reply.progress(session);
        } else {
            reply = reply.header("X-Capsule-Committed", "");
        }
        reply.body =
            format!(r#"{{"id":"{id}","upload_url":"/upload/{id}","suggested_chunk_size":262144}}"#);
        reply
    }

    fn head(&mut self, id: &str) -> Reply {
        let size = self.size;
        match self.session(id) {
            Some(session) => {
                // A full session nobody claimed is finalized by the `HEAD` that meets it.
                if session.full() {
                    session.finalized = true;
                }
                Reply::new(200)
                    .progress(session)
                    .header("X-Capsule-Content-Length", size)
                    .header("X-Capsule-Advice", "ranges=4")
            }
            None => Reply::new(404),
        }
    }

    fn patch(&mut self, id: &str, req: &Request, fault: Fault) -> Reply {
        let size = self.size;
        let stale = self.stale;
        let told = self.told.clone();
        let offset: usize = req
            .header("X-Capsule-Offset")
            .and_then(|v| v.parse().ok())
            .unwrap_or(usize::MAX);
        let end = offset.saturating_add(req.body.len());
        let checksum = hex::encode(Sha256::digest(&req.body));
        let mut violations = Vec::new();
        if req.header("X-Capsule-Checksum") != Some(checksum.as_str()) {
            violations.push(format!("range at {offset} carried a wrong checksum"));
        }
        if !offset.is_multiple_of(GRID)
            || (!req.body.len().is_multiple_of(GRID) && end != size)
            || end > size
            || req.body.is_empty()
        {
            violations.push(format!(
                "range [{offset}, {end}) is off the grid or the blob"
            ));
        }
        let reply = match self.session(id) {
            None => Reply::error(404, error_codes::UPLOAD_SESSION_NOT_FOUND),
            Some(_) if !violations.is_empty() => {
                Reply::error(400, error_codes::UPLOAD_CHECKSUM_MISMATCH)
            }
            Some(session) if session.finalized => Reply::new(204).progress(session),
            Some(session) => {
                let overlaps_told = told.get(offset..end).is_some_and(|t| t.iter().any(|&h| h));
                if overlaps_told && !stale {
                    violations.push(format!(
                        "range [{offset}, {end}) overlaps bytes the client was told are committed"
                    ));
                }
                let held = &session.held[offset..end];
                match fault {
                    Fault::CorruptInTransit => {
                        Reply::error(400, error_codes::UPLOAD_CHECKSUM_MISMATCH)
                    }
                    Fault::OverlapInFlight => Reply::wait(409, error_codes::UPLOAD_RANGE_CONFLICT)
                        .header("X-Capsule-Committed", session.committed()),
                    Fault::WindowFull => Reply::wait(409, error_codes::UPLOAD_TOO_MANY_IN_FLIGHT),
                    Fault::Fragmented => Reply::wait(409, error_codes::UPLOAD_TOO_FRAGMENTED),
                    _ if held.iter().all(|&h| h) => Reply::new(204).progress(session),
                    _ if held.iter().any(|&h| h) => {
                        Reply::error(409, error_codes::UPLOAD_RANGE_CONFLICT)
                            .header("X-Capsule-Committed", session.committed())
                    }
                    _ => {
                        session.staged[offset..end].copy_from_slice(&req.body);
                        session.held[offset..end].fill(true);
                        let mut reply = Reply::new(204).progress(session);
                        if session.full() {
                            session.finalized = true;
                            reply = Reply::new(204)
                                .header("X-Capsule-Offset", size)
                                .header("X-Capsule-Committed", session.committed())
                                .header("X-Capsule-Upload-Status", "waiting_for_processing");
                        }
                        reply
                    }
                }
            }
        };
        self.violations.extend(violations);
        reply
    }
}

/// `409 duplicate_blob` naming the existing asset in the structured member, and in a detail that
/// does not end in it, so only the structured member names it.
fn duplicate() -> Reply {
    let mut reply = Reply::error(409, error_codes::UPLOAD_DUPLICATE_BLOB);
    reply.body = format!(
        r#"{{"error":"this library already holds these bytes","code":"{}","existing_asset":"existing-asset"}}"#,
        error_codes::UPLOAD_DUPLICATE_BLOB
    );
    reply
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
    ranges: u32,
    faults: Vec<Fault>,
) -> Result<(Model, Result<UploadOutcome, UploadError>), TestCaseError> {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("addr");
    // A schedule that ends in a version refusal latches a refusing window for this origin; a
    // later case that lands on the same ephemeral port must start from nothing.
    capsule_sdk::net::negotiation::forget(&format!("http://{addr}").parse().expect("url"));
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
        .filter(|f| {
            matches!(
                f,
                Fault::Drop
                    | Fault::LostResponse
                    | Fault::Kill
                    // Waits on the retry engine's backoff, which a long enough run of them
                    // exhausts.
                    | Fault::ServerError
                    | Fault::InconsistentAfterApply
            )
        })
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
        ))
        .with_ranges_window(ranges);
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
            // With ranges in flight together, a refusal one of them drew may never reach the
            // process that died; the next attempt meets whatever the server still answers.
            Attempt::Killed => {
                model.lock().expect("model lock").refusal = None;
            }
            Attempt::Finished(Err(
                UploadError::Transport(_)
                | UploadError::RetriesExhausted(_)
                | UploadError::WaitsExhausted { .. },
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
            let expected = blob(size, salt);
            prop_assert!(
                finalized[0].staged == expected,
                "the server holds other bytes than the client's"
            );
            prop_assert_eq!(
                hex::encode(Sha256::digest(&finalized[0].staged)),
                hex::encode(Sha256::digest(&expected)),
                "the finalized blob hashes to the declared hash"
            );
        }
        (Some(Refusal::Version), Err(UploadError::UpgradeRequired { min, max, .. })) => {
            prop_assert_eq!(min.as_deref(), Some("2099-01-01"));
            prop_assert_eq!(max.as_deref(), Some("2099-12-31"));
        }
        // However the duplicate is met — on the first create or re-creating a lost session —
        // it is a merge naming the structured `existing_asset`.
        (Some(Refusal::Duplicate), Ok(UploadOutcome::AlreadyStored { asset_ref })) => {
            prop_assert_eq!(asset_ref, "existing-asset");
        }
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
        1u32..=4,
        proptest::collection::vec(fault(), 0..MAX_FAULTS),
    );
    let mut runner = TestRunner::new(config());
    runner
        .run(&schedule, |(size, salt, ranges, faults)| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .start_paused(true)
                .build()
                .expect("runtime");
            let (model, outcome) = runtime.block_on(run_schedule(size, salt, ranges, faults))?;
            check(size, salt, &model, &outcome)
        })
        .expect("every fault schedule reaches the right end state");
}
