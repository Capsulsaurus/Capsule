//! The transfer board's fold: each event's effect on progress, the chunk in flight, scars,
//! adaptive chunk size, resume, throughput and custody, and the recording format and feed.

use std::time::Duration;

use super::*;

const KIB: u64 = 1024;

fn at(at_ms: u64, event: TransferEvent) -> Stamped {
    Stamped { at_ms, event }
}

fn created(size: u64, resume_offset: u64, session: &str) -> TransferEvent {
    TransferEvent::SessionCreated {
        blob: "b10b".into(),
        session_id: session.into(),
        size,
        resume_offset,
    }
}

fn sent(offset: u64, len: u64) -> TransferEvent {
    TransferEvent::ChunkSent {
        blob: "b10b".into(),
        session_id: "s1".into(),
        offset,
        len,
    }
}

fn acked(offset: u64, len: u64) -> TransferEvent {
    TransferEvent::ChunkAcked {
        blob: "b10b".into(),
        session_id: "s1".into(),
        offset,
        len,
        acked_offset: offset + len,
    }
}

fn retried(offset: u64, len: u64, reason: RetryReason) -> TransferEvent {
    TransferEvent::ChunkRetried {
        blob: "b10b".into(),
        session_id: "s1".into(),
        offset,
        len,
        reason,
    }
}

fn board(events: Vec<Stamped>) -> TransferBoard {
    let mut board = TransferBoard::default();
    for event in events {
        board.apply(event);
    }
    board
}

fn only(board: &TransferBoard) -> &Transfer {
    assert_eq!(board.transfers().len(), 1, "{:?}", board.transfers());
    &board.transfers()[0]
}

#[test]
fn a_new_board_is_empty() {
    let board = TransferBoard::default();
    assert!(board.transfers().is_empty());
    assert_eq!(board.throughput(), 0);
    assert_eq!(board.active(), 0);
    assert_eq!(board.missed(), 0);
    assert_eq!(board.now_ms(), 0);
}

#[test]
fn a_clean_upload_moves_its_prefix_and_finalizes() {
    let b = board(vec![
        at(0, created(512 * KIB, 0, "s1")),
        at(10, sent(0, 256 * KIB)),
    ]);
    let t = only(&b);
    assert_eq!(t.size, Some(512 * KIB));
    assert_eq!(t.session_id.as_deref(), Some("s1"));
    assert_eq!(
        t.active,
        Some(ActiveChunk {
            offset: 0,
            len: Some(256 * KIB),
            state: ChunkState::InFlight
        })
    );

    let b = board(vec![
        at(0, created(512 * KIB, 0, "s1")),
        at(10, sent(0, 256 * KIB)),
        at(20, acked(0, 256 * KIB)),
        at(30, sent(256 * KIB, 256 * KIB)),
        at(40, acked(256 * KIB, 256 * KIB)),
        at(
            50,
            TransferEvent::Finalize {
                blob: "b10b".into(),
                session_id: "s1".into(),
                size: 512 * KIB,
            },
        ),
    ]);
    let t = only(&b);
    assert_eq!(t.done, 512 * KIB);
    assert_eq!(t.moved, 512 * KIB);
    assert_eq!(t.active, None);
    assert_eq!(t.phase, Phase::Finalizing);
    assert_eq!(t.held, 0);
    assert_eq!(t.retries + t.rejects, 0);
    // Finalizing is still live: a receipt has yet to verify it.
    assert_eq!(b.active(), 1);
}

#[test]
fn a_short_write_leaves_the_prefix_at_what_the_server_kept() {
    let b = board(vec![
        at(0, created(512 * KIB, 0, "s1")),
        at(1, sent(0, 256 * KIB)),
        at(
            2,
            TransferEvent::ChunkAcked {
                blob: "b10b".into(),
                session_id: "s1".into(),
                offset: 0,
                len: 256 * KIB,
                acked_offset: 100 * KIB,
            },
        ),
    ]);
    assert_eq!(only(&b).done, 100 * KIB);
    assert_eq!(only(&b).moved, 100 * KIB);
}

