//! Account screen tests: the sign-in form, the second factor, sign-out, the identity-provider
//! sign-in, and the frames they draw.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use capsule_sdk::auth::OidcDeviceAuthorization;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use secrecy::SecretString;

use super::*;
use crate::oidc::{DeviceCeremony, DeviceStep};
use crate::remote::{LoginStep, RemoteError};
use crate::tui::driver::{Driver, frame_text};
use crate::tui::ports::{Ports, ProviderSlot, browser_sign_in, device_sign_in};
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

// ---- the identity-provider sign-in (issue #856) ------------------------------------------

fn device_prompt() -> ProviderPrompt {
    ProviderPrompt::Device {
        verification_uri: "https://idp.test/device".to_owned(),
        user_code: Secret::new("WDJB-MJHT"),
        verification_uri_complete: Some(Secret::new("https://idp.test/device?user_code=WDJB-MJHT")),
        wait_secs: 600,
    }
}

fn browser_prompt(opened: bool) -> ProviderPrompt {
    ProviderPrompt::Browser {
        authorization_url: Secret::new("https://idp.test/authorize?state=st-4417"),
        opened,
        wait_secs: 300,
    }
}

/// A screen whose device sign-in has begun and shows its code.
fn waiting_on_device() -> AuthScreen {
    let mut screen = AuthScreen::default();
    screen.update(AuthAction::BeginProvider(ProviderFlow::Device));
    screen.update(AuthAction::ProviderBegun {
        attempt: 1,
        result: Ok(device_prompt()),
    });
    screen
}

#[test]
fn beginning_a_provider_sign_in_asks_the_port_to_begin_and_shows_it_is_working() {
    let mut screen = AuthScreen::default();
    assert_eq!(
        screen.update(AuthAction::BeginProvider(ProviderFlow::Device)),
        vec![Effect::Auth(AuthEffect::BeginProvider {
            attempt: 1,
            flow: ProviderFlow::Device,
        })]
    );
    assert!(screen.status().is_working());
    assert_eq!(
        screen
            .provider()
            .map(|wait| (wait.attempt, wait.prompt.is_none())),
        Some((1, true))
    );
    // One sign-in at a time: neither flow, a password sign-in nor a sign-out starts while it
    // runs.
    for action in [
        AuthAction::BeginProvider(ProviderFlow::Browser),
        AuthAction::BeginSignIn,
        AuthAction::SignOut,
    ] {
        assert!(screen.update(action).is_empty());
    }
    assert!(!screen.is_editing());
    assert!(!screen.is_confirming());
}

#[test]
fn a_begun_sign_in_shows_its_prompt_and_starts_waiting() {
    let mut screen = AuthScreen::default();
    screen.update(AuthAction::BeginProvider(ProviderFlow::Device));
    assert_eq!(
        screen.update(AuthAction::ProviderBegun {
            attempt: 1,
            result: Ok(device_prompt()),
        }),
        vec![Effect::Auth(AuthEffect::AwaitProvider { attempt: 1 })]
    );
    assert_eq!(
        screen.provider().and_then(|wait| wait.prompt.clone()),
        Some(device_prompt())
    );
    assert!(screen.status().is_working());
}

#[test]
fn approval_signs_in_and_reloads_the_session() {
    let mut screen = waiting_on_device();
    assert_eq!(
        screen.update(AuthAction::ProviderFinished {
            attempt: 1,
            result: Ok(LoginResult::SignedIn),
        }),
        vec![Effect::LoadSession]
    );
    assert_eq!(screen.status(), &Status::Done("Signed in.".to_owned()));
    assert!(screen.provider().is_none());
}

#[test]
fn approval_of_an_account_with_a_second_factor_asks_for_the_code() {
    let mut screen = waiting_on_device();
    assert!(
        screen
            .update(AuthAction::ProviderFinished {
                attempt: 1,
                result: Ok(LoginResult::SecondFactor { mfa_token: token() }),
            })
            .is_empty()
    );
    assert!(screen.awaiting_code());
    assert!(screen.is_editing());
    assert!(screen.provider().is_none());
    type_text(&mut screen, "123456");
    assert_eq!(
        screen.on_form_key(FormKey::Submit),
        vec![Effect::Auth(AuthEffect::VerifySecondFactor {
            mfa_token: token(),
            code: Secret::new("123456"),
        })]
    );
}

