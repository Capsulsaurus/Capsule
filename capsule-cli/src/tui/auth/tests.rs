//! Account screen tests: the sign-in form, the second factor, sign-out, and the frames they draw.

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::*;
use crate::tui::driver::frame_text;
use crate::tui::state::{Action, AppState, Screen};
use crate::tui::view;

fn type_text(screen: &mut AuthScreen, text: &str) {
    for c in text.chars() {
        assert!(screen.on_form_key(FormKey::Char(c)).is_empty());
    }
}

/// A screen with the credentials typed in and not yet submitted.
fn filled() -> AuthScreen {
    let mut screen = AuthScreen::default();
    screen.update(AuthAction::BeginSignIn);
    type_text(&mut screen, "me@example.com");
    screen.on_form_key(FormKey::Next);
    type_text(&mut screen, "hunter2");
    screen
}

fn token() -> Secret {
    Secret::new("mfa-token")
}

#[test]
fn begin_sign_in_shows_the_form_taking_text() {
    let mut screen = AuthScreen::default();
    assert!(!screen.is_editing());
    assert!(screen.update(AuthAction::BeginSignIn).is_empty());
    assert!(screen.is_editing());
    assert_eq!(screen.form().fields().len(), 2);
}

#[test]
fn submitting_credentials_signs_in_and_forgets_the_password() {
    let mut screen = filled();
    let effects = screen.on_form_key(FormKey::Submit);
    assert_eq!(
        effects,
        vec![Effect::Auth(AuthEffect::Login {
            email: "me@example.com".to_owned(),
            password: Secret::new("hunter2"),
        })]
    );
    assert!(!screen.is_editing());
    assert_eq!(screen.status(), &Status::Working("Signing in…"));
    assert!(screen.form().secret(PASSWORD).is_empty());
    // A second submission while the first is in flight asks for nothing.
    screen.update(AuthAction::BeginSignIn);
    assert!(!screen.is_editing());
}

#[test]
fn an_empty_field_is_refused_before_any_request() {
    let mut screen = AuthScreen::default();
    screen.update(AuthAction::BeginSignIn);
    type_text(&mut screen, "me@example.com");
    assert!(screen.on_form_key(FormKey::Submit).is_empty());
    assert!(screen.is_editing());
    assert_eq!(
        screen.status(),
        &Status::Failed("Enter an email and a password.".to_owned())
    );
}

#[test]
fn a_successful_sign_in_reloads_the_session() {
    let mut screen = filled();
    screen.on_form_key(FormKey::Submit);
    assert_eq!(
        screen.update(AuthAction::LoginFinished(Ok(LoginResult::SignedIn))),
        vec![Effect::LoadSession]
    );
    assert_eq!(screen.status(), &Status::Done("Signed in.".to_owned()));
}

#[test]
fn a_second_factor_asks_for_the_code_and_sends_it_with_the_challenge() {
    let mut screen = filled();
    screen.on_form_key(FormKey::Submit);
    assert!(
        screen
            .update(AuthAction::LoginFinished(Ok(LoginResult::SecondFactor {
                mfa_token: token()
            })))
            .is_empty()
    );
    assert!(screen.awaiting_code());
    assert!(screen.is_editing());
    type_text(&mut screen, " 123456 ");
    assert_eq!(
        screen.on_form_key(FormKey::Submit),
        vec![Effect::Auth(AuthEffect::VerifySecondFactor {
            mfa_token: token(),
            code: Secret::new("123456"),
        })]
    );
    assert_eq!(
        screen.update(AuthAction::SecondFactorFinished(Ok(()))),
        vec![Effect::LoadSession]
    );
    assert!(!screen.awaiting_code());
}

