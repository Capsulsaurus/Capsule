//! Sync screen tests: the reducer runs one sync at a time and keeps its outcome, and each state
//! draws the right frame.

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::*;
use crate::tui::driver::frame_text;
use crate::tui::state::{Action, AppState, Screen, SessionSummary};
use crate::tui::view;

fn on_sync() -> AppState {
    let (mut state, _) = AppState::init();
    state.update(Action::SessionLoaded(Ok(SessionSummary::SignedIn {
        access_token_fresh: true,
    })));
    state.update(Action::Open(Screen::Sync));
    state
}

fn draw(state: &AppState) -> String {
    let mut terminal = Terminal::new(TestBackend::new(80, 14)).expect("test terminal");
    terminal
        .draw(|frame| view::render(frame, state))
        .expect("draw on a test backend");
    frame_text(terminal.backend().buffer())
}

fn summary(applied: usize, resynced: bool) -> SyncSummary {
    SyncSummary {
        applied,
        albums: 2,
        pages: 1,
        dry_run: false,
        resynced,
    }
}

#[test]
fn run_marks_loading_and_asks_for_one_sync() {
    let mut screen = SyncScreen::default();
    assert_eq!(screen.run(), None);
    assert_eq!(
        screen.update(SyncAction::Run { from_start: true }),
        vec![Effect::Sync(SyncEffect::Run { from_start: true })]
    );
    assert_eq!(screen.run(), Some(&Loadable::Loading));
    // Pressed again while the first drains: no second sync.
    assert!(
        screen
            .update(SyncAction::Run { from_start: false })
            .is_empty()
    );
}

#[test]
fn finished_keeps_the_outcome_and_a_new_run_can_start() {
    let mut screen = SyncScreen::default();
    screen.update(SyncAction::Run { from_start: false });
    assert!(
        screen
            .update(SyncAction::Finished(Ok(summary(3, false))))
            .is_empty()
    );
    assert_eq!(screen.run(), Some(&Loadable::Ready(summary(3, false))));
    screen.update(SyncAction::Finished(Err("no".to_owned())));
    assert_eq!(screen.run(), Some(&Loadable::Failed("no".to_owned())));
    assert_eq!(
        screen.update(SyncAction::Run { from_start: false }).len(),
        1
    );
}

#[test]
fn a_failed_effect_reports_the_run_failing() {
    assert_eq!(
        SyncEffect::Run { from_start: false }.failed("panicked".to_owned()),
        SyncAction::Finished(Err("panicked".to_owned()))
    );
}

#[test]
fn sync_before_the_first_run() {
    insta::assert_snapshot!(draw(&on_sync()));
}

#[test]
fn sync_while_running() {
    let mut state = on_sync();
    state.update(Action::Sync(SyncAction::Run { from_start: false }));
    insta::assert_snapshot!(draw(&state));
}

#[test]
fn sync_up_to_date() {
    let mut state = on_sync();
    state.update(Action::Sync(SyncAction::Finished(Ok(summary(0, false)))));
    assert!(draw(&state).contains("Already up to date."));
}

#[test]
fn sync_applied_after_a_rebuilt_index() {
    let mut state = on_sync();
    state.update(Action::Sync(SyncAction::Finished(Ok(summary(7, true)))));
    insta::assert_snapshot!(draw(&state));
}

#[test]
fn sync_failed() {
    let mut state = on_sync();
    state.update(Action::Sync(SyncAction::Finished(Err(
        "Not authenticated. Run 'capsule auth login' first.".to_owned(),
    ))));
    insta::assert_snapshot!(draw(&state));
}