#[test]
fn a_sign_in_that_did_not_begin_or_finish_says_why() {
    let mut screen = AuthScreen::default();
    screen.update(AuthAction::BeginProvider(ProviderFlow::Browser));
    assert!(
        screen
            .update(AuthAction::ProviderBegun {
                attempt: 1,
                result: Err("no loopback".to_owned()),
            })
            .is_empty()
    );
    assert_eq!(
        screen.status(),
        &Status::Failed("Sign-in failed: no loopback".to_owned())
    );
    assert!(screen.provider().is_none());

    let mut screen = waiting_on_device();
    assert!(
        screen
            .update(AuthAction::ProviderFinished {
                attempt: 1,
                result: Err("expired".to_owned()),
            })
            .is_empty()
    );
    assert_eq!(
        screen.status(),
        &Status::Failed("Sign-in failed: expired".to_owned())
    );
    // And a new one can start.
    assert_eq!(
        screen.update(AuthAction::BeginProvider(ProviderFlow::Device)),
        vec![Effect::Auth(AuthEffect::BeginProvider {
            attempt: 2,
            flow: ProviderFlow::Device,
        })]
    );
}

#[test]
fn cancelling_stops_the_wait_and_drops_what_the_cancelled_attempt_answers() {
    let mut screen = waiting_on_device();
    assert_eq!(
        screen.update(AuthAction::CancelProvider),
        vec![Effect::Auth(AuthEffect::CancelProvider { attempt: 1 })]
    );
    assert_eq!(
        screen.status(),
        &Status::Failed("Sign-in cancelled.".to_owned())
    );
    assert!(screen.provider().is_none());
    // Nothing waits: cancelling again asks for nothing.
    assert!(screen.update(AuthAction::CancelProvider).is_empty());
    // The cancelled wait's answer changes nothing on screen...
    assert!(
        screen
            .update(AuthAction::ProviderFinished {
                attempt: 1,
                result: Err(crate::tui::ports::PROVIDER_CANCELLED.to_owned()),
            })
            .is_empty()
    );
    assert!(
        screen
            .update(AuthAction::ProviderFinished {
                attempt: 1,
                result: Ok(LoginResult::SecondFactor { mfa_token: token() }),
            })
            .is_empty()
    );
    assert!(!screen.awaiting_code());
    // ...but one approved just as it was cancelled stored a session the header should show.
    assert_eq!(
        screen.update(AuthAction::ProviderFinished {
            attempt: 1,
            result: Ok(LoginResult::SignedIn),
        }),
        vec![Effect::LoadSession]
    );
    assert_eq!(
        screen.status(),
        &Status::Failed("Sign-in cancelled.".to_owned())
    );
    assert!(
        screen
            .update(AuthAction::ProviderCancelled { attempt: 1 })
            .is_empty()
    );
}

#[test]
fn a_sign_in_cancelled_before_it_began_is_stopped_when_it_does() {
    let mut screen = AuthScreen::default();
    screen.update(AuthAction::BeginProvider(ProviderFlow::Device));
    screen.update(AuthAction::CancelProvider);
    // The port may begin it after the cancel ran: the reducer cancels it again, never waits.
    assert_eq!(
        screen.update(AuthAction::ProviderBegun {
            attempt: 1,
            result: Ok(device_prompt()),
        }),
        vec![Effect::Auth(AuthEffect::CancelProvider { attempt: 1 })]
    );
    assert!(screen.provider().is_none());
    assert!(
        screen
            .update(AuthAction::ProviderBegun {
                attempt: 1,
                result: Err("refused".to_owned()),
            })
            .is_empty()
    );
    assert_eq!(
        screen.status(),
        &Status::Failed("Sign-in cancelled.".to_owned())
    );
}

#[test]
fn a_provider_sign_in_drops_a_pending_challenge() {
    let mut screen = filled();
    screen.on_form_key(FormKey::Submit);
    screen.update(AuthAction::LoginFinished(Ok(LoginResult::SecondFactor {
        mfa_token: token(),
    })));
    screen.on_form_key(FormKey::Cancel);
    screen.update(AuthAction::BeginProvider(ProviderFlow::Device));
    assert!(!screen.awaiting_code());
    assert!(!screen.is_editing());
}