#[test]
fn a_refused_code_starts_the_sign_in_over() {
    let mut screen = filled();
    screen.on_form_key(FormKey::Submit);
    screen.update(AuthAction::LoginFinished(Ok(LoginResult::SecondFactor {
        mfa_token: token(),
    })));
    type_text(&mut screen, "000000");
    screen.on_form_key(FormKey::Submit);
    screen.update(AuthAction::SecondFactorFinished(Err(
        "wrong code".to_owned()
    )));
    assert!(!screen.awaiting_code());
    assert_eq!(
        screen.status(),
        &Status::Failed("Sign-in failed: wrong code".to_owned())
    );
}

#[test]
fn cancelling_the_code_drops_the_challenge() {
    let mut screen = filled();
    screen.on_form_key(FormKey::Submit);
    screen.update(AuthAction::LoginFinished(Ok(LoginResult::SecondFactor {
        mfa_token: token(),
    })));
    screen.on_form_key(FormKey::Cancel);
    assert!(!screen.awaiting_code());
    assert!(!screen.is_editing());
}

#[test]
fn begin_sign_in_keeps_the_email_but_not_the_password() {
    let mut screen = filled();
    screen.on_form_key(FormKey::Submit);
    screen.update(AuthAction::LoginFinished(Err("refused".to_owned())));
    screen.update(AuthAction::BeginSignIn);
    assert_eq!(screen.form().value(EMAIL), "me@example.com");
    assert!(screen.form().secret(PASSWORD).is_empty());
}

#[test]
fn sign_out_reloads_the_session_whatever_the_server_said() {
    for (result, status) in [
        (Ok(true), Status::Done("Signed out.".to_owned())),
        (
            Ok(false),
            Status::Done("You were not signed in.".to_owned()),
        ),
        (
            Err("offline".to_owned()),
            Status::Failed("Sign-out: offline".to_owned()),
        ),
    ] {
        let mut screen = AuthScreen::default();
        assert!(screen.update(AuthAction::SignOut).is_empty(), "asks first");
        assert_eq!(
            screen.on_answer(Answer::Yes),
            vec![Effect::Auth(AuthEffect::Logout)]
        );
        assert!(
            screen.update(AuthAction::SignOut).is_empty(),
            "one at a time"
        );
        assert!(!screen.is_confirming(), "no second question while it runs");
        assert_eq!(
            screen.update(AuthAction::SignOutFinished(result)),
            vec![Effect::LoadSession]
        );
        assert_eq!(screen.status(), &status);
    }
}

#[test]
fn sign_out_asks_before_it_acts() {
    let mut screen = AuthScreen::default();
    assert!(screen.update(AuthAction::SignOut).is_empty());
    assert!(screen.is_confirming());
    assert!(matches!(screen.status(), Status::Asking(_)));
    // Asking again, or starting a sign-in, leaves the question as it was.
    assert!(screen.update(AuthAction::SignOut).is_empty());
    screen.update(AuthAction::BeginSignIn);
    assert!(screen.is_confirming());
    assert!(!screen.is_editing());
    // No: nothing is asked of the ports, and the question is gone.
    assert!(screen.on_answer(Answer::No).is_empty());
    assert!(!screen.is_confirming());
    assert_eq!(screen.status(), &Status::Idle);
    // An answer with no question pending does nothing, in particular never signs out.
    assert!(screen.on_answer(Answer::Yes).is_empty());
    assert_eq!(screen.status(), &Status::Idle);
}

#[test]
fn each_failed_effect_reports_its_own_outcome() {
    let reason = || "panicked".to_owned();
    assert_eq!(
        AuthEffect::Login {
            email: String::new(),
            password: Secret::default()
        }
        .failed(reason()),
        AuthAction::LoginFinished(Err(reason()))
    );
    assert_eq!(
        AuthEffect::VerifySecondFactor {
            mfa_token: token(),
            code: Secret::default()
        }
        .failed(reason()),
        AuthAction::SecondFactorFinished(Err(reason()))
    );
    assert_eq!(
        AuthEffect::Logout.failed(reason()),
        AuthAction::SignOutFinished(Err(reason()))
    );
}