#[test]
fn a_receipt_verifies_the_upload_and_counts_toward_custody() {
    let mut b = board(vec![
        at(0, created(KIB, 0, "s1")),
        at(
            5,
            TransferEvent::Finalize {
                blob: "b10b".into(),
                session_id: "s1".into(),
                size: KIB,
            },
        ),
    ]);
    b.apply(at(
        6,
        TransferEvent::Verify {
            asset_id: "a1".into(),
            durable: true,
            blobs: 1,
            not_durable: 0,
        },
    ));
    b.apply(at(
        7,
        TransferEvent::Receipt {
            asset_id: "a1".into(),
            blob: "b10b".into(),
            receipt_seq: 4,
        },
    ));
    let t = only(&b);
    assert_eq!(t.phase, Phase::Verified);
    assert_eq!(t.receipt_seq, Some(4));
    assert_eq!(b.active(), 0);
    assert_eq!(
        b.custody(),
        Custody {
            durable: 1,
            not_durable: 0,
            receipts: 1
        }
    );
}

#[test]
fn a_receipt_for_an_unseen_blob_only_counts() {
    let b = board(vec![
        at(
            0,
            TransferEvent::Receipt {
                asset_id: "a1".into(),
                blob: "elsewhere".into(),
                receipt_seq: 1,
            },
        ),
        at(
            1,
            TransferEvent::Verify {
                asset_id: "a1".into(),
                durable: false,
                blobs: 2,
                not_durable: 1,
            },
        ),
    ]);
    assert!(b.transfers().is_empty());
    assert_eq!(b.custody().receipts, 1);
    assert_eq!(b.custody().not_durable, 1);
}

#[test]
fn a_retry_marks_the_chunk_and_leaves_a_scar_after_it_lands() {
    let mut b = board(vec![
        at(0, created(512 * KIB, 0, "s1")),
        at(1, sent(0, 256 * KIB)),
        at(
            2,
            retried(0, 256 * KIB, RetryReason::TransportReset { wait_ms: 250 }),
        ),
    ]);
    let t = only(&b);
    assert_eq!(t.retries, 1);
    assert_eq!(
        t.last_retry,
        Some(RetryNote::Upload(RetryReason::TransportReset {
            wait_ms: 250
        }))
    );
    assert_eq!(t.active.map(|a| a.state), Some(ChunkState::Retrying));
    assert_eq!(t.cells(2), Some(vec![Cell::Retrying, Cell::Pending]));

    b.apply(at(3, sent(0, 256 * KIB)));
    b.apply(at(4, acked(0, 256 * KIB)));
    assert_eq!(
        only(&b).cells(2),
        Some(vec![Cell::AckedAfterRetry, Cell::Pending])
    );
}

#[test]
fn a_checksum_rejection_counts_and_marks_the_chunk() {
    let mut b = board(vec![
        at(0, created(512 * KIB, 0, "s1")),
        at(1, sent(0, 256 * KIB)),
        at(
            2,
            TransferEvent::ChecksumRejected {
                blob: "b10b".into(),
                session_id: "s1".into(),
                offset: 0,
                len: 256 * KIB,
            },
        ),
    ]);
    assert_eq!(only(&b).rejects, 1);
    assert_eq!(only(&b).cells(2), Some(vec![Cell::Rejected, Cell::Pending]));
    b.apply(at(3, sent(0, 256 * KIB)));
    assert_eq!(only(&b).cells(2), Some(vec![Cell::InFlight, Cell::Pending]));
    b.apply(at(4, acked(0, 256 * KIB)));
    assert_eq!(
        only(&b).cells(2),
        Some(vec![Cell::AckedAfterReject, Cell::Pending])
    );
}

#[test]
fn an_offset_realign_moves_the_prefix_back() {
    let b = board(vec![
        at(0, created(1024 * KIB, 0, "s1")),
        at(1, sent(0, 512 * KIB)),
        at(2, acked(0, 512 * KIB)),
        at(3, sent(512 * KIB, 512 * KIB)),
        at(
            4,
            retried(
                512 * KIB,
                512 * KIB,
                RetryReason::OffsetRealign { to: 256 * KIB },
            ),
        ),
    ]);
    let t = only(&b);
    assert_eq!(t.done, 256 * KIB);
    assert_eq!(
        t.cells(4),
        Some(vec![
            Cell::Acked,
            Cell::Pending,
            Cell::Retrying,
            Cell::Retrying
        ])
    );
}