#[test]
fn each_failed_provider_effect_reports_its_own_outcome() {
    let reason = || "panicked".to_owned();
    assert_eq!(
        AuthEffect::BeginProvider {
            attempt: 3,
            flow: ProviderFlow::Browser
        }
        .failed(reason()),
        AuthAction::ProviderBegun {
            attempt: 3,
            result: Err(reason())
        }
    );
    assert_eq!(
        AuthEffect::AwaitProvider { attempt: 3 }.failed(reason()),
        AuthAction::ProviderFinished {
            attempt: 3,
            result: Err(reason())
        }
    );
    assert_eq!(
        AuthEffect::CancelProvider { attempt: 3 }.failed(reason()),
        AuthAction::ProviderCancelled { attempt: 3 }
    );
}

#[test]
fn no_debug_print_of_a_provider_sign_in_holds_the_code_or_the_address() {
    // `perform` logs every action with `?action`, and the reducer every action at trace:
    // whatever `Debug` prints reaches `tui.log`.
    let begun = [device_prompt(), browser_prompt(true)].map(|prompt| {
        Action::Auth(AuthAction::ProviderBegun {
            attempt: 1,
            result: Ok(prompt),
        })
    });
    let printed = format!("{begun:?} {begun:#?} {:?}", waiting_on_device());
    for secret in ["WDJB-MJHT", "st-4417"] {
        assert!(!printed.contains(secret), "{secret} leaked into {printed}");
    }
}

fn on_auth_with(prompt: ProviderPrompt, flow: ProviderFlow) -> AppState {
    let mut state = on_auth();
    state.update(Action::Auth(AuthAction::BeginProvider(flow)));
    state.update(Action::Auth(AuthAction::ProviderBegun {
        attempt: 1,
        result: Ok(prompt),
    }));
    state
}

fn draw_tall(state: &AppState) -> String {
    let mut terminal = Terminal::new(TestBackend::new(80, 20)).expect("test terminal");
    terminal
        .draw(|frame| view::render(frame, state))
        .expect("draw on a test backend");
    frame_text(terminal.backend().buffer())
}

#[test]
fn account_waiting_for_a_device_approval() {
    let frame = draw_tall(&on_auth_with(device_prompt(), ProviderFlow::Device));
    assert!(
        frame.contains("WDJB-MJHT"),
        "the code is on screen: {frame}"
    );
    insta::assert_snapshot!(frame);
}

#[test]
fn account_waiting_for_a_browser_sign_in_that_could_not_open_one() {
    insta::assert_snapshot!(draw_tall(&on_auth_with(
        browser_prompt(false),
        ProviderFlow::Browser
    )));
}

/// A device ceremony on a paused clock: pending once, then approved, settling as `settled`.
/// Its begin answers with `text`'s code and addresses.
struct ScriptedDevice {
    settled: fn() -> LoginStep,
    text: DeviceText,
    polls: Mutex<u32>,
}

/// The provider-written text a scripted device ceremony begins with.
struct DeviceText {
    user_code: &'static str,
    verification_uri: &'static str,
    verification_uri_complete: Option<&'static str>,
}

const PLAIN_DEVICE_TEXT: DeviceText = DeviceText {
    user_code: "WDJB-MJHT",
    verification_uri: "https://idp.test/device",
    verification_uri_complete: None,
};

impl DeviceCeremony for ScriptedDevice {
    async fn begin_device(&self) -> Result<OidcDeviceAuthorization, RemoteError> {
        let now = u64::try_from(jiff::Timestamp::now().as_second()).unwrap_or(0);
        Ok(OidcDeviceAuthorization {
            handle: SecretString::from("handle"),
            binding: SecretString::from("binding"),
            user_code: self.text.user_code.to_owned(),
            verification_uri: self.text.verification_uri.to_owned(),
            verification_uri_complete: self.text.verification_uri_complete.map(str::to_owned),
            interval_secs: 5,
            expires_by: now + 600,
        })
    }

    async fn poll_device(&self, _: &OidcDeviceAuthorization) -> Result<DeviceStep, RemoteError> {
        let mut polls = self.polls.lock().expect("unpoisoned");
        *polls += 1;
        Ok(if *polls == 1 {
            DeviceStep::Pending
        } else {
            DeviceStep::Settled((self.settled)())
        })
    }
}

/// The real two-effect plumbing ([`ProviderSlot`], [`device_sign_in`]) over a scripted
/// ceremony, and a second factor that accepts any code.
struct DevicePorts {
    ceremony: Arc<ScriptedDevice>,
    slot: ProviderSlot,
}

impl DevicePorts {
    fn new(settled: fn() -> LoginStep) -> Self {
        Self::writing(settled, PLAIN_DEVICE_TEXT)
    }

