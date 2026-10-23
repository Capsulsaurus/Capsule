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
