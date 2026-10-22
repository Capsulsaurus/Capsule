//! The transfer event stream: its zero-cost and never-blocking contract, its wire shape, and the
//! sequence a scripted fault schedule produces — snapshot-tested and replayed.
//!
//! The scripted schedules are fixed, not generated: the same faults at the same requests every
//! run, so the event sequence is a pure function of the script. Each schedule runs twice and the
//! two sequences must be equal (the replay), and the sequence is pinned by an `insta` snapshot,
//! so a change to what a progress UI sees is a reviewed diff. Values that come from the retry
//! engine's full jitter are redacted from the snapshot; their presence and order are not.

#![allow(clippy::unwrap_used)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use capsule_i18n::error_codes;
use sha2::{Digest, Sha256};
use tokio::sync::broadcast::error::{RecvError, TryRecvError};

use super::*;
use crate::fetch::{BlobSource, FetchError, RangeOutcome, RangedFetcher};
use crate::net::{ConnectionClass, MonotonicClock, Sleeper, StallConfig};
use crate::testmock::{MockRequest, MockResponse, MockServer};
use crate::upload::{BlobRole, CreateUploadRequest, ManifestEnvelope, UploadOutcome};

// ─── The handle's contract ───────────────────────────────────────────────────

fn sample(offset: u64) -> TransferEvent {
    TransferEvent::RangeReceived {
        blob: "b".into(),
        offset,
        len: 1,
    }
}

#[test]
fn a_disabled_handle_never_builds_an_event() {
    let events = TransferEvents::disabled();
    events.emit(|| panic!("a disabled handle must not build the event"));
    assert!(!events.is_observed());
    assert!(events.subscribe().is_none());
    // The default is the disabled handle.
    TransferEvents::default().emit(|| panic!("the default handle is disabled"));
}

#[test]
fn a_channel_nobody_subscribed_to_never_builds_an_event() {
    let events = TransferEvents::channel(8);
    events.emit(|| panic!("no subscriber, so the event must not be built"));
    assert!(!events.is_observed());

    // Once the last subscriber leaves, emitting is free again.
    let subscriber = events.subscribe().unwrap();
    assert!(events.is_observed());
    drop(subscriber);
    events.emit(|| panic!("the subscriber left, so the event must not be built"));
}

#[test]
fn every_subscriber_receives_every_event_in_order() {
    let events = TransferEvents::channel(8);
    let mut first = events.subscribe().unwrap();
    let mut second = events.clone().subscribe().unwrap();
    for offset in 0..3 {
        events.emit(|| sample(offset));
    }
    for subscriber in [&mut first, &mut second] {
        let got: Vec<TransferEvent> = (0..3).map(|_| subscriber.try_recv().unwrap()).collect();
        assert_eq!(got, vec![sample(0), sample(1), sample(2)]);
        assert_eq!(subscriber.try_recv(), Err(TryRecvError::Empty));
    }
}

/// A subscriber that stops reading never holds the transfer up: the publisher keeps going and
/// the subscriber learns how much it missed.
#[tokio::test]
async fn a_lagging_subscriber_is_told_what_it_missed_and_never_blocks_the_publisher() {
    let events = TransferEvents::channel(2);
    let mut slow = events.subscribe().unwrap();
    for offset in 0..5 {
        events.emit(|| sample(offset));
    }
    assert_eq!(slow.recv().await, Err(RecvError::Lagged(3)));
    assert_eq!(slow.recv().await.unwrap(), sample(3));
    assert_eq!(slow.recv().await.unwrap(), sample(4));
}

#[test]
fn a_zero_capacity_is_raised_to_one() {
    let events = TransferEvents::channel(0);
    let mut subscriber = events.subscribe().unwrap();
    events.emit(|| sample(7));
    assert_eq!(subscriber.try_recv().unwrap(), sample(7));
}