    /// Ports whose ceremony begins with `text`.
    fn writing(settled: fn() -> LoginStep, text: DeviceText) -> Self {
        Self {
            ceremony: Arc::new(ScriptedDevice {
                settled,
                text,
                polls: Mutex::new(0),
            }),
            slot: ProviderSlot::default(),
        }
    }
}

impl Ports for DevicePorts {
    async fn session_summary(&self) -> Result<SessionSummary, String> {
        Ok(SessionSummary::SignedOut)
    }

    async fn begin_provider(
        &self,
        attempt: u64,
        flow: ProviderFlow,
    ) -> Result<ProviderPrompt, String> {
        assert_eq!(flow, ProviderFlow::Device);
        let ceremony = Arc::clone(&self.ceremony);
        self.slot
            .begin(attempt, move |prompt| async move {
                device_sign_in(ceremony.as_ref(), prompt, Duration::from_mins(15)).await
            })
            .await
    }

    async fn await_provider(&self, attempt: u64) -> Result<LoginResult, String> {
        self.slot.wait(attempt).await
    }

    async fn cancel_provider(&self, attempt: u64) {
        self.slot.cancel(attempt);
    }

    async fn verify_second_factor(&self, mfa_token: Secret, code: Secret) -> Result<(), String> {
        assert_eq!(mfa_token.expose(), "provider-challenge");
        assert_eq!(code.expose(), "123456");
        Ok(())
    }
}

