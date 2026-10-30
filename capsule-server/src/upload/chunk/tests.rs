use super::*;

#[test]
fn an_offset_is_at_the_head_behind_or_ahead() {
    assert_eq!(classify_offset(0, 0), OffsetPosition::AtHead);
    assert_eq!(classify_offset(4096, 4096), OffsetPosition::AtHead);
    assert_eq!(classify_offset(0, 4096), OffsetPosition::Behind);
    assert_eq!(classify_offset(8192, 4096), OffsetPosition::Ahead);
}

#[test]
fn an_offset_header_is_read_strictly_or_not_at_all() {
    assert_eq!(parse_offset("0"), Some(0));
    assert_eq!(parse_offset("4096"), Some(4096));
    for bad in [
        "", " 4096", "4096 ", "+4096", "-1", "0x10", "4_096", "4.0", "nine",
    ] {
        assert_eq!(parse_offset(bad), None, "{bad:?} is not an offset");
    }
}

#[test]
fn a_checksum_header_is_a_bare_lowercase_sha256() {
    let good = "a".repeat(64);
    assert_eq!(parse_checksum(&good), Some(good.as_str()));
    assert_eq!(
        parse_checksum(&"0123456789abcdef".repeat(4)),
        Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
    );

    for bad in [
        String::new(),
        "a".repeat(63),
        "a".repeat(65),
        "A".repeat(64),
        "g".repeat(64),
        format!("sha256:{}", "a".repeat(64)),
    ] {
        assert_eq!(parse_checksum(&bad), None, "{bad:?} is not a checksum");
    }
}

#[test]
fn every_chunk_but_the_last_is_aligned() {
    // A 10 000-byte blob sent as 4096 + 4096 + 1808.
    assert!(alignment_ok(0, 4096, 10_000));
    assert!(alignment_ok(4096, 4096, 10_000));
    assert!(
        alignment_ok(8192, 1808, 10_000),
        "the final chunk is exempt"
    );

    // The same blob sent as 4000 + … is a client whose arithmetic has drifted.
    assert!(!alignment_ok(0, 4000, 10_000));

    // A blob smaller than the alignment is one final chunk.
    assert!(alignment_ok(0, 10, 10));
}

#[test]
fn the_chunk_ceiling_is_inclusive() {
    assert!(within_chunk_ceiling(MAX_CHUNK_BYTES));
    assert!(!within_chunk_ceiling(MAX_CHUNK_BYTES + 1));
}

#[test]
fn the_chunk_media_type_ignores_parameters_and_case() {
    assert!(is_chunk_media_type("application/octet-stream"));
    assert!(is_chunk_media_type(
        "application/octet-stream; charset=binary"
    ));
    assert!(is_chunk_media_type("Application/Octet-Stream"));
    assert!(!is_chunk_media_type("application/json"));
    assert!(
        !is_chunk_media_type("application/partial-upload"),
        "TUS v2's media type would claim a compatibility we do not have"
    );
}

#[test]
fn the_suggested_size_follows_the_deployment_tiers() {
    assert_eq!(suggested_chunk_size(1), 256 * 1024);
    assert_eq!(suggested_chunk_size(10 * 1024 * 1024 - 1), 256 * 1024);
    assert_eq!(suggested_chunk_size(10 * 1024 * 1024), 1024 * 1024);
    assert_eq!(suggested_chunk_size(100 * 1024 * 1024), 4 * 1024 * 1024);

    // Whatever the tier, the suggestion is itself a legal non-final chunk — a suggestion
    // the protocol would reject is worse than no suggestion.
    for total in [1, 10 * 1024 * 1024, 100 * 1024 * 1024, u64::MAX] {
        let suggested = suggested_chunk_size(total);
        assert!(suggested.is_multiple_of(CHUNK_ALIGNMENT));
        assert!(within_chunk_ceiling(suggested));
    }
}

#[tokio::test]
async fn one_session_appends_one_chunk_at_a_time_and_the_lock_map_forgets_it() {
    use std::time::Duration;

    use crate::store::UploadId;

    let locks = SessionLocks::default();
    let first_session = UploadId::new("first");
    let second_session = UploadId::new("second");

    let held = locks.acquire(&first_session).await;
    let other = tokio::time::timeout(Duration::from_secs(5), locks.acquire(&second_session))
        .await
        .expect("another session's appends are not held up");
    assert!(
        tokio::time::timeout(Duration::from_millis(50), locks.acquire(&first_session))
            .await
            .is_err(),
        "a second append to the same session waits for the first"
    );

    drop(held);
    let next = tokio::time::timeout(Duration::from_secs(5), locks.acquire(&first_session))
        .await
        .expect("and takes its turn once the first is done");
    assert_eq!(locks.tracked(), 2);

    drop(next);
    drop(other);
    let _sweep = locks.acquire(&UploadId::new("third")).await;
    assert_eq!(
        locks.tracked(),
        1,
        "a session with nothing in flight holds no entry"
    );
    assert_eq!(
        locks
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len(),
        1,
        "and its dead entry is swept on the next acquisition"
    );
}

