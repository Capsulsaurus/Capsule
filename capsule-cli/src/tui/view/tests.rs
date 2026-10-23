//! Render tests: each state draws the right frame, snapshotted as plain text with `insta`.

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::*;
use crate::tui::driver::frame_text;
use crate::tui::state::Action;

const WIDTH: u16 = 80;
const HEIGHT: u16 = 12;

fn draw(state: &AppState) -> String {
    draw_sized(state, HEIGHT)
}

fn draw_sized(state: &AppState, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, height)).expect("test terminal");
    terminal
        .draw(|frame| render(frame, state))
        .expect("draw on a test backend");
    frame_text(terminal.backend().buffer())
}

fn loaded(result: Result<SessionSummary, String>) -> AppState {
    let (mut state, _) = AppState::init();
    state.update(Action::SessionLoaded(result));
    state
}

#[test]
fn home_while_the_session_loads() {
    let (state, _) = AppState::init();
    insta::assert_snapshot!(draw(&state));
}

#[test]
fn home_signed_out() {
    insta::assert_snapshot!(draw(&loaded(Ok(SessionSummary::SignedOut))));
}

#[test]
fn home_signed_in_with_a_fresh_token() {
    insta::assert_snapshot!(draw(&loaded(Ok(SessionSummary::SignedIn {
        access_token_fresh: true
    }))));
}

#[test]
fn home_signed_in_with_an_expired_token() {
    insta::assert_snapshot!(draw(&loaded(Ok(SessionSummary::SignedIn {
        access_token_fresh: false
    }))));
}

#[test]
fn home_when_the_session_cannot_be_read() {
    insta::assert_snapshot!(draw(&loaded(Err(
        "could not read the session: malformed session file".to_owned()
    ))));
}

#[test]
fn help_lists_every_binding() {
    let mut state = loaded(Ok(SessionSummary::SignedOut));
    state.update(Action::Open(Screen::Help));
    // Tall enough for every binding: a person scrolls nothing, and the test sees every row.
    let rows = u16::try_from(BINDINGS.len()).expect("a handful of bindings");
    let frame = draw_sized(&state, rows + 6);
    for binding in BINDINGS {
        assert!(frame.contains(binding.description), "{}", binding.label);
    }
    insta::assert_snapshot!(frame);
}

#[test]
fn a_tiny_terminal_draws_without_panicking() {
    let mut terminal = Terminal::new(TestBackend::new(10, 2)).expect("test terminal");
    let state = loaded(Ok(SessionSummary::SignedOut));
    terminal
        .draw(|frame| render(frame, &state))
        .expect("draw on a test backend");
}