#[test]
fn nothing_secret_reaches_debug() {
    let mut screen = filled();
    let effects = screen.on_form_key(FormKey::Submit);
    let mut second = filled();
    second.on_form_key(FormKey::Submit);
    second.update(AuthAction::LoginFinished(Ok(LoginResult::SecondFactor {
        mfa_token: token(),
    })));
    let printed = format!("{:?} {effects:?} {second:?} {:?}", filled(), second);
    assert!(!printed.contains("hunter2"), "{printed}");
    assert!(!printed.contains("mfa-token"), "{printed}");
}

#[test]
fn no_debug_print_of_an_effect_holds_the_code_the_password_or_the_challenge() {
    // `state.rs`, `ports.rs` and `runtime.rs` log effects with `?effect`; whatever `Debug`
    // prints reaches `tui.log`.
    let verify = Effect::Auth(AuthEffect::VerifySecondFactor {
        mfa_token: Secret::new("challenge-4417"),
        code: Secret::new("918273"),
    });
    let login = Effect::Auth(AuthEffect::Login {
        email: "me@example.com".to_owned(),
        password: Secret::new("hunter2-4417"),
    });
    let finished = Action::Auth(AuthAction::LoginFinished(Ok(LoginResult::SecondFactor {
        mfa_token: Secret::new("challenge-4417"),
    })));
    for printed in [
        format!("{verify:?}"),
        format!("{verify:#?}"),
        format!("{login:?}"),
        format!("{finished:?}"),
    ] {
        for secret in ["918273", "hunter2", "challenge-4417"] {
            assert!(!printed.contains(secret), "{secret} leaked into {printed}");
        }
    }
}

#[test]
fn the_code_is_masked_on_screen_and_cleared_on_submit() {
    let mut screen = filled();
    screen.on_form_key(FormKey::Submit);
    screen.update(AuthAction::LoginFinished(Ok(LoginResult::SecondFactor {
        mfa_token: token(),
    })));
    type_text(&mut screen, "918273");
    assert!(screen.form().fields()[CODE].is_secret());
    screen.on_form_key(FormKey::Submit);
    assert!(screen.form().secret(CODE).is_empty());
}

fn draw(state: &AppState) -> String {
    let mut terminal = Terminal::new(TestBackend::new(80, 14)).expect("test terminal");
    terminal
        .draw(|frame| view::render(frame, state))
        .expect("draw on a test backend");
    frame_text(terminal.backend().buffer())
}

fn on_auth() -> AppState {
    let (mut state, _) = AppState::init();
    state.update(Action::SessionLoaded(Ok(SessionSummary::SignedOut)));
    state.update(Action::Open(Screen::Auth));
    state
}

fn typed(state: &mut AppState, text: &str) {
    for c in text.chars() {
        state.update(Action::Form(FormKey::Char(c)));
    }
}

#[test]
fn account_signed_out() {
    insta::assert_snapshot!(draw(&on_auth()));
}

#[test]
fn account_typing_the_password() {
    let mut state = on_auth();
    state.update(Action::Auth(AuthAction::BeginSignIn));
    typed(&mut state, "me@example.com");
    state.update(Action::Form(FormKey::Next));
    typed(&mut state, "hunter2");
    let frame = draw(&state);
    assert!(!frame.contains("hunter2"), "{frame}");
    insta::assert_snapshot!(frame);
}

#[test]
fn account_asking_for_the_code() {
    let mut state = on_auth();
    state.update(Action::Auth(AuthAction::LoginFinished(Ok(
        LoginResult::SecondFactor { mfa_token: token() },
    ))));
    insta::assert_snapshot!(draw(&state));
}

#[test]
fn account_asking_whether_to_sign_out() {
    let mut state = on_auth();
    state.update(Action::Auth(AuthAction::SignOut));
    insta::assert_snapshot!(draw(&state));
}

#[test]
fn account_sign_in_refused() {
    let mut state = on_auth();
    state.update(Action::Auth(AuthAction::LoginFinished(Err(
        "Invalid email or password.".to_owned(),
    ))));
    insta::assert_snapshot!(draw(&state));
}
