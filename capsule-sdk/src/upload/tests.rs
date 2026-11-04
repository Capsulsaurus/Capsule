//! Tests for the upload client.
//!
//! Two layers:
//! - **Adaptive strategy (unit).** The normative chunk-size algorithm: warm-up,
//!   double/halve, tier clamping, and the *by-construction* 4 KiB-alignment /
//!   protocol-bounds guarantees.
//! - **Protocol conformance + recovery matrix (smoke).** The client drives an
//!   in-process mock HTTP server that replays the real wire (statuses, headers,
//!   `ApiError` JSON, `error.*` codes taken straight from the server's upload handlers). One
//!   test per recovery-matrix code, plus the happy-path round-trip, resume, and
//!   the `426` abort-with-upgrade against the real handshake shape.
//!
//! These mock-level tests are deterministic and hermetic. The cross-module case —
//! this client driving the REAL `capsule-api/upload` server (testcontainer
//! Postgres + Valkey, real protocol gate, real disk) — lives in that crate's
//! `src/tests/sdk_client.rs`, which dev-depends on `capsule-sdk`.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::*;
use crate::testmock::{MockRequest, MockResponse, MockServer};

// ─── Adaptive strategy (unit) ───────────────────────────────────────────────

/// The duration one chunk of `bytes` takes at `bytes_per_second`.
fn at_rate(bytes: u64, bytes_per_second: f64) -> Duration {
    Duration::from_secs_f64(bytes as f64 / bytes_per_second)
}

#[test]
fn tier_selection_by_file_size() {
    let small = AdaptiveChunkSizeStrategy::for_file_size(5 * 1024 * 1024);
    assert_eq!(small.current_size, CHUNK_SIZE_256KB);
    assert_eq!(small.max_size, CHUNK_SIZE_1MB);

    let medium = AdaptiveChunkSizeStrategy::for_file_size(50 * 1024 * 1024);
    assert_eq!(medium.current_size, CHUNK_SIZE_1MB);
    assert_eq!(medium.max_size, CHUNK_SIZE_4MB);

    let large = AdaptiveChunkSizeStrategy::for_file_size(200 * 1024 * 1024);
    assert_eq!(large.current_size, CHUNK_SIZE_4MB);
    assert_eq!(large.max_size, CHUNK_SIZE_16MB);
    assert_eq!(large.target_duration(), TARGET_REQUEST_DURATION);
}

#[test]
fn every_tier_sits_inside_the_protocol_bounds_and_is_aligned() {
    for size in [1u64, 5 << 20, 50 << 20, 500 << 20] {
        let s = AdaptiveChunkSizeStrategy::for_file_size(size);
        assert!(s.current_size >= PROTOCOL_MIN_CHUNK);
        assert!(s.max_size <= PROTOCOL_MAX_CHUNK);
        assert!(s.max_size.is_multiple_of(4096));
        assert!(s.current_size.is_multiple_of(4096));
    }
}

/// **Slow but steady.** The size is the one that completes in the target duration at the
/// measured goodput, far below the old 4 MiB tier floor: at 600 ms RTT-class 6.7 KiB/s, eight
/// seconds is about 53 KiB. It is never halved towards 4 KiB, and never cut for taking long.
#[test]
fn a_slow_link_is_sized_to_the_target_duration_not_halved_to_the_minimum() {
    let rate = 6.7 * 1024.0;
    let mut s = AdaptiveChunkSizeStrategy::for_file_size(200 * 1024 * 1024); // starts at 4 MiB
    let first = s.next_chunk_size();
    s.record_chunk(first, at_rate(first, rate));
    let expected = ((rate * TARGET_REQUEST_DURATION.as_secs_f64()) as u64 / 4096) * 4096;
    assert_eq!(
        s.next_chunk_size(),
        expected,
        "one slow chunk is enough to shrink"
    );

    // Steady at that rate: the size holds at the target, and every chunk takes about it.
    for _ in 0..20 {
        let cs = s.next_chunk_size();
        s.record_chunk(cs, at_rate(cs, rate));
        assert!(cs.is_multiple_of(4096));
        assert!(cs > PROTOCOL_MIN_CHUNK, "never halved towards the minimum");
    }
    let settled = s.next_chunk_size();
    assert!(at_rate(settled, rate) <= TARGET_REQUEST_DURATION);
    assert!(at_rate(settled, rate) > TARGET_REQUEST_DURATION / 2);
}

/// A fast link grows by doubling after the warm-up, up to the tier ceiling and no further.
#[test]
fn a_fast_link_doubles_after_the_warmup_up_to_the_tier_ceiling() {
    let mut s = AdaptiveChunkSizeStrategy::for_file_size(500 * 1024 * 1024); // 4–16 MiB
    let fast = 100.0 * 1024.0 * 1024.0;
    let start = s.next_chunk_size();
    for _ in 0..WARMUP_CHUNKS - 1 {
        s.record_chunk(start, at_rate(start, fast));
        assert_eq!(s.next_chunk_size(), start, "no growth before the warm-up");
    }
    s.record_chunk(start, at_rate(start, fast));
    assert_eq!(s.next_chunk_size(), 2 * start, "doubles after it");
    for _ in 0..40 {
        let cs = s.next_chunk_size();
        s.record_chunk(cs, at_rate(cs, fast));
        assert!(cs.is_multiple_of(4096));
        assert!(cs <= s.max_size);
    }
    assert_eq!(s.next_chunk_size(), PROTOCOL_MAX_CHUNK);
}

/// Under `adverse` each request targets the shorter duration, so the same goodput yields a
/// quarter the chunk.
#[test]
fn adverse_targets_a_shorter_request_duration() {
    let rate = 512.0 * 1024.0;
    let sized = |class| {
        let mut s = AdaptiveChunkSizeStrategy::for_file_size(500 * 1024 * 1024)
            .with_connection_class(class);
        for _ in 0..30 {
            let cs = s.next_chunk_size();
            s.record_chunk(cs, at_rate(cs, rate));
        }
        s.next_chunk_size()
    };
    let unmetered = sized(ConnectionClass::Unmetered);
    let adverse = sized(ConnectionClass::Adverse);
    assert!(
        at_rate(unmetered, rate) <= TARGET_REQUEST_DURATION
            && at_rate(unmetered, rate) > TARGET_REQUEST_DURATION / 2,
        "unmetered settles near {TARGET_REQUEST_DURATION:?}: {unmetered}"
    );
    assert!(
        at_rate(adverse, rate) <= ADVERSE_TARGET_REQUEST_DURATION
            && at_rate(adverse, rate) > ADVERSE_TARGET_REQUEST_DURATION / 2,
        "adverse settles near {ADVERSE_TARGET_REQUEST_DURATION:?}: {adverse}"
    );
    assert!(adverse < unmetered);
}

#[test]
fn an_extremely_slow_link_is_held_at_the_protocol_minimum() {
    let mut s = AdaptiveChunkSizeStrategy::for_file_size(1024 * 1024);
    for _ in 0..20 {
        let cs = s.next_chunk_size();
        s.record_chunk(cs, at_rate(cs, 10.0));
    }
    assert_eq!(s.next_chunk_size(), PROTOCOL_MIN_CHUNK);
}

/// A chunk that stalled is re-sent at half its length, aligned, down to the protocol minimum.
#[test]
fn a_stall_halves_the_chunk_down_to_the_protocol_minimum() {
    let mut s = AdaptiveChunkSizeStrategy::for_file_size(200 * 1024 * 1024);
    assert_eq!(s.next_chunk_size(), CHUNK_SIZE_4MB);

    let mut sizes = Vec::new();
    while s.shrink_after_stall(s.next_chunk_size()) {
        let next = s.next_chunk_size();
        assert!(next.is_multiple_of(4096), "size drifted off 4 KiB");
        sizes.push(next);
    }
    assert_eq!(sizes.first(), Some(&(2 * 1024 * 1024)));
    assert_eq!(sizes.last(), Some(&PROTOCOL_MIN_CHUNK));
    assert!(
        !s.shrink_after_stall(PROTOCOL_MIN_CHUNK),
        "nothing below the protocol minimum"
    );
    assert_eq!(s.next_chunk_size(), PROTOCOL_MIN_CHUNK);
}

/// A stalled final remainder halves too, aligned down; one already at or under 4 KiB cannot
/// shrink.
#[test]
fn a_stalled_remainder_halves_aligned_down() {
    let mut s = AdaptiveChunkSizeStrategy::for_file_size(5 * 1024 * 1024);
    assert!(s.shrink_after_stall(100 * 1024 + 123));
    assert_eq!(s.next_chunk_size(), 48 * 1024);

    let mut s = AdaptiveChunkSizeStrategy::for_file_size(5 * 1024 * 1024);
    assert!(!s.shrink_after_stall(3000));
    assert_eq!(
        s.next_chunk_size(),
        CHUNK_SIZE_256KB,
        "an unshrinkable stall leaves the size alone"
    );
}

/// After a stall the size does not grow until a run of successes shows the link recovered,
/// however fast those successes are.
#[test]
fn after_a_stall_growth_waits_for_a_run_of_successes() {
    let mut s = AdaptiveChunkSizeStrategy::for_file_size(200 * 1024 * 1024);
    assert!(s.shrink_after_stall(CHUNK_SIZE_4MB)); // 2 MiB
    let fast = 100.0 * 1024.0 * 1024.0;
    for _ in 0..GROW_BACK_AFTER_SUCCESSES.max(WARMUP_CHUNKS) - 1 {
        s.record_chunk(2 * 1024 * 1024, at_rate(2 * 1024 * 1024, fast));
        assert_eq!(s.next_chunk_size(), 2 * 1024 * 1024, "held after the stall");
    }
    s.record_chunk(2 * 1024 * 1024, at_rate(2 * 1024 * 1024, fast));
    assert_eq!(
        s.next_chunk_size(),
        CHUNK_SIZE_4MB,
        "grows once the run shows the link recovered"
    );
}

#[test]
fn seeded_from_suggested_aligns_down_and_clamps_into_the_bounds() {
    // 50 MiB → ceiling 4 MiB.
    let tier = || AdaptiveChunkSizeStrategy::for_file_size(50 * 1024 * 1024);

    // An unaligned suggestion aligns down and stays put.
    let s = tier().seeded_from_suggested(2 * 1024 * 1024 + 123);
    assert_eq!(s.current_size, 2 * 1024 * 1024);

    // Below the tier's starting size is fine — there is no floor but the protocol's.
    let s = tier().seeded_from_suggested(64 * 1024);
    assert_eq!(s.current_size, 64 * 1024);
    let s = tier().seeded_from_suggested(100);
    assert_eq!(s.current_size, PROTOCOL_MIN_CHUNK);

    // Above the tier ceiling → clamped down to it.
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

/// A `201` as the server sends it: the body's suggestion, and the same number as the advice's
/// `chunk` member beside a fresh session's relative expiry.
fn created(id: &str, suggested: u64) -> MockResponse {
    created_without_advice(id, suggested)
        .header(
            "X-Capsule-Advice",
            format!("chunk={suggested}, sessions=4, ranges=1, ttl=300"),
        )
        .header("X-Capsule-Session-Expires-In", "86400")
        .header("X-Capsule-Session-Idle-Expires-In", "3600")
}

/// A `201` from a server that predates advice: the body's suggestion and nothing else.
fn created_without_advice(id: &str, suggested: u64) -> MockResponse {
    MockResponse::new(201, "Created")
        .header("Location", format!("/upload/{id}"))
        .json_body(format!(
            r#"{{"id":{id:?},"upload_url":"/upload/{id}","suggested_chunk_size":{suggested}}}"#
        ))
}

/// The offsets of every PATCH a mock server answered, for a session `id`.
fn recording_server(
    create: MockResponse,
    id: &'static str,
) -> (
    impl Fn(&MockRequest) -> MockResponse + Send + Sync + 'static,
    Arc<Mutex<Vec<u64>>>,
) {
    let offsets = Arc::new(Mutex::new(Vec::<u64>::new()));
    let seen = offsets.clone();
    let handler = move |req: &MockRequest| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => create.clone(),
        ("PATCH", path) if path == format!("/{id}") => {
            let offset: u64 = req.header("X-Capsule-Offset").unwrap().parse().unwrap();
            seen.lock().unwrap().push(offset);
            let new_offset = ack_offset(req);
            MockResponse::new(204, "No Content").header("X-Capsule-Offset", new_offset.to_string())
        }
        other => panic!("unexpected {other:?}"),
    };
    (handler, offsets)
}

/// The advice's `chunk` member seeds the first chunk, over the body's suggestion.
#[tokio::test]
async fn the_advice_chunk_seeds_the_first_chunk() {
    let advised = 512 * KB;
    let create = created_without_advice("sess-adv", CHUNK_SIZE_256KB).header(
        "X-Capsule-Advice",
        format!("chunk={advised}, sessions=4, ranges=1, ttl=300"),
    );
    let (handler, offsets) = recording_server(create, "sess-adv");
    let server = MockServer::start(handler).await;

    let size = 600 * KB;
    let data = vec![5u8; size as usize];
    let outcome = server.client(PROTOCOL).upload(&request(size), &data).await;

    assert!(matches!(outcome, Ok(UploadOutcome::Completed { .. })));
    assert_eq!(*offsets.lock().unwrap(), vec![0, advised]);
}

/// A server that sends no advice leaves the client on the body's suggestion.
#[tokio::test]
async fn absent_advice_falls_back_to_the_bodys_suggestion() {
    let suggested = 512 * KB;
    let (handler, offsets) =
        recording_server(created_without_advice("sess-old", suggested), "sess-old");
    let server = MockServer::start(handler).await;

    let size = 600 * KB;
    let data = vec![6u8; size as usize];
    // One range at a time, so the recorded offsets are the chunk sizes in order.
    let outcome = server
        .client(PROTOCOL)
        .with_ranges_window(1)
        .upload(&request(size), &data)
        .await;

    assert!(matches!(outcome, Ok(UploadOutcome::Completed { .. })));
    assert_eq!(*offsets.lock().unwrap(), vec![0, suggested]);
}