#[test]
fn a_resumed_upload_shows_what_the_server_held() {
    let b = board(vec![
        at(0, created(1024 * KIB, 512 * KIB, "s1")),
        at(1, sent(512 * KIB, 256 * KIB)),
        at(2, acked(512 * KIB, 256 * KIB)),
    ]);
    let t = only(&b);
    assert_eq!(t.held, 512 * KIB);
    assert_eq!(t.resumes, 1);
    assert_eq!(t.resumed_at, Some(512 * KIB));
    assert_eq!(t.moved, 256 * KIB);
    assert_eq!(
        t.cells(4),
        Some(vec![Cell::Held, Cell::Held, Cell::Acked, Cell::Pending])
    );
}

#[test]
fn a_resume_after_a_cut_continues_the_same_transfer() {
    let b = board(vec![
        at(0, created(1024 * KIB, 0, "s1")),
        at(1, sent(0, 256 * KIB)),
        at(2, acked(0, 256 * KIB)),
        at(3, sent(256 * KIB, 256 * KIB)),
        at(
            4,
            TransferEvent::SessionResumed {
                blob: "b10b".into(),
                session_id: "s1".into(),
                size: 1024 * KIB,
                offset: 256 * KIB,
            },
        ),
    ]);
    let t = only(&b);
    assert_eq!(t.resumes, 1);
    // The same session, resumed: still one.
    assert_eq!(t.sessions, 1);
    assert_eq!(t.active, None);
    // Bytes this run moved are not "held before this run".
    assert_eq!(t.held, 0);
    assert_eq!(t.done, 256 * KIB);
}

#[test]
fn a_lost_session_reopens_on_the_same_transfer() {
    let b = board(vec![
        at(0, created(1024 * KIB, 0, "s1")),
        at(1, sent(0, 256 * KIB)),
        at(2, acked(0, 256 * KIB)),
        at(3, sent(256 * KIB, 256 * KIB)),
        at(4, retried(256 * KIB, 256 * KIB, RetryReason::SessionLost)),
        at(5, created(1024 * KIB, 128 * KIB, "s2")),
    ]);
    let t = only(&b);
    assert_eq!(t.sessions, 2);
    assert_eq!(t.session_id.as_deref(), Some("s2"));
    assert_eq!(t.done, 128 * KIB);
    assert_eq!(t.resumed_at, Some(128 * KIB));
}

#[test]
fn an_upload_after_finalizing_starts_a_new_transfer() {
    let b = board(vec![
        at(0, created(KIB, 0, "s1")),
        at(
            1,
            TransferEvent::Finalize {
                blob: "b10b".into(),
                session_id: "s1".into(),
                size: KIB,
            },
        ),
        at(2, created(KIB, 0, "s2")),
    ]);
    assert_eq!(b.transfers().len(), 2);
}

#[test]
fn an_event_without_a_create_still_gets_a_transfer() {
    // The view lagged past the create: what follows is still shown.
    let b = board(vec![at(0, sent(0, 4 * KIB))]);
    let t = only(&b);
    assert_eq!(t.size, None);
    assert_eq!(t.cells(10), None);
    assert_eq!(t.chunk_size(), Some(4 * KIB));
}

#[test]
fn a_merge_sends_nothing() {
    let b = board(vec![at(
        0,
        TransferEvent::Merged {
            blob: "b10b".into(),
            asset_ref: "a1".into(),
        },
    )]);
    let t = only(&b);
    assert_eq!(t.phase, Phase::Merged);
    assert_eq!(b.active(), 0);
}

