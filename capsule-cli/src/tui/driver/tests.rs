//! Scripted key-driver tests: whole interactions, key by key, with the ports mocked.
//!
//! The responsiveness bound (no input waits more than one frame for a visible response) is
//! asserted structurally: the reducer is synchronous, the driver draws right after each press,
//! and these tests check that the frame drawn after a press already differs from the one before
//! it, while every effect the press asked for is still pending.

use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::tui::state::{Loadable, Screen, SessionSummary};

/// Answers every session read with `answer`, counting the reads.
struct Mock {
    answer: Result<SessionSummary, String>,
    reads: AtomicUsize,
}

impl Mock {
    fn new(answer: Result<SessionSummary, String>) -> Self {
        Self {
            answer,
            reads: AtomicUsize::new(0),
        }
    }
}

impl Ports for Mock {
    async fn session_summary(&self) -> Result<SessionSummary, String> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.answer.clone()
    }
}

fn driver(answer: Result<SessionSummary, String>) -> Driver<Mock> {
    Driver::new(Mock::new(answer), 80, 12)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[tokio::test]
async fn the_first_frame_shows_loading_and_settling_fills_it() {
    let mut d = driver(Ok(SessionSummary::SignedOut));
    assert_eq!(d.pending(), vec![Effect::LoadSession]);
    assert!(d.frame().contains("Reading the session…"), "{}", d.frame());
    d.settle().await;
    assert!(d.pending().is_empty());
    assert!(d.frame().contains("Not signed in."), "{}", d.frame());
    assert_eq!(d.ports.reads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn every_press_that_sends_an_action_changes_the_next_frame() {
    let mut d = driver(Ok(SessionSummary::SignedIn {
        access_token_fresh: true,
    }));
    d.settle().await;
    let script = [
        key(KeyCode::Char('?')),
        key(KeyCode::Esc),
        key(KeyCode::Char('r')),
    ];
    for press in script {
        let before = d.frame();
        let action = d.press(press);
        assert!(action.is_some(), "{press:?} is bound here");
        assert_ne!(d.frame(), before, "{press:?} drew nothing new");
    }
    // The reload's read is still pending: the press above was visible without it.
    assert_eq!(d.pending(), vec![Effect::LoadSession]);
}

#[tokio::test]
async fn reload_shows_loading_before_the_port_answers_and_the_answer_after() {
    let mut d = driver(Ok(SessionSummary::SignedOut));
    d.settle().await;
    d.press(key(KeyCode::Char('r')));
    assert_eq!(d.state().session(), &Loadable::Loading);
    assert!(d.frame().contains("Session: loading…"), "{}", d.frame());
    d.settle().await;
    assert_eq!(
        d.state().session(),
        &Loadable::Ready(SessionSummary::SignedOut)
    );
    assert_eq!(d.ports.reads.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn reload_mashed_while_loading_reads_once() {
    let mut d = driver(Ok(SessionSummary::SignedOut));
    d.settle().await;
    d.type_keys("rrr");
    d.settle().await;
    assert_eq!(d.ports.reads.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_failed_read_is_shown_with_its_reason() {
    let mut d = driver(Err("could not read the session: boom".to_owned()));
    d.settle().await;
    assert!(d.frame().contains("Session unavailable"), "{}", d.frame());
    assert!(d.frame().contains("boom"), "{}", d.frame());
}

#[tokio::test]
async fn help_then_back_then_quit() {
    let mut d = driver(Ok(SessionSummary::SignedOut));
    d.settle().await;
    d.type_keys("?");
    assert_eq!(d.state().screen(), Screen::Help);
    d.press(key(KeyCode::Esc));
    assert_eq!(d.state().screen(), Screen::Home);
    d.type_keys("q");
    assert!(!d.state().is_running());
}

#[tokio::test]
async fn an_effect_outcome_dispatched_while_on_help_lands_under_it() {
    let mut d = driver(Ok(SessionSummary::SignedOut));
    d.type_keys("?");
    d.dispatch(Action::SessionLoaded(Ok(SessionSummary::SignedIn {
        access_token_fresh: true,
    })));
    assert_eq!(d.state().screen(), Screen::Help);
    assert!(d.frame().contains("Signed in"), "{}", d.frame());
}

/// Signs in only with an authenticator code, and counts the sign-ins asked for.
struct TwoFactor {
    logins: AtomicUsize,
}

impl Ports for TwoFactor {
    async fn session_summary(&self) -> Result<SessionSummary, String> {
        Ok(if self.logins.load(Ordering::SeqCst) == 0 {
            SessionSummary::SignedOut
        } else {
            SessionSummary::SignedIn {
                access_token_fresh: true,
            }
        })
    }

    async fn login(
        &self,
        email: String,
        password: crate::tui::form::Secret,
    ) -> Result<crate::tui::auth::LoginResult, String> {
        assert_eq!(
            (email.as_str(), password.expose()),
            ("me@example.com", "pw")
        );
        Ok(crate::tui::auth::LoginResult::SecondFactor {
            mfa_token: crate::tui::form::Secret::new("challenge"),
        })
    }

    async fn verify_second_factor(
        &self,
        mfa_token: crate::tui::form::Secret,
        code: String,
    ) -> Result<(), String> {
        assert_eq!((mfa_token.expose(), code.as_str()), ("challenge", "123456"));
        self.logins.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// The whole sign-in, key by key: open the account screen, type the credentials (a `q` in the
/// password is text, not quit), answer the second factor, and land signed in.
#[tokio::test]
async fn signing_in_with_a_second_factor_key_by_key() {
    let mut d = Driver::new(
        TwoFactor {
            logins: AtomicUsize::new(0),
        },
        80,
        16,
    );
    d.settle().await;
    d.type_keys("al");
    assert_eq!(d.state().screen(), Screen::Auth);
    assert!(d.state().is_editing());
    d.type_keys("me@example.com");
    d.press(key(KeyCode::Tab));
    d.type_keys("pq");
    d.press(key(KeyCode::Backspace));
    d.type_keys("w");
    assert!(d.state().is_running(), "q in a field is text");
    assert!(!d.frame().contains("pw"), "{}", d.frame());
    d.press(key(KeyCode::Enter));
    assert!(d.frame().contains("Signing in…"), "{}", d.frame());
    d.settle().await;
    assert!(d.frame().contains("Authenticator code"), "{}", d.frame());
    d.type_keys("123456");
    d.press(key(KeyCode::Enter));
    d.settle().await;
    assert_eq!(
        d.state().session(),
        &Loadable::Ready(SessionSummary::SignedIn {
            access_token_fresh: true
        })
    );
    assert!(d.frame().contains("Signed in."), "{}", d.frame());
    // Not typing any more: Esc goes back, q quits.
    d.press(key(KeyCode::Esc));
    assert_eq!(d.state().screen(), Screen::Home);
    d.type_keys("q");
    assert!(!d.state().is_running());
}

/// A sync run from the sync screen, with the default port answering that it is not offered:
/// the failure is shown, not waited on.
#[tokio::test]
async fn a_port_a_double_left_out_shows_as_a_failure() {
    let mut d = driver(Ok(SessionSummary::SignedOut));
    d.settle().await;
    d.type_keys("ss");
    assert!(d.frame().contains("Syncing…"), "{}", d.frame());
    d.settle().await;
    assert!(
        d.frame().contains(crate::tui::ports::NOT_OFFERED),
        "{}",
        d.frame()
    );
}

#[test]
fn frame_text_trims_each_row() {
    let buffer = Buffer::with_lines(["ab  ", "    "]);
    assert_eq!(frame_text(&buffer), "ab\n\n");
}
