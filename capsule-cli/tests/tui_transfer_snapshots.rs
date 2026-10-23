//! The transfer screen (issue #684) driven only by recorded transfer event streams.
//!
//! Each recording under `tests/fixtures/transfer/` is a stream as the TUI's feed stamps it: one
//! JSON event per line with the millisecond it arrived. A test replays it through the public
//! driver — the same keymap, reducer and view a terminal runs — up to a point in stream time,
//! ticks the frame clock there, and snapshots the frame. No wall clock is read, so every replay
//! of a recording draws the same frames.
//!
//! ## Test list
//!
//! - `an_upload_under_faults_*` — adaptive resize, a transport reset, a checksum refusal and a
//!   server wait, then a receipt verifying it; frames mid-retry, mid-refusal, mid-wait, done.
//! - `a_resume_after_a_cut_*` — a later run resumes a held prefix, loses the session and
//!   re-sends from zero under a new one.
//! - `a_fetch_that_stalls_*` — a stall cut, the resumed window, and the verdict.
//! - `concurrent_transfers_share_the_screen` — the three streams interleaved by arrival.
//! - `a_replay_draws_the_same_frames_every_time` — determinism, frame by frame.
//! - `the_screen_opens_from_home_and_escape_returns` — reachability through the keymap.

use capsule_cli::tui::Ports;
use capsule_cli::tui::driver::Driver;
use capsule_cli::tui::state::{Action, Screen, SessionSummary};
use capsule_cli::tui::transfer::{Stamped, parse_recording};

const UPLOAD_WITH_FAULTS: &str = include_str!("fixtures/transfer/upload_with_faults.jsonl");
const RESUME_AFTER_CUT: &str = include_str!("fixtures/transfer/resume_after_cut.jsonl");
const FETCH_WITH_STALLS: &str = include_str!("fixtures/transfer/fetch_with_stalls.jsonl");

struct SignedOut;

impl Ports for SignedOut {
    async fn session_summary(&self) -> Result<SessionSummary, String> {
        Ok(SessionSummary::SignedOut)
    }
}

fn recording(text: &str) -> Vec<Stamped> {
    parse_recording(text).expect("a valid recording")
}

/// A driver on the transfer screen, its session settled, on a `height`-row terminal.
async fn on_transfers(height: u16) -> Driver<SignedOut> {
    let mut driver = Driver::new(SignedOut, 80, height);
    driver.settle().await;
    driver.type_keys("t");
    assert_eq!(driver.state().screen(), Screen::Transfers);
    driver
}

/// Feed every event stamped at or before `until_ms`, then tick the frame clock to it.
fn replay_until(driver: &mut Driver<SignedOut>, events: &[Stamped], until_ms: u64) {
    for event in events.iter().filter(|e| e.at_ms <= until_ms) {
        driver.dispatch(Action::Transfer(event.clone()));
    }
    driver.dispatch(Action::Tick(until_ms));
}

async fn frame_at(text: &str, until_ms: u64) -> String {
    let mut driver = on_transfers(12).await;
    replay_until(&mut driver, &recording(text), until_ms);
    driver.frame()
}

#[tokio::test]
async fn an_upload_under_faults_mid_retry() {
    let frame = frame_at(UPLOAD_WITH_FAULTS, 100).await;
    assert!(frame.contains("retrying"), "{frame}");
    assert!(frame.contains("connection reset, waited 120 ms"), "{frame}");
    insta::assert_snapshot!(frame);
}

#[tokio::test]
async fn an_upload_under_faults_mid_refusal() {
    let frame = frame_at(UPLOAD_WITH_FAULTS, 302).await;
    assert!(frame.contains("re-sending rejected chunk"), "{frame}");
    assert!(frame.contains("1 reject"), "{frame}");
    insta::assert_snapshot!(frame);
}

