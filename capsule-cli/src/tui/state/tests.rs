//! Reducer tests: each action produces the right state and the right effects.

use super::*;

fn ready() -> AppState {
    let (mut state, _) = AppState::init();
    state.update(Action::SessionLoaded(Ok(SessionSummary::SignedOut)));
    state
}

#[test]
fn init_opens_home_running_and_asks_for_the_session() {
    let (state, effects) = AppState::init();
    assert_eq!(state.screen(), Screen::Home);
    assert_eq!(state.depth(), 1);
    assert!(state.is_running());
    assert_eq!(state.session(), &Loadable::Loading);
    assert_eq!(effects, vec![Effect::LoadSession]);
}

#[test]
fn quit_stops_the_loop_and_asks_for_nothing() {
    let mut state = ready();
    assert!(state.update(Action::Quit).is_empty());
    assert!(!state.is_running());
}

#[test]
fn open_pushes_a_screen_and_back_pops_it() {
    let mut state = ready();
    assert!(state.update(Action::Open(Screen::Help)).is_empty());
    assert_eq!(state.screen(), Screen::Help);
    assert_eq!(state.depth(), 2);
    assert!(state.update(Action::Back).is_empty());
    assert_eq!(state.screen(), Screen::Home);
    assert_eq!(state.depth(), 1);
}

#[test]
fn opening_the_screen_on_top_does_not_stack_it_twice() {
    let mut state = ready();
    state.update(Action::Open(Screen::Help));
    state.update(Action::Open(Screen::Help));
    assert_eq!(state.depth(), 2);
}

#[test]
fn back_on_the_root_screen_keeps_it() {
    let mut state = ready();
    let before = state.clone();
    assert!(state.update(Action::Back).is_empty());
    assert_eq!(state, before);
}

#[test]
fn refresh_marks_the_session_loading_and_requests_one_read() {
    let mut state = ready();
    assert_eq!(
        state.update(Action::RefreshSession),
        vec![Effect::LoadSession]
    );
    // The press is visible at once: the session shows as loading before any port answers.
    assert_eq!(state.session(), &Loadable::Loading);
}

#[test]
fn refresh_while_a_read_is_in_flight_requests_no_second_read() {
    let (mut state, _) = AppState::init();
    assert!(state.update(Action::RefreshSession).is_empty());
    assert_eq!(state.session(), &Loadable::Loading);
}

#[test]
fn a_loaded_session_is_shown_ready() {
    let (mut state, _) = AppState::init();
    let summary = SessionSummary::SignedIn {
        access_token_fresh: true,
    };
    assert!(state.update(Action::SessionLoaded(Ok(summary))).is_empty());
    assert_eq!(state.session(), &Loadable::Ready(summary));
}

#[test]
fn a_failed_session_read_keeps_its_reason() {
    let (mut state, _) = AppState::init();
    state.update(Action::SessionLoaded(Err("disk on fire".to_owned())));
    assert_eq!(
        state.session(),
        &Loadable::Failed("disk on fire".to_owned())
    );
}

#[test]
fn a_session_answer_does_not_move_the_navigation_stack() {
    let (mut state, _) = AppState::init();
    state.update(Action::Open(Screen::Help));
    state.update(Action::SessionLoaded(Ok(SessionSummary::SignedOut)));
    assert_eq!(state.screen(), Screen::Help);
}

/// Transfer actions fold into the board and ask for nothing; none of them moves the stack.
#[test]
fn transfer_actions_fold_into_the_board_without_effects() {
    use capsule_sdk::events::TransferEvent;

    let mut state = ready();
    let event = Stamped {
        at_ms: 40,
        event: TransferEvent::Merged {
            blob: "b".into(),
            asset_ref: "a".into(),
        },
    };
    assert!(state.update(Action::Transfer(event)).is_empty());
    assert!(state.update(Action::TransfersLagged(2)).is_empty());
    assert!(state.update(Action::Tick(90)).is_empty());
    assert_eq!(state.transfers().transfers().len(), 1);
    assert_eq!(state.transfers().missed(), 2);
    assert_eq!(state.transfers().now_ms(), 90);
    assert_eq!(state.screen(), Screen::Home);
}