#[test]
fn chunk_size_changes_are_counted_and_the_tail_is_not_one() {
    let b = board(vec![
        at(0, created(1000 * KIB, 0, "s1")),
        at(1, sent(0, 128 * KIB)),
        at(2, acked(0, 128 * KIB)),
        at(3, sent(128 * KIB, 128 * KIB)),
        at(4, acked(128 * KIB, 128 * KIB)),
        at(5, sent(256 * KIB, 256 * KIB)),
        at(6, acked(256 * KIB, 256 * KIB)),
        at(7, sent(512 * KIB, 128 * KIB)),
        at(8, acked(512 * KIB, 128 * KIB)),
        // The tail: what remains, not a resize.
        at(9, sent(640 * KIB, 360 * KIB)),
    ]);
    let t = only(&b);
    assert_eq!(t.chunk_size_changes, 2);
    assert_eq!(t.chunk_sizes, vec![256 * KIB, 128 * KIB]);
    assert_eq!(t.chunk_size(), Some(128 * KIB));
}

#[test]
fn chunk_size_history_is_bounded() {
    let mut events = vec![at(0, created(100_000 * KIB, 0, "s1"))];
    let mut offset = 0;
    for (i, len) in [4, 8, 16, 32, 64, 128].iter().enumerate() {
        events.push(at(i as u64 + 1, sent(offset, len * KIB)));
        offset += len * KIB;
    }
    let b = board(events);
    let t = only(&b);
    assert_eq!(t.chunk_size_changes, 5);
    assert_eq!(t.chunk_sizes.len(), CHUNK_SIZES_KEPT);
    assert_eq!(t.chunk_size(), Some(128 * KIB));
}

#[test]
fn throughput_is_the_window_average_and_decays_on_ticks() {
    let mut b = board(vec![
        at(0, created(8192 * KIB, 0, "s1")),
        at(500, acked(0, 1024 * KIB)),
        at(1500, acked(1024 * KIB, 1024 * KIB)),
    ]);
    // 2 MiB over a 2 s window: 1 MiB/s.
    assert_eq!(b.throughput(), 1024 * KIB);
    b.tick(2500);
    // The first sample (500 ms) has left the window (500, 2500].
    assert_eq!(b.throughput(), 512 * KIB);
    b.tick(10_000);
    assert_eq!(b.throughput(), 0);
    // The clock never goes back.
    b.tick(5);
    assert_eq!(b.now_ms(), 10_000);
}

#[test]
fn lag_is_counted() {
    let mut b = TransferBoard::default();
    b.lagged(3);
    b.lagged(4);
    assert_eq!(b.missed(), 7);
}

#[test]
fn a_fetch_tracks_its_landed_prefix_retries_and_verdict() {
    let fetch = |event| at(0, event);
    let mut b = board(vec![
        fetch(TransferEvent::RangeRequested {
            blob: "f".into(),
            offset: 0,
            max_len: Some(512),
        }),
        fetch(TransferEvent::RangeReceived {
            blob: "f".into(),
            offset: 0,
            len: 512,
        }),
        fetch(TransferEvent::RangeRequested {
            blob: "f".into(),
            offset: 512,
            max_len: Some(512),
        }),
        fetch(TransferEvent::FetchRetried {
            blob: "f".into(),
            offset: 512,
            stalled: true,
            wait_ms: 40,
        }),
    ]);
    let t = only(&b);
    assert_eq!(t.direction, Direction::Fetch);
    assert_eq!(t.done, 512);
    assert_eq!(t.retries, 1);
    assert_eq!(t.chunk_size(), Some(512));
    assert_eq!(
        t.last_retry,
        Some(RetryNote::Fetch {
            stalled: true,
            wait_ms: 40
        })
    );
    assert_eq!(
        t.cells(4),
        None,
        "a fetch's size is unknown until its verdict"
    );
    b.apply(fetch(TransferEvent::FetchVerified {
        blob: "f".into(),
        size: 1024,
    }));
    let t = only(&b);
    assert_eq!(t.phase, Phase::Verified);
    assert_eq!(t.cells(2), Some(vec![Cell::Verified, Cell::Verified]));
}