fn press(d: &mut Driver<DevicePorts>, c: char) {
    d.press(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
}

#[tokio::test(start_paused = true)]
async fn the_d_key_shows_the_code_at_once_and_approval_lands_signed_in() {
    let mut d = Driver::new(DevicePorts::new(|| LoginStep::Complete), 80, 20);
    d.settle().await;
    press(&mut d, 'a');
    press(&mut d, 'd');
    // The press is visible before any port answered.
    assert!(d.frame().contains("Starting the sign-in"), "{}", d.frame());
    // Settling runs the begin (which answers with the code while the poll keeps going), the
    // wait, and the session read the approval asks for.
    d.settle().await;
    assert_eq!(
        d.state().auth().status(),
        &Status::Done("Signed in.".to_owned())
    );
    assert!(d.pending().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_device_approval_for_an_account_with_a_second_factor_asks_for_the_code() {
    let mut d = Driver::new(
        DevicePorts::new(|| LoginStep::SecondFactorRequired {
            mfa_token: SecretString::from("provider-challenge"),
        }),
        80,
        20,
    );
    d.settle().await;
    press(&mut d, 'a');
    press(&mut d, 'd');
    d.settle().await;
    assert!(d.state().auth().awaiting_code());
    d.type_keys("123456");
    d.press(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    d.settle().await;
    assert_eq!(
        d.state().auth().status(),
        &Status::Done("Signed in.".to_owned())
    );
}

#[tokio::test(start_paused = true)]
async fn the_code_is_on_screen_while_the_wait_runs_and_x_cancels_it() {
    let mut d = Driver::new(DevicePorts::new(|| LoginStep::Complete), 80, 20);
    d.settle().await;
    press(&mut d, 'a');
    press(&mut d, 'd');
    assert_eq!(
        d.pending(),
        vec![Effect::Auth(AuthEffect::BeginProvider {
            attempt: 1,
            flow: ProviderFlow::Device,
        })]
    );
    // Run only the begin: its answer shows the code and asks for the wait, which runs on.
    assert!(d.settle_one().await);
    assert!(d.frame().contains("WDJB-MJHT"), "{}", d.frame());
    assert!(
        d.frame().contains("x cancels the sign-in."),
        "{}",
        d.frame()
    );
    assert_eq!(
        d.pending(),
        vec![Effect::Auth(AuthEffect::AwaitProvider { attempt: 1 })]
    );
    // Leaving the screen cancels nothing; back on it, x does.
    d.press(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(d.state().screen(), Screen::Home);
    assert!(d.state().auth().provider().is_some());
    press(&mut d, 'a');
    press(&mut d, 'x');
    assert_eq!(
        d.pending(),
        vec![
            Effect::Auth(AuthEffect::AwaitProvider { attempt: 1 }),
            Effect::Auth(AuthEffect::CancelProvider { attempt: 1 }),
        ]
    );
    assert!(!d.frame().contains("WDJB-MJHT"), "{}", d.frame());
    // The driver performs effects one after another, so this wait runs to the approval before
    // the cancel reaches the port (the port-level abort is `ports::tests`'): the cancelled
    // attempt's answer only reloads the session it stored.
    d.settle().await;
    assert_eq!(
        d.state().auth().status(),
        &Status::Failed("Sign-in cancelled.".to_owned())
    );
}

/// Whether `frame` holds a control character other than the line breaks `frame_text` adds.
fn holds_a_control_character(frame: &str) -> bool {
    frame.chars().any(|c| c.is_control() && c != '\n')
}

/// Provider-written device text carrying escape sequences: a clear-screen, a window-title OSC
/// ended by a bell, and a colour change.
const ESCAPING_DEVICE_TEXT: DeviceText = DeviceText {
    user_code: "WDJB\u{1b}[2J-MJHT",
    verification_uri: "https://idp.test/\u{1b}]0;pwned\u{7}device",
    verification_uri_complete: Some("https://idp.test/d?c=\u{1b}[31mWDJB"),
};

#[tokio::test(start_paused = true)]
async fn a_device_prompt_drops_the_control_characters_the_provider_wrote() {
    let mut d = Driver::new(
        DevicePorts::writing(|| LoginStep::Complete, ESCAPING_DEVICE_TEXT),
        80,
        20,
    );
    d.settle().await;
    press(&mut d, 'a');
    press(&mut d, 'd');
    // Run only the begin, through the real `device_sign_in`: its prompt is what the screen holds.
    assert!(d.settle_one().await);
    let prompt = d
        .state()
        .auth()
        .provider()
        .and_then(|wait| wait.prompt.clone());
    let Some(ProviderPrompt::Device {
        verification_uri,
        user_code,
        verification_uri_complete,
        ..
    }) = prompt
    else {
        panic!("a device prompt: {prompt:?}");
    };
    assert_eq!(user_code.expose(), "WDJB[2J-MJHT");
    assert_eq!(verification_uri, "https://idp.test/]0;pwneddevice");
    assert_eq!(
        verification_uri_complete.as_ref().map(Secret::expose),
        Some("https://idp.test/d?c=[31mWDJB")
    );
    let frame = d.frame();
    assert!(!holds_a_control_character(&frame), "{frame:?}");
    for shown in [
        "and enter this code: WDJB[2J-MJHT",
        "https://idp.test/]0;pwneddevice",
        "https://idp.test/d?c=[31mWDJB",
    ] {
        assert!(frame.contains(shown), "{shown} is on screen: {frame}");
    }
}

/// A browser ceremony whose authorization address carries a clear-screen escape, and which is
/// never completed.
struct EscapingBrowser;

impl crate::oidc::Ceremony for EscapingBrowser {
    async fn begin(
        &self,
        redirect_uri: &str,
    ) -> Result<capsule_sdk::auth::OidcAuthorization, RemoteError> {
        let now = u64::try_from(jiff::Timestamp::now().as_second()).unwrap_or(0);
        Ok(capsule_sdk::auth::OidcAuthorization {
            authorization_url: format!(
                "https://idp.test/authorize\u{1b}[2J?redirect_uri={redirect_uri}"
            ),
            state: SecretString::from("state"),
            binding: SecretString::from("binding"),
            expires_by: now + 600,
        })
    }

    async fn complete(
        &self,
        _: &capsule_sdk::auth::OidcAuthorization,
        _: &str,
        _: &str,
    ) -> Result<LoginStep, RemoteError> {
        unreachable!("no redirect is followed")
    }
}

#[tokio::test]
async fn a_browser_prompt_drops_the_control_characters_the_provider_wrote() {
    let slot = ProviderSlot::default();
    let begun = slot
        .begin(1, |prompt| async move {
            browser_sign_in(&EscapingBrowser, prompt, Duration::from_mins(5), |_| false).await
        })
        .await
        .expect("begun");
    slot.cancel(1);
    let ProviderPrompt::Browser {
        authorization_url, ..
    } = &begun
    else {
        panic!("a browser prompt: {begun:?}");
    };
    assert!(
        authorization_url
            .expose()
            .starts_with("https://idp.test/authorize[2J?redirect_uri="),
        "{}",
        authorization_url.expose()
    );
    assert!(!authorization_url.expose().contains(char::is_control));
    let frame = draw_tall(&on_auth_with(begun, ProviderFlow::Browser));
    assert!(!holds_a_control_character(&frame), "{frame:?}");
    assert!(
        frame.contains("https://idp.test/authorize[2J?redirect_uri="),
        "{frame}"
    );
}
