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

/// `screen` opened over Home.
fn at(screen: Screen) -> AppState {
    let mut state = home();
    state.update(Action::Open(screen));
    state
}

/// The account screen with its sign-in form taking text.
fn typing() -> AppState {
    let mut state = at(Screen::Auth);
    state.update(Action::Auth(AuthAction::BeginSignIn));
    assert!(state.is_editing());
    state
}

#[test]
fn no_two_bindings_claim_the_same_key_on_the_same_screen() {
    let states = vec![
        home(),
        help(),
        at(Screen::Auth),
        at(Screen::Sync),
        at(Screen::Restore),
        at(Screen::Share),
        at(Screen::Library),
        at(Screen::Labels),
        at(Screen::Repair),
        typing(),
        asking(),
    ];
    for state in states {
        let keys: Vec<_> = BINDINGS
            .iter()
            .filter(|b| applies(b, &state))
            .map(|b| (b.code, b.modifiers))
            .collect();
        for (i, k) in keys.iter().enumerate() {
            assert!(
                !keys[i + 1..].contains(k),
                "{k:?} bound twice on {:?}",
                state.screen()
            );
        }
    }
}

#[test]
fn home_opens_each_feature_screen() {
    for (c, screen) in [
        ('a', Screen::Auth),
        ('s', Screen::Sync),
        ('d', Screen::Restore),
        ('l', Screen::Share),
    ] {
        assert_eq!(
            action_for_key(&home(), key(KeyCode::Char(c))),
            Some(Action::Open(screen))
        );
        // The same letter means something else, or nothing, once that screen is on top.
        assert_ne!(
            action_for_key(&at(screen), key(KeyCode::Char(c))),
            Some(Action::Open(screen))
        );
    }
}

#[test]
fn each_feature_screen_binds_its_own_keys() {
    let cases = [
        (Screen::Auth, 'l', Action::Auth(AuthAction::BeginSignIn)),
        (Screen::Auth, 'o', Action::Auth(AuthAction::SignOut)),
        (
            Screen::Sync,
            's',
            Action::Sync(SyncAction::Run { from_start: false }),
        ),
        (
            Screen::Sync,
            'f',
            Action::Sync(SyncAction::Run { from_start: true }),
        ),
        (Screen::Restore, 'e', Action::Restore(RestoreAction::Edit)),
        (Screen::Share, 'c', Action::Share(ShareAction::BeginCreate)),
        (Screen::Share, 'x', Action::Share(ShareAction::BeginRevoke)),
    ];
    for (screen, c, action) in cases {
        assert_eq!(
            action_for_key(&at(screen), key(KeyCode::Char(c))),
            Some(action)
        );
    }
}

#[test]
fn while_typing_characters_are_text_not_bindings() {
    let state = typing();
    for c in ['q', '?', 'r', 'Q', ' '] {
        assert_eq!(
            action_for_key(&state, key(KeyCode::Char(c))),
            Some(Action::Form(FormKey::Char(c))),
            "{c:?}"
        );
    }
    // A shifted capital is text whether or not the terminal reports SHIFT.
    assert_eq!(
        action_for_key(
            &state,
            KeyEvent::new(KeyCode::Char('Q'), KeyModifiers::SHIFT)
        ),
        Some(Action::Form(FormKey::Char('Q')))
    );
    // An Alt chord is neither text nor a binding.
    assert_eq!(
        action_for_key(&state, KeyEvent::new(KeyCode::Char('q'), KeyModifiers::ALT)),
        None
    );
}

#[test]
fn while_typing_the_editing_keys_drive_the_form_and_ctrl_c_still_quits() {
    let state = typing();
    let cases = [
        (key(KeyCode::Enter), Action::Form(FormKey::Submit)),
        (key(KeyCode::Tab), Action::Form(FormKey::Next)),
        (key(KeyCode::Down), Action::Form(FormKey::Next)),
        (key(KeyCode::BackTab), Action::Form(FormKey::Previous)),
        (
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
            Action::Form(FormKey::Previous),
        ),
        (key(KeyCode::Up), Action::Form(FormKey::Previous)),
        (key(KeyCode::Backspace), Action::Form(FormKey::Backspace)),
        (key(KeyCode::Esc), Action::Form(FormKey::Cancel)),
        (
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Action::Quit,
        ),
    ];
    for (press, action) in cases {
        assert_eq!(action_for_key(&state, press), Some(action), "{press:?}");
    }
}

#[test]
fn the_editing_keys_do_nothing_when_no_form_takes_text() {
    let state = at(Screen::Auth);
    assert_eq!(action_for_key(&state, key(KeyCode::Enter)), None);
    assert_eq!(action_for_key(&state, key(KeyCode::Tab)), None);
    assert_eq!(
        action_for_key(&state, key(KeyCode::Esc)),
        Some(Action::Back)
    );
}

/// The account screen asking whether to sign out.
fn asking() -> AppState {
    let mut state = at(Screen::Auth);
    state.update(Action::Auth(AuthAction::SignOut));
    assert!(state.is_confirming());
    state
}

#[test]
fn while_asking_only_the_answers_and_ctrl_c_act() {
    let state = asking();
    for (code, action) in [
        (KeyCode::Char('y'), Some(Action::Answer(Answer::Yes))),
        (KeyCode::Char('n'), Some(Action::Answer(Answer::No))),
        (KeyCode::Esc, Some(Action::Answer(Answer::No))),
        // Neither a stray `o` nor `q` or `?` is an answer, and none leaves the question.
        (KeyCode::Char('o'), None),
        (KeyCode::Char('l'), None),
        (KeyCode::Char('q'), None),
        (KeyCode::Char('?'), None),
    ] {
        assert_eq!(action_for_key(&state, key(code)), action, "{code:?}");
    }
    assert_eq!(
        action_for_key(
            &state,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
        ),
        Some(Action::Quit)
    );
}

#[test]
fn y_and_n_mean_nothing_when_nothing_is_asked() {
    for state in [home(), at(Screen::Auth), at(Screen::Share)] {
        for c in ['y', 'n'] {
            assert_eq!(action_for_key(&state, key(KeyCode::Char(c))), None);
        }
    }
}