/// Advice the client cannot use is clamped or ignored, never an error: an off-grid chunk is
/// aligned down into the tier, and an unknown member is skipped.
#[tokio::test]
async fn unusable_advice_is_clamped_rather_than_refused() {
    let create = created_without_advice("sess-odd", CHUNK_SIZE_256KB).header(
        "X-Capsule-Advice",
        "chunk=530000, burst=99, sessions=notanumber",
    );
    let (handler, offsets) = recording_server(create, "sess-odd");
    let server = MockServer::start(handler).await;

    let size = 600 * KB;
    let data = vec![4u8; size as usize];
    // One range at a time, so the recorded offsets are the chunk sizes in order.
    let outcome = server
        .client(PROTOCOL)
        .with_ranges_window(1)
        .upload(&request(size), &data)
        .await;

    assert!(matches!(outcome, Ok(UploadOutcome::Completed { .. })));
    // 530 000 bytes aligned down to the 4 KiB grid.
    assert_eq!(*offsets.lock().unwrap(), vec![0, 528_384]);
}

/// Create and `HEAD` both surface the advice and the session's relative expiry.
#[tokio::test]
async fn create_and_head_surface_advice_and_relative_expiry() {
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => created("sess-x", CHUNK_SIZE_256KB),
        ("HEAD", "/sess-x") => MockResponse::new(200, "OK")
            .header("X-Capsule-Offset", "4096")
            .header("X-Capsule-Content-Length", "8192")
            .header("X-Capsule-Upload-Status", "uploading")
            .header(
                "X-Capsule-Advice",
                "chunk=8192, sessions=2, ranges=1, ttl=60",
            )
            .header("X-Capsule-Session-Expires-In", "80000")
            .header("X-Capsule-Session-Idle-Expires-In", "3000"),
        other => panic!("unexpected {other:?}"),
    })
    .await;
    let client = server.client(PROTOCOL);

    let CreateOutcome::Created { advice, expiry, .. } =
        client.create_session(&request(8192)).await.unwrap()
    else {
        panic!("expected a session");
    };
    assert_eq!(advice.chunk(Duration::ZERO), Some(CHUNK_SIZE_256KB));
    assert_eq!(advice.sessions_window(8, Duration::ZERO), 4);
    assert_eq!(advice.ranges_window(8), 1);
    assert_eq!(expiry.expires_in, Some(Duration::from_hours(24)));
    assert_eq!(expiry.idle_expires_in, Some(Duration::from_secs(3600)));

    let head = client.head("sess-x").await.unwrap().expect("a session");
    assert_eq!(head.advice.chunk(Duration::ZERO), Some(8192));
    assert_eq!(head.advice.ttl(), Duration::from_secs(60));
    assert_eq!(head.expiry.expires_in, Some(Duration::from_secs(80_000)));
    assert_eq!(head.expiry.idle_expires_in, Some(Duration::from_mins(50)));
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
            .header(
                "X-Capsule-Advice",
                format!("chunk={CHUNK_SIZE_256KB}, sessions=4, ranges=1, ttl=300"),
            )
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

/// The production composition: an upload driven through the SDK session, whose
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
}

/// **Every upload answer is classified before it is retried** (networking doc, Retry Policy
/// Classes; upload recovery matrix). A code the recovery matrix names an action for keeps it;
/// a code that asks for a wait waits; otherwise any `5xx`, `408`, `425` and `429` wait — a
/// proxy's bare `502` and a `500 storage_inconsistent` alike — and every other answer ends
/// the transfer.
#[test]
fn every_upload_answer_waits_recovers_or_ends_the_transfer() {
    use RetryVerdict::{Recover, Retryable, Terminal};

    let table: &[(u16, Option<&str>, RetryVerdict)] = &[
        // Waits: the three wait codes, and the transient statuses coded or not.
        (429, Some(error_codes::UPLOAD_RATE_LIMITED), Retryable),
        (503, Some(error_codes::UPLOAD_UNAVAILABLE), Retryable),
        (507, Some(error_codes::UPLOAD_STORAGE_FULL), Retryable),
        (
            500,
            Some(error_codes::UPLOAD_STORAGE_INCONSISTENT),
            Retryable,
        ),
        (
            503,
            Some(error_codes::UPLOAD_STORAGE_INCONSISTENT),
            Retryable,
        ),
        (500, None, Retryable),
        (502, None, Retryable),
        (503, None, Retryable),
        (504, None, Retryable),
        (507, None, Retryable),
        (429, None, Retryable),
        (408, None, Retryable),
        (425, None, Retryable),
        // A write refused as TLS early data: re-sent after the handshake, by its status.
        (425, Some(error_codes::REQUEST_TOO_EARLY), Retryable),
        // Recovers: the matrix's own actions, whatever the status.
        (409, Some(error_codes::UPLOAD_OFFSET_MISMATCH), Recover),
        (404, Some(error_codes::UPLOAD_SESSION_NOT_FOUND), Recover),
        (400, Some(error_codes::UPLOAD_CHECKSUM_MISMATCH), Recover),
        (408, Some(error_codes::UPLOAD_CHUNK_TIMEOUT), Recover),
        (409, Some(error_codes::UPLOAD_DUPLICATE_BLOB), Recover),
        // Ends the transfer: every other `4xx`, coded or not.
        (400, None, Terminal),
        (403, Some(error_codes::QUOTA_EXCEEDED), Terminal),
        (409, Some(error_codes::UPLOAD_SESSION_NOT_ACTIVE), Terminal),
        (413, Some(error_codes::UPLOAD_CHUNK_TOO_LARGE), Terminal),
        (
            426,
            Some(error_codes::PROTOCOL_VERSION_UNSUPPORTED),
            Terminal,
        ),
        (426, None, Terminal),
        (404, None, Terminal),
    ];
    for &(status, code, expected) in table {
        assert_eq!(
            upload_answer_verdict(status, code),
            expected,
            "{status} {code:?}"
        );
        assert_eq!(
            answer_asks_to_wait(status, code),
            expected == Retryable,
            "{status} {code:?}"
        );
    }
}

/// **A server error is a wait, not a failure.** A proxy's bare `502` on the create, a bare
/// `504` on the `HEAD` after it, and a bare `500` on a chunk are each waited out on the retry
/// engine's backoff (none names a `Retry-After`), and the chunk resumes from `HEAD`.
#[tokio::test(start_paused = true)]
async fn a_server_error_on_any_upload_request_is_waited_out() {
    let creates = Arc::new(AtomicUsize::new(0));
    let heads = Arc::new(AtomicUsize::new(0));
    let patch_offsets = Arc::new(Mutex::new(Vec::<u64>::new()));
    let (cr, hc, po) = (creates.clone(), heads.clone(), patch_offsets.clone());
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => {
            if cr.fetch_add(1, Ordering::SeqCst) == 0 {
                MockResponse::new(502, "Bad Gateway")
            } else {
                created("sess-5xx", CHUNK_SIZE_256KB)
            }
        }
        ("HEAD", "/sess-5xx") => {
            if hc.fetch_add(1, Ordering::SeqCst) == 0 {
                MockResponse::new(504, "Gateway Timeout")
            } else {
                MockResponse::new(200, "OK")
                    .header("X-Capsule-Offset", "0")
                    .header("X-Capsule-Content-Length", "4096")
                    .header("X-Capsule-Upload-Status", "uploading")
            }
        }
        ("PATCH", "/sess-5xx") => {
            let mut seen = po.lock().unwrap();
            seen.push(req.header("X-Capsule-Offset").unwrap().parse().unwrap());
            if seen.len() == 1 {
                MockResponse::new(500, "Internal Server Error")
            } else {
                MockResponse::new(204, "No Content").header("X-Capsule-Offset", "4096")
            }
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let client = server.client(PROTOCOL);
    let outcome = client.upload(&request(4096), &[9u8; 4096]).await.unwrap();

    assert!(
        matches!(outcome, UploadOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(creates.load(Ordering::SeqCst), 2, "one wait, one create");
    assert_eq!(
        heads.load(Ordering::SeqCst),
        2,
        "one waited HEAD, then the answer"
    );
    assert_eq!(
        *patch_offsets.lock().unwrap(),
        vec![0, 0],
        "resumed where HEAD said"
    );
}

/// **A completing chunk answered `500 storage_inconsistent` resumes from `HEAD`.** The server
/// may have kept the chunk and finalized: the client waits, asks `HEAD`, and settles on what
/// it says — the finalized outcome, without re-sending a byte, or the refusal it reports.
#[tokio::test(start_paused = true)]
async fn a_completing_chunk_answered_storage_inconsistent_settles_from_head() {
    for (status, error, expected_ok) in [
        ("completed", None, true),
        (
            "failed_processing",
            Some(error_codes::UPLOAD_STORAGE_INCONSISTENT),
            false,
        ),
    ] {
        let patches = Arc::new(AtomicUsize::new(0));
        let pc = patches.clone();
        let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
            ("POST", "/") => created("sess-500", CHUNK_SIZE_256KB),
            ("PATCH", "/sess-500") => {
                pc.fetch_add(1, Ordering::SeqCst);
                MockResponse::api_error(
                    500,
                    "Internal Server Error",
                    error_codes::UPLOAD_STORAGE_INCONSISTENT,
                    "the staged upload and the session's byte count disagree",
                )
            }
            ("HEAD", "/sess-500") => {
                let head = MockResponse::new(200, "OK")
                    .header("X-Capsule-Offset", "4096")
                    .header("X-Capsule-Content-Length", "4096")
                    .header("X-Capsule-Upload-Status", status);
                match error {
                    Some(code) => head.header("X-Capsule-Upload-Error", code),
                    None => head,
                }
            }
            other => panic!("unexpected {other:?}"),
        })
        .await;

        let client = server.client(PROTOCOL);
        let outcome = client.upload(&request(4096), &[9u8; 4096]).await;

        assert_eq!(patches.load(Ordering::SeqCst), 1, "{status}: never re-sent");
        if expected_ok {
            assert!(
                matches!(outcome, Ok(UploadOutcome::Completed { .. })),
                "{outcome:?}"
            );
        } else {
            match outcome {
                Err(UploadError::Rejected { code, .. }) => {
                    assert_eq!(code.as_deref(), error, "{status}");
                }
                other => panic!("expected the finalization refusal, got {other:?}"),
            }
        }
    }
}

/// **Giving up on a coded `5xx` keeps its code.** A chunk answered `500 storage_inconsistent`
/// every time, with no `Retry-After`, is waited out on the engine's backoff until it is spent;
/// the transfer then ends with the last answer's status and stable code, so a client still
/// localizes it, and the reason names the spent backoff rather than a wait nobody asked for.
#[tokio::test(start_paused = true)]
async fn giving_up_on_a_coded_server_error_keeps_its_code() {
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => created("sess-stuck", CHUNK_SIZE_256KB),
        ("PATCH", "/sess-stuck") => MockResponse::api_error(
            500,
            "Internal Server Error",
            error_codes::UPLOAD_STORAGE_INCONSISTENT,
            "the staged upload and the session's byte count disagree",
        ),
        ("HEAD", "/sess-stuck") => MockResponse::new(200, "OK")
            .header("X-Capsule-Offset", "0")
            .header("X-Capsule-Content-Length", "4096")
            .header("X-Capsule-Upload-Status", "pending"),
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let client = server.client(PROTOCOL);
    let err = client
        .upload(&request(4096), &[9u8; 4096])
        .await
        .unwrap_err();

    match &err {
        UploadError::WaitsExhausted {
            status,
            code,
            reason,
            ..
        } => {
            assert_eq!(*status, 500);
            assert_eq!(
                code.as_deref(),
                Some(error_codes::UPLOAD_STORAGE_INCONSISTENT)
            );
            assert!(reason.contains("name no Retry-After"), "{reason}");
        }
        other => panic!("expected the give-up to carry the answer, got {other:?}"),
    }
    assert_eq!(
        err.error_code(),
        Some(error_codes::UPLOAD_STORAGE_INCONSISTENT)
    );
}

/// **A server error that never ends still ends the transfer.** Every wait charges the
/// transfer's recovery budget and the lane's shared one, so a server answering every chunk
/// `500` costs a bounded number of requests and fails loudly.
#[tokio::test(start_paused = true)]
async fn a_server_that_always_errs_exhausts_the_budget() {
    let requests = Arc::new(AtomicUsize::new(0));
    let rq = requests.clone();
    let server = MockServer::start(move |req| {
        rq.fetch_add(1, Ordering::SeqCst);
        match (req.method.as_str(), req.path.as_str()) {
            ("POST", "/") => created("sess-down", CHUNK_SIZE_256KB),
            _ => MockResponse::new(500, "Internal Server Error").header("Retry-After", "0"),
        }
    })
    .await;

    let lane = LaneRetries::new(10);
    let client = server.client(PROTOCOL).with_lane_retries(lane.clone());
    let err = client
        .upload(&request(4096), &[9u8; 4096])
        .await
        .unwrap_err();

    assert!(
        matches!(
            err,
            UploadError::WaitsExhausted {
                status: 500,
                code: None,
                ..
            }
        ),
        "the give-up carries the last answer: {err:?}"
    );
    assert_eq!(lane.remaining(), 0, "the lane's budget bounded the waits");
    assert!(
        requests.load(Ordering::SeqCst) <= 2 + 10,
        "a bounded number of requests"
    );
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
    assert!(
        matches!(err, UploadError::WaitsExhausted { status: 503, .. }),
        "{err:?}"
    );
    assert_eq!(err.error_code(), Some(error_codes::UPLOAD_UNAVAILABLE));
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

/// A blob of zeroes of any declared size, produced a window at a time, so a
/// large-file tier can be driven without holding the file. It cannot change, so it affirms.
struct Zeroes(u64);

impl UploadSource for Zeroes {
    fn read_at(&self, offset: u64, len: usize) -> std::io::Result<Cow<'_, [u8]>> {
        if offset + len as u64 > self.0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        Ok(Cow::Owned(vec![0u8; len]))
    }

    fn check_unchanged(&self) -> Result<SourceIdentity, SourceChanged> {
        Ok(SourceIdentity::Affirmed)
    }
}

/// Zeroes behind the default identity check: no change found, none affirmed.
struct Unaffirmed(u64);

impl UploadSource for Unaffirmed {
    fn read_at(&self, offset: u64, len: usize) -> std::io::Result<Cow<'_, [u8]>> {
        Zeroes(self.0)
            .read_at(offset, len)
            .map(|w| Cow::Owned(w.into_owned()))
    }
}