#[test]
fn a_fetch_that_fails_its_hash_is_rejected_whole() {
    let b = board(vec![at(
        0,
        TransferEvent::IntegrityRejected {
            blob: "f".into(),
            size: 10,
        },
    )]);
    let t = only(&b);
    assert_eq!(t.phase, Phase::IntegrityRejected);
    assert_eq!(t.rejects, 1);
    assert_eq!(t.cells(2), Some(vec![Cell::Rejected, Cell::Rejected]));
}

#[test]
fn an_open_ended_fetch_covers_the_rest_of_the_grid() {
    let mut t = Transfer::new("f", Direction::Fetch, 0);
    t.size = Some(400);
    t.done = 100;
    t.active = Some(ActiveChunk {
        offset: 100,
        len: None,
        state: ChunkState::InFlight,
    });
    assert_eq!(
        t.cells(4),
        Some(vec![
            Cell::Acked,
            Cell::InFlight,
            Cell::InFlight,
            Cell::InFlight
        ])
    );
}

#[test]
fn the_grid_has_no_more_cells_than_bytes_and_none_for_an_empty_blob() {
    let mut t = Transfer::new("b", Direction::Upload, 0);
    t.size = Some(3);
    assert_eq!(t.cells(10).map(|c| c.len()), Some(3));
    t.size = Some(0);
    assert_eq!(t.cells(10), None);
}

#[test]
fn a_partly_acked_cell_is_still_pending() {
    let b = board(vec![
        at(0, created(400, 0, "s1")),
        at(1, sent(0, 150)),
        at(2, acked(0, 150)),
    ]);
    assert_eq!(
        only(&b).cells(4),
        Some(vec![
            Cell::Acked,
            Cell::Pending,
            Cell::Pending,
            Cell::Pending
        ])
    );
}

#[test]
fn finished_transfers_beyond_the_bound_are_dropped_oldest_first() {
    let mut b = TransferBoard::default();
    for i in 0..(FINISHED_KEPT + 3) {
        b.apply(at(
            i as u64,
            TransferEvent::FetchVerified {
                blob: format!("f{i}"),
                size: 1,
            },
        ));
    }
    // A live one is never dropped.
    b.apply(at(100, sent(0, 1)));
    assert_eq!(b.transfers().len(), FINISHED_KEPT + 1);
    assert_eq!(b.transfers()[0].blob, "f3");
}

fn created_blob(blob: &str) -> TransferEvent {
    TransferEvent::SessionCreated {
        blob: blob.into(),
        session_id: "s".into(),
        size: KIB,
        resume_offset: 0,
    }
}

#[test]
fn an_unfinished_transfer_silent_for_the_stale_interval_ages_out() {
    let mut b = TransferBoard::default();
    b.apply(at(0, created_blob("quiet")));
    b.apply(at(1, created_blob("busy")));
    b.tick(STALE_AFTER_MS - 1);
    assert_eq!(b.transfers().len(), 2, "not yet silent long enough");
    // An event keeps one alive; the other crosses the interval on a tick alone.
    b.apply(at(STALE_AFTER_MS - 1, created_blob("busy")));
    b.tick(STALE_AFTER_MS);
    assert_eq!(b.transfers().len(), 1);
    // The survivor is whichever was silent less long, and is only dropped once it is silent too.
    b.tick(2 * STALE_AFTER_MS);
    assert!(b.transfers().is_empty());
}

#[test]
fn a_finalizing_transfer_with_no_receipt_ages_out() {
    let mut b = board(vec![
        at(0, created(KIB, 0, "s1")),
        at(
            2,
            TransferEvent::Finalize {
                blob: "b10b".into(),
                session_id: "s1".into(),
                size: KIB,
            },
        ),
    ]);
    assert_eq!(only(&b).phase, Phase::Finalizing);
    b.tick(2 + STALE_AFTER_MS);
    assert!(b.transfers().is_empty());
}

#[test]
fn an_event_after_ageing_out_starts_a_fresh_transfer() {
    let mut b = board(vec![at(0, created(KIB, 0, "s1"))]);
    b.tick(STALE_AFTER_MS);
    b.apply(at(STALE_AFTER_MS + 1, sent(0, 1)));
    assert_eq!(b.transfers().len(), 1);
    assert_eq!(only(&b).started_ms, STALE_AFTER_MS + 1);
}

