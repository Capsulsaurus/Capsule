//! Render tests for the transfer screen: each board draws the right body, snapshotted as plain
//! text with `insta`.

use capsule_sdk::events::TransferEvent;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::*;
use crate::tui::driver::frame_text;
use crate::tui::transfer::Stamped;

const WIDTH: u16 = 80;

fn draw(board: &TransferBoard, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, height)).expect("test terminal");
    terminal
        .draw(|frame| render(frame, frame.area(), board, "Transfers"))
        .expect("draw on a test backend");
    frame_text(terminal.backend().buffer())
}

fn board(events: Vec<(u64, TransferEvent)>) -> TransferBoard {
    let mut board = TransferBoard::default();
    for (at_ms, event) in events {
        board.apply(Stamped { at_ms, event });
    }
    board
}

fn created(blob: &str, size: u64, resume_offset: u64) -> TransferEvent {
    TransferEvent::SessionCreated {
        blob: blob.into(),
        session_id: "sess-1".into(),
        size,
        resume_offset,
    }
}

fn sent(blob: &str, offset: u64, len: u64) -> TransferEvent {
    TransferEvent::ChunkSent {
        blob: blob.into(),
        session_id: "sess-1".into(),
        offset,
        len,
    }
}

fn acked(blob: &str, offset: u64, len: u64) -> TransferEvent {
    TransferEvent::ChunkAcked {
        blob: blob.into(),
        session_id: "sess-1".into(),
        offset,
        len,
        acked_offset: offset + len,
    }
}

#[test]
fn an_empty_board_says_nothing_is_moving() {
    let frame = draw(&TransferBoard::default(), 8);
    assert!(frame.contains("No transfers yet."), "{frame}");
    insta::assert_snapshot!(frame);
}

#[test]
fn an_upload_mid_flight_with_a_rejected_chunk() {
    const CHUNK: u64 = 64 * 1024;
    let b = board(vec![
        (0, created("9f86d081884c7d65", 16 * CHUNK, 0)),
        (10, sent("9f86d081884c7d65", 0, CHUNK)),
        (20, acked("9f86d081884c7d65", 0, CHUNK)),
        (30, sent("9f86d081884c7d65", CHUNK, CHUNK)),
        (
            40,
            TransferEvent::ChecksumRejected {
                blob: "9f86d081884c7d65".into(),
                session_id: "sess-1".into(),
                offset: CHUNK,
                len: CHUNK,
            },
        ),
        (50, sent("9f86d081884c7d65", CHUNK, CHUNK)),
        (60, acked("9f86d081884c7d65", CHUNK, CHUNK)),
        (70, sent("9f86d081884c7d65", 2 * CHUNK, 2 * CHUNK)),
    ]);
    insta::assert_snapshot!(draw(&b, 10));
}

#[test]
fn a_verified_upload_and_a_fetch_of_unknown_size() {
    let b = board(vec![
        (0, created("aaaa", 1024, 512)),
        (5, sent("aaaa", 512, 512)),
        (9, acked("aaaa", 512, 512)),
        (
            10,
            TransferEvent::Finalize {
                blob: "aaaa".into(),
                session_id: "sess-1".into(),
                size: 1024,
            },
        ),
        (
            11,
            TransferEvent::Receipt {
                asset_id: "asset".into(),
                blob: "aaaa".into(),
                receipt_seq: 7,
            },
        ),
        (
            12,
            TransferEvent::RangeRequested {
                blob: "bbbb".into(),
                offset: 0,
                max_len: None,
            },
        ),
    ]);
    insta::assert_snapshot!(draw(&b, 17));
}

#[test]
fn more_transfers_than_fit_show_the_newest_and_a_count() {
    let b = board(
        (0..5)
            .map(|i| {
                (
                    i,
                    TransferEvent::Merged {
                        blob: format!("blob{i}"),
                        asset_ref: "a".into(),
                    },
                )
            })
            .collect(),
    );
    // 14 inner rows: the summary, the legend's two, and room for two whole five-line transfers.
    let frame = draw(&b, 16);
    assert!(frame.contains("blob4"), "{frame}");
    assert!(!frame.contains("blob0"), "{frame}");
    assert!(frame.contains("… and 3 older"), "{frame}");
    insta::assert_snapshot!(frame);
}

#[test]
fn missed_events_are_flagged() {
    let mut b = TransferBoard::default();
    b.lagged(12);
    assert!(draw(&b, 6).contains("missed 12 events"));
}

#[test]
fn every_cell_state_has_its_own_glyph_in_the_legend() {
    let all: Vec<Cell> = LEGEND
        .iter()
        .flat_map(|l| l.iter().map(|(c, _)| *c))
        .collect();
    let mut glyphs: Vec<&str> = all.iter().map(|c| glyph(*c).0).collect();
    assert_eq!(all.len(), 9);
    glyphs.sort_unstable();
    glyphs.dedup();
    assert_eq!(glyphs.len(), all.len(), "two states share a glyph");
}

#[test]
fn a_tiny_area_draws_without_panicking() {
    let b = board(vec![(0, created("aaaa", 10, 0))]);
    for (width, height) in [(1, 1), (3, 3), (10, 4)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
        terminal
            .draw(|frame| render(frame, frame.area(), &b, "Transfers"))
            .expect("draw on a test backend");
    }
}
