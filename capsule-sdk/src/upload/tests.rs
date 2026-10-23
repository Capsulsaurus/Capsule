//! Tests for the S-D1 upload client.
//!
//! Two layers:
//! - **Adaptive strategy (unit).** The normative chunk-size algorithm: warm-up,
//!   double/halve, tier clamping, and the *by-construction* 4 KiB-alignment /
//!   protocol-bounds guarantees.
//! - **Protocol conformance + recovery matrix (smoke).** The client drives an
//!   in-process mock HTTP server that replays the real wire (statuses, headers,
//!   `ApiError` JSON, `error.*` codes taken straight from the S-C1 handlers). One
//!   test per recovery-matrix code, plus the happy-path round-trip, resume, and
//!   the `426` abort-with-upgrade against the real handshake shape.
//!
//! These mock-level tests are deterministic and hermetic. The cross-module case —
//! this client driving the REAL `capsule-api/upload` server (testcontainer
//! Postgres + Valkey, real protocol gate, real disk) — lives in that crate's
//! `src/tests/sdk_client.rs`, which dev-depends on `capsule-sdk`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::*;
use crate::testmock::{MockRequest, MockResponse, MockServer};

// ─── Adaptive strategy (unit) ───────────────────────────────────────────────

#[test]
fn tier_selection_by_file_size() {
    let small = AdaptiveChunkSizeStrategy::for_file_size(5 * 1024 * 1024);
    assert_eq!(small.current_size, CHUNK_SIZE_256KB);
    assert_eq!(small.min_size, CHUNK_SIZE_256KB);
    assert_eq!(small.max_size, CHUNK_SIZE_1MB);

    let medium = AdaptiveChunkSizeStrategy::for_file_size(50 * 1024 * 1024);
    assert_eq!(medium.current_size, CHUNK_SIZE_1MB);
    assert_eq!(medium.min_size, CHUNK_SIZE_1MB);
    assert_eq!(medium.max_size, CHUNK_SIZE_4MB);

    let large = AdaptiveChunkSizeStrategy::for_file_size(200 * 1024 * 1024);
    assert_eq!(large.current_size, CHUNK_SIZE_4MB);
    assert_eq!(large.min_size, CHUNK_SIZE_4MB);
    assert_eq!(large.max_size, CHUNK_SIZE_16MB);
}

#[test]
fn every_tier_range_sits_inside_the_protocol_bounds_and_is_aligned() {
    for size in [1u64, 5 << 20, 50 << 20, 500 << 20] {
        let s = AdaptiveChunkSizeStrategy::for_file_size(size);
        assert!(
            s.min_size >= PROTOCOL_MIN_CHUNK,
            "tier min below protocol min"
        );
        assert!(
            s.max_size <= PROTOCOL_MAX_CHUNK,
            "tier max above protocol max"
        );
        assert!(s.min_size.is_multiple_of(4096));
        assert!(s.max_size.is_multiple_of(4096));
        assert!(s.current_size.is_multiple_of(4096));
    }
}

#[test]
fn warmup_defers_scaling_then_doubles_above_5mbps() {
    let mut s = AdaptiveChunkSizeStrategy::for_file_size(50 * 1024 * 1024);
    // < 5 chunks and < 8 MiB: no adjustment on a cold window.
    for _ in 0..3 {
        s.record_chunk(CHUNK_SIZE_1MB, Duration::from_millis(100));
    }
    assert_eq!(s.current_size, CHUNK_SIZE_1MB);
    // Past the warm-up, sustained > 5 MB/s doubles.
    for _ in 0..2 {
        s.record_chunk(CHUNK_SIZE_1MB, Duration::from_millis(100));
    }
    assert!(s.current_size > CHUNK_SIZE_1MB);
}

/// **Chunk-size floor coupling.** Under `adverse` the strategy pins to the
/// tier chunk floor and suppresses adaptive growth, so each request stays small
/// enough to usually complete between mid-transfer resets; releasing the coupling
/// lets the normative adaptive algorithm grow again.
#[test]
fn adverse_pins_chunk_size_to_the_tier_floor() {
    use crate::net::ConnectionClass;

    let mut adverse = AdaptiveChunkSizeStrategy::for_file_size(500 * 1024 * 1024) // 4–16 MiB
        .with_connection_class(ConnectionClass::Adverse);
    let floor = adverse.min_size;
    assert_eq!(adverse.current_size, floor, "pinned to the tier floor");
    assert_eq!(adverse.next_chunk_size(), floor);

    // Even sustained high-throughput chunks that would normally double the size
    // leave it pinned at the floor while adverse.
    for _ in 0..10 {
        adverse.record_chunk(floor, Duration::from_millis(1));
    }
    assert_eq!(adverse.current_size, floor, "no growth under adverse");

    // Control: an unmetered client on the same tier is free to grow past the floor.
    let mut unmetered = AdaptiveChunkSizeStrategy::for_file_size(500 * 1024 * 1024)
        .with_connection_class(ConnectionClass::Unmetered);
    for _ in 0..6 {
        let cs = unmetered.current_size;
        unmetered.record_chunk(cs, Duration::from_millis(1));
    }
    assert!(
        unmetered.current_size > floor,
        "unmetered grows above the floor"
    );
}

#[test]
fn doubling_never_exceeds_the_tier_max_and_stays_aligned() {
    let mut s = AdaptiveChunkSizeStrategy::for_file_size(500 * 1024 * 1024); // 4–16 MiB
    for _ in 0..40 {
        let cs = s.current_size;
        s.record_chunk(cs, Duration::from_millis(50)); // very fast → keep doubling
        assert!(
            s.current_size.is_multiple_of(4096),
            "size drifted off 4 KiB"
        );
        assert!(s.current_size <= s.max_size, "exceeded tier max");
        assert!(
            (PROTOCOL_MIN_CHUNK..=PROTOCOL_MAX_CHUNK).contains(&s.next_chunk_size()),
            "escaped protocol bounds"
        );
    }
    assert_eq!(
        s.current_size, s.max_size,
        "should clamp up to the tier max"
    );
    assert_eq!(s.max_size, PROTOCOL_MAX_CHUNK);
}