/// Replays far more never-finished transfers than the board may hold, all inside one stale
/// interval so only the cap can bound them, then spreads more over a long stream so only age can.
#[test]
fn many_never_finished_transfers_keep_the_board_bounded() {
    let mut b = TransferBoard::default();
    for i in 0..(TRANSFERS_KEPT * 4) {
        b.apply(at(i as u64, created_blob(&format!("c{i}"))));
        assert!(b.transfers().len() <= TRANSFERS_KEPT);
    }
    assert_eq!(b.transfers().len(), TRANSFERS_KEPT);
    // The ones silent longest were the ones dropped.
    assert_eq!(b.transfers()[0].blob, format!("c{}", TRANSFERS_KEPT * 3));

    let mut b = TransferBoard::default();
    let spacing = STALE_AFTER_MS / 8;
    let mut peak = 0;
    for i in 0..2_000_u64 {
        b.apply(at(i * spacing, created_blob(&format!("a{i}"))));
        peak = peak.max(b.transfers().len());
    }
    assert!(peak <= 8, "age alone keeps {peak} entries");
    assert!(b.active() <= 8);
}

#[test]
fn scars_are_bounded() {
    let mut events = vec![at(0, created(KIB, 0, "s1"))];
    for i in 0..(SCARS_KEPT + 5) {
        events.push(at(
            i as u64,
            retried(0, 1, RetryReason::ServerWait { wait_ms: 0 }),
        ));
    }
    let b = board(events);
    assert_eq!(only(&b).scars.len(), SCARS_KEPT);
    assert_eq!(only(&b).retries as usize, SCARS_KEPT + 5);
}

#[test]
fn a_recording_parses_and_round_trips() {
    let events = vec![
        at(0, created(KIB, 0, "s1")),
        at(
            12,
            retried(0, KIB, RetryReason::TransportReset { wait_ms: 30 }),
        ),
    ];
    let text: String = events
        .iter()
        .map(|e| serde_json::to_string(e).expect("serialize") + "\n")
        .collect();
    assert!(text.starts_with("{\"at_ms\":0,"), "{text}");
    assert_eq!(parse_recording(&format!("\n{text}\n")), Ok(events));
}

#[test]
fn a_bad_recording_line_is_named() {
    let err = parse_recording("\n{\"at_ms\":0,\"event\":\"nonsense\"}\n").expect_err("bad line");
    assert!(err.starts_with("line 2:"), "{err}");
}

#[tokio::test(start_paused = true)]
async fn the_feed_stamps_each_event_with_its_arrival() {
    let events = TransferEvents::channel(8);
    let mut feed = Feed::new(&events);
    tokio::time::advance(Duration::from_millis(250)).await;
    events.emit(|| sent(0, 1));
    assert_eq!(feed.next().await, Action::Transfer(at(250, sent(0, 1))));
    assert_eq!(feed.now_ms(), 250);
}

#[tokio::test(start_paused = true)]
async fn the_feed_reports_lag() {
    let events = TransferEvents::channel(1);
    let mut feed = Feed::new(&events);
    events.emit(|| sent(0, 1));
    events.emit(|| sent(1, 1));
    events.emit(|| sent(2, 1));
    assert_eq!(feed.next().await, Action::TransfersLagged(2));
    assert_eq!(feed.next().await, Action::Transfer(at(0, sent(2, 1))));
}

#[tokio::test(start_paused = true)]
async fn a_disabled_or_closed_feed_never_yields() {
    let mut disabled = Feed::new(&TransferEvents::disabled());
    assert!(
        tokio::time::timeout(Duration::from_secs(1), disabled.next())
            .await
            .is_err()
    );
    let events = TransferEvents::channel(4);
    let mut closed = Feed::new(&events);
    drop(events);
    assert!(
        tokio::time::timeout(Duration::from_secs(1), closed.next())
            .await
            .is_err()
    );
}
