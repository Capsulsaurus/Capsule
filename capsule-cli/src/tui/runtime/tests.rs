//! Event-loop tests: the real loop on a `TestBackend`, fed a scripted event stream.

use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use futures::stream;
use ratatui::backend::TestBackend;

use super::*;
use crate::tui::driver::frame_text;
use crate::tui::state::{Loadable, Screen, SessionSummary};

struct SignedOut;

impl Ports for SignedOut {
    async fn session_summary(&self) -> Result<SessionSummary, String> {
        Ok(SessionSummary::SignedOut)
    }
}

struct Panics;

impl Ports for Panics {
    async fn session_summary(&self) -> Result<SessionSummary, String> {
        panic!("the port blew up");
    }
}

/// A port that panics is contained on its task: the session leaves `Loading` as a failure the
/// screen shows, and the loop keeps running until the scripted `q`.
#[tokio::test(start_paused = true)]
async fn a_panicking_effect_becomes_a_failure_and_the_loop_survives() {
    let mut terminal = terminal();
    let events = Box::pin(stream::once(async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        press('q')
    }));
    let state = event_loop(&mut terminal, events, Arc::new(Panics))
        .await
        .expect("loop");
    assert!(
        matches!(state.session(), Loadable::Failed(reason) if reason.contains("panicked")),
        "{:?}",
        state.session()
    );
    let frame = frame_text(terminal.backend().buffer());
    assert!(frame.contains("panicked"), "{frame}");
}

fn press(c: char) -> std::io::Result<Event> {
    Ok(Event::Key(KeyEvent::new(
        KeyCode::Char(c),
        KeyModifiers::NONE,
    )))
}

fn terminal() -> Terminal<TestBackend> {
    Terminal::new(TestBackend::new(80, 12)).expect("test terminal")
}

#[tokio::test]
async fn q_quits_the_loop() {
    let mut terminal = terminal();
    let events = stream::iter([press('?'), press('q'), press('?')]);
    let state = event_loop(&mut terminal, events, Arc::new(SignedOut))
        .await
        .expect("loop");
    assert!(!state.is_running());
    // The press after `q` was never read.
    assert_eq!(state.screen(), Screen::Help);
    assert_eq!(state.depth(), 2);
}

#[tokio::test]
async fn closed_input_ends_the_loop() {
    let mut terminal = terminal();
    let state = event_loop(
        &mut terminal,
        stream::iter([press('?')]),
        Arc::new(SignedOut),
    )
    .await
    .expect("loop");
    assert!(state.is_running());
    assert_eq!(state.screen(), Screen::Help);
    assert!(frame_text(terminal.backend().buffer()).contains("Key bindings"));
}

#[tokio::test]
async fn an_input_error_is_returned() {
    let mut terminal = terminal();
    let events = stream::iter([Err(std::io::Error::other("tty gone"))]);
    let error = event_loop(&mut terminal, events, Arc::new(SignedOut))
        .await
        .expect_err("input error");
    assert!(format!("{error:#}").contains("tty gone"), "{error:#}");
}

/// Ports whose transfer handle the test publishes on, as an upload the TUI started would.
struct Transferring(capsule_sdk::events::TransferEvents);

impl Ports for Transferring {
    async fn session_summary(&self) -> Result<SessionSummary, String> {
        Ok(SessionSummary::SignedOut)
    }

    fn transfer_events(&self) -> capsule_sdk::events::TransferEvents {
        self.0.clone()
    }
}

/// The loop subscribes to the ports' transfer handle, stamps each event on its own monotonic
/// clock, and the open transfer screen draws it on the frame tick, which also moves the
/// board's clock while nothing arrives.
#[tokio::test(start_paused = true)]
async fn transfer_events_reach_the_transfer_screen_on_the_frame_tick() {
    use capsule_sdk::events::{TransferEvent, TransferEvents};

    let handle = TransferEvents::channel(16);
    let publisher = handle.clone();
    let emitter = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        publisher.emit(|| TransferEvent::SessionCreated {
            blob: "feedfacecafe0001".into(),
            session_id: "sess-1".into(),
            size: 4096,
            resume_offset: 0,
        });
        publisher.emit(|| TransferEvent::ChunkSent {
            blob: "feedfacecafe0001".into(),
            session_id: "sess-1".into(),
            offset: 0,
            len: 1024,
        });
    });
    let mut terminal = terminal();
    let events = Box::pin(stream::iter([press('t')]).chain(stream::once(async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        press('q')
    })));
    let state = event_loop(&mut terminal, events, Arc::new(Transferring(handle)))
        .await
        .expect("loop");
    emitter.await.expect("emitter");

    assert_eq!(state.screen(), Screen::Transfers);
    let transfer = &state.transfers().transfers()[0];
    assert_eq!(transfer.started_ms, 100, "stamped on arrival");
    // The frame tick kept the board's clock moving after the last event.
    assert!(
        state.transfers().now_ms() >= 900,
        "{}",
        state.transfers().now_ms()
    );
    let frame = frame_text(terminal.backend().buffer());
    assert!(frame.contains("feedfacecafe"), "{frame}");
    assert!(frame.contains("sending"), "{frame}");
}

/// With the clock paused, the scripted `q` arrives only once every other task is idle, so the
/// session read has answered and been drawn by then: effect outcomes reach the frame while input
/// is quiet.
#[tokio::test(start_paused = true)]
async fn an_effect_outcome_is_applied_and_drawn_between_inputs() {
    let mut terminal = terminal();
    let events = Box::pin(stream::once(async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        press('q')
    }));
    let state = event_loop(&mut terminal, events, Arc::new(SignedOut))
        .await
        .expect("loop");
    assert_eq!(state.session(), &Loadable::Ready(SessionSummary::SignedOut));
    let frame = frame_text(terminal.backend().buffer());
    assert!(frame.contains("Not signed in."), "{frame}");
}