#[test]
fn halving_never_drops_below_the_tier_min_and_stays_aligned() {
    let mut s = AdaptiveChunkSizeStrategy::for_file_size(50 * 1024 * 1024); // 1–4 MiB
    // Start high within the tier, then feed only slow chunks.
    s.current_size = CHUNK_SIZE_4MB;
    for _ in 0..40 {
        let cs = s.current_size;
        s.record_chunk(cs, Duration::from_secs(30)); // ~140 KB/s → keep halving
        assert!(
            s.current_size.is_multiple_of(4096),
            "size drifted off 4 KiB"
        );
        assert!(s.current_size >= s.min_size, "dropped below tier min");
    }
    assert_eq!(
        s.current_size, s.min_size,
        "should clamp down to the tier min"
    );
}

#[test]
fn at_min_tier_slow_throughput_never_underflows() {
    let mut s = AdaptiveChunkSizeStrategy::for_file_size(1024 * 1024); // 256 KiB–1 MiB
    for _ in 0..20 {
        let cs = s.current_size;
        s.record_chunk(cs, Duration::from_secs(30));
    }
    assert_eq!(s.current_size, s.min_size);
    assert_eq!(s.min_size, CHUNK_SIZE_256KB);
}

#[test]
fn seeded_from_suggested_aligns_down_and_clamps_into_the_tier() {
    // 50 MiB → tier [1 MiB, 4 MiB].
    let tier = || AdaptiveChunkSizeStrategy::for_file_size(50 * 1024 * 1024);

    // An unaligned mid-tier suggestion aligns down and stays put.
    let s = tier().seeded_from_suggested(2 * 1024 * 1024 + 123);
    assert_eq!(s.current_size, 2 * 1024 * 1024);
    assert!(s.current_size.is_multiple_of(4096));

    // Below the tier floor → clamped up to the min.
    let s = tier().seeded_from_suggested(100);
    assert_eq!(s.current_size, CHUNK_SIZE_1MB);

    // Above the tier ceiling → clamped down to the max.
    let s = tier().seeded_from_suggested(999 * 1024 * 1024);
    assert_eq!(s.current_size, CHUNK_SIZE_4MB);
}

