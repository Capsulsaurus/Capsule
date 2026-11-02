use super::*;

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
fn every_range_sits_on_the_grid_and_only_the_last_may_be_short() {
    // A 10 000-byte blob sent as 4096 + 4096 + 1808, in any order.
    assert!(alignment_ok(0, 4096, 10_000));
    assert!(alignment_ok(4096, 4096, 10_000));
    assert!(
        alignment_ok(8192, 1808, 10_000),
        "the range that ends the blob may be short"
    );
    assert!(alignment_ok(0, 8192, 10_000), "a range spans whole blocks");

    // A client whose arithmetic has drifted.
    assert!(
        !alignment_ok(0, 4000, 10_000),
        "a short range not at the end"
    );
    assert!(!alignment_ok(100, 4096, 10_000), "an offset off the grid");
    assert!(
        !alignment_ok(8292, 1708, 10_000),
        "an offset off the grid, even for the range that ends the blob"
    );

    // A blob smaller than the grid is one range that ends it.
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

const BLOCK: u64 = CHUNK_ALIGNMENT;

/// The committed set holding `blocks`, each one 4 KiB block by index.
fn blocks(blocks: &[u64]) -> CommittedSet {
    blocks.iter().fold(CommittedSet::default(), |set, &block| {
        set.with_range(block * BLOCK, (block + 1) * BLOCK)
    })
}

#[test]
fn a_range_wholly_inside_the_committed_set_is_a_duplicate() {
    let committed = blocks(&[0, 1, 2, 5]);
    assert_eq!(admit(&committed, &[], 4, 0, BLOCK), Admission::Duplicate);
    assert_eq!(
        admit(&committed, &[], 4, BLOCK, 3 * BLOCK),
        Admission::Duplicate,
        "a re-send resized within what committed is still a duplicate"
    );
    assert_eq!(
        admit(
            &committed,
            &[(3 * BLOCK, 4 * BLOCK)],
            1,
            5 * BLOCK,
            6 * BLOCK
        ),
        Admission::Duplicate,
        "a duplicate is answered whatever else is in flight"
    );
}

#[test]
fn exactly_a_range_in_flight_is_a_re_send_of_it() {
    let committed = blocks(&[0]);
    assert_eq!(
        admit(
            &committed,
            &[(4 * BLOCK, 6 * BLOCK)],
            4,
            4 * BLOCK,
            6 * BLOCK
        ),
        Admission::SameRangeInFlight,
        "the same offset and length as a range in flight is a re-send of it"
    );
    assert_eq!(
        admit(
            &committed,
            &[(4 * BLOCK, 6 * BLOCK)],
            4,
            4 * BLOCK,
            5 * BLOCK
        ),
        Admission::RangeConflict { in_flight: true },
        "a different length at the same offset is an overlap, not a re-send"
    );
}

#[test]
fn an_overlap_is_a_conflict_and_says_whether_it_is_in_flight() {
    let committed = blocks(&[0, 1]);
    assert_eq!(
        admit(
            &committed,
            &[(4 * BLOCK, 6 * BLOCK)],
            4,
            5 * BLOCK,
            7 * BLOCK
        ),
        Admission::RangeConflict { in_flight: true },
        "a range in flight is never written twice at once"
    );
    assert_eq!(
        admit(&committed, &[], 4, BLOCK, 3 * BLOCK),
        Admission::RangeConflict { in_flight: false },
        "a range partly committed writes nothing"
    );
    assert_eq!(
        admit(
            &committed,
            &[(2 * BLOCK, 3 * BLOCK)],
            4,
            3 * BLOCK,
            4 * BLOCK
        ),
        Admission::Claim,
        "touching a range in flight is not overlapping it"
    );
}

#[test]
fn the_in_flight_cap_bounds_a_session() {
    let in_flight = [(0, BLOCK), (2 * BLOCK, 3 * BLOCK)];
    assert_eq!(
        admit(
            &CommittedSet::default(),
            &in_flight,
            2,
            4 * BLOCK,
            5 * BLOCK
        ),
        Admission::TooManyInFlight
    );
    assert_eq!(
        admit(
            &CommittedSet::default(),
            &in_flight,
            3,
            4 * BLOCK,
            5 * BLOCK
        ),
        Admission::Claim
    );
}

#[test]
fn the_fragment_bound_counts_the_merge_and_every_other_range_in_flight() {
    // 64 fragments: every even block of the first 128.
    let even: Vec<u64> = (0..64).map(|index| index * 2).collect();
    let full = blocks(&even);
    assert_eq!(full.fragments(), MAX_FRAGMENTS);

    assert_eq!(
        admit(&full, &[], 16, BLOCK, 2 * BLOCK),
        Admission::Claim,
        "with nothing in flight, a gap fill is always admissible"
    );
    assert_eq!(
        admit(&full, &[], 16, 200 * BLOCK, 201 * BLOCK),
        Admission::TooFragmented { in_flight: false },
        "a detached range would make 65"
    );
    assert_eq!(
        admit(&full, &[(3 * BLOCK, 4 * BLOCK)], 16, BLOCK, 2 * BLOCK),
        Admission::Claim,
        "a gap fill leaves 63, and the other range in flight at most one more"
    );

    let sixty_three = blocks(&even[..63]);
    assert_eq!(
        admit(&sixty_three, &[], 16, 200 * BLOCK, 201 * BLOCK),
        Admission::Claim,
        "64 after the merge is within the bound"
    );
    assert_eq!(
        admit(
            &sixty_three,
            &[(300 * BLOCK, 301 * BLOCK)],
            16,
            200 * BLOCK,
            201 * BLOCK
        ),
        Admission::TooFragmented { in_flight: true },
        "64 after the merge, and one more if the range in flight commits apart"
    );
}

#[tokio::test]
async fn one_session_takes_one_turn_at_a_time_and_the_claim_map_forgets_it() {
    use std::time::Duration;

    let claims = SessionClaims::default();
    let first_session = UploadId::new("first");
    let second_session = UploadId::new("second");

    let held = claims.turn(&first_session).await;
    let other = tokio::time::timeout(Duration::from_secs(5), claims.turn(&second_session))
        .await
        .expect("another session's admission is not held up");
    assert!(
        tokio::time::timeout(Duration::from_millis(50), claims.turn(&first_session))
            .await
            .is_err(),
        "a second admission to the same session waits for the first"
    );

    // A claim outlives the turn that took it, and is what the next turn sees.
    let claim = held.claim((0, BLOCK));
    drop(held);
    let next = tokio::time::timeout(Duration::from_secs(5), claims.turn(&first_session))
        .await
        .expect("and takes its turn once the first is done");
    assert_eq!(next.in_flight(), vec![(0, BLOCK)]);
    drop(next);
    drop(claim);
    let after = claims.turn(&first_session).await;
    assert!(after.in_flight().is_empty(), "a dropped claim is released");
    assert_eq!(claims.tracked(), 2);

    drop(after);
    drop(other);
    let _sweep = claims.turn(&UploadId::new("third")).await;
    assert_eq!(
        claims.tracked(),
        1,
        "a session with nothing in flight holds no entry"
    );
    assert_eq!(
        claims
            .held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
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
    in_flight_cap: u8,
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
        committed: CommittedSet::default(),
        in_flight_cap,
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

/// Ranges sent out of order commit where they lie and the session completes when the set is
/// `[0, size)`, with the stage holding every byte in place.
#[tokio::test]
async fn ranges_out_of_order_commit_where_they_lie_and_complete_the_session() {
    let (sessions, blobs, clock, stale) = opened_session("out-of-order", 3 * BLOCK, 4).await;
    let block = |fill: u8| vec![fill; BLOCK as usize];

    let last = write_range(&sessions, &blobs, &clock, &stale, 2 * BLOCK, &block(3))
        .await
        .expect("the last block lands first");
    assert_eq!(
        last,
        Accepted::Committed {
            committed: blocks(&[2]),
            complete: false
        }
    );
    write_range(&sessions, &blobs, &clock, &stale, 0, &block(1))
        .await
        .expect("the first block lands");
    let gap = write_range(&sessions, &blobs, &clock, &stale, BLOCK, &block(2))
        .await
        .expect("the gap fills");
    assert_eq!(
        gap,
        Accepted::Committed {
            committed: blocks(&[0, 1, 2]),
            complete: true
        },
        "the set coalesces to [0, size) and completes the session"
    );

    let mut expected = block(1);
    expected.extend(block(2));
    expected.extend(block(3));
    assert_eq!(
        blobs.staged_bytes(&stale.upload_id),
        Some(expected),
        "every range sits at its own offset"
    );
}

/// Disjoint ranges of one session within its cap are written concurrently and all commit.
#[tokio::test]
async fn disjoint_ranges_in_flight_together_all_commit() {
    let (sessions, blobs, clock, stale) = opened_session("concurrent", 4 * BLOCK, 4).await;
    let block = |fill: u8| vec![fill; BLOCK as usize];
    let (b0, b1, b2, b3) = (block(1), block(2), block(3), block(4));

    let answers = tokio::join!(
        write_range(&sessions, &blobs, &clock, &stale, 3 * BLOCK, &b3),
        write_range(&sessions, &blobs, &clock, &stale, BLOCK, &b1),
        write_range(&sessions, &blobs, &clock, &stale, 0, &b0),
        write_range(&sessions, &blobs, &clock, &stale, 2 * BLOCK, &b2),
    );
    let completed = [answers.0, answers.1, answers.2, answers.3]
        .into_iter()
        .map(|answer| answer.expect("every range is answered"))
        .filter(|answer| {
            assert!(
                matches!(answer, Accepted::Committed { .. }),
                "a disjoint range within the cap commits: {answer:?}"
            );
            matches!(answer, Accepted::Committed { complete: true, .. })
        })
        .count();
    assert_eq!(completed, 1, "exactly one range completes the session");

    let mut expected = b0.clone();
    expected.extend(&b1);
    expected.extend(&b2);
    expected.extend(&b3);
    assert_eq!(blobs.staged_bytes(&stale.upload_id), Some(expected));
}

/// Hold `range` of `upload` in flight, as a first attempt still being written would.
async fn first_attempt(upload: &UploadId, range: (u64, u64)) -> RangeClaim {
    admission_turn(upload).await.claim(range)
}

/// A re-send of a range whose first attempt is still in flight waits for it, and is the
/// duplicate it is once that attempt commits — the answer the session-locked server gave.
#[tokio::test(start_paused = true)]
async fn a_re_send_waits_out_its_first_attempt_and_is_a_duplicate_when_it_commits() {
    use crate::store::{Clock as _, UploadSessionStore as _};

    let (sessions, blobs, clock, stale) = opened_session("resend-committed", 2 * BLOCK, 4).await;
    let claim = first_attempt(&stale.upload_id, (0, BLOCK)).await;
    let bytes = vec![1u8; BLOCK as usize];

    let (answer, ()) = tokio::join!(
        write_range(&sessions, &blobs, &clock, &stale, 0, &bytes),
        async {
            tokio::time::sleep(SAME_RANGE_WAIT / 2).await;
            sessions
                .record_range(
                    &stale.upload_id,
                    CommittedRange {
                        start: 0,
                        end: BLOCK,
                        accepted_at: clock.now(),
                    },
                )
                .await
                .expect("the first attempt records");
            drop(claim);
        }
    );
    assert_eq!(
        answer.expect("the re-send is answered"),
        Accepted::Duplicate {
            committed: blocks(&[0]),
            unfinished: false
        }
    );
    assert_eq!(
        blobs.staged_bytes(&stale.upload_id),
        Some(Vec::new()),
        "the re-send wrote nothing over the first attempt"
    );
}

/// A re-send whose first attempt ends without committing is written in its place.
#[tokio::test(start_paused = true)]
async fn a_re_send_whose_first_attempt_failed_is_written() {
    let (sessions, blobs, clock, stale) = opened_session("resend-failed", 2 * BLOCK, 4).await;
    let claim = first_attempt(&stale.upload_id, (0, BLOCK)).await;
    let bytes = vec![1u8; BLOCK as usize];

    let (answer, ()) = tokio::join!(
        write_range(&sessions, &blobs, &clock, &stale, 0, &bytes),
        async {
            tokio::time::sleep(SAME_RANGE_WAIT / 2).await;
            drop(claim);
        }
    );
    assert_eq!(
        answer.expect("the re-send is answered"),
        Accepted::Committed {
            committed: blocks(&[0]),
            complete: false
        }
    );
    assert_eq!(blobs.staged_bytes(&stale.upload_id), Some(bytes));
}

/// A first attempt that outlasts the wait leaves the re-send a conflict in flight, writing
/// nothing; a different range is never made to wait.
#[tokio::test(start_paused = true)]
async fn a_re_send_past_the_wait_is_a_conflict_in_flight() {
    let (sessions, blobs, clock, stale) = opened_session("resend-stuck", 2 * BLOCK, 4).await;
    let claim = first_attempt(&stale.upload_id, (0, 2 * BLOCK)).await;

    let started = tokio::time::Instant::now();
    let answer = write_range(
        &sessions,
        &blobs,
        &clock,
        &stale,
        0,
        &vec![1u8; 2 * BLOCK as usize],
    )
    .await
    .expect("the re-send is answered");
    assert_eq!(
        answer,
        Accepted::RangeConflict {
            committed: CommittedSet::default(),
            in_flight: true
        }
    );
    assert_eq!(
        started.elapsed(),
        SAME_RANGE_WAIT,
        "it waited the bound, no longer"
    );

    let started = tokio::time::Instant::now();
    let overlap = write_range(
        &sessions,
        &blobs,
        &clock,
        &stale,
        0,
        &vec![1u8; BLOCK as usize],
    )
    .await
    .expect("the overlap is answered");
    assert_eq!(
        overlap,
        Accepted::RangeConflict {
            committed: CommittedSet::default(),
            in_flight: true
        }
    );
    assert_eq!(
        started.elapsed(),
        std::time::Duration::ZERO,
        "a non-identical overlap is refused at once"
    );
    drop(claim);
    assert_eq!(
        blobs.staged_bytes(&stale.upload_id),
        Some(Vec::new()),
        "neither wrote a byte"
    );
}

/// A duplicate writes nothing — not even a re-send carrying different bytes — and a partial
/// overlap is refused with the set to re-plan from.
#[tokio::test]
async fn a_committed_range_is_never_written_again() {
    let (sessions, blobs, clock, stale) = opened_session("never-twice", 4 * BLOCK, 4).await;
    let original = vec![1u8; 2 * BLOCK as usize];
    write_range(&sessions, &blobs, &clock, &stale, 0, &original)
        .await
        .expect("two blocks land");

    let changed = vec![9u8; BLOCK as usize];
    assert_eq!(
        write_range(&sessions, &blobs, &clock, &stale, BLOCK, &changed)
            .await
            .expect("the re-send is answered"),
        Accepted::Duplicate {
            committed: blocks(&[0, 1]),
            unfinished: false
        },
        "a resized re-send inside the committed set is a duplicate"
    );
    let straddle = vec![9u8; 2 * BLOCK as usize];
    assert_eq!(
        write_range(&sessions, &blobs, &clock, &stale, BLOCK, &straddle)
            .await
            .expect("the overlap is answered"),
        Accepted::RangeConflict {
            committed: blocks(&[0, 1]),
            in_flight: false
        }
    );
    assert_eq!(
        blobs.staged_bytes(&stale.upload_id),
        Some(original),
        "no committed byte was touched"
    );
}

/// A range off the grid, or past the declared size, is refused before anything is written.
#[tokio::test]
async fn a_range_off_the_grid_or_past_the_size_writes_nothing() {
    let (sessions, blobs, clock, stale) = opened_session("off-grid", 2 * BLOCK, 4).await;
    assert_eq!(
        write_range(&sessions, &blobs, &clock, &stale, 100, &[1u8; 4096])
            .await
            .expect("answered"),
        Accepted::NotAligned
    );
    assert_eq!(
        write_range(&sessions, &blobs, &clock, &stale, BLOCK, &[1u8; 8192])
            .await
            .expect("answered"),
        Accepted::SizeExceeded
    );
    assert_eq!(blobs.staged_bytes(&stale.upload_id), Some(Vec::new()));
}

/// A concurrent range completed the session and finalization claimed it before this one's turn
/// came. Decided on the stale record, this range would be written onto a stage the finalizer is
/// reading; the re-read under the turn answers `NotActive`, and the stage is untouched.
#[tokio::test]
async fn a_range_behind_a_completing_one_finds_the_session_not_active() {
    use crate::store::UploadSessionStore as _;

    let (sessions, blobs, clock, stale) =
        opened_session("completed-while-waiting", 2 * BLOCK, 4).await;
    let id = stale.upload_id.clone();

    let first = write_range(&sessions, &blobs, &clock, &stale, 0, &[7u8; 8192])
        .await
        .expect("the completing range lands");
    assert_eq!(
        first,
        Accepted::Committed {
            committed: blocks(&[0, 1]),
            complete: true
        }
    );
    sessions
        .claim_finalize(&id)
        .await
        .expect("finalization claims the session");

    let mut stale_short = stale.clone();
    stale_short.total_size = 3 * BLOCK;
    let late = write_range(
        &sessions,
        &blobs,
        &clock,
        &stale_short,
        2 * BLOCK,
        &[8u8; 4096],
    )
    .await
    .expect("the late range is answered");
    assert_eq!(late, Accepted::NotActive);
    assert_eq!(
        blobs.staged_bytes(&id).map(|bytes| bytes.len()),
        Some(8192),
        "nothing landed on the claimed stage"
    );
}

/// A duplicate says whether the session it lands on is full and unclaimed — the one state in
/// which the duplicate is the request left to finish it — and only then.
#[tokio::test]
async fn a_duplicate_names_a_full_session_nobody_has_finished() {
    use crate::store::UploadSessionStore as _;

    let (sessions, blobs, clock, stale) = opened_session("duplicate-when-full", 2 * BLOCK, 4).await;
    let id = stale.upload_id.clone();
    let first = [1u8; 4096];
    let last = [2u8; 4096];

    write_range(&sessions, &blobs, &clock, &stale, 0, &first)
        .await
        .expect("the first range lands");
    assert_eq!(
        write_range(&sessions, &blobs, &clock, &stale, 0, &first)
            .await
            .expect("the duplicate is answered"),
        Accepted::Duplicate {
            committed: blocks(&[0]),
            unfinished: false
        },
        "a session still short of its size is not unfinished, merely in progress"
    );

    write_range(&sessions, &blobs, &clock, &stale, BLOCK, &last)
        .await
        .expect("the completing range lands");
    for (offset, bytes) in [(BLOCK, &last), (0, &first)] {
        assert_eq!(
            write_range(&sessions, &blobs, &clock, &stale, offset, bytes)
                .await
                .expect("the duplicate is answered"),
            Accepted::Duplicate {
                committed: blocks(&[0, 1]),
                unfinished: true
            },
            "any duplicate on a full, unclaimed session names it unfinished"
        );
    }

    sessions
        .claim_finalize(&id)
        .await
        .expect("finalization claims the session");
    assert_eq!(
        write_range(&sessions, &blobs, &clock, &stale, BLOCK, &last)
            .await
            .expect("the duplicate is answered"),
        Accepted::Duplicate {
            committed: blocks(&[0, 1]),
            unfinished: false
        },
        "a claimed session has a finisher"
    );
}

/// Finalization claimed the session before this range's turn came. Nothing may land on a stage
/// the finalizer is reading.
#[tokio::test]
async fn a_range_of_a_claimed_session_lands_nothing() {
    use crate::store::UploadSessionStore as _;

    let (sessions, blobs, clock, stale) =
        opened_session("claimed-while-waiting", 2 * BLOCK, 4).await;
    let id = stale.upload_id.clone();
    sessions
        .claim_finalize(&id)
        .await
        .expect("finalization claims the session");

    let answer = write_range(&sessions, &blobs, &clock, &stale, 0, &[7u8; 4096])
        .await
        .expect("the range is answered");
    assert_eq!(answer, Accepted::NotActive);
    assert_eq!(
        blobs.staged_bytes(&id),
        Some(Vec::new()),
        "the stage is untouched"
    );
}

/// A range written after its session was cancelled is never recorded, and recreates nothing.
#[tokio::test]
async fn a_range_against_a_removed_session_is_discarded() {
    use crate::blob::BlobStore as _;
    use crate::store::UploadSessionStore as _;

    let (sessions, blobs, clock, stale) = opened_session("removed", 2 * BLOCK, 4).await;
    let id = stale.upload_id.clone();
    blobs.abandon(&id).await.expect("the stage goes");
    sessions.discard(&id).await.expect("the session goes");

    assert_eq!(
        write_range(&sessions, &blobs, &clock, &stale, 0, &[7u8; 4096])
            .await
            .expect("answered"),
        Accepted::SessionGone
    );
    assert_eq!(blobs.staged_bytes(&id), None, "no stage was recreated");
}