/// The serialized shape is what a `--json` stream would carry: a snake_case `event` tag, and a
/// retry's reason flattened beside the chunk it refused.
#[test]
fn events_serialize_with_a_snake_case_tag_and_a_flat_reason() {
    let retried = TransferEvent::ChunkRetried {
        blob: "ab".into(),
        session_id: "s1".into(),
        offset: 4096,
        len: 4096,
        reason: RetryReason::ServerWait { wait_ms: 1500 },
    };
    assert_eq!(
        serde_json::to_value(&retried).unwrap(),
        serde_json::json!({
            "event": "chunk_retried",
            "blob": "ab",
            "session_id": "s1",
            "offset": 4096,
            "len": 4096,
            "reason": "server_wait",
            "wait_ms": 1500,
        })
    );
    let lost = TransferEvent::ChunkRetried {
        blob: "ab".into(),
        session_id: "s1".into(),
        offset: 0,
        len: 1,
        reason: RetryReason::SessionLost,
    };
    assert_eq!(
        serde_json::to_value(&lost).unwrap()["reason"],
        "session_lost"
    );
}

#[test]
fn millis_saturates() {
    assert_eq!(millis(Duration::from_millis(1500)), 1500);
    assert_eq!(millis(Duration::MAX), u64::MAX);
}

// ─── Rendering a sequence for a snapshot ─────────────────────────────────────