#[test]
fn checksum_is_lowercase_hex_sha256() {
    // Known-answer: SHA-256("") = e3b0c442...
    assert_eq!(
        chunk_checksum(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    let cs = chunk_checksum(b"capsule");
    assert_eq!(cs.len(), 64);
    assert!(
        cs.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
}

// ─── Fixtures ───────────────────────────────────────────────────────────────

const PROTOCOL: &str = "2026-07-10";

fn envelope() -> ManifestEnvelope {
    ManifestEnvelope {
        crypto_suite_id: 1,
        protocol_version: PROTOCOL.into(),
        album_id: Some("album-1".into()),
        file_id: "0192f000-0000-7000-8000-000000000001".into(),
        amk_version: 1,
        ciphertext_hash: "0".repeat(64),
        plaintext_size: 100,
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
    }
}

fn request(size: u64) -> CreateUploadRequest {
    CreateUploadRequest {
        size,
        hash: "0".repeat(64),
        content_type: "image/jpeg".into(),
        crypto_suite_id: 1,
        protocol_version: PROTOCOL.into(),
        blob_role: BlobRole::Original,
        manifest_envelope: envelope(),
        album_id: Some("album-1".into()),
        owner_id: None,
        intent_id: None,
    }
}

fn created(id: &str, suggested: u64) -> MockResponse {
    MockResponse::new(201, "Created")
        .header("Location", format!("/upload/{id}"))
        .header("X-Capsule-Suggested-Chunk-Size", suggested.to_string())
        .json_body(format!(
            r#"{{"id":{id:?},"upload_url":"/upload/{id}","suggested_chunk_size":{suggested}}}"#
        ))
}

/// The offset a PATCH should ACK, honoring the client's `X-Capsule-Offset`. Also
/// asserts the client sent a correct checksum + octet-stream content type.
fn ack_offset(req: &MockRequest) -> u64 {
    assert_eq!(
        req.header("Content-Type"),
        Some("application/octet-stream"),
        "chunk must be octet-stream"
    );
    let offset: u64 = req.header("X-Capsule-Offset").unwrap().parse().unwrap();
    let expect = chunk_checksum(&req.body);
    assert_eq!(
        req.header("X-Capsule-Checksum"),
        Some(expect.as_str()),
        "client must send the lowercase-hex SHA-256 of the chunk"
    );
    assert_eq!(
        req.header("X-Capsule-Protocol"),
        Some(PROTOCOL),
        "every request carries the protocol handshake header"
    );
    offset + req.body.len() as u64
}

// ─── Happy-path conformance ─────────────────────────────────────────────────

#[tokio::test]
async fn happy_path_single_chunk_round_trip() {
    let patches = Arc::new(Mutex::new(Vec::<(u64, usize)>::new()));
    let p = patches.clone();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => created("sess-1", CHUNK_SIZE_256KB),
        ("PATCH", "/sess-1") => {
            let new_offset = ack_offset(req);
            let offset = new_offset - req.body.len() as u64;
            p.lock().unwrap().push((offset, req.body.len()));
            MockResponse::new(204, "No Content").header("X-Capsule-Offset", new_offset.to_string())
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let client = server.client(PROTOCOL);
    let data = vec![7u8; 4096];
    let outcome = client.upload(&request(4096), &data).await.unwrap();

    match outcome {
        UploadOutcome::Completed { session_id } => assert_eq!(session_id, "sess-1"),
        other @ UploadOutcome::AlreadyStored { .. } => panic!("expected Completed, got {other:?}"),
    }
    assert_eq!(*patches.lock().unwrap(), vec![(0, 4096)]);
}

#[tokio::test]
async fn happy_path_multi_chunk_sequential_offsets() {
    let patches = Arc::new(Mutex::new(Vec::<u64>::new()));
    let p = patches.clone();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => created("sess-2", CHUNK_SIZE_256KB),
        ("PATCH", "/sess-2") => {
            let offset: u64 = req.header("X-Capsule-Offset").unwrap().parse().unwrap();
            p.lock().unwrap().push(offset);
            let new_offset = ack_offset(req);
            MockResponse::new(204, "No Content").header("X-Capsule-Offset", new_offset.to_string())
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let client = server.client(PROTOCOL);
    // 300 KiB → 256 KiB aligned chunk + 44 KiB final chunk.
    let size = 300 * 1024;
    let data = vec![3u8; size as usize];
    let outcome = client.upload(&request(size), &data).await.unwrap();

    assert!(matches!(outcome, UploadOutcome::Completed { .. }));
    assert_eq!(*patches.lock().unwrap(), vec![0, CHUNK_SIZE_256KB]);
}

/// The landed server's idempotent re-create: `POST /upload` for a tuple with an
/// active session returns `200` + the existing session + its authoritative
/// `X-Capsule-Offset` — the client resumes from there without a `HEAD`.
#[tokio::test]
async fn create_existing_session_resumes_from_advertised_offset() {
    let patch_offsets = Arc::new(Mutex::new(Vec::<u64>::new()));
    let po = patch_offsets.clone();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => MockResponse::new(200, "OK")
            .header("Location", "/upload/sess-ex")
            .header("X-Capsule-Offset", CHUNK_SIZE_256KB.to_string())
            .header("X-Capsule-Suggested-Chunk-Size", CHUNK_SIZE_256KB.to_string())
            .json_body(format!(
                r#"{{"id":"sess-ex","upload_url":"/upload/sess-ex","suggested_chunk_size":{CHUNK_SIZE_256KB}}}"#
            )),
        ("PATCH", "/sess-ex") => {
            let offset: u64 = req.header("X-Capsule-Offset").unwrap().parse().unwrap();
            po.lock().unwrap().push(offset);
            let new_offset = ack_offset(req);
            MockResponse::new(204, "No Content").header("X-Capsule-Offset", new_offset.to_string())
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let client = server.client(PROTOCOL);
    let size = 300 * 1024;
    let data = vec![8u8; size as usize];
    let outcome = client.upload(&request(size), &data).await.unwrap();

    assert!(matches!(outcome, UploadOutcome::Completed { .. }));
    // Only the tail was sent — the 200's offset was honored, no HEAD needed.
    assert_eq!(*patch_offsets.lock().unwrap(), vec![CHUNK_SIZE_256KB]);
}

/// The production composition: an upload driven through the `S-D7` session, whose
/// token store injects the bearer (the upload client never sees the token).
#[tokio::test]
async fn session_composed_upload_injects_the_session_bearer() {
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/upload") => {
            assert_eq!(
                req.header("Authorization"),
                Some("Bearer session-access-token"),
                "the session must inject its bearer"
            );
            created("sess-s7", CHUNK_SIZE_256KB)
        }
        ("PATCH", "/upload/sess-s7") => {
            assert_eq!(
                req.header("Authorization"),
                Some("Bearer session-access-token"),
                "every chunk rides the session token"
            );
            let new_offset = ack_offset(req);
            MockResponse::new(204, "No Content").header("X-Capsule-Offset", new_offset.to_string())
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    // The session's auth endpoints and the upload endpoint share the mock host;
    // the upload transport is rooted under `/upload`.
    let auth = crate::auth::AuthClient::new(&server.base_url()).expect("auth client");
    let session = auth
        .resume(crate::auth::PersistedSession {
            access_token: "session-access-token".into(),
            refresh_token: "refresh-token".into(),
            access_expires_at_unix: jiff::Timestamp::now().as_second() + 3600,
        })
        .expect("resume session");
    let base = server.base_url();
    let transport = UploadTransport::with_session(session, format!("{base}/upload"), PROTOCOL);
    let client = UploadClient::new(transport);

    let data = vec![11u8; 4096];
    let outcome = client.upload(&request(4096), &data).await.unwrap();
    match outcome {
        UploadOutcome::Completed { session_id } => assert_eq!(session_id, "sess-s7"),
        other @ UploadOutcome::AlreadyStored { .. } => panic!("expected Completed, got {other:?}"),
    }
}

// ─── Recovery matrix — one test per error.* code ────────────────────────────

#[tokio::test]
async fn recovery_offset_mismatch_realigns_via_header() {
    let patch_offsets = Arc::new(Mutex::new(Vec::<u64>::new()));
    let po = patch_offsets.clone();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => created("sess-om", CHUNK_SIZE_256KB),
        ("PATCH", "/sess-om") => {
            let offset: u64 = req.header("X-Capsule-Offset").unwrap().parse().unwrap();
            po.lock().unwrap().push(offset);
            if offset == 0 {
                // Server already holds 256 KiB (lost-ACK); reject with the
                // authoritative offset in the header.
                MockResponse::api_error(
                    409,
                    "Conflict",
                    error_codes::UPLOAD_OFFSET_MISMATCH,
                    "Invalid offset",
                )
                .header("X-Capsule-Offset", CHUNK_SIZE_256KB.to_string())
            } else {
                let new_offset = ack_offset(req);
                MockResponse::new(204, "No Content")
                    .header("X-Capsule-Offset", new_offset.to_string())
            }
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let client = server.client(PROTOCOL);
    let size = 300 * 1024;
    let data = vec![1u8; size as usize];
    let outcome = client.upload(&request(size), &data).await.unwrap();

    assert!(matches!(outcome, UploadOutcome::Completed { .. }));
    // Re-aligned from the 409 header (no HEAD needed): tried 0, then 256 KiB.
    assert_eq!(*patch_offsets.lock().unwrap(), vec![0, CHUNK_SIZE_256KB]);
}

#[tokio::test]
async fn recovery_offset_mismatch_realigns_via_head_when_header_absent() {
    let head_calls = Arc::new(AtomicUsize::new(0));
    let patch_offsets = Arc::new(Mutex::new(Vec::<u64>::new()));
    let hc = head_calls.clone();
    let po = patch_offsets.clone();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => created("sess-h", CHUNK_SIZE_256KB),
        ("HEAD", "/sess-h") => {
            hc.fetch_add(1, Ordering::SeqCst);
            MockResponse::new(200, "OK")
                .header("X-Capsule-Offset", CHUNK_SIZE_256KB.to_string())
                .header("X-Capsule-Content-Length", (300 * 1024).to_string())
                .header("X-Capsule-Upload-Status", "uploading")
        }
        ("PATCH", "/sess-h") => {
            let offset: u64 = req.header("X-Capsule-Offset").unwrap().parse().unwrap();
            po.lock().unwrap().push(offset);
            if offset == 0 {
                // offset_mismatch with NO authoritative header → client must HEAD.
                MockResponse::api_error(
                    409,
                    "Conflict",
                    error_codes::UPLOAD_OFFSET_MISMATCH,
                    "Invalid offset",
                )
            } else {
                let new_offset = ack_offset(req);
                MockResponse::new(204, "No Content")
                    .header("X-Capsule-Offset", new_offset.to_string())
            }
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let client = server.client(PROTOCOL);
    let size = 300 * 1024;
    let data = vec![1u8; size as usize];
    let outcome = client.upload(&request(size), &data).await.unwrap();

    assert!(matches!(outcome, UploadOutcome::Completed { .. }));
    assert_eq!(
        head_calls.load(Ordering::SeqCst),
        1,
        "must HEAD to re-align"
    );
    assert_eq!(*patch_offsets.lock().unwrap(), vec![0, CHUNK_SIZE_256KB]);
}

/// `rate_limited`, `unavailable` and `storage_full` all mean "wait, then resume
/// from HEAD" (transfer upload doc, §Hostile Clients): the refused chunk is sent
/// again at the offset HEAD names, and nothing is re-created.
#[tokio::test]
async fn recovery_a_server_asked_wait_resumes_from_head() {
    for code in [
        error_codes::UPLOAD_RATE_LIMITED,
        error_codes::UPLOAD_UNAVAILABLE,
        error_codes::UPLOAD_STORAGE_FULL,
    ] {
        let head_calls = Arc::new(AtomicUsize::new(0));
        let patch_offsets = Arc::new(Mutex::new(Vec::<u64>::new()));
        let creates = Arc::new(AtomicUsize::new(0));
        let hc = head_calls.clone();
        let po = patch_offsets.clone();
        let cr = creates.clone();
        let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
            ("POST", "/") => {
                cr.fetch_add(1, Ordering::SeqCst);
                created("sess-wait", CHUNK_SIZE_256KB)
            }
            ("HEAD", "/sess-wait") => {
                hc.fetch_add(1, Ordering::SeqCst);
                MockResponse::new(200, "OK")
                    .header("X-Capsule-Offset", "0")
                    .header("X-Capsule-Content-Length", "4096")
                    .header("X-Capsule-Upload-Status", "pending")
            }
            ("PATCH", "/sess-wait") => {
                let offset: u64 = req.header("X-Capsule-Offset").unwrap().parse().unwrap();
                let mut seen = po.lock().unwrap();
                seen.push(offset);
                if seen.len() == 1 {
                    // Zero seconds, so the test does not sleep; the value is what
                    // the client honours, whatever it is.
                    MockResponse::api_error(503, "Service Unavailable", code, "wait")
                        .header("Retry-After", "0")
                } else {
                    let new_offset = ack_offset(req);
                    MockResponse::new(204, "No Content")
                        .header("X-Capsule-Offset", new_offset.to_string())
                }
            }
            other => panic!("unexpected {other:?}"),
        })
        .await;

        let client = server.client(PROTOCOL);
        let data = vec![9u8; 4096];
        let outcome = client.upload(&request(4096), &data).await.unwrap();

        assert!(matches!(outcome, UploadOutcome::Completed { .. }), "{code}");
        assert_eq!(
            head_calls.load(Ordering::SeqCst),
            1,
            "{code}: resumes from HEAD"
        );
        assert_eq!(
            *patch_offsets.lock().unwrap(),
            vec![0, 0],
            "{code}: the same chunk again"
        );
        assert_eq!(
            creates.load(Ordering::SeqCst),
            1,
            "{code}: the session is kept"
        );
    }
}

/// The wait a server names is read from `Retry-After`, and honoured only up to
/// the client's own ceiling.
#[test]
fn a_server_named_wait_is_read_and_bounded() {
    let mut headers = reqwest::header::HeaderMap::new();
    assert_eq!(retry_after(&headers), None, "no header, no named wait");
    headers.insert("Retry-After", "30".parse().unwrap());
    assert_eq!(retry_after(&headers), Some(Duration::from_secs(30)));
    headers.insert(
        "Retry-After",
        "Wed, 21 Oct 2015 07:28:00 GMT".parse().unwrap(),
    );
    assert_eq!(
        retry_after(&headers),
        None,
        "an HTTP-date leaves the client's own backoff"
    );

    assert!(asks_to_wait(error_codes::UPLOAD_STORAGE_FULL));
    assert!(asks_to_wait(error_codes::UPLOAD_RATE_LIMITED));
    assert!(asks_to_wait(error_codes::UPLOAD_UNAVAILABLE));
    assert!(!asks_to_wait(error_codes::UPLOAD_STORAGE_INCONSISTENT));
    assert_eq!(
        Duration::from_hours(1).min(MAX_SERVER_BACKOFF),
        Duration::from_mins(5),
        "an hour-long ask is waited in five-minute steps"
    );

    // A HEAD answer has no body, so its status names the wait; a code, when
    // there is one, decides.
    for status in [429, 503, 507] {
        assert!(answer_asks_to_wait(status, None), "{status}");
    }
    assert!(!answer_asks_to_wait(500, None));
    assert!(!answer_asks_to_wait(
        503,
        Some(error_codes::UPLOAD_STORAGE_INCONSISTENT)
    ));
}

/// An outage that outlasts one wait: the `HEAD` after the wait is itself a
/// body-less `503`, and the client waits again rather than failing the transfer.
#[tokio::test]
async fn recovery_a_head_asked_to_wait_waits_again() {
    let head_calls = Arc::new(AtomicUsize::new(0));
    let patch_offsets = Arc::new(Mutex::new(Vec::<u64>::new()));
    let hc = head_calls.clone();
    let po = patch_offsets.clone();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => created("sess-out", CHUNK_SIZE_256KB),
        ("HEAD", "/sess-out") => {
            if hc.fetch_add(1, Ordering::SeqCst) < 2 {
                MockResponse::new(503, "Service Unavailable").header("Retry-After", "0")
            } else {
                MockResponse::new(200, "OK")
                    .header("X-Capsule-Offset", "0")
                    .header("X-Capsule-Content-Length", "4096")
                    .header("X-Capsule-Upload-Status", "pending")
            }
        }
        ("PATCH", "/sess-out") => {
            let mut seen = po.lock().unwrap();
            seen.push(req.header("X-Capsule-Offset").unwrap().parse().unwrap());
            if seen.len() == 1 {
                MockResponse::api_error(
                    503,
                    "Service Unavailable",
                    error_codes::UPLOAD_UNAVAILABLE,
                    "wait",
                )
                .header("Retry-After", "0")
            } else {
                MockResponse::new(204, "No Content").header("X-Capsule-Offset", "4096")
            }
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let client = server.client(PROTOCOL);
    let outcome = client.upload(&request(4096), &[9u8; 4096]).await.unwrap();

    assert!(matches!(outcome, UploadOutcome::Completed { .. }));
    assert_eq!(
        head_calls.load(Ordering::SeqCst),
        3,
        "two waited HEADs, then the one that answers"
    );
    assert_eq!(*patch_offsets.lock().unwrap(), vec![0, 0]);
}

/// The create is paced, not failed: `rate_limited`, `unavailable` and
/// `storage_full` on `POST /upload` are waited out before the first chunk.
#[tokio::test]
async fn recovery_a_create_asked_to_wait_is_waited_out() {
    for (status, code) in [
        (429, error_codes::UPLOAD_RATE_LIMITED),
        (503, error_codes::UPLOAD_UNAVAILABLE),
        (507, error_codes::UPLOAD_STORAGE_FULL),
    ] {
        let creates = Arc::new(AtomicUsize::new(0));
        let cr = creates.clone();
        let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
            ("POST", "/") => {
                if cr.fetch_add(1, Ordering::SeqCst) < 2 {
                    MockResponse::api_error(status, "Wait", code, "wait").header("Retry-After", "0")
                } else {
                    created("sess-paced", CHUNK_SIZE_256KB)
                }
            }
            ("PATCH", "/sess-paced") => {
                let new_offset = ack_offset(req);
                MockResponse::new(204, "No Content")
                    .header("X-Capsule-Offset", new_offset.to_string())
            }
            other => panic!("unexpected {other:?}"),
        })
        .await;

        let client = server.client(PROTOCOL);
        let outcome = client.upload(&request(4096), &[9u8; 4096]).await.unwrap();

        assert!(matches!(outcome, UploadOutcome::Completed { .. }), "{code}");
        assert_eq!(
            creates.load(Ordering::SeqCst),
            3,
            "{code}: two waits, one create"
        );

        // The single-shot call still reports the answer as it came.
        let one = client.create_session(&request(4096)).await;
        assert!(one.is_ok(), "{code}: the server now creates");
    }
}

/// A re-create (after `session_not_found`) that the server asks to wait is
/// waited out too, rather than failing the transfer mid-way.
#[tokio::test]
async fn recovery_a_recreate_asked_to_wait_is_waited_out() {
    let creates = Arc::new(AtomicUsize::new(0));
    let cr = creates.clone();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => match cr.fetch_add(1, Ordering::SeqCst) {
            0 => created("sess-A", CHUNK_SIZE_256KB),
            1 => MockResponse::api_error(
                429,
                "Too Many Requests",
                error_codes::UPLOAD_RATE_LIMITED,
                "wait",
            )
            .header("Retry-After", "0"),
            _ => created("sess-B", CHUNK_SIZE_256KB),
        },
        ("PATCH", "/sess-A") => MockResponse::api_error(
            404,
            "Not Found",
            error_codes::UPLOAD_SESSION_NOT_FOUND,
            "Session not found",
        ),
        ("PATCH", "/sess-B") => {
            let new_offset = ack_offset(req);
            MockResponse::new(204, "No Content").header("X-Capsule-Offset", new_offset.to_string())
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let client = server.client(PROTOCOL);
    let outcome = client.upload(&request(4096), &[9u8; 4096]).await.unwrap();

    match outcome {
        UploadOutcome::Completed { session_id } => assert_eq!(session_id, "sess-B"),
        other @ UploadOutcome::AlreadyStored { .. } => panic!("expected Completed, got {other:?}"),
    }
    assert_eq!(creates.load(Ordering::SeqCst), 3);
}

/// A server that never stops asking for a wait spends the recovery budget and
/// fails the transfer loudly, and the single-shot create reports the rejection.
#[tokio::test]
async fn a_create_that_never_stops_waiting_exhausts_the_budget() {
    let creates = Arc::new(AtomicUsize::new(0));
    let cr = creates.clone();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => {
            cr.fetch_add(1, Ordering::SeqCst);
            MockResponse::api_error(
                503,
                "Service Unavailable",
                error_codes::UPLOAD_UNAVAILABLE,
                "down",
            )
            .header("Retry-After", "0")
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let client = server.client(PROTOCOL);
    let err = client
        .upload(&request(4096), &[9u8; 4096])
        .await
        .unwrap_err();
    assert!(matches!(err, UploadError::RetriesExhausted(_)), "{err:?}");
    assert_eq!(
        creates.load(Ordering::SeqCst),
        MAX_RECOVERIES as usize + 1,
        "every wait charged the budget"
    );

    match client.create_session(&request(4096)).await.unwrap_err() {
        UploadError::Rejected { status, code, .. } => {
            assert_eq!(status, 503);
            assert_eq!(code.as_deref(), Some(error_codes::UPLOAD_UNAVAILABLE));
        }
        other => panic!("expected Rejected, got {other:?}"),
    }
}

#[tokio::test]
async fn recovery_session_not_found_recreates() {
    let creates = Arc::new(AtomicUsize::new(0));
    let cc = creates.clone();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => {
            let n = cc.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                created("sess-A", CHUNK_SIZE_256KB)
            } else {
                created("sess-B", CHUNK_SIZE_256KB)
            }
        }
        ("PATCH", "/sess-A") => MockResponse::api_error(
            404,
            "Not Found",
            error_codes::UPLOAD_SESSION_NOT_FOUND,
            "Session not found",
        ),
        ("PATCH", "/sess-B") => {
            let new_offset = ack_offset(req);
            MockResponse::new(204, "No Content").header("X-Capsule-Offset", new_offset.to_string())
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let client = server.client(PROTOCOL);
    let data = vec![9u8; 4096];
    let outcome = client.upload(&request(4096), &data).await.unwrap();

    match outcome {
        UploadOutcome::Completed { session_id } => assert_eq!(session_id, "sess-B"),
        other @ UploadOutcome::AlreadyStored { .. } => {
            panic!("expected Completed on the re-created session, got {other:?}")
        }
    }
    assert_eq!(
        creates.load(Ordering::SeqCst),
        2,
        "must re-create the session"
    );
}

#[tokio::test]
async fn recovery_duplicate_blob_merges() {
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => MockResponse::api_error(
            409,
            "Conflict",
            error_codes::UPLOAD_DUPLICATE_BLOB,
            "This content is already stored as asset asset-xyz",
        ),
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let client = server.client(PROTOCOL);
    let data = vec![5u8; 4096];
    let outcome = client.upload(&request(4096), &data).await.unwrap();

    match outcome {
        UploadOutcome::AlreadyStored { asset_ref } => assert_eq!(asset_ref, "asset-xyz"),
        other @ UploadOutcome::Completed { .. } => {
            panic!("expected AlreadyStored (merge), got {other:?}")
        }
    }
}

#[tokio::test]
async fn recovery_checksum_mismatch_resends_same_chunk() {
    let patch_count = Arc::new(AtomicUsize::new(0));
    let bodies = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let pc = patch_count.clone();
    let bd = bodies.clone();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => created("sess-cm", CHUNK_SIZE_256KB),
        ("PATCH", "/sess-cm") => {
            bd.lock().unwrap().push(req.body.clone());
            let n = pc.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // Transit corruption: reject, nothing persisted, offset unchanged.
                MockResponse::api_error(
                    400,
                    "Bad Request",
                    error_codes::UPLOAD_CHECKSUM_MISMATCH,
                    "Chunk checksum mismatch",
                )
            } else {
                let new_offset = ack_offset(req);
                MockResponse::new(204, "No Content")
                    .header("X-Capsule-Offset", new_offset.to_string())
            }
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let client = server.client(PROTOCOL);
    let data = vec![4u8; 4096];
    let outcome = client.upload(&request(4096), &data).await.unwrap();

    assert!(matches!(outcome, UploadOutcome::Completed { .. }));
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2, "must re-send after a checksum mismatch");
    assert_eq!(bodies[0], bodies[1], "the re-send is byte-identical");
}

#[tokio::test]
async fn upgrade_required_aborts_at_create_with_the_advertised_window() {
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => MockResponse::api_error(
            426,
            "Upgrade Required",
            error_codes::PROTOCOL_VERSION_UNSUPPORTED,
            "Protocol version outside the supported window",
        )
        .header("X-Capsule-Protocol-Min", "2026-01-01")
        .header("X-Capsule-Protocol-Max", "2026-06-30"),
        other => panic!("unexpected {other:?}"),
    })
    .await;

    // This client speaks a version past the server's window.
    let client = server.client(PROTOCOL);
    let data = vec![0u8; 4096];
    let err = client.upload(&request(4096), &data).await.unwrap_err();

    match err {
        UploadError::UpgradeRequired { min, max, .. } => {
            assert_eq!(min.as_deref(), Some("2026-01-01"));
            assert_eq!(max.as_deref(), Some("2026-06-30"));
        }
        other => panic!("expected UpgradeRequired, got {other:?}"),
    }
}

#[tokio::test]
async fn upgrade_required_aborts_mid_stream() {
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => created("sess-up", CHUNK_SIZE_256KB),
        ("PATCH", "/sess-up") => MockResponse::api_error(
            426,
            "Upgrade Required",
            error_codes::PROTOCOL_VERSION_UNSUPPORTED,
            "Protocol version retired mid-transfer",
        )
        .header("X-Capsule-Protocol-Min", "2026-01-01")
        .header("X-Capsule-Protocol-Max", "2026-06-30"),
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let client = server.client(PROTOCOL);
    let data = vec![0u8; 4096];
    let err = client.upload(&request(4096), &data).await.unwrap_err();
    assert!(matches!(err, UploadError::UpgradeRequired { .. }));
}

// ─── Resume semantics ───────────────────────────────────────────────────────

#[tokio::test]
async fn resume_does_not_resend_bytes_the_server_already_has() {
    let patch_offsets = Arc::new(Mutex::new(Vec::<u64>::new()));
    let po = patch_offsets.clone();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("HEAD", "/sess-R") => MockResponse::new(200, "OK")
            .header("X-Capsule-Offset", CHUNK_SIZE_256KB.to_string())
            .header("X-Capsule-Content-Length", (300 * 1024).to_string())
            .header("X-Capsule-Upload-Status", "uploading"),
        ("PATCH", "/sess-R") => {
            let offset: u64 = req.header("X-Capsule-Offset").unwrap().parse().unwrap();
            po.lock().unwrap().push(offset);
            let new_offset = ack_offset(req);
            MockResponse::new(204, "No Content").header("X-Capsule-Offset", new_offset.to_string())
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let client = server.client(PROTOCOL);
    let size = 300 * 1024;
    let data = vec![2u8; size as usize];
    let outcome = client
        .upload_resuming("sess-R", &request(size), &data)
        .await
        .unwrap();

    assert!(matches!(outcome, UploadOutcome::Completed { .. }));
    // Only the tail chunk is sent — nothing at offset 0.
    assert_eq!(*patch_offsets.lock().unwrap(), vec![CHUNK_SIZE_256KB]);
}

#[tokio::test]
async fn resume_recreates_when_session_is_gone() {
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("HEAD", "/sess-gone") => MockResponse::new(404, "Not Found"),
        ("POST", "/") => created("sess-new", CHUNK_SIZE_256KB),
        ("PATCH", "/sess-new") => {
            let new_offset = ack_offset(req);
            MockResponse::new(204, "No Content").header("X-Capsule-Offset", new_offset.to_string())
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let client = server.client(PROTOCOL);
    let data = vec![6u8; 4096];
    let outcome = client
        .upload_resuming("sess-gone", &request(4096), &data)
        .await
        .unwrap();
    match outcome {
        UploadOutcome::Completed { session_id } => assert_eq!(session_id, "sess-new"),
        other @ UploadOutcome::AlreadyStored { .. } => {
            panic!("expected re-created Completed, got {other:?}")
        }
    }
}

// ─── HEAD / DELETE / list ───────────────────────────────────────────────────

#[tokio::test]
async fn head_reports_offset_and_status() {
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("HEAD", "/s1") => MockResponse::new(200, "OK")
            .header("X-Capsule-Offset", "100")
            .header("X-Capsule-Content-Length", "200")
            .header("X-Capsule-Upload-Status", "uploading"),
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let info = server.client(PROTOCOL).head("s1").await.unwrap().unwrap();
    assert_eq!(info.offset, 100);
    assert_eq!(info.total_size, Some(200));
    assert_eq!(info.status, "uploading");
}

#[tokio::test]
async fn head_returns_none_on_404() {
    let server = MockServer::start(move |_req| MockResponse::new(404, "Not Found")).await;
    let info = server.client(PROTOCOL).head("missing").await.unwrap();
    assert!(info.is_none());
}

#[tokio::test]
async fn list_sessions_parses_rows() {
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/sessions") => MockResponse::new(200, "OK").json_body(
            r#"{"sessions":[{"id":"s1","asset_id":"a1","received_bytes":100,"total_size":200,"status":"Uploading","extra_unknown":true}]}"#,
        ),
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let rows = server.client(PROTOCOL).list_sessions().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "s1");
    assert_eq!(rows[0].received_bytes, 100);
    assert_eq!(rows[0].status, "Uploading");
}

#[tokio::test]
async fn delete_is_ok_on_204_and_on_404() {
    let server = MockServer::start(move |req| match req.path.as_str() {
        "/present" => MockResponse::new(204, "No Content"),
        "/gone" => MockResponse::new(404, "Not Found"),
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let client = server.client(PROTOCOL);
    client.delete("present").await.unwrap();
    client.delete("gone").await.unwrap();
}

#[tokio::test]
async fn delete_during_finalization_surfaces_the_conflict() {
    let server = MockServer::start(move |_req| {
        MockResponse::api_error(
            409,
            "Conflict",
            error_codes::UPLOAD_SESSION_NOT_ACTIVE,
            "Session is already finished or being processed",
        )
    })
    .await;

    let err = server
        .client(PROTOCOL)
        .delete("finalizing")
        .await
        .unwrap_err();
    match err {
        UploadError::Rejected { status, code, .. } => {
            assert_eq!(status, 409);
            assert_eq!(
                code.as_deref(),
                Some(error_codes::UPLOAD_SESSION_NOT_ACTIVE)
            );
        }
        other => panic!("expected Rejected 409, got {other:?}"),
    }
}

// ─── Windowed sources ────────────────────────────────────────────────────────

/// A source that holds nothing: it computes each requested window and records the request,
/// so a test can see the client read exactly the chunks it sent — never the blob.
struct ComputedSource {
    reads: Mutex<Vec<(u64, usize)>>,
    /// Bytes to withhold from the end of every window, to model a source that changed.
    short_by: usize,
}

impl ComputedSource {
    fn new(short_by: usize) -> Self {
        Self {
            reads: Mutex::new(Vec::new()),
            short_by,
        }
    }

    fn byte_at(offset: u64) -> u8 {
        (offset % 251) as u8
    }
}

impl UploadSource for ComputedSource {
    fn read_at(&self, offset: u64, len: usize) -> std::io::Result<Cow<'_, [u8]>> {
        self.reads.lock().unwrap().push((offset, len));
        let len = len.saturating_sub(self.short_by);
        Ok(Cow::Owned(
            (offset..offset + len as u64).map(Self::byte_at).collect(),
        ))
    }
}

#[tokio::test]
async fn upload_reads_the_source_one_chunk_window_at_a_time() {
    let received = Arc::new(Mutex::new(Vec::<(u64, Vec<u8>)>::new()));
    let r = received.clone();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => created("sess-w", CHUNK_SIZE_256KB),
        ("PATCH", "/sess-w") => {
            let offset: u64 = req.header("X-Capsule-Offset").unwrap().parse().unwrap();
            r.lock().unwrap().push((offset, req.body.clone()));
            let new_offset = ack_offset(req);
            MockResponse::new(204, "No Content").header("X-Capsule-Offset", new_offset.to_string())
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let client = server.client(PROTOCOL);
    let size = 600 * 1024;
    let source = ComputedSource::new(0);
    let outcome = client.upload(&request(size), &source).await.unwrap();
    assert!(matches!(outcome, UploadOutcome::Completed { .. }));

    let received = received.lock().unwrap().clone();
    let reads = source.reads.lock().unwrap().clone();
    assert_eq!(
        reads,
        received
            .iter()
            .map(|(offset, body)| (*offset, body.len()))
            .collect::<Vec<_>>(),
        "every read is exactly one chunk the client then sent"
    );
    assert!(
        reads.iter().all(|(_, len)| *len as u64 <= CHUNK_SIZE_256KB),
        "no read is larger than a chunk: {reads:?}"
    );
    for (offset, body) in &received {
        let expected: Vec<u8> = (*offset..*offset + body.len() as u64)
            .map(ComputedSource::byte_at)
            .collect();
        assert_eq!(body, &expected, "chunk at {offset}");
    }
}

/// A source that returns fewer bytes than the window it was asked for has changed under the
/// upload: the client stops with an I/O error rather than sending a chunk it cannot complete.
#[tokio::test]
async fn a_short_source_read_fails_the_upload() {
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => created("sess-s", CHUNK_SIZE_256KB),
        other => panic!("nothing is sent from a short read: {other:?}"),
    })
    .await;
    let client = server.client(PROTOCOL);
    let source = ComputedSource::new(1);
    let err = client.upload(&request(4096), &source).await.unwrap_err();
    assert!(matches!(err, UploadError::Io(_)), "{err:?}");
}

#[test]
fn a_byte_slice_source_refuses_a_window_past_its_end() {
    let bytes = [1u8, 2, 3, 4];
    assert_eq!(&*bytes.read_at(1, 2).unwrap(), &[2, 3]);
    assert!(bytes.read_at(3, 2).is_err());
    assert!(bytes.read_at(u64::MAX, 1).is_err());
}

// ─── The protocol window on the upload route class (issue #674) ─────────────

/// A server whose every answer advertises `floor` as its client-build floor, recording each
/// request's method; a `HEAD` is a `404` (no such session) and anything else a `201`.
async fn floored_server(floor: &'static str) -> (MockServer, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let server = MockServer::start(move |req| {
        log.lock().unwrap().push(req.method.clone());
        let response = if req.method == "HEAD" {
            MockResponse::new(404, "Not Found")
        } else {
            created("sess-floor", CHUNK_SIZE_256KB)
        };
        response
            .header("X-Capsule-Protocol-Min", "2026-01-01")
            .header("X-Capsule-Protocol-Max", "2099-12-31")
            .header("X-Capsule-Min-Client-Build", floor)
    })
    .await;
    (server, seen)
}

/// A server that advertised a client-build floor above this build on a read refuses the
/// next upload **before it leaves**: the create never reaches the wire, and the refusal is
/// the typed `UpgradeRequired` carrying the floor.
#[tokio::test]
async fn a_latched_build_floor_refuses_an_upload_before_it_leaves() {
    let (server, seen) = floored_server("999.0.0").await;
    let client = server.client(PROTOCOL);

    assert!(
        client.head("absent").await.unwrap().is_none(),
        "the read is served"
    );
    let err = client
        .upload(&request(4096), &[0u8; 4096])
        .await
        .unwrap_err();
    match err {
        UploadError::UpgradeRequired {
            reason,
            min_client_build,
            ..
        } => {
            assert_eq!(reason, negotiation::UpgradeReason::BuildBelowFloor);
            assert_eq!(min_client_build.as_deref(), Some("999.0.0"));
        }
        other => panic!("expected UpgradeRequired, got {other:?}"),
    }
    assert_eq!(
        *seen.lock().unwrap(),
        vec!["HEAD".to_owned()],
        "the create never left"
    );
}

/// A floor this build meets refuses nothing: the same sequence reaches the create.
#[tokio::test]
async fn a_build_floor_this_build_meets_refuses_nothing() {
    let (server, seen) = floored_server("0.0.0").await;
    let client = server.client(PROTOCOL);

    assert!(client.head("absent").await.unwrap().is_none());
    let created = client.create_session(&request(4096)).await.unwrap();
    assert!(
        matches!(created, CreateOutcome::Created { .. }),
        "{created:?}"
    );
    assert_eq!(
        *seen.lock().unwrap(),
        vec!["HEAD".to_owned(), "POST".to_owned()]
    );
}

/// The server's `426` on an upload is the same typed refusal, naming the bound the window it
/// sent says this client broke.
#[tokio::test]
async fn an_upload_426_names_the_bound_it_broke() {
    let server = MockServer::start(|_req| {
        MockResponse::api_error(
            426,
            "Upgrade Required",
            error_codes::PROTOCOL_VERSION_UNSUPPORTED,
            "too old",
        )
        .header("X-Capsule-Protocol-Min", "2099-01-01")
        .header("X-Capsule-Protocol-Max", "2099-12-31")
    })
    .await;
    let err = server
        .client(PROTOCOL)
        .upload(&request(4096), &[0u8; 4096])
        .await
        .unwrap_err();
    match err {
        UploadError::UpgradeRequired {
            reason, message, ..
        } => {
            assert_eq!(reason, negotiation::UpgradeReason::ProtocolBelowWindow);
            assert_eq!(message, "too old");
        }
        other => panic!("expected UpgradeRequired, got {other:?}"),
    }
}
