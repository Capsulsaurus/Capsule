//! Keymap tests: each key sends the bound action where the binding is available, and nothing
//! anywhere else.

use crossterm::event::{KeyEventState, MouseEvent, MouseEventKind};

use super::*;
use crate::tui::state::SessionSummary;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn home() -> AppState {
    let (mut state, _) = AppState::init();
    state.update(Action::SessionLoaded(Ok(SessionSummary::SignedOut)));
    state
}

fn help() -> AppState {
    let mut state = home();
    state.update(Action::Open(Screen::Help));
    state
}

#[test]
fn q_and_ctrl_c_quit_on_every_screen() {
    for state in [home(), help()] {
        assert_eq!(
            action_for_key(&state, key(KeyCode::Char('q'))),
            Some(Action::Quit)
        );
        assert_eq!(
            action_for_key(
                &state,
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
            ),
            Some(Action::Quit)
        );
    }
}

#[test]
fn question_mark_opens_help_unless_help_is_on_top() {
    assert_eq!(
        action_for_key(&home(), key(KeyCode::Char('?'))),
        Some(Action::Open(Screen::Help))
    );
    assert_eq!(action_for_key(&help(), key(KeyCode::Char('?'))), None);
}

#[test]
fn a_shifted_character_matches_its_binding() {
    // Terminals disagree on whether `?` carries SHIFT; both spellings open help.
    assert_eq!(
        action_for_key(
            &home(),
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT)
        ),
        Some(Action::Open(Screen::Help))
    );
}

#[test]
fn another_modifier_makes_a_character_not_match() {
    assert_eq!(
        action_for_key(
            &home(),
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::ALT)
        ),
        None
    );
}

#[test]
fn esc_goes_back_only_where_there_is_somewhere_to_go() {
    assert_eq!(
        action_for_key(&help(), key(KeyCode::Esc)),
        Some(Action::Back)
    );
    assert_eq!(action_for_key(&home(), key(KeyCode::Esc)), None);
}

#[test]
fn r_reloads_the_session_on_home_only() {
    assert_eq!(
        action_for_key(&home(), key(KeyCode::Char('r'))),
        Some(Action::RefreshSession)
    );
    assert_eq!(action_for_key(&help(), key(KeyCode::Char('r'))), None);
}

#[test]
fn an_unbound_key_sends_nothing() {
    assert_eq!(action_for_key(&home(), key(KeyCode::Char('z'))), None);
    assert_eq!(action_for_key(&home(), key(KeyCode::F(5))), None);
}

#[test]
fn a_release_is_not_a_press() {
    let release = KeyEvent {
        code: KeyCode::Char('q'),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Release,
        state: KeyEventState::NONE,
    };
    assert_eq!(action_for_key(&home(), release), None);
}

#[test]
fn only_key_events_send_actions() {
    let state = home();
    assert_eq!(
        action_for_event(&state, &Event::Key(key(KeyCode::Char('q')))),
        Some(Action::Quit)
    );
    assert_eq!(action_for_event(&state, &Event::Resize(80, 24)), None);
    assert_eq!(action_for_event(&state, &Event::FocusGained), None);
    let mouse = MouseEvent {
        kind: MouseEventKind::Moved,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    };
    assert_eq!(action_for_event(&state, &Event::Mouse(mouse)), None);
}

#[test]
fn no_two_bindings_claim_the_same_key_on_the_same_screen() {
    for screen in [Screen::Home, Screen::Help] {
        let keys: Vec<_> = BINDINGS
            .iter()
            .filter(|b| applies(b, screen))
            .map(|b| (b.code, b.modifiers))
            .collect();
        for (i, k) in keys.iter().enumerate() {
            assert!(
                !keys[i + 1..].contains(k),
                "{k:?} bound twice on {screen:?}"
            );
        }
    }
}