/// A fresh session in the in-memory doubles, staged and empty, with the record as the
/// caller read it before taking its turn.
async fn opened_session(
    name: &str,
    total_size: u64,
) -> (
    crate::store::memory::InMemoryUploadSessions,
    crate::blob::memory::InMemoryBlobStore,
    crate::store::memory::ManualClock,
    crate::store::UploadSessionRecord,
) {
    use crate::blob::BlobStore as _;
    use crate::store::{Clock as _, UploadSessionStore as _};

    let clock = crate::store::memory::ManualClock::default();
    let sessions = crate::store::memory::InMemoryUploadSessions::with_default_ttl(
        std::sync::Arc::new(clock.clone()),
    );
    let blobs = crate::blob::memory::InMemoryBlobStore::new();
    let record = crate::store::UploadSessionRecord {
        upload_id: crate::store::UploadId::new(name),
        asset_id: crate::store::AssetId::new(format!("{name}-asset")),
        owner_id: crate::store::OwnerId::new("owner"),
        upload_user_id: crate::store::UserId::new("uploader"),
        album_id: None,
        content_type: None,
        expected_hash: "0".repeat(64),
        crypto_suite_id: 1,
        protocol_version: "2026-08-29".to_owned(),
        blob_role: crate::store::upload::BlobRole::Original,
        intent_id: None,
        manifest_envelope: "{}".to_owned(),
        received_bytes: 0,
        total_size,
        status: crate::store::upload::UploadSessionStatus::Pending,
        failure_code: None,
        created_at: clock.now(),
        last_progress_at: clock.now(),
    };
    sessions
        .open(record.clone())
        .await
        .expect("the session opens");
    blobs
        .begin(&record.upload_id)
        .await
        .expect("the stage begins");
    (sessions, blobs, clock, record)
}

/// A concurrent chunk completed the session and finalization claimed it while this one waited
/// its turn. Decided on the stale record, this chunk at the new head would read as
/// `SizeExceeded` and the session would be abandoned under the finalizer; the post-lock
/// re-read answers `NotActive`, and the stage is untouched.
#[tokio::test]
async fn a_chunk_behind_a_completing_one_finds_the_session_not_active() {
    use crate::blob::BlobStore as _;
    use crate::store::UploadSessionStore as _;

    let (sessions, blobs, clock, stale) = opened_session("completed-while-waiting", 4096).await;
    let id = stale.upload_id.clone();
    let bytes = [7u8; 4096];

    let first = append(&sessions, &blobs, &clock, &stale, 0, &bytes, "a")
        .await
        .expect("the completing chunk lands");
    assert_eq!(
        first,
        Accepted::Advanced {
            next_offset: 4096,
            complete: true
        }
    );
    sessions
        .claim_finalize(&id)
        .await
        .expect("finalization claims the session");

    let late = append(&sessions, &blobs, &clock, &stale, 4096, &bytes, "b")
        .await
        .expect("the late chunk is answered");
    assert_eq!(late, Accepted::NotActive, "not SizeExceeded");
    assert_eq!(
        blobs.staged_len(&id).await.expect("the stage reads"),
        Some(4096),
        "nothing landed on the claimed stage"
    );
}

/// A replay says whether the session it lands on is full and unclaimed — the one state in
/// which the replay is the request left to finish it — and only then.
#[tokio::test]
async fn a_replay_names_a_full_session_nobody_has_finished() {
    use crate::store::UploadSessionStore as _;

    let (sessions, blobs, clock, stale) = opened_session("replayed-when-full", 8192).await;
    let id = stale.upload_id.clone();
    let first = [1u8; 4096];
    let last = [2u8; 4096];

    append(&sessions, &blobs, &clock, &stale, 0, &first, "a")
        .await
        .expect("the first chunk lands");
    assert_eq!(
        append(&sessions, &blobs, &clock, &stale, 0, &first, "a")
            .await
            .expect("the replay is answered"),
        Accepted::Replayed {
            next_offset: 4096,
            unfinished: false
        },
        "a session still short of its size is not unfinished, merely in progress"
    );

    append(&sessions, &blobs, &clock, &stale, 4096, &last, "b")
        .await
        .expect("the completing chunk lands");
    for (offset, bytes, hash, next_offset) in [(4096, &last, "b", 8192), (0, &first, "a", 4096)] {
        assert_eq!(
            append(&sessions, &blobs, &clock, &stale, offset, bytes, hash)
                .await
                .expect("the replay is answered"),
            Accepted::Replayed {
                next_offset,
                unfinished: true
            },
            "any replay on a full, unclaimed session names it unfinished"
        );
    }

    sessions
        .claim_finalize(&id)
        .await
        .expect("finalization claims the session");
    assert_eq!(
        append(&sessions, &blobs, &clock, &stale, 4096, &last, "b")
            .await
            .expect("the replay is answered"),
        Accepted::Replayed {
            next_offset: 8192,
            unfinished: false
        },
        "a claimed session has a finisher"
    );
}

/// Finalization claimed the session before this chunk's turn came, with the offset still at
/// the head the caller read. Nothing may land on a stage the finalizer is reading.
#[tokio::test]
async fn a_chunk_at_the_head_of_a_claimed_session_lands_nothing() {
    use crate::blob::BlobStore as _;
    use crate::store::UploadSessionStore as _;

    let (sessions, blobs, clock, stale) = opened_session("claimed-while-waiting", 8192).await;
    let id = stale.upload_id.clone();
    sessions
        .claim_finalize(&id)
        .await
        .expect("finalization claims the session");

    let answer = append(&sessions, &blobs, &clock, &stale, 0, &[7u8; 4096], "a")
        .await
        .expect("the chunk is answered");
    assert_eq!(answer, Accepted::NotActive);
    assert_eq!(
        blobs.staged_len(&id).await.expect("the stage reads"),
        Some(0),
        "the stage is untouched"
    );
}