#[tokio::test]
async fn an_upload_under_faults_mid_wait_shows_throughput_decaying() {
    // Nothing has moved since 350 ms, so the window (300, 2300] holds only the last ack.
    let frame = frame_at(UPLOAD_WITH_FAULTS, 2300).await;
    assert!(frame.contains("server asked to wait 3000 ms"), "{frame}");
    assert!(frame.contains("128 KiB/s"), "{frame}");
    insta::assert_snapshot!(frame);
}

#[tokio::test]
async fn an_upload_under_faults_verified() {
    let frame = frame_at(UPLOAD_WITH_FAULTS, 3500).await;
    assert!(frame.contains("verified (receipt 1)"), "{frame}");
    assert!(frame.contains("128 KiB → 256 KiB (1 change)"), "{frame}");
    assert!(frame.contains("1 durable"), "{frame}");
    insta::assert_snapshot!(frame);
}

#[tokio::test]
async fn a_resume_after_a_cut_shows_the_held_prefix() {
    let frame = frame_at(RESUME_AFTER_CUT, 30).await;
    assert!(frame.contains("resumed at 384 KiB"), "{frame}");
    assert!(frame.contains('='), "{frame}");
    insta::assert_snapshot!(frame);
}

#[tokio::test]
async fn a_resume_after_a_cut_loses_its_session() {
    let frame = frame_at(RESUME_AFTER_CUT, 100).await;
    assert!(frame.contains("session lost, opened a new one"), "{frame}");
    insta::assert_snapshot!(frame);
}

#[tokio::test]
async fn a_resume_after_a_cut_finalizes_under_a_new_session() {
    let frame = frame_at(RESUME_AFTER_CUT, 400).await;
    assert!(frame.contains("sess-8 (2 sessions)"), "{frame}");
    assert!(frame.contains("sent; finalizing"), "{frame}");
    insta::assert_snapshot!(frame);
}

#[tokio::test]
async fn a_fetch_that_stalls_mid_stall() {
    let frame = frame_at(FETCH_WITH_STALLS, 600).await;
    assert!(frame.contains("stalled, waited 200 ms"), "{frame}");
    insta::assert_snapshot!(frame);
}

#[tokio::test]
async fn a_fetch_that_stalls_verified() {
    let frame = frame_at(FETCH_WITH_STALLS, 800).await;
    assert!(frame.contains("verified"), "{frame}");
    insta::assert_snapshot!(frame);
}

#[tokio::test]
async fn concurrent_transfers_share_the_screen() {
    let mut events: Vec<Stamped> = [UPLOAD_WITH_FAULTS, RESUME_AFTER_CUT, FETCH_WITH_STALLS]
        .into_iter()
        .flat_map(recording)
        .collect();
    // Interleave by arrival; within a stream the order is already the stream's own.
    events.sort_by_key(|e| e.at_ms);
    let mut driver = on_transfers(22).await;
    replay_until(&mut driver, &events, 280);
    let frame = driver.frame();
    assert_eq!(driver.state().transfers().transfers().len(), 3);
    insta::assert_snapshot!(frame);
}

#[tokio::test]
async fn a_replay_draws_the_same_frames_every_time() {
    let events = recording(UPLOAD_WITH_FAULTS);
    let mut first = on_transfers(12).await;
    let mut second = on_transfers(12).await;
    for event in &events {
        first.dispatch(Action::Transfer(event.clone()));
        second.dispatch(Action::Transfer(event.clone()));
        assert_eq!(first.frame(), second.frame(), "diverged at {}", event.at_ms);
    }
    assert_eq!(first.state(), second.state());
}

#[tokio::test]
async fn the_screen_opens_from_home_and_escape_returns() {
    let mut driver = Driver::new(SignedOut, 80, 12);
    driver.settle().await;
    assert!(driver.frame().contains("t Watch transfers"));
    driver.type_keys("t");
    assert!(driver.frame().contains("No transfers yet."));
    driver.press(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Esc,
        crossterm::event::KeyModifiers::NONE,
    ));
    assert_eq!(driver.state().screen(), Screen::Home);
}