/// One JSON line per event, with jittered waits redacted: a transport reset's and a fetch
/// retry's backoff comes from the retry engine's full jitter, so only its presence is stable.
fn render(events: &[TransferEvent]) -> String {
    events
        .iter()
        .map(|event| {
            let mut value = serde_json::to_value(event).unwrap();
            let jittered = matches!(
                event,
                TransferEvent::FetchRetried { .. }
                    | TransferEvent::ChunkRetried {
                        reason: RetryReason::TransportReset { .. },
                        ..
                    }
            );
            if jittered {
                value["wait_ms"] = serde_json::Value::String("[jitter]".into());
            }
            value.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn drain(subscriber: &mut tokio::sync::broadcast::Receiver<TransferEvent>) -> Vec<TransferEvent> {
    let mut out = Vec::new();
    loop {
        match subscriber.try_recv() {
            Ok(event) => out.push(event),
            Err(TryRecvError::Empty) => return out,
            Err(other) => panic!("the test channel never lags: {other:?}"),
        }
    }
}

// ─── Upload: a scripted fault schedule ───────────────────────────────────────

const PROTOCOL: &str = "2026-07-10";
const CHUNK: u64 = 256 * 1024;
/// Three chunks at the small-file tier's floor: under the adaptive strategy's warm-up (five
/// chunks, 8 MiB), so the chunk size never depends on measured throughput.
const SIZE: u64 = 3 * CHUNK;
const BLOB: &str = "b10b";

fn request() -> CreateUploadRequest {
    CreateUploadRequest {
        size: SIZE,
        hash: BLOB.into(),
        content_type: "image/jpeg".into(),
        crypto_suite_id: 1,
        protocol_version: PROTOCOL.into(),
        blob_role: BlobRole::Original,
        manifest_envelope: ManifestEnvelope {
            crypto_suite_id: 1,
            protocol_version: PROTOCOL.into(),
            album_id: None,
            file_id: "0192f000-0000-7000-8000-000000000001".into(),
            amk_version: 1,
            ciphertext_hash: BLOB.into(),
            plaintext_size: SIZE,
            chunk_size: 65536,
            key_mode: "derived".into(),
            metadata_blob_hash: None,
            derivative_role: None,
            derivative_format: None,
            created_by_user: "user-1".into(),
            created_by_device: "device-1".into(),
            client_version: "capsule-test/0".into(),
            timestamp: "2026-07-10T00:00:00Z".into(),
            action: "create".into(),
            prior_provenance_hash: None,
            retention_until: None,
        },
        album_id: None,
        owner_id: None,
        intent_id: None,
    }
}

/// What the scripted server does to the `n`th `PATCH` (1-based) instead of answering honestly.
#[derive(Debug, Clone, Copy)]
enum ChunkFault {
    /// `checksum_mismatch`: nothing persisted.
    Corrupt,
    /// `rate_limited` with `Retry-After: 0`: nothing persisted.
    Wait,
    /// Keep the first half of the chunk and acknowledge that.
    ShortWrite,
    /// `offset_mismatch` naming an offset half a chunk back (the server lost bytes).
    Rollback,
    /// `session_not_found`: the session expired.
    SessionLost,
}

/// The scripted model server: honest except where `script` names a fault for a `PATCH`.
struct Scripted {
    script: Vec<(usize, ChunkFault)>,
    patches: AtomicUsize,
    sessions: AtomicUsize,
    received: Mutex<u64>,
}

impl Scripted {
    fn new(script: Vec<(usize, ChunkFault)>) -> Self {
        Self {
            script,
            patches: AtomicUsize::new(0),
            sessions: AtomicUsize::new(0),
            received: Mutex::new(0),
        }
    }

    fn answer(&self, req: &MockRequest) -> MockResponse {
        match req.method.as_str() {
            "POST" => {
                let n = self.sessions.fetch_add(1, Ordering::SeqCst) + 1;
                *self.received.lock().unwrap() = 0;
                let id = format!("sess-{n}");
                MockResponse::new(201, "Created")
                    .header("X-Capsule-Offset", "0")
                    .json_body(format!(
                        r#"{{"id":{id:?},"upload_url":"/{id}","suggested_chunk_size":{CHUNK}}}"#
                    ))
            }
            "HEAD" => MockResponse::new(200, "OK")
                .header(
                    "X-Capsule-Offset",
                    self.received.lock().unwrap().to_string(),
                )
                .header("X-Capsule-Upload-Status", "pending"),
            "PATCH" => self.chunk(req),
            other => panic!("unexpected {other}"),
        }
    }

    fn chunk(&self, req: &MockRequest) -> MockResponse {
        let n = self.patches.fetch_add(1, Ordering::SeqCst) + 1;
        let offset: u64 = req.header("X-Capsule-Offset").unwrap().parse().unwrap();
        let mut received = self.received.lock().unwrap();
        assert_eq!(offset, *received, "the client sends at the server's offset");
        let fault = self
            .script
            .iter()
            .find(|(at, _)| *at == n)
            .map(|(_, fault)| *fault);
        let len = req.body.len() as u64;
        match fault {
            Some(ChunkFault::Corrupt) => MockResponse::api_error(
                400,
                "Bad Request",
                error_codes::UPLOAD_CHECKSUM_MISMATCH,
                "checksum",
            ),
            Some(ChunkFault::Wait) => MockResponse::api_error(
                429,
                "Too Many Requests",
                error_codes::UPLOAD_RATE_LIMITED,
                "wait",
            )
            .header("Retry-After", "0"),
            Some(ChunkFault::ShortWrite) => {
                *received += len / 2;
                MockResponse::new(204, "No Content")
                    .header("X-Capsule-Offset", received.to_string())
            }
            Some(ChunkFault::Rollback) => {
                *received -= CHUNK / 2;
                MockResponse::api_error(
                    409,
                    "Conflict",
                    error_codes::UPLOAD_OFFSET_MISMATCH,
                    "offset",
                )
                .header("X-Capsule-Offset", received.to_string())
            }
            Some(ChunkFault::SessionLost) => MockResponse::api_error(
                404,
                "Not Found",
                error_codes::UPLOAD_SESSION_NOT_FOUND,
                "gone",
            ),
            None => {
                let expected = hex::encode(Sha256::digest(&req.body));
                assert_eq!(req.header("X-Capsule-Checksum"), Some(expected.as_str()));
                *received += len;
                MockResponse::new(204, "No Content")
                    .header("X-Capsule-Offset", received.to_string())
            }
        }
    }
}

/// Run one upload against `script`, returning its outcome and every event it published.
async fn scripted_upload(script: Vec<(usize, ChunkFault)>) -> (UploadOutcome, Vec<TransferEvent>) {
    let state = Arc::new(Scripted::new(script));
    let server = MockServer::start(move |req: &MockRequest| state.answer(req)).await;
    let events = TransferEvents::channel(1024);
    let mut subscriber = events.subscribe().unwrap();
    let client = server.client(PROTOCOL).with_events(events);
    let data: Vec<u8> = (0..SIZE).map(|i| (i % 251) as u8).collect();
    let outcome = client.upload(&request(), &data).await.unwrap();
    (outcome, drain(&mut subscriber))
}

/// The schedule the snapshot pins: a corrupt chunk, a server-asked wait, a short write, a
/// rolled-back offset and an expired session, each once, and the transfer completes in the
/// session that replaced the lost one.
fn upload_schedule() -> Vec<(usize, ChunkFault)> {
    vec![
        (1, ChunkFault::Corrupt),
        (3, ChunkFault::Wait),
        (4, ChunkFault::ShortWrite),
        (5, ChunkFault::Rollback),
        (7, ChunkFault::SessionLost),
    ]
}

#[tokio::test]
async fn an_upload_under_a_scripted_fault_schedule_publishes_a_replayable_sequence() {
    let (outcome, first) = scripted_upload(upload_schedule()).await;
    assert!(
        matches!(&outcome, UploadOutcome::Completed { session_id } if session_id == "sess-2"),
        "{outcome:?}"
    );
    let (_, replay) = scripted_upload(upload_schedule()).await;
    assert_eq!(
        first, replay,
        "the same schedule publishes the same sequence"
    );
    insta::assert_snapshot!(render(&first));
}

#[tokio::test]
async fn an_honest_upload_publishes_create_chunks_and_finalize_only() {
    let (_, events) = scripted_upload(Vec::new()).await;
    insta::assert_snapshot!(render(&events));
}

#[tokio::test]
async fn a_duplicate_blob_publishes_a_merge_and_nothing_else() {
    let server = MockServer::start(|_: &MockRequest| {
        MockResponse::api_error(
            409,
            "Conflict",
            error_codes::UPLOAD_DUPLICATE_BLOB,
            "This content is already stored as asset asset-9",
        )
    })
    .await;
    let events = TransferEvents::channel(16);
    let mut subscriber = events.subscribe().unwrap();
    let client = server.client(PROTOCOL).with_events(events);
    client.upload(&request(), &vec![0u8; 16]).await.unwrap();
    assert_eq!(
        drain(&mut subscriber),
        vec![TransferEvent::Merged {
            blob: BLOB.into(),
            asset_ref: "asset-9".into(),
        }]
    );
}

#[tokio::test]
async fn a_resumed_upload_publishes_where_it_resumed() {
    let state = Arc::new(Scripted::new(Vec::new()));
    *state.received.lock().unwrap() = 2 * CHUNK;
    let server = MockServer::start({
        let state = state.clone();
        move |req: &MockRequest| state.answer(req)
    })
    .await;
    let events = TransferEvents::channel(16);
    let mut subscriber = events.subscribe().unwrap();
    let client = server.client(PROTOCOL).with_events(events);
    let data: Vec<u8> = (0..SIZE).map(|i| (i % 251) as u8).collect();
    client
        .upload_resuming("sess-0", &request(), &data)
        .await
        .unwrap();
    insta::assert_snapshot!(render(&drain(&mut subscriber)));
}

// ─── Fetch: a scripted fault schedule ────────────────────────────────────────

/// A clock each read advances by a fixed tick, so the stall cut trips at a scripted read.
#[derive(Clone)]
struct TickClock(Arc<Mutex<u64>>);

impl MonotonicClock for TickClock {
    fn now_millis(&self) -> u64 {
        *self.0.lock().unwrap()
    }
}

#[derive(Clone)]
struct NoSleep;

impl Sleeper for NoSleep {
    async fn sleep(&self, _: Duration) {}
}

/// Serves `data` in the windows asked for, except that the reads `blackholes` names (1-based)
/// bring nothing back, and `corrupt` flips the first byte it serves.
struct ScriptedSource {
    data: Vec<u8>,
    clock: TickClock,
    reads: AtomicUsize,
    blackholes: Vec<usize>,
    corrupt: bool,
}

impl BlobSource for ScriptedSource {
    async fn get_range(&self, _hash: &str, start: u64, max_len: Option<u64>) -> RangeOutcome {
        *self.clock.0.lock().unwrap() += 300;
        let n = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
        if self.blackholes.contains(&n) {
            return RangeOutcome::Partial { bytes: Vec::new() };
        }
        let start = start as usize;
        let end = max_len.map_or(self.data.len(), |len| {
            (start + len as usize).min(self.data.len())
        });
        let mut bytes = self.data[start..end].to_vec();
        if self.corrupt && start == 0 {
            bytes[0] ^= 0xff;
        }
        if end == self.data.len() {
            RangeOutcome::Complete { bytes }
        } else {
            RangeOutcome::Partial { bytes }
        }
    }
}

async fn scripted_fetch(
    blackholes: Vec<usize>,
    corrupt: bool,
) -> (Result<Vec<u8>, FetchError>, Vec<TransferEvent>) {
    let data: Vec<u8> = (0..2000u32).map(|i| (i % 251) as u8).collect();
    let hash = hex::encode(Sha256::digest(&data));
    let clock = TickClock(Arc::new(Mutex::new(0)));
    let source = ScriptedSource {
        data,
        clock: clock.clone(),
        reads: AtomicUsize::new(0),
        blackholes,
        corrupt,
    };
    let events = TransferEvents::channel(1024);
    let mut subscriber = events.subscribe().unwrap();
    // Stall bound 500 ms against a 300 ms tick: the second empty read in a row trips it.
    let fetcher = RangedFetcher::from_parts(
        ConnectionClass::Adverse,
        StallConfig::new(Duration::from_millis(500)),
        Some(512),
        NoSleep,
        clock,
    )
    .with_events(events);
    let result = fetcher.fetch(&source, &hash, 2000).await;
    (result, drain(&mut subscriber))
}

#[tokio::test]
async fn a_fetch_under_a_scripted_fault_schedule_publishes_a_replayable_sequence() {
    let (result, first) = scripted_fetch(vec![2, 3], false).await;
    assert_eq!(result.unwrap().len(), 2000);
    let (_, replay) = scripted_fetch(vec![2, 3], false).await;
    // The backoffs are full-jitter draws, so the replay is equal up to their values.
    assert_eq!(
        render(&first),
        render(&replay),
        "the same schedule publishes the same sequence"
    );
    insta::assert_snapshot!(render(&first));
}

#[tokio::test]
async fn a_fetch_whose_bytes_do_not_match_publishes_the_rejection() {
    let (result, events) = scripted_fetch(Vec::new(), true).await;
    assert!(matches!(result, Err(FetchError::IntegrityFailed { .. })));
    assert!(matches!(
        events.last(),
        Some(TransferEvent::IntegrityRejected { size: 2000, .. })
    ));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, TransferEvent::FetchVerified { .. }))
    );
}

#[tokio::test]
async fn an_unsized_fetch_publishes_its_windows_and_verdict() {
    let data: Vec<u8> = (0..100u32).map(|i| i as u8).collect();
    let hash = hex::encode(Sha256::digest(&data));
    let clock = TickClock(Arc::new(Mutex::new(0)));
    let source = ScriptedSource {
        data,
        clock: clock.clone(),
        reads: AtomicUsize::new(0),
        blackholes: Vec::new(),
        corrupt: false,
    };
    let events = TransferEvents::channel(64);
    let mut subscriber = events.subscribe().unwrap();
    let fetcher = RangedFetcher::from_parts(
        ConnectionClass::Adverse,
        StallConfig::new(Duration::from_millis(500)),
        Some(64),
        NoSleep,
        clock,
    )
    .with_events(events);
    fetcher.fetch_unsized(&source, &hash, 1000).await.unwrap();
    insta::assert_snapshot!(render(&drain(&mut subscriber)));
}