/// The `408` the server answers a chunk body it cut for stalling.
fn chunk_timeout() -> MockResponse {
    MockResponse::api_error(
        408,
        "Request Timeout",
        error_codes::UPLOAD_CHUNK_TIMEOUT,
        "The chunk took too long to arrive",
    )
}

/// **Stalling link.** A large file on a link that stalls on every body larger
/// than 256 KiB: the server cuts each one. The client re-sends at the same
/// offset, halving each time, until a chunk arrives; once one does, the
/// transfer progresses.
#[tokio::test]
async fn recovery_chunk_timeout_resends_shorter_at_the_same_offset() {
    const LINK_LIMIT: usize = 256 * 1024;
    let size = 200 * 1024 * 1024;
    let patches = Arc::new(Mutex::new(Vec::<(u64, usize)>::new()));
    let p = patches.clone();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => created("sess-slow", CHUNK_SIZE_4MB),
        ("PATCH", "/sess-slow") => {
            let offset: u64 = req.header("X-Capsule-Offset").unwrap().parse().unwrap();
            p.lock().unwrap().push((offset, req.body.len()));
            if req.body.len() > LINK_LIMIT {
                return chunk_timeout();
            }
            let new_offset = ack_offset(req);
            // The first chunk that arrives is enough to prove progress: the
            // server reports every remaining byte held, so the transfer ends.
            let acked = if new_offset == LINK_LIMIT as u64 {
                size
            } else {
                new_offset
            };
            MockResponse::new(204, "No Content").header("X-Capsule-Offset", acked.to_string())
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let client = server.client(PROTOCOL);
    let outcome = client.upload(&request(size), &Zeroes(size)).await.unwrap();

    assert!(matches!(outcome, UploadOutcome::Completed { .. }));
    assert_eq!(
        *patches.lock().unwrap(),
        vec![
            (0, 4 * 1024 * 1024),
            (0, 2 * 1024 * 1024),
            (0, 1024 * 1024),
            (0, 512 * 1024),
            (0, LINK_LIMIT),
        ],
        "each stalled chunk is re-sent at the same offset, half the size"
    );
}

/// A link that stalls on even the 4 KiB protocol minimum
/// fails loudly on the recovery budget rather than re-sending forever.
#[tokio::test]
async fn a_chunk_that_always_times_out_exhausts_the_budget() {
    let patch_count = Arc::new(AtomicUsize::new(0));
    let pc = patch_count.clone();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => created("sess-dead", CHUNK_SIZE_256KB),
        ("PATCH", "/sess-dead") => {
            pc.fetch_add(1, Ordering::SeqCst);
            chunk_timeout()
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let client = server.client(PROTOCOL);
    let data = vec![1u8; 1024 * 1024];
    let err = client
        .upload(&request(data.len() as u64), &data)
        .await
        .unwrap_err();

    assert!(
        matches!(err, UploadError::RetriesExhausted(_)),
        "got {err:?}"
    );
    // Six halvings take 256 KiB to the 4 KiB minimum; those cuts are charged to the budget
    // progress refills, and every re-send at the minimum to the per-transfer one.
    assert_eq!(
        patch_count.load(Ordering::SeqCst),
        6 + MAX_RECOVERIES as usize + 1,
        "every re-send at the minimum is charged to the per-transfer budget"
    );
}

/// **A long upload over a link that slows down again and again.** Every chunk above the
/// protocol minimum times out, so each time the size grows back after a run of landed chunks
/// it is cut down again. Shrinking cuts are charged to a budget each accepted chunk refills, so
/// a transfer that keeps making progress completes however many cuts it takes in total, well
/// past the per-transfer [`MAX_RECOVERIES`] ceiling.
#[tokio::test]
async fn shrinking_cuts_between_accepted_chunks_do_not_spend_the_per_transfer_budget() {
    let cuts = Arc::new(AtomicUsize::new(0));
    let c = cuts.clone();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => created("sess-flaky", CHUNK_SIZE_256KB),
        ("PATCH", "/sess-flaky") => {
            if req.body.len() as u64 > PROTOCOL_MIN_CHUNK {
                c.fetch_add(1, Ordering::SeqCst);
                return chunk_timeout();
            }
            MockResponse::new(204, "No Content")
                .header("X-Capsule-Offset", ack_offset(req).to_string())
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let client = server.client(PROTOCOL);
    let data = vec![2u8; 2 * 1024 * 1024];
    let outcome = client
        .upload(&request(data.len() as u64), &data)
        .await
        .expect("progress refills the shrinking-cut budget");

    assert!(matches!(outcome, UploadOutcome::Completed { .. }));
    assert!(
        cuts.load(Ordering::SeqCst) > MAX_RECOVERIES as usize,
        "the transfer took {} cuts, more than the per-transfer ceiling",
        cuts.load(Ordering::SeqCst)
    );
}

/// A run of shrinking cuts that lands nothing fails loudly once it passes
/// [`MAX_CUTS_SINCE_PROGRESS`], and an accepted chunk refills that budget without refilling
/// the per-transfer one.
#[test]
fn the_shrinking_cut_budget_refills_on_progress_and_bounds_a_run_without_it() {
    let mut recovery = Recovery::new(LaneRetries::new(u32::MAX), None);
    for _ in 0..MAX_CUTS_SINCE_PROGRESS {
        recovery.charge_cut("cut").unwrap();
    }
    recovery.on_progress();
    for _ in 0..MAX_CUTS_SINCE_PROGRESS {
        recovery.charge_cut("cut").unwrap();
    }
    assert!(matches!(
        recovery.charge_cut("cut"),
        Err(UploadError::RetriesExhausted(_))
    ));

    let mut recovery = Recovery::new(LaneRetries::new(u32::MAX), None);
    for _ in 0..MAX_RECOVERIES {
        recovery.charge("re-align").unwrap();
    }
    recovery.on_progress();
    assert!(
        matches!(
            recovery.charge("re-align"),
            Err(UploadError::RetriesExhausted(_))
        ),
        "progress does not refill the per-transfer budget"
    );
}

/// A shrinking cut is a retry to the server lane: it spends from the lane's shared budget, and
/// fails loudly once that is spent even with the cut budget unspent; the accepted chunks that
/// refill the cut budget earn toward the lane's.
#[test]
fn a_shrinking_cut_spends_the_lane_retry_budget() {
    let lane = LaneRetries::new(2);
    let mut recovery = Recovery::new(lane.clone(), None);
    recovery.charge_cut("cut").unwrap();
    recovery.charge_cut("cut").unwrap();
    assert_eq!(lane.remaining(), 0);
    assert!(matches!(
        recovery.charge_cut("cut"),
        Err(UploadError::RetriesExhausted(_))
    ));

    for _ in 0..crate::net::LANE_SUCCESSES_PER_RETRY {
        recovery.on_progress();
    }
    assert_eq!(lane.remaining(), 1, "accepted chunks earn toward the lane");
    recovery.charge_cut("cut").unwrap();
}

/// Bounds short enough to cut a black-holed request inside a test: a 200 ms stall bound and a
/// 400 ms response wait.
fn short_bounds() -> RequestBounds {
    RequestBounds::new(Duration::from_millis(200), Duration::from_millis(400))
}

/// A `HEAD` answer for a session that holds nothing yet, counting how often it was asked.
fn empty_head(heads: &AtomicUsize, size: u64) -> MockResponse {
    heads.fetch_add(1, Ordering::SeqCst);
    MockResponse::new(200, "OK")
        .header("X-Capsule-Offset", "0")
        .header("X-Capsule-Committed", "")
        .header("X-Capsule-Content-Length", size.to_string())
        .header("X-Capsule-Upload-Status", "pending")
}

/// A `HEAD` answer for a session that holds the prefix `[0, held)`, the truth a server that
/// acknowledged every chunk up to `held` reports.
fn prefix_head(held: u64, size: u64) -> MockResponse {
    MockResponse::new(200, "OK")
        .header("X-Capsule-Offset", held.to_string())
        .header(
            "X-Capsule-Committed",
            CommittedSet::prefix(held).to_header(),
        )
        .header("X-Capsule-Content-Length", size.to_string())
        .header("X-Capsule-Upload-Status", "uploading")
}

/// **Black-holed request.** The server reads a whole chunk and then never answers, holding the
/// connection open. Without a bound of its own the client would wait until the OS gave up;
/// with one it cuts the request at the response wait. The answer was lost, so it reads `HEAD`
/// before it sends anything again, then re-sends the same offset, shorter, which lands.
#[tokio::test]
async fn a_request_that_is_never_answered_is_cut_and_resent() {
    let patches = Arc::new(Mutex::new(Vec::<(u64, usize)>::new()));
    let heads = Arc::new(AtomicUsize::new(0));
    let p = patches.clone();
    let h = heads.clone();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => created("sess-hole", CHUNK_SIZE_256KB),
        ("HEAD", "/sess-hole") => empty_head(&h, 64 * 1024),
        ("PATCH", "/sess-hole") => {
            let mut seen = p.lock().unwrap();
            let offset = req.header("X-Capsule-Offset").unwrap().parse().unwrap();
            seen.push((offset, req.body.len()));
            if seen.len() == 1 {
                return MockResponse::hold_open();
            }
            MockResponse::new(204, "No Content")
                .header("X-Capsule-Offset", ack_offset(req).to_string())
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let client = server.client(PROTOCOL).with_request_bounds(short_bounds());
    let data = vec![3u8; 64 * 1024];
    let started = std::time::Instant::now();
    let outcome = tokio::time::timeout(
        Duration::from_secs(20),
        client.upload(&request(data.len() as u64), &data),
    )
    .await
    .expect("the cut ends the black-holed request")
    .unwrap();

    assert!(matches!(outcome, UploadOutcome::Completed { .. }));
    assert_eq!(
        *patches.lock().unwrap(),
        vec![(0, 64 * 1024), (0, 32 * 1024), (32 * 1024, 32 * 1024)],
        "cut, then re-sent at the same offset, half the size"
    );
    assert_eq!(
        heads.load(Ordering::SeqCst),
        1,
        "the lost answer is followed by a HEAD before the re-send"
    );
    assert!(started.elapsed() >= short_bounds().response_wait);
}

/// **A link too slow to drain the buffered tail.** The client hands a whole chunk to the HTTP
/// stack long before it is on the wire, so on a slow enough link the response wait runs out
/// while the tail is still in the send buffers, and the server — which accepts every byte it
/// receives — never got the chance to answer. Here every body larger than the link can drain
/// inside the response wait goes unanswered. The client shrinks on each cut rather than
/// re-sending the same size until its retries run out, and the transfer completes.
#[tokio::test]
async fn a_chunk_too_slow_to_drain_inside_the_response_wait_is_resent_shorter() {
    const DRAINS_IN_TIME: usize = 64 * 1024;
    let patches = Arc::new(Mutex::new(Vec::<(u64, usize)>::new()));
    let heads = Arc::new(AtomicUsize::new(0));
    let p = patches.clone();
    let h = heads.clone();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => created("sess-drain", CHUNK_SIZE_256KB),
        ("HEAD", "/sess-drain") => empty_head(&h, CHUNK_SIZE_256KB),
        ("PATCH", "/sess-drain") => {
            let offset = req.header("X-Capsule-Offset").unwrap().parse().unwrap();
            p.lock().unwrap().push((offset, req.body.len()));
            if req.body.len() > DRAINS_IN_TIME {
                return MockResponse::hold_open();
            }
            MockResponse::new(204, "No Content")
                .header("X-Capsule-Offset", ack_offset(req).to_string())
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let client = server.client(PROTOCOL).with_request_bounds(short_bounds());
    let data = vec![4u8; CHUNK_SIZE_256KB as usize];
    let outcome = tokio::time::timeout(
        Duration::from_secs(30),
        client.upload(&request(data.len() as u64), &data),
    )
    .await
    .expect("the shrinking re-sends converge")
    .expect("a slow but live link is never given up on");

    assert!(matches!(outcome, UploadOutcome::Completed { .. }));
    let patches = patches.lock().unwrap();
    assert_eq!(
        patches[..3],
        [(0, 256 * 1024), (0, 128 * 1024), (0, DRAINS_IN_TIME)],
        "each unanswered chunk is re-sent at the same offset, half the size"
    );
    assert!(
        patches[3..].iter().all(|&(_, len)| len <= DRAINS_IN_TIME),
        "the size holds after the cut: {patches:?}"
    );
    assert_eq!(
        heads.load(Ordering::SeqCst),
        2,
        "every unanswered chunk is followed by a HEAD before anything is re-sent"
    );
}

/// **A ceiling-sized chunk on a link that falls to about 1 KiB/s.** A chunk grown to the
/// 16 MiB ceiling needs twelve halvings to reach what such a link drains inside the response
/// wait. Here the link drains only the protocol minimum until a chunk lands, then recovers.
/// Every cut that shrinks is a stall re-send, not a transport reset, so the eight reset retries
/// are never spent on them and the transfer completes rather than giving up at 64 KiB.
///
/// `HEAD` reports what the server acknowledged. Once the link recovers the chunk grows again,
/// and a slow runner can still miss the response wait on a grown chunk; the server has already
/// accepted it, so the honest answer moves the client on rather than back to offset 0, where
/// the crawl would start over.
#[tokio::test]
async fn a_ceiling_chunk_on_a_link_that_falls_to_a_crawl_shrinks_past_the_reset_budget() {
    const SIZE: u64 = 100 * 1024 * 1024;
    let patches = Arc::new(Mutex::new(Vec::<(u64, usize)>::new()));
    let heads_while_crawling = Arc::new(AtomicUsize::new(0));
    let landed = Arc::new(AtomicU64::new(0));
    let p = patches.clone();
    let h = heads_while_crawling.clone();
    let l = landed.clone();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => created("sess-crawl", CHUNK_SIZE_16MB),
        ("HEAD", "/sess-crawl") => {
            let held = l.load(Ordering::SeqCst);
            if held == 0 {
                h.fetch_add(1, Ordering::SeqCst);
            }
            prefix_head(held, SIZE)
        }
        ("PATCH", "/sess-crawl") => {
            let mut seen = p.lock().unwrap();
            let offset = req.header("X-Capsule-Offset").unwrap().parse().unwrap();
            seen.push((offset, req.body.len()));
            // Slow until the first chunk lands: everything above the minimum goes unanswered.
            if offset == 0 && req.body.len() as u64 > PROTOCOL_MIN_CHUNK {
                return MockResponse::hold_open();
            }
            let acked = ack_offset(req);
            l.fetch_max(acked, Ordering::SeqCst);
            MockResponse::new(204, "No Content").header("X-Capsule-Offset", acked.to_string())
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let client = server.client(PROTOCOL).with_request_bounds(short_bounds());
    // The 16 MiB ceiling is the >= 100 MiB tier's.
    let data = vec![6u8; SIZE as usize];
    let outcome = tokio::time::timeout(
        Duration::from_secs(60),
        client.upload(&request(data.len() as u64), &data),
    )
    .await
    .expect("shrinking cuts take no reset backoff")
    .expect("a cut that shrinks does not spend the reset retries");

    assert!(matches!(outcome, UploadOutcome::Completed { .. }));
    let patches = patches.lock().unwrap();
    let halvings: Vec<(u64, usize)> = (0..=12)
        .map(|n| (0, (CHUNK_SIZE_16MB >> n) as usize))
        .collect();
    assert_eq!(
        patches[..13],
        halvings[..],
        "each unanswered cut is re-sent at the same offset, half the size, down to the minimum"
    );
    assert_eq!(
        patches[13].0, PROTOCOL_MIN_CHUNK,
        "the minimum chunk landed"
    );
    assert_eq!(
        heads_while_crawling.load(Ordering::SeqCst),
        12,
        "each unanswered cut reads HEAD before the shorter re-send"
    );
}

/// **Slow server, live link.** A server that takes longer than the stall bound to write and
/// answer a chunk it has received whole is inside the longer response-wait bound: the chunk is
/// not cut, and nothing is re-sent.
#[tokio::test]
async fn a_slow_answer_inside_the_response_wait_is_not_cut() {
    let patch_count = Arc::new(AtomicUsize::new(0));
    let pc = patch_count.clone();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => created("sess-slow-fsync", CHUNK_SIZE_256KB),
        ("PATCH", "/sess-slow-fsync") => {
            pc.fetch_add(1, Ordering::SeqCst);
            MockResponse::new(204, "No Content")
                .header("X-Capsule-Offset", ack_offset(req).to_string())
                .after(Duration::from_millis(300))
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    // The answer takes 300 ms: past the 100 ms stall bound, inside the 2 s response wait.
    let client = server
        .client(PROTOCOL)
        .with_request_bounds(RequestBounds::new(
            Duration::from_millis(100),
            Duration::from_secs(2),
        ));
    let data = vec![5u8; 64 * 1024];
    let outcome = client
        .upload(&request(data.len() as u64), &data)
        .await
        .unwrap();

    assert!(matches!(outcome, UploadOutcome::Completed { .. }));
    assert_eq!(patch_count.load(Ordering::SeqCst), 1, "nothing was cut");
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

/// A resume via `HEAD` seeds its first chunk from the advice `HEAD` carries, not from the
/// size tier alone.
#[tokio::test]
async fn resume_seeds_the_first_chunk_from_heads_advice() {
    let advised = 512 * KB;
    let size = 600 * KB;
    let patch_offsets = Arc::new(Mutex::new(Vec::<u64>::new()));
    let po = patch_offsets.clone();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("HEAD", "/sess-HA") => MockResponse::new(200, "OK")
            .header("X-Capsule-Offset", "0")
            .header("X-Capsule-Content-Length", size.to_string())
            .header("X-Capsule-Upload-Status", "pending")
            .header(
                "X-Capsule-Advice",
                format!("chunk={advised}, sessions=4, ranges=1, ttl=300"),
            ),
        ("PATCH", "/sess-HA") => {
            let offset: u64 = req.header("X-Capsule-Offset").unwrap().parse().unwrap();
            po.lock().unwrap().push(offset);
            let new_offset = ack_offset(req);
            MockResponse::new(204, "No Content").header("X-Capsule-Offset", new_offset.to_string())
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let data = vec![3u8; size as usize];
    let outcome = server
        .client(PROTOCOL)
        .upload_resuming("sess-HA", &request(size), &data)
        .await;

    assert!(matches!(outcome, Ok(UploadOutcome::Completed { .. })));
    assert_eq!(*patch_offsets.lock().unwrap(), vec![0, advised]);
}

/// A session lost mid-drive is re-created, and the re-created session's first chunk follows
/// the advice its create carried rather than the size tier the drive restarts on.
#[tokio::test]
async fn a_mid_drive_recreate_seeds_the_first_chunk_from_the_new_advice() {
    let advised = 512 * KB;
    let size = 600 * KB;
    let creates = Arc::new(AtomicUsize::new(0));
    let cc = creates.clone();
    let patch_offsets = Arc::new(Mutex::new(Vec::<u64>::new()));
    let po = patch_offsets.clone();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => {
            if cc.fetch_add(1, Ordering::SeqCst) == 0 {
                created("sess-L", CHUNK_SIZE_256KB)
            } else {
                created_without_advice("sess-M", CHUNK_SIZE_256KB).header(
                    "X-Capsule-Advice",
                    format!("chunk={advised}, sessions=4, ranges=1, ttl=300"),
                )
            }
        }
        ("PATCH", "/sess-L") => MockResponse::api_error(
            404,
            "Not Found",
            error_codes::UPLOAD_SESSION_NOT_FOUND,
            "Session not found",
        ),
        ("PATCH", "/sess-M") => {
            let offset: u64 = req.header("X-Capsule-Offset").unwrap().parse().unwrap();
            po.lock().unwrap().push(offset);
            let new_offset = ack_offset(req);
            MockResponse::new(204, "No Content").header("X-Capsule-Offset", new_offset.to_string())
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let data = vec![8u8; size as usize];
    let outcome = server.client(PROTOCOL).upload(&request(size), &data).await;

    assert!(matches!(outcome, Ok(UploadOutcome::Completed { .. })));
    assert_eq!(
        creates.load(Ordering::SeqCst),
        2,
        "the session is re-created"
    );
    assert_eq!(*patch_offsets.lock().unwrap(), vec![0, advised]);
}

// ─── Finalization is read from HEAD ─────────────────────────────────────────

/// A `HEAD` answer for an 8 KiB session in `status`, with the failure code when one is given.
fn head_at_size(status: &str, error: Option<&str>) -> MockResponse {
    let response = MockResponse::new(200, "OK")
        .header("X-Capsule-Offset", "8192")
        .header("X-Capsule-Content-Length", "8192")
        .header("X-Capsule-Upload-Status", status);
    match error {
        Some(code) => response.header("X-Capsule-Upload-Error", code),
        None => response,
    }
}

/// A server that answers the completing chunk `waiting_for_processing` and then reports
/// `states` to successive `HEAD`s, the last of them for every `HEAD` after it. Counts the
/// `HEAD`s it answered.
fn finalizing_server(
    states: Vec<(&'static str, Option<&'static str>)>,
) -> (
    impl Fn(&MockRequest) -> MockResponse + Send + Sync + 'static,
    Arc<AtomicUsize>,
) {
    let heads = Arc::new(AtomicUsize::new(0));
    let seen = heads.clone();
    let handler = move |req: &MockRequest| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => created("sess-fin", CHUNK_SIZE_256KB),
        ("PATCH", "/sess-fin") => {
            let new_offset = ack_offset(req);
            let status = if new_offset == 8192 {
                "waiting_for_processing"
            } else {
                "uploading"
            };
            MockResponse::new(204, "No Content")
                .header("X-Capsule-Offset", new_offset.to_string())
                .header("X-Capsule-Upload-Status", status)
        }
        ("HEAD", "/sess-fin") => {
            let n = seen.fetch_add(1, Ordering::SeqCst);
            let (status, error) = states[n.min(states.len() - 1)];
            head_at_size(status, error)
        }
        other => panic!("unexpected {other:?}"),
    };
    (handler, heads)
}

/// The completing chunk is answered `waiting_for_processing`, so the client polls `HEAD` with
/// backoff until the server reports the session `completed` — and that is the outcome.
#[tokio::test]
async fn a_waiting_completion_polls_head_until_the_session_completes() {
    let (handler, heads) = finalizing_server(vec![
        ("waiting_for_processing", None),
        ("waiting_for_processing", None),
        ("completed", None),
    ]);
    let server = MockServer::start(handler).await;

    let data = vec![1u8; 8192];
    let outcome = server
        .client(PROTOCOL)
        .upload(&request(8192), &data)
        .await
        .unwrap();

    match outcome {
        UploadOutcome::Completed { session_id } => assert_eq!(session_id, "sess-fin"),
        other @ UploadOutcome::AlreadyStored { .. } => panic!("expected Completed, got {other:?}"),
    }
    assert_eq!(
        heads.load(Ordering::SeqCst),
        3,
        "polled until the terminal state"
    );
}

/// A session finalization refused is the refusal, carrying the code `HEAD` reported in
/// `X-Capsule-Upload-Error` — the code a caller switches on, as it did when the completing chunk
/// itself was refused.
#[tokio::test]
async fn a_failed_finalization_is_the_rejection_head_names() {
    let (handler, heads) = finalizing_server(vec![
        ("waiting_for_processing", None),
        (
            "failed_processing",
            Some(error_codes::UPLOAD_ENVELOPE_REJECTED),
        ),
    ]);
    let server = MockServer::start(handler).await;

    let data = vec![2u8; 8192];
    let error = server
        .client(PROTOCOL)
        .upload(&request(8192), &data)
        .await
        .expect_err("a refused finalization fails the upload");

    match error {
        UploadError::Rejected { code, .. } => {
            assert_eq!(code.as_deref(), Some(error_codes::UPLOAD_ENVELOPE_REJECTED));
        }
        other => panic!("expected the finalization's rejection, got {other:?}"),
    }
    assert_eq!(heads.load(Ordering::SeqCst), 2);
}

/// A source whose identity check reports a change from the `n`th check on (1-based); `None`
/// never does.
struct ChangingSource {
    bytes: Vec<u8>,
    changes_at: Option<usize>,
    checks: AtomicUsize,
}

impl ChangingSource {
    fn new(bytes: Vec<u8>, changes_at: Option<usize>) -> Self {
        Self {
            bytes,
            changes_at,
            checks: AtomicUsize::new(0),
        }
    }
}

impl UploadSource for ChangingSource {
    fn read_at(&self, offset: u64, len: usize) -> std::io::Result<Cow<'_, [u8]>> {
        self.bytes.read_at(offset, len)
    }

    fn check_unchanged(&self) -> Result<SourceIdentity, SourceChanged> {
        let n = self.checks.fetch_add(1, Ordering::SeqCst) + 1;
        match self.changes_at {
            Some(at) if n >= at => Err(SourceChanged::new("the file's modification time moved")),
            _ => Ok(SourceIdentity::Affirmed),
        }
    }
}

/// **`content_hash_mismatch`, source unchanged.** Finalization refused the hash, but the source
/// is still what the bundle was built from, so the session is re-created once and the blob sent
/// again; a second refusal is the rejection.
#[tokio::test]
async fn a_refused_hash_of_an_unchanged_source_recreates_the_session_once() {
    let creates = Arc::new(AtomicUsize::new(0));
    let c = creates.clone();
    let (inner, heads) = finalizing_server(vec![(
        "failed_processing",
        Some(error_codes::UPLOAD_CONTENT_HASH_MISMATCH),
    )]);
    let server = MockServer::start(move |req: &MockRequest| {
        if req.method == "POST" {
            c.fetch_add(1, Ordering::SeqCst);
        }
        inner(req)
    })
    .await;

    let data = ChangingSource::new(vec![2u8; 8192], None);
    let error = server
        .client(PROTOCOL)
        .upload(&request(8192), &data)
        .await
        .expect_err("a second refusal fails the upload");

    assert_eq!(
        error.error_code(),
        Some(error_codes::UPLOAD_CONTENT_HASH_MISMATCH)
    );
    assert_eq!(creates.load(Ordering::SeqCst), 2, "re-created exactly once");
    assert_eq!(
        heads.load(Ordering::SeqCst),
        2,
        "each session's outcome read once"
    );
}

/// **`content_hash_mismatch`, source changed.** The source no longer matches its bundle, so the
/// transfer ends with the typed source-changed error and its client-side code, and nothing is
/// re-created.
#[tokio::test]
async fn a_refused_hash_of_a_changed_source_is_the_typed_source_change() {
    let creates = Arc::new(AtomicUsize::new(0));
    let c = creates.clone();
    let (inner, _heads) = finalizing_server(vec![(
        "failed_processing",
        Some(error_codes::UPLOAD_CONTENT_HASH_MISMATCH),
    )]);
    let server = MockServer::start(move |req: &MockRequest| {
        if req.method == "POST" {
            c.fetch_add(1, Ordering::SeqCst);
        }
        inner(req)
    })
    .await;

    // The check before the only window passes; the one after the refusal finds the change.
    let data = ChangingSource::new(vec![2u8; 8192], Some(2));
    let error = server
        .client(PROTOCOL)
        .upload(&request(8192), &data)
        .await
        .expect_err("a changed source fails the upload");

    assert!(
        matches!(error, UploadError::SourceChanged(_)),
        "typed: {error:?}"
    );
    assert_eq!(error.error_code(), Some(error_codes::UPLOAD_SOURCE_CHANGED));
    assert_eq!(creates.load(Ordering::SeqCst), 1, "nothing re-created");
}

/// **`content_hash_mismatch`, identity not affirmable.** A source that finds no change but
/// cannot affirm there was none (every original re-derived from a file on disk, today) gets the
/// refusal: re-sending would seal bytes that may differ under the nonces already spent, so
/// nothing is re-created and nothing sent again.
#[tokio::test]
async fn a_refused_hash_of_a_source_that_cannot_affirm_its_identity_sends_nothing_again() {
    let creates = Arc::new(AtomicUsize::new(0));
    let patches = Arc::new(AtomicUsize::new(0));
    let (c, p) = (creates.clone(), patches.clone());
    let (inner, heads) = finalizing_server(vec![(
        "failed_processing",
        Some(error_codes::UPLOAD_CONTENT_HASH_MISMATCH),
    )]);
    let server = MockServer::start(move |req: &MockRequest| {
        match req.method.as_str() {
            "POST" => c.fetch_add(1, Ordering::SeqCst),
            "PATCH" => p.fetch_add(1, Ordering::SeqCst),
            _ => 0,
        };
        inner(req)
    })
    .await;

    // `Unaffirmed` keeps the default identity check: no change found, none affirmed.
    let data = Unaffirmed(8192);
    assert_eq!(data.check_unchanged(), Ok(SourceIdentity::Unknown));
    let error = server
        .client(PROTOCOL)
        .upload(&request(8192), &data)
        .await
        .expect_err("the refusal ends the upload");

    assert_eq!(
        error.error_code(),
        Some(error_codes::UPLOAD_CONTENT_HASH_MISMATCH)
    );
    assert_eq!(creates.load(Ordering::SeqCst), 1, "nothing re-created");
    assert_eq!(patches.load(Ordering::SeqCst), 1, "nothing sent again");
    assert_eq!(heads.load(Ordering::SeqCst), 1, "the outcome read once");
}

/// Bytes held in memory cannot change, so they affirm their identity.
#[test]
fn in_memory_sources_affirm_their_identity() {
    let bytes = vec![1u8; 4];
    assert_eq!(bytes.check_unchanged(), Ok(SourceIdentity::Affirmed));
    assert_eq!(
        bytes.as_slice().check_unchanged(),
        Ok(SourceIdentity::Affirmed)
    );
    assert_eq!([0u8; 3].check_unchanged(), Ok(SourceIdentity::Affirmed));
    // A borrowed source answers for what it borrows.
    fn through<S: UploadSource>(source: S) -> Result<SourceIdentity, SourceChanged> {
        source.check_unchanged()
    }
    assert_eq!(through(&bytes), Ok(SourceIdentity::Affirmed));
    assert_eq!(
        through::<&Unaffirmed>(&Unaffirmed(1)),
        Ok(SourceIdentity::Unknown)
    );
}

/// **A server that keeps losing a range it acknowledged.** Every answer covers the blob, but
/// every `HEAD` says the server holds nothing: each resume is charged to the recovery budget,
/// so the transfer ends exhausted instead of re-sending forever.
#[tokio::test]
async fn a_server_that_keeps_losing_ranges_exhausts_the_recovery_budget() {
    let patches = Arc::new(AtomicUsize::new(0));
    let p = patches.clone();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => created("sess-lose", CHUNK_SIZE_256KB),
        ("PATCH", "/sess-lose") => {
            p.fetch_add(1, Ordering::SeqCst);
            MockResponse::new(204, "No Content")
                .header("X-Capsule-Offset", ack_offset(req).to_string())
                .header("X-Capsule-Upload-Status", "uploading")
        }
        ("HEAD", "/sess-lose") => MockResponse::new(200, "OK")
            .header("X-Capsule-Offset", "0")
            .header("X-Capsule-Content-Length", "8192")
            .header("X-Capsule-Upload-Status", "uploading"),
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let data = vec![3u8; 8192];
    let error = server
        .client(PROTOCOL)
        .upload(&request(8192), &data)
        .await
        .expect_err("a server that never keeps the blob ends the upload");

    assert!(
        matches!(error, UploadError::RetriesExhausted(_)),
        "{error:?}"
    );
    let sent = patches.load(Ordering::SeqCst);
    assert!(
        sent >= 2 && sent <= MAX_RECOVERIES as usize + 1,
        "re-sent within the budget, not forever: {sent}"
    );
}

/// **A `HEAD` that drops ranges the server acknowledged, mid-transfer.** Chunks up to 1 MiB are
/// accepted, larger ones go unanswered, and every `HEAD` says the server holds nothing. Each
/// accepted chunk refills the budgets progress refills, so only the per-transfer recovery
/// budget can end this: every `HEAD` below what was acknowledged is charged to it, and the
/// transfer ends exhausted within a bounded number of `HEAD`s instead of restarting from 0
/// forever. Time is paused, so the response waits are not slept.
#[tokio::test(start_paused = true)]
async fn a_head_below_what_the_server_acknowledged_exhausts_the_recovery_budget() {
    const SIZE: u64 = 64 * 1024 * 1024;
    const ANSWERED: usize = 1024 * 1024;
    const HEAD_BOUND: usize = 2 * MAX_RECOVERIES as usize;
    let heads = Arc::new(AtomicUsize::new(0));
    let accepted = Arc::new(AtomicUsize::new(0));
    let h = heads.clone();
    let a = accepted.clone();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => created("sess-forget", CHUNK_SIZE_16MB),
        // Past the bound the server reports the blob finalized, so a client that is never
        // charged for the dropped ranges completes and fails the test instead of hanging it.
        ("HEAD", "/sess-forget") if h.load(Ordering::SeqCst) >= HEAD_BOUND => {
            MockResponse::new(200, "OK")
                .header("X-Capsule-Offset", SIZE.to_string())
                .header("X-Capsule-Content-Length", SIZE.to_string())
                .header("X-Capsule-Upload-Status", "completed")
        }
        ("HEAD", "/sess-forget") => empty_head(&h, SIZE),
        ("PATCH", "/sess-forget") => {
            if req.body.len() > ANSWERED {
                return MockResponse::hold_open();
            }
            a.fetch_add(1, Ordering::SeqCst);
            MockResponse::new(204, "No Content")
                .header("X-Capsule-Offset", ack_offset(req).to_string())
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let client = server.client(PROTOCOL).with_request_bounds(short_bounds());
    let data = vec![8u8; SIZE as usize];
    let error = tokio::time::timeout(
        Duration::from_hours(1),
        client.upload(&request(SIZE), &data),
    )
    .await
    .expect("the transfer ends rather than restarting forever")
    .expect_err("a server that keeps forgetting what it acknowledged ends the upload");

    assert!(
        matches!(error, UploadError::RetriesExhausted(_)),
        "{error:?}"
    );
    let heads = heads.load(Ordering::SeqCst);
    assert!(
        accepted.load(Ordering::SeqCst) > 0,
        "the server acknowledged ranges before it dropped them"
    );
    assert!(
        heads < HEAD_BOUND,
        "each HEAD below what was acknowledged is charged: {heads} HEADs"
    );
}

/// **A source that changes mid-upload.** The identity check before a window's read finds the
/// change: nothing of that window is sent, the session is cancelled, and the transfer ends with
/// the typed error.
#[tokio::test]
async fn a_source_that_changes_before_a_window_is_read_stops_and_cancels_the_session() {
    let patches = Arc::new(Mutex::new(Vec::<u64>::new()));
    let deletes = Arc::new(AtomicUsize::new(0));
    let p = patches.clone();
    let d = deletes.clone();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => created("sess-chg", CHUNK_SIZE_256KB),
        ("PATCH", "/sess-chg") => {
            p.lock()
                .unwrap()
                .push(req.header("X-Capsule-Offset").unwrap().parse().unwrap());
            MockResponse::new(204, "No Content")
                .header("X-Capsule-Offset", ack_offset(req).to_string())
        }
        ("DELETE", "/sess-chg") => {
            d.fetch_add(1, Ordering::SeqCst);
            MockResponse::new(204, "No Content")
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let size = 3 * CHUNK_SIZE_256KB;
    let data = ChangingSource::new(vec![9u8; size as usize], Some(2));
    let error = server
        .client(PROTOCOL)
        .upload(&request(size), &data)
        .await
        .expect_err("a changed source fails the upload");

    assert!(matches!(error, UploadError::SourceChanged(_)), "{error:?}");
    assert_eq!(
        *patches.lock().unwrap(),
        vec![0],
        "nothing after the change is sent"
    );
    assert_eq!(
        deletes.load(Ordering::SeqCst),
        1,
        "the session is cancelled"
    );
}

/// A source that finds a re-derived window differs from what it first sealed refuses the read
/// with [`SourceChanged`] as the error's payload; the transfer ends typed, not as an I/O error.
#[tokio::test]
async fn a_read_refused_as_a_source_change_is_typed() {
    struct TagMismatch;
    impl UploadSource for TagMismatch {
        fn read_at(&self, _: u64, _: usize) -> std::io::Result<Cow<'_, [u8]>> {
            Err(std::io::Error::other(SourceChanged::new(
                "a re-derived STREAM chunk's tag differs from the one recorded",
            )))
        }
    }
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => created("sess-tag", CHUNK_SIZE_256KB),
        ("DELETE", "/sess-tag") => MockResponse::new(204, "No Content"),
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let error = server
        .client(PROTOCOL)
        .upload(&request(8192), &TagMismatch)
        .await
        .expect_err("a refused read fails the upload");
    match error {
        UploadError::SourceChanged(changed) => assert!(changed.reason.contains("tag")),
        other => panic!("expected a typed source change, got {other:?}"),
    }

    // Any other read failure stays an I/O error.
    let plain = UploadError::of_read(std::io::Error::other("disk on fire"));
    assert!(matches!(plain, UploadError::Io(_)), "{plain:?}");
}

/// A duplicate create that hands back a full session `waiting_for_processing` sends nothing more
/// and reads the outcome from `HEAD`.
#[tokio::test]
async fn a_resumed_full_session_waiting_for_processing_is_polled_not_resent() {
    let heads = Arc::new(AtomicUsize::new(0));
    let seen = heads.clone();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => MockResponse::new(200, "OK")
            .header("Location", "/upload/sess-full")
            .header("X-Capsule-Offset", "8192")
            .header("X-Capsule-Upload-Status", "waiting_for_processing")
            .json_body(format!(
                r#"{{"id":"sess-full","upload_url":"/upload/sess-full","suggested_chunk_size":{CHUNK_SIZE_256KB}}}"#
            )),
        ("HEAD", "/sess-full") => {
            seen.fetch_add(1, Ordering::SeqCst);
            head_at_size("completed", None)
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let data = vec![3u8; 8192];
    let outcome = server
        .client(PROTOCOL)
        .upload(&request(8192), &data)
        .await
        .unwrap();

    assert!(matches!(outcome, UploadOutcome::Completed { .. }));
    assert_eq!(heads.load(Ordering::SeqCst), 1);
}

/// A server that finalizes before it answers sends no state on the completing chunk; its
/// accepted chunk is the outcome and nothing is polled. Every mock above that answers no `HEAD`
/// is such a server; this one says so.
#[tokio::test]
async fn a_server_that_sends_no_state_is_not_polled() {
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => created("sess-old", CHUNK_SIZE_256KB),
        ("PATCH", "/sess-old") => MockResponse::new(204, "No Content")
            .header("X-Capsule-Offset", ack_offset(req).to_string()),
        other => panic!("an older server is never asked {other:?}"),
    })
    .await;

    let data = vec![4u8; 4096];
    let outcome = server
        .client(PROTOCOL)
        .upload(&request(4096), &data)
        .await
        .unwrap();
    assert!(matches!(outcome, UploadOutcome::Completed { .. }));
}

/// Against a server that stays `waiting_for_processing`, the poll's waits start at 100 ms and
/// double up to their 5 s ceiling, and the poll gives up with `WaitsExhausted` — a slow server,
/// not a stuck link — once their sum reaches the 30-minute deadline rather than polling a
/// server that never finishes. Time is
/// paused, so the waits are read off the clock the `HEAD`s arrive at, not slept.
#[tokio::test(start_paused = true)]
async fn the_finalization_poll_backs_off_and_is_bounded() {
    let arrivals = Arc::new(Mutex::new(Vec::new()));
    let seen = arrivals.clone();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => created("sess-stuck", CHUNK_SIZE_256KB),
        ("PATCH", "/sess-stuck") => MockResponse::new(204, "No Content")
            .header("X-Capsule-Offset", ack_offset(req).to_string())
            .header("X-Capsule-Upload-Status", "waiting_for_processing"),
        ("HEAD", "/sess-stuck") => {
            seen.lock()
                .expect("arrivals")
                .push(tokio::time::Instant::now());
            head_at_size("waiting_for_processing", None)
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let data = vec![5u8; 8192];
    let error = server
        .client(PROTOCOL)
        .upload(&request(8192), &data)
        .await
        .expect_err("a finalization that never ends fails the upload");
    assert!(
        matches!(error, UploadError::WaitsExhausted { status: 200, .. }),
        "expected the poll to give up waiting, got {error:?}"
    );

    let arrivals = arrivals.lock().expect("arrivals");
    let waits: Vec<Duration> = arrivals
        .windows(2)
        .map(|pair| pair[1].duration_since(pair[0]))
        .collect();
    let ms = Duration::from_millis;
    assert_eq!(
        waits[..7],
        [
            ms(100),
            ms(200),
            ms(400),
            ms(800),
            ms(1600),
            ms(3200),
            ms(5000)
        ],
        "the waits double from 100 ms to the 5 s ceiling"
    );
    assert!(
        waits[7..]
            .iter()
            .all(|wait| *wait == Duration::from_secs(5)),
        "the waits hold at the ceiling"
    );
    let total: Duration = waits.iter().sum();
    let deadline = Duration::from_mins(30);
    assert!(
        total >= deadline,
        "gave up after {total:?}, before the deadline"
    );
    assert!(
        total.saturating_sub(waits[waits.len() - 1]) < deadline,
        "polled again after the deadline had passed ({total:?})"
    );
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
    assert_eq!(info.error_code, None);
}

#[tokio::test]
async fn head_reports_a_failed_sessions_error_code() {
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("HEAD", "/s1") => head_at_size(
            "failed_processing",
            Some(error_codes::UPLOAD_ENVELOPE_REJECTED),
        ),
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let info = server.client(PROTOCOL).head("s1").await.unwrap().unwrap();
    assert_eq!(info.status, "failed_processing");
    assert_eq!(
        info.error_code.as_deref(),
        Some(error_codes::UPLOAD_ENVELOPE_REJECTED)
    );
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
            r#"{"sessions":[{"id":"s1","asset_id":"a1","received_bytes":100,"total_size":200,"status":"Uploading","extra_unknown":true},{"id":"s2","received_bytes":200,"total_size":200,"status":"failed_processing","error_code":"error.upload.stale_revival"}]}"#,
        ),
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let rows = server.client(PROTOCOL).list_sessions().await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, "s1");
    assert_eq!(rows[0].received_bytes, 100);
    assert_eq!(rows[0].status, "Uploading");
    assert_eq!(rows[0].error_code, None);
    assert_eq!(rows[1].status, "failed_processing");
    assert_eq!(
        rows[1].error_code.as_deref(),
        Some("error.upload.stale_revival")
    );
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

// ─── The lane's shared retry budget and the transfer gate ───────────────────

/// **A failing server costs at most the lane's retry budget.** Every transfer to one server
/// spends from one shared budget, so once a server that only ever asks for a wait has taken the
/// lane's three retries, the next transfer gives up at its first refusal instead of taking a
/// full recovery ladder of its own.
#[tokio::test]
async fn transfers_to_one_server_share_its_lane_retry_budget() {
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

    let lane = LaneRetries::new(3);
    let client = server.client(PROTOCOL).with_lane_retries(lane.clone());
    assert!(client.lane_retries().same_as(&lane));
    let first = client.upload(&request(4096), &[9u8; 4096]).await;
    let first = first.unwrap_err();
    assert!(
        matches!(first, UploadError::WaitsExhausted { status: 503, .. }),
        "{first:?}"
    );
    assert_eq!(first.error_code(), Some(error_codes::UPLOAD_UNAVAILABLE));
    assert_eq!(
        creates.load(Ordering::SeqCst),
        4,
        "one try and three retries"
    );

    let second = client.clone().upload(&request(4096), &[9u8; 4096]).await;
    let second = second.unwrap_err();
    assert!(
        matches!(second, UploadError::WaitsExhausted { status: 503, .. }),
        "{second:?}"
    );
    assert_eq!(second.error_code(), Some(error_codes::UPLOAD_UNAVAILABLE));
    assert_eq!(
        creates.load(Ordering::SeqCst),
        5,
        "the second transfer to the same server got no retry at all"
    );
    assert_eq!(lane.remaining(), 0);
}

/// Accepted chunks earn the lane's retries back, so a server that recovers is retried again.
#[tokio::test]
async fn accepted_chunks_earn_the_lane_budget_back() {
    let (handler, _) = recording_server(created("sess-earn", CHUNK_SIZE_256KB), "sess-earn");
    let server = MockServer::start(handler).await;
    let lane = LaneRetries::new(2);
    assert!(lane.try_spend());
    assert!(lane.try_spend());
    let client = server.client(PROTOCOL).with_lane_retries(lane.clone());
    // An answered create and four chunks of 256 KiB: two earned retries at one per two
    // successes.
    let size = 4 * CHUNK_SIZE_256KB;
    let data = vec![1u8; size as usize];
    client.upload(&request(size), &data).await.unwrap();
    assert_eq!(lane.remaining(), 2);
}

/// **A server that paces creates does not drain the lane.** Every create is answered `429`
/// with a `Retry-After` once before it is accepted; the answered create earns toward the lane
/// as its accepted chunk does, so a one-retry lane honours the wait on every one of many
/// single-chunk transfers instead of failing them once its first token is spent.
#[tokio::test]
async fn answered_creates_earn_the_lane_budget_back() {
    let creates = Arc::new(AtomicUsize::new(0));
    let cr = creates.clone();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => {
            if cr.fetch_add(1, Ordering::SeqCst).is_multiple_of(2) {
                MockResponse::api_error(
                    429,
                    "Too Many Requests",
                    error_codes::UPLOAD_RATE_LIMITED,
                    "wait",
                )
                .header("Retry-After", "0")
            } else {
                created("sess-paced", CHUNK_SIZE_256KB)
            }
        }
        ("PATCH", "/sess-paced") => {
            let new_offset = ack_offset(req);
            MockResponse::new(204, "No Content").header("X-Capsule-Offset", new_offset.to_string())
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let lane = LaneRetries::new(1);
    let client = server.client(PROTOCOL).with_lane_retries(lane.clone());
    for item in 0..10 {
        let outcome = client.upload(&request(4096), &[9u8; 4096]).await;
        assert!(
            matches!(outcome, Ok(UploadOutcome::Completed { .. })),
            "item {item}: {outcome:?}"
        );
    }
    assert_eq!(
        creates.load(Ordering::SeqCst),
        20,
        "one wait and one create each"
    );
    assert_eq!(lane.remaining(), 1);
}

/// What a recording gate saw, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
enum GateEvent {
    Reserved(u64),
    Released,
    Advised(Option<u32>),
}

/// A gate that reserves every range at once and records what the transfer asked of it.
struct RecordingGate {
    queue: Mutex<Option<(queue::UploadQueue<()>, queue::GrantId)>>,
    events: Mutex<Vec<GateEvent>>,
}

impl RecordingGate {
    fn new() -> Self {
        let mut queue = queue::UploadQueue::new(1 << 30, Duration::from_secs(30));
        let server = queue::ServerKey::new("https://gate.test");
        queue.enqueue(
            &server,
            queue::QueuedJob::new(capsule_library::import::UploadTier::Original, ()),
        );
        let grant = queue.next(std::time::Instant::now()).unwrap();
        Self {
            queue: Mutex::new(Some((queue, grant.id))),
            events: Mutex::default(),
        }
    }

    fn in_use(&self) -> u64 {
        self.queue.lock().unwrap().as_ref().unwrap().0.in_use()
    }
}

impl TransferGate for RecordingGate {
    fn reserve(
        &self,
        len: u64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<RangeId>> + Send + '_>> {
        let mut held = self.queue.lock().unwrap();
        let (queue, grant) = held.as_mut().unwrap();
        let range = queue
            .reserve_range(*grant, len, std::time::Instant::now())
            .expect("one range at a time fits");
        self.events.lock().unwrap().push(GateEvent::Reserved(len));
        Box::pin(std::future::ready(Some(range)))
    }

    fn release(&self, range: RangeId) {
        let mut held = self.queue.lock().unwrap();
        let (queue, _) = held.as_mut().unwrap();
        assert!(queue.release_range(range, std::time::Instant::now()));
        self.events.lock().unwrap().push(GateEvent::Released);
    }

    fn advised(&self, advice: &ServerAdvice) {
        self.events.lock().unwrap().push(GateEvent::Advised(Some(
            advice.sessions_window(16, Duration::ZERO),
        )));
    }
}

/// **Every range reserves its length while in flight, and only then.** A gated transfer
/// reserves each chunk before reading it, gives it back once its request is answered, holds
/// one range at a time, and reports the advice its create carried.
#[tokio::test]
async fn a_gated_transfer_reserves_each_range_while_it_is_in_flight() {
    let (handler, offsets) = recording_server(created("sess-gate", CHUNK_SIZE_256KB), "sess-gate");
    let server = MockServer::start(handler).await;
    let gate = RecordingGate::new();
    let size = 300 * 1024;
    let data = vec![4u8; size as usize];
    let outcome = server
        .client(PROTOCOL)
        .upload_gated(&request(size), &data, &gate)
        .await
        .unwrap();
    assert!(matches!(outcome, UploadOutcome::Completed { .. }));
    assert_eq!(*offsets.lock().unwrap(), vec![0, CHUNK_SIZE_256KB]);
    assert_eq!(
        *gate.events.lock().unwrap(),
        vec![
            GateEvent::Advised(Some(4)),
            GateEvent::Reserved(CHUNK_SIZE_256KB),
            GateEvent::Released,
            GateEvent::Reserved(size - CHUNK_SIZE_256KB),
            GateEvent::Released,
        ]
    );
    assert_eq!(
        gate.in_use(),
        0,
        "nothing is held once the transfer is done"
    );
}

/// A range whose request fails still gives its bytes back.
#[tokio::test]
async fn a_refused_range_gives_its_reservation_back() {
    let server = MockServer::start(|req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => created("sess-refused", CHUNK_SIZE_256KB),
        ("PATCH", "/sess-refused") => MockResponse::api_error(
            403,
            "Forbidden",
            error_codes::UPLOAD_ALBUM_ACCESS_DENIED,
            "no",
        ),
        other => panic!("unexpected {other:?}"),
    })
    .await;
    let gate = RecordingGate::new();
    let err = server
        .client(PROTOCOL)
        .upload_gated(&request(4096), &[0u8; 4096], &gate)
        .await
        .unwrap_err();
    assert!(
        matches!(err, UploadError::Rejected { status: 403, .. }),
        "{err:?}"
    );
    assert_eq!(gate.in_use(), 0);
    assert_eq!(
        gate.events.lock().unwrap().last(),
        Some(&GateEvent::Released)
    );
}

// ─── The range scheduler on the block grid ───────────────────────────────────

/// What a [`Grid`] script may do with one `PATCH`: answer it, and change what the server holds
/// first.
type Script = dyn Fn(usize, (u64, u64), &mut CommittedSet) -> Option<MockResponse> + Send + Sync;

/// A block-grid server for one session `sess-grid`: it commits each range it is sent and answers
/// `204` with the committed prefix, the committed set and the session's state, after `delay`.
/// `script(n, range, held)` may answer the `n`th `PATCH` (1-based) instead, and may change what
/// the server holds. `HEAD` reports the committed set, `completed` once it is full.
struct Grid {
    size: u64,
    advice: &'static str,
    delay: Duration,
    held: Mutex<CommittedSet>,
    /// Every `PATCH` as (offset, length, when it arrived).
    patches: Mutex<Vec<(u64, u64, std::time::Instant)>>,
    /// Every request's method, in arrival order.
    methods: Mutex<Vec<&'static str>>,
    script: Box<Script>,
}

impl Grid {
    fn new(size: u64, advice: &'static str, delay: Duration, script: Box<Script>) -> Arc<Self> {
        Arc::new(Self {
            size,
            advice,
            delay,
            held: Mutex::new(CommittedSet::empty()),
            patches: Mutex::new(Vec::new()),
            methods: Mutex::new(Vec::new()),
            script,
        })
    }

    fn progress(&self, response: MockResponse, held: &CommittedSet) -> MockResponse {
        let status = if held.covers(self.size) {
            "waiting_for_processing"
        } else {
            "uploading"
        };
        response
            .header("X-Capsule-Offset", held.prefix_end().to_string())
            .header("X-Capsule-Committed", held.to_header())
            .header("X-Capsule-Upload-Status", status)
    }

    fn answer(&self, req: &MockRequest) -> MockResponse {
        match (req.method.as_str(), req.path.as_str()) {
            ("POST", "/") => {
                self.methods.lock().unwrap().push("POST");
                created_without_advice("sess-grid", CHUNK_SIZE_256KB)
                    .header("X-Capsule-Advice", self.advice)
            }
            ("HEAD", "/sess-grid") => {
                self.methods.lock().unwrap().push("HEAD");
                let held = self.held.lock().unwrap();
                let status = if held.covers(self.size) {
                    "completed"
                } else {
                    "uploading"
                };
                MockResponse::new(200, "OK")
                    .header("X-Capsule-Offset", held.prefix_end().to_string())
                    .header("X-Capsule-Committed", held.to_header())
                    .header("X-Capsule-Content-Length", self.size.to_string())
                    .header("X-Capsule-Upload-Status", status)
                    .header("X-Capsule-Advice", self.advice)
            }
            ("PATCH", "/sess-grid") => {
                self.methods.lock().unwrap().push("PATCH");
                let end = ack_offset(req);
                let offset = end - req.body.len() as u64;
                let n = {
                    let mut patches = self.patches.lock().unwrap();
                    patches.push((offset, end - offset, std::time::Instant::now()));
                    patches.len()
                };
                let mut held = self.held.lock().unwrap();
                if let Some(answer) = (self.script)(n, (offset, end), &mut held) {
                    return answer;
                }
                assert!(
                    !held.contains(offset, end) || held.is_empty(),
                    "the client re-sent [{offset}, {end}), which it was told is committed"
                );
                held.insert(offset, end);
                self.progress(MockResponse::new(204, "No Content"), &held)
                    .after(self.delay)
            }
            ("DELETE", "/sess-grid") => {
                self.methods.lock().unwrap().push("DELETE");
                MockResponse::new(204, "No Content")
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    fn offsets(&self) -> Vec<u64> {
        self.patches.lock().unwrap().iter().map(|p| p.0).collect()
    }

    fn heads(&self) -> usize {
        self.methods
            .lock()
            .unwrap()
            .iter()
            .filter(|m| **m == "HEAD")
            .count()
    }
}

async fn grid_server(grid: &Arc<Grid>) -> MockServer {
    let grid = grid.clone();
    MockServer::start(move |req: &MockRequest| grid.answer(req)).await
}

const C: u64 = CHUNK_SIZE_256KB;

/// **P ranges at once, lowest gap first.** With a window of four, the first four ranges are in
/// flight together — the four lowest — and every byte lands.
#[tokio::test]
async fn a_window_of_four_sends_the_four_lowest_ranges_at_once() {
    let size = 8 * C;
    let grid = Grid::new(
        size,
        "chunk=262144, sessions=4, ranges=4, ttl=300",
        Duration::from_millis(300),
        Box::new(|_, _, _| None),
    );
    let server = grid_server(&grid).await;
    let data = vec![1u8; size as usize];
    let outcome = server
        .client(PROTOCOL)
        .with_ranges_window(4)
        .upload(&request(size), &data)
        .await
        .unwrap();

    assert!(matches!(outcome, UploadOutcome::Completed { .. }));
    let patches = grid.patches.lock().unwrap().clone();
    assert_eq!(patches.len(), 8, "every range once: {patches:?}");
    let mut first: Vec<u64> = patches[..4].iter().map(|p| p.0).collect();
    first.sort_unstable();
    assert_eq!(
        first,
        vec![0, C, 2 * C, 3 * C],
        "the four lowest gaps first"
    );
    assert!(
        patches[3].2 - patches[0].2 < Duration::from_millis(250),
        "the first four were in flight together"
    );
    assert!(grid.held.lock().unwrap().covers(size));
}

/// The session's advertised `ranges` narrows the client's own window.
#[tokio::test]
async fn the_sessions_advertised_ranges_narrow_the_window() {
    let size = 4 * C;
    let grid = Grid::new(
        size,
        "chunk=262144, sessions=4, ranges=2, ttl=300",
        Duration::from_millis(300),
        Box::new(|_, _, _| None),
    );
    let server = grid_server(&grid).await;
    let data = vec![1u8; size as usize];
    server
        .client(PROTOCOL)
        .with_ranges_window(4)
        .upload(&request(size), &data)
        .await
        .unwrap();

    let patches = grid.patches.lock().unwrap().clone();
    assert_eq!(patches.len(), 4);
    assert!(
        patches[2].2 - patches[0].2 >= Duration::from_millis(250),
        "the third range waited for one of the first two: {patches:?}"
    );
}

/// The window is clamped into the protocol's bounds.
#[test]
fn the_client_window_is_clamped_into_the_protocol_bounds() {
    let transport = UploadTransport::with_static_token(
        reqwest::Client::new(),
        "http://localhost",
        PROTOCOL,
        StaticToken("t".into()),
    );
    let client = UploadClient::new(transport);
    assert_eq!(client.ranges_window(), DEFAULT_CLIENT_RANGES);
    assert_eq!(client.clone().with_ranges_window(0).ranges_window(), 1);
    assert_eq!(
        client.with_ranges_window(99).ranges_window(),
        MAX_RANGES_WINDOW
    );
}

/// The default window is the lane's P: a client left as built sends a source that verifies
/// what it re-derives four ranges at once.
#[tokio::test]
async fn the_default_window_sends_a_verifying_source_in_parallel() {
    assert_eq!(DEFAULT_CLIENT_RANGES, 4);
    let size = 8 * C;
    let grid = Grid::new(
        size,
        "chunk=262144, sessions=4, ranges=4, ttl=300",
        Duration::from_millis(300),
        Box::new(|_, _, _| None),
    );
    let server = grid_server(&grid).await;
    let data = vec![1u8; size as usize];
    assert!(data.verifies_rederived());
    server
        .client(PROTOCOL)
        .upload(&request(size), &data)
        .await
        .unwrap();

    let patches = grid.patches.lock().unwrap().clone();
    assert_eq!(patches.len(), 8, "every range once: {patches:?}");
    assert!(
        patches[3].2 - patches[0].2 < Duration::from_millis(250),
        "the first four were in flight together: {patches:?}"
    );
}

/// Bytes behind a source that affirms its identity but does not verify what it re-derives.
struct UnverifiedBytes(Vec<u8>);

impl UploadSource for UnverifiedBytes {
    fn read_at(&self, offset: u64, len: usize) -> std::io::Result<Cow<'_, [u8]>> {
        self.0.read_at(offset, len)
    }

    fn check_unchanged(&self) -> Result<SourceIdentity, SourceChanged> {
        Ok(SourceIdentity::Affirmed)
    }
}

/// A source that does not verify what it re-derives is sent one range at a time, whatever the
/// client's and the session's windows: a parallel schedule re-sends more, and such a source
/// could re-derive changed bytes under the nonces the first read spent.
#[tokio::test]
async fn a_source_that_does_not_verify_rederived_bytes_is_sent_one_range_at_a_time() {
    let size = 4 * C;
    let grid = Grid::new(
        size,
        "chunk=262144, sessions=4, ranges=4, ttl=300",
        Duration::from_millis(200),
        Box::new(|_, _, _| None),
    );
    let server = grid_server(&grid).await;
    let data = UnverifiedBytes(vec![1u8; size as usize]);
    assert!(!data.verifies_rederived());
    let outcome = server
        .client(PROTOCOL)
        .with_ranges_window(4)
        .upload(&request(size), &data)
        .await
        .unwrap();

    assert!(matches!(outcome, UploadOutcome::Completed { .. }));
    let patches = grid.patches.lock().unwrap().clone();
    assert_eq!(patches.len(), 4, "every range once: {patches:?}");
    for pair in patches.windows(2) {
        assert!(
            pair[1].2 - pair[0].2 >= Duration::from_millis(150),
            "each range waited for the one before it: {patches:?}"
        );
    }
    let offsets: Vec<u64> = patches.iter().map(|p| p.0).collect();
    assert_eq!(offsets, vec![0, C, 2 * C, 3 * C], "in order");
}

/// **`range_conflict` carrying the committed set.** The server already held the first range (a
/// lost answer from an earlier attempt), so the client re-plans from the set the refusal
/// carries: nothing it names is sent again, and no `HEAD` is needed.
#[tokio::test]
async fn a_range_conflict_carrying_the_committed_set_replans_without_head() {
    let size = 3 * C;
    let grid = Grid::new(
        size,
        "chunk=262144, ranges=1",
        Duration::ZERO,
        Box::new(|n, _, held| {
            (n == 1).then(|| {
                held.insert(0, C);
                MockResponse::api_error(
                    409,
                    "Conflict",
                    error_codes::UPLOAD_RANGE_CONFLICT,
                    "overlap",
                )
                .header("X-Capsule-Committed", held.to_header())
            })
        }),
    );
    let server = grid_server(&grid).await;
    let data = vec![2u8; size as usize];
    server
        .client(PROTOCOL)
        .upload(&request(size), &data)
        .await
        .unwrap();

    assert_eq!(grid.offsets(), vec![0, C, 2 * C]);
    assert_eq!(grid.heads(), 1, "only the finalization poll");
}

/// **`range_conflict` without a committed set** re-plans from `HEAD`.
#[tokio::test]
async fn a_range_conflict_without_a_committed_set_replans_from_head() {
    let size = 3 * C;
    let grid = Grid::new(
        size,
        "chunk=262144, ranges=1",
        Duration::ZERO,
        Box::new(|n, _, held| {
            (n == 1).then(|| {
                held.insert(0, C);
                MockResponse::api_error(
                    409,
                    "Conflict",
                    error_codes::UPLOAD_RANGE_CONFLICT,
                    "overlap",
                )
            })
        }),
    );
    let server = grid_server(&grid).await;
    let data = vec![2u8; size as usize];
    server
        .client(PROTOCOL)
        .upload(&request(size), &data)
        .await
        .unwrap();

    assert_eq!(grid.offsets(), vec![0, C, 2 * C]);
    assert_eq!(
        grid.methods.lock().unwrap()[..3],
        ["POST", "PATCH", "HEAD"],
        "the refusal is followed by a HEAD"
    );
}

/// **`range_conflict` with `Retry-After`**: the overlap is with a range still in flight, so the
/// client waits it out before it re-plans.
#[tokio::test]
async fn a_range_conflict_with_retry_after_is_waited_out() {
    let size = C;
    let grid = Grid::new(
        size,
        "chunk=262144, ranges=1",
        Duration::ZERO,
        Box::new(|n, _, held| {
            (n == 1).then(|| {
                MockResponse::api_error(
                    409,
                    "Conflict",
                    error_codes::UPLOAD_RANGE_CONFLICT,
                    "in flight",
                )
                .header("X-Capsule-Committed", held.to_header())
                .header("Retry-After", "1")
            })
        }),
    );
    let server = grid_server(&grid).await;
    let data = vec![2u8; size as usize];
    server
        .client(PROTOCOL)
        .upload(&request(size), &data)
        .await
        .unwrap();

    let patches = grid.patches.lock().unwrap().clone();
    assert_eq!(patches.len(), 2);
    assert_eq!(
        (patches[0].0, patches[1].0),
        (0, 0),
        "the same range, re-sent"
    );
    assert!(patches[1].2 - patches[0].2 >= Duration::from_secs(1));
}

/// **`too_fragmented`**: the ranges in flight finish first, then the refused range is re-planned.
#[tokio::test]
async fn too_fragmented_lets_the_ranges_in_flight_finish_then_replans() {
    let size = 4 * C;
    let grid = Grid::new(
        size,
        "chunk=262144, ranges=4",
        Duration::from_millis(300),
        Box::new(|n, _, _| {
            (n == 2).then(|| {
                MockResponse::api_error(
                    409,
                    "Conflict",
                    error_codes::UPLOAD_TOO_FRAGMENTED,
                    "fragments",
                )
                .header("Retry-After", "0")
            })
        }),
    );
    let server = grid_server(&grid).await;
    let data = vec![3u8; size as usize];
    server
        .client(PROTOCOL)
        .with_ranges_window(2)
        .upload(&request(size), &data)
        .await
        .unwrap();

    let patches = grid.patches.lock().unwrap().clone();
    assert_eq!(patches.len(), 5, "the refused range once more: {patches:?}");
    assert!(
        patches[2].2 - patches[0].2 >= Duration::from_millis(250),
        "nothing is sent until the range in flight returned: {patches:?}"
    );
    assert!(grid.held.lock().unwrap().covers(size));
}

/// **`too_many_in_flight`**: the client waits, then narrows its window to the ranges it still
/// had in flight, so it never asks for more than the server is holding.
#[tokio::test]
async fn too_many_in_flight_narrows_the_window() {
    let size = 6 * C;
    let grid = Grid::new(
        size,
        "chunk=262144, ranges=4",
        Duration::from_millis(300),
        Box::new(|n, _, _| {
            (n == 2).then(|| {
                MockResponse::api_error(
                    409,
                    "Conflict",
                    error_codes::UPLOAD_TOO_MANY_IN_FLIGHT,
                    "window",
                )
                .header("Retry-After", "0")
            })
        }),
    );
    let server = grid_server(&grid).await;
    let data = vec![4u8; size as usize];
    server
        .client(PROTOCOL)
        .with_ranges_window(2)
        .upload(&request(size), &data)
        .await
        .unwrap();

    let patches = grid.patches.lock().unwrap().clone();
    assert_eq!(patches.len(), 7);
    for pair in patches[2..].windows(2) {
        assert!(
            pair[1].2 - pair[0].2 >= Duration::from_millis(250),
            "one range at a time after the refusal: {patches:?}"
        );
    }
}

/// **A lost ACK.** The server committed the range, then the connection dropped before its
/// answer. The client reads `HEAD` before it sends anything else, so the committed range is not
/// sent again.
#[tokio::test]
async fn a_lost_answer_reads_head_and_sends_nothing_already_committed() {
    let size = 3 * C;
    let grid = Grid::new(
        size,
        "chunk=262144, ranges=1",
        Duration::ZERO,
        Box::new(|n, (offset, end), held| {
            (n == 1).then(|| {
                held.insert(offset, end);
                MockResponse::drop_connection()
            })
        }),
    );
    let server = grid_server(&grid).await;
    let data = vec![5u8; size as usize];
    server
        .client(PROTOCOL)
        .upload(&request(size), &data)
        .await
        .unwrap();

    assert_eq!(grid.offsets(), vec![0, C, 2 * C], "nothing re-sent");
    assert_eq!(
        grid.methods.lock().unwrap()[..3],
        ["POST", "PATCH", "HEAD"],
        "the reset is followed by a HEAD"
    );
}

/// **The committed set on a `204`.** The answer says the server also holds a later range (an
/// earlier attempt's), so the client plans around it.
#[tokio::test]
async fn the_committed_set_a_chunk_answer_carries_is_planned_from() {
    let size = 3 * C;
    let grid = Grid::new(
        size,
        "chunk=262144, ranges=1",
        Duration::ZERO,
        Box::new(|n, _, held| {
            if n == 1 {
                held.insert(2 * C, 3 * C);
            }
            None
        }),
    );
    let server = grid_server(&grid).await;
    let data = vec![6u8; size as usize];
    server
        .client(PROTOCOL)
        .upload(&request(size), &data)
        .await
        .unwrap();

    assert_eq!(grid.offsets(), vec![0, C], "the held range is never sent");
}

/// **A record the server lost.** Every range was answered, but the last answer says the session
/// is still `uploading`: the client polls `HEAD`, re-sends what the server no longer holds, and
/// only then is the upload complete.
#[tokio::test]
async fn a_range_the_server_lost_is_sent_again_before_completion() {
    let size = 2 * C;
    let grid = Grid::new(
        size,
        "chunk=262144, ranges=1",
        Duration::ZERO,
        Box::new(|n, (offset, end), held| {
            (n == 2).then(|| {
                // The record of the first range is lost as the second commits.
                *held = CommittedSet::empty();
                held.insert(offset, end);
                MockResponse::new(204, "No Content")
                    .header("X-Capsule-Offset", "0")
                    .header("X-Capsule-Committed", held.to_header())
                    .header("X-Capsule-Upload-Status", "uploading")
            })
        }),
    );
    let server = grid_server(&grid).await;
    let data = vec![7u8; size as usize];
    let outcome = server
        .client(PROTOCOL)
        .upload(&request(size), &data)
        .await
        .unwrap();

    assert!(matches!(outcome, UploadOutcome::Completed { .. }));
    assert_eq!(
        grid.offsets(),
        vec![0, C, 0],
        "the lost range is sent again"
    );
    assert!(grid.held.lock().unwrap().covers(size));
}

// ─── Re-sends only from a source that affirms it is unchanged ───────────────

/// A lost answer for a range the server did **not** keep: the first `PATCH` resets.
fn a_lost_uncommitted_answer() -> Box<Script> {
    Box::new(|n, _, _| (n == 1).then(MockResponse::drop_connection))
}

/// **A lost answer, source unchanged.** The range the server did not keep is read again and
/// re-sent, because the source affirms it returns the same bytes.
#[tokio::test]
async fn a_lost_answer_re_sends_from_a_source_that_affirms_it_is_unchanged() {
    let size = 2 * C;
    let grid = Grid::new(
        size,
        "chunk=262144, ranges=1",
        Duration::ZERO,
        a_lost_uncommitted_answer(),
    );
    let server = grid_server(&grid).await;
    let data = ChangingSource::new(vec![5u8; size as usize], None);
    let outcome = server
        .client(PROTOCOL)
        .upload(&request(size), &data)
        .await
        .unwrap();

    assert!(matches!(outcome, UploadOutcome::Completed { .. }));
    assert_eq!(grid.offsets(), vec![0, 0, C], "the lost range once more");
}

/// **A lost answer, source changed before the re-send.** The check just before the range would
/// be read again finds the change: the transfer ends typed, the session is cancelled, and the
/// range is never sent a second time.
#[tokio::test]
async fn a_lost_answer_of_a_source_changed_since_is_never_re_sent() {
    let size = 2 * C;
    let grid = Grid::new(
        size,
        "chunk=262144, ranges=1",
        Duration::ZERO,
        a_lost_uncommitted_answer(),
    );
    let server = grid_server(&grid).await;
    // The check before the first window passes; the one before its re-send finds the change.
    let data = ChangingSource::new(vec![5u8; size as usize], Some(2));
    let error = server
        .client(PROTOCOL)
        .upload(&request(size), &data)
        .await
        .expect_err("a changed source ends the transfer");

    assert!(
        matches!(error, UploadError::SourceChanged(_)),
        "typed: {error:?}"
    );
    assert_eq!(grid.offsets(), vec![0], "nothing sent again");
    assert_eq!(
        grid.methods.lock().unwrap().last(),
        Some(&"DELETE"),
        "the session is cancelled"
    );
}

/// **A lost answer, identity not affirmable.** A source that finds no change but cannot affirm
/// there was none is never read twice for the same range: the transfer ends typed.
#[tokio::test]
async fn a_lost_answer_of_a_source_that_cannot_affirm_its_identity_is_never_re_sent() {
    let size = 2 * C;
    let grid = Grid::new(
        size,
        "chunk=262144, ranges=1",
        Duration::ZERO,
        a_lost_uncommitted_answer(),
    );
    let server = grid_server(&grid).await;
    let error = server
        .client(PROTOCOL)
        .upload(&request(size), &Unaffirmed(size))
        .await
        .expect_err("an unaffirmed re-send ends the transfer");

    assert!(
        matches!(error, UploadError::SourceChanged(_)),
        "typed: {error:?}"
    );
    assert_eq!(grid.offsets(), vec![0], "nothing sent again");
}

/// **A lost range record, source changed.** Finalization finds a range missing that was sent;
/// the source changed since, so it is not sent again.
#[tokio::test]
async fn a_lost_range_record_of_a_changed_source_is_never_re_sent() {
    let size = 2 * C;
    let grid = Grid::new(
        size,
        "chunk=262144, ranges=1",
        Duration::ZERO,
        Box::new(|n, (offset, end), held| {
            (n == 2).then(|| {
                *held = CommittedSet::empty();
                held.insert(offset, end);
                MockResponse::new(204, "No Content")
                    .header("X-Capsule-Offset", "0")
                    .header("X-Capsule-Committed", held.to_header())
                    .header("X-Capsule-Upload-Status", "uploading")
            })
        }),
    );
    let server = grid_server(&grid).await;
    // Two windows pass their checks; the third, before the lost range is read again, fails.
    let data = ChangingSource::new(vec![7u8; size as usize], Some(3));
    let error = server
        .client(PROTOCOL)
        .upload(&request(size), &data)
        .await
        .expect_err("a changed source ends the transfer");

    assert!(
        matches!(error, UploadError::SourceChanged(_)),
        "typed: {error:?}"
    );
    assert_eq!(
        grid.offsets(),
        vec![0, C],
        "the lost range is not sent again"
    );
}

/// **A resume, source changed.** A session from before this transfer may hold any byte already
/// sent, so a resumed read is a re-send: a source changed since is not read at all.
#[tokio::test]
async fn a_resume_of_a_changed_source_sends_nothing() {
    let patches = Arc::new(AtomicUsize::new(0));
    let deletes = Arc::new(AtomicUsize::new(0));
    let (p, d) = (patches.clone(), deletes.clone());
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("HEAD", "/sess-R") => MockResponse::new(200, "OK")
            .header("X-Capsule-Offset", CHUNK_SIZE_256KB.to_string())
            .header("X-Capsule-Content-Length", (300 * 1024).to_string())
            .header("X-Capsule-Upload-Status", "uploading"),
        ("PATCH", "/sess-R") => {
            p.fetch_add(1, Ordering::SeqCst);
            MockResponse::new(204, "No Content")
                .header("X-Capsule-Offset", ack_offset(req).to_string())
        }
        ("DELETE", "/sess-R") => {
            d.fetch_add(1, Ordering::SeqCst);
            MockResponse::new(204, "No Content")
        }
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let size = 300 * 1024;
    let data = ChangingSource::new(vec![2u8; size as usize], Some(1));
    let error = server
        .client(PROTOCOL)
        .upload_resuming("sess-R", &request(size), &data)
        .await
        .expect_err("a changed source ends the resume");

    assert!(
        matches!(error, UploadError::SourceChanged(_)),
        "typed: {error:?}"
    );
    assert_eq!(patches.load(Ordering::SeqCst), 0, "nothing sent");
    assert_eq!(
        deletes.load(Ordering::SeqCst),
        1,
        "the session is cancelled"
    );
}

/// **A resume, identity not affirmable.** Nothing is read from a source that cannot affirm it
/// is what the session's earlier bytes were read from.
#[tokio::test]
async fn a_resume_of_a_source_that_cannot_affirm_its_identity_sends_nothing() {
    let patches = Arc::new(AtomicUsize::new(0));
    let p = patches.clone();
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("HEAD", "/sess-R") => MockResponse::new(200, "OK")
            .header("X-Capsule-Offset", "0")
            .header("X-Capsule-Content-Length", "8192")
            .header("X-Capsule-Upload-Status", "uploading"),
        ("PATCH", "/sess-R") => {
            p.fetch_add(1, Ordering::SeqCst);
            MockResponse::new(204, "No Content")
                .header("X-Capsule-Offset", ack_offset(req).to_string())
        }
        ("DELETE", "/sess-R") => MockResponse::new(204, "No Content"),
        other => panic!("unexpected {other:?}"),
    })
    .await;

    let error = server
        .client(PROTOCOL)
        .upload_resuming("sess-R", &request(8192), &Unaffirmed(8192))
        .await
        .expect_err("an unaffirmed resume ends the transfer");

    assert!(
        matches!(error, UploadError::SourceChanged(_)),
        "typed: {error:?}"
    );
    assert_eq!(patches.load(Ordering::SeqCst), 0, "nothing sent");
}

/// A real library original: its workspace's temp dir, the bundle and the plaintext's path.
fn library_original() -> (
    tempfile::TempDir,
    capsule_library::lifecycle::UploadBundle,
    std::path::PathBuf,
) {
    use capsule_core::crypto::primitives::Argon2Params;
    use capsule_library::lifecycle::Workspace;

    let dir = tempfile::TempDir::new().unwrap();
    let lib = dir.path().join("library");
    std::fs::create_dir_all(&lib).unwrap();
    let src = dir.path().join("photo.jpg");
    std::fs::write(&src, b"\xFF\xD8\xFF an original edited while it is pushed").unwrap();
    let fast = Argon2Params {
        mem_kib: 64,
        t_cost: 1,
        p_cost: 1,
    };
    let mut ws = Workspace::create_with_params(&lib, b"passphrase", fast).unwrap();
    let album = ws.default_album_id();
    ws.ensure_album(album, "Imports").unwrap();
    let asset_id = ws.import_asset(album, &src).unwrap();
    let bundle = ws.upload_bundle(&asset_id).unwrap();
    let path = ws.original_path(&asset_id).unwrap();
    (dir, bundle, path)
}

/// A one-range session whose first `PATCH` runs `on_first` and then resets, and whose later
/// `PATCH`es commit the blob. Counts each method.
async fn lost_first_answer_server(
    size: u64,
    on_first: impl Fn() + Send + Sync + 'static,
) -> (MockServer, Arc<Mutex<Vec<&'static str>>>) {
    let methods = Arc::new(Mutex::new(Vec::new()));
    let m = methods.clone();
    let held = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let server = MockServer::start(move |req: &MockRequest| {
        let committed = |done: bool| {
            if done {
                format!("0-{}", size - 1)
            } else {
                String::new()
            }
        };
        match (req.method.as_str(), req.path.as_str()) {
            ("POST", "/") => {
                m.lock().unwrap().push("POST");
                created("sess-orig", CHUNK_SIZE_256KB)
            }
            ("HEAD", "/sess-orig") => {
                m.lock().unwrap().push("HEAD");
                let done = held.load(Ordering::SeqCst);
                MockResponse::new(200, "OK")
                    .header("X-Capsule-Offset", if done { size } else { 0 }.to_string())
                    .header("X-Capsule-Committed", committed(done))
                    .header("X-Capsule-Content-Length", size.to_string())
                    .header(
                        "X-Capsule-Upload-Status",
                        if done { "completed" } else { "uploading" },
                    )
            }
            ("PATCH", "/sess-orig") => {
                let first = !m.lock().unwrap().contains(&"PATCH");
                m.lock().unwrap().push("PATCH");
                if first {
                    on_first();
                    return MockResponse::drop_connection();
                }
                held.store(true, Ordering::SeqCst);
                MockResponse::new(204, "No Content")
                    .header("X-Capsule-Offset", size.to_string())
                    .header("X-Capsule-Committed", committed(true))
                    .header("X-Capsule-Upload-Status", "waiting_for_processing")
            }
            ("DELETE", "/sess-orig") => {
                m.lock().unwrap().push("DELETE");
                MockResponse::new(204, "No Content")
            }
            other => panic!("unexpected {other:?}"),
        }
    })
    .await;
    (server, methods)
}

/// **A library original, unchanged.** Its first answer is lost; the file on disk is the one
/// the bundle verified, so the original affirms it and its window is sent again.
#[tokio::test]
async fn an_unchanged_library_original_is_re_sent_after_a_lost_answer() {
    let (_dir, bundle, _path) = library_original();
    let data = crate::push::BlobBytes::Original(&bundle.original);
    let size = bundle.original.size();
    let (server, methods) = lost_first_answer_server(size, || {}).await;
    let outcome = server
        .client(PROTOCOL)
        .upload(&request(size), &data)
        .await
        .unwrap();

    assert!(matches!(outcome, UploadOutcome::Completed { .. }));
    let methods = methods.lock().unwrap();
    assert_eq!(
        methods.iter().filter(|m| **m == "PATCH").count(),
        2,
        "re-sent once: {methods:?}"
    );
}

/// **A library original edited mid-upload.** The plaintext is rewritten while its first
/// window is in flight and that answer is lost: the re-send's check sees the file is no longer
/// the one verified, so no second ciphertext is sealed under the window's nonces.
#[tokio::test]
async fn a_library_original_edited_mid_upload_is_never_re_sent() {
    let (_dir, bundle, path) = library_original();
    let data = crate::push::BlobBytes::Original(&bundle.original);
    let size = bundle.original.size();
    let edited = path.clone();
    let (server, methods) = lost_first_answer_server(size, move || {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&edited)
            .unwrap();
        std::io::Write::write_all(&mut &file, b"\xFF\xD8\xFF EDITED").unwrap();
        file.set_modified(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1))
            .unwrap();
    })
    .await;
    let error = server
        .client(PROTOCOL)
        .upload(&request(size), &data)
        .await
        .expect_err("an edited original ends the transfer");

    assert!(
        matches!(error, UploadError::SourceChanged(_)),
        "typed: {error:?}"
    );
    assert_eq!(error.error_code(), Some(error_codes::UPLOAD_SOURCE_CHANGED));
    let methods = methods.lock().unwrap();
    assert_eq!(
        methods.iter().filter(|m| **m == "PATCH").count(),
        1,
        "nothing sent again: {methods:?}"
    );
    assert_eq!(methods.last(), Some(&"DELETE"), "the session is cancelled");
}

/// A `duplicate_blob` names the existing asset in its structured `existing_asset` member, which
/// wins over anything the English detail says.
#[tokio::test]
async fn a_duplicate_blob_is_merged_onto_its_structured_existing_asset() {
    let server = MockServer::start(move |req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/") => MockResponse::new(409, "Conflict").json_body(format!(
            r#"{{"error":"This content is already stored as asset from-detail","code":{:?},"existing_asset":"from-member"}}"#,
            error_codes::UPLOAD_DUPLICATE_BLOB
        )),
        other => panic!("unexpected {other:?}"),
    })
    .await;
    let outcome = server
        .client(PROTOCOL)
        .upload(&request(4096), &[0u8; 4096])
        .await
        .unwrap();
    match outcome {
        UploadOutcome::AlreadyStored { asset_ref } => assert_eq!(asset_ref, "from-member"),
        other @ UploadOutcome::Completed { .. } => panic!("expected a merge, got {other:?}"),
    }
    assert_eq!(duplicate_asset_ref(Some("  "), "as asset x"), "x");
    assert_eq!(duplicate_asset_ref(None, "no id here"), "no id here");
}

/// A session's state never moves backwards when answers return out of order, and an answer
/// with no state (an older server) leaves none.
#[test]
fn the_session_state_only_moves_forward() {
    let s = |v: &str| Some(v.to_owned());
    assert_eq!(
        later_state(s(STATUS_WAITING), s("uploading")),
        s(STATUS_WAITING)
    );
    assert_eq!(
        later_state(s("uploading"), s(STATUS_WAITING)),
        s(STATUS_WAITING)
    );
    assert_eq!(later_state(s(STATUS_WAITING), None), None);
    assert_eq!(later_state(None, s("uploading")), s("uploading"));
}
