//! The account screen: sign in with a password or through the server's identity provider (and
//! an authenticator code when the account asks for one), sign out, and see the local session's
//! state.
//!
//! The scriptable twins are `capsule auth login --json`, `capsule auth login --oidc [--device]
//! --json`, `capsule auth logout --json` and `capsule auth status --json`. Creating an account
//! stays on `capsule auth register`, which the screen names.
//!
//! # Signing in through the identity provider
//!
//! Both of [`crate::oidc`]'s flows show the person something (the authorization address, or the
//! device code and where to enter it) and then wait for up to fifteen minutes. The effect
//! contract answers each effect with one action, so the sign-in is two effects:
//!
//! 1. [`AuthEffect::BeginProvider`] begins the ceremony and answers
//!    [`AuthAction::ProviderBegun`] with the [`ProviderPrompt`] to show, as soon as there is one.
//!    The port keeps the begun ceremony waiting on a task of its own.
//! 2. [`AuthEffect::AwaitProvider`] waits for that task and answers
//!    [`AuthAction::ProviderFinished`]: signed in, a second factor to ask for, or why not.
//!
//! Each sign-in carries an attempt number, so an answer for one the person cancelled
//! ([`AuthEffect::CancelProvider`]) is told apart from the current one. Leaving the screen
//! cancels nothing: the outcome lands on this screen's state whichever screen is on top.
//! Quitting drops the wait, and the server expires the ceremony on its own. The device code and
//! the authorization address travel as [`Secret`]s, so neither reaches `tui.log`.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::form::{Field, Form, FormKey, FormOutcome, Secret, Status};
use super::state::{Answer, Effect, Loadable, SessionSummary};

/// How far a password sign-in got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginResult {
    /// Signed in; the session is stored.
    SignedIn,
    /// The password verified and the account asks for an authenticator code.
    SecondFactor {
        /// The challenge the code answers. Good once, and for five minutes.
        mfa_token: Secret,
    },
}

/// Which of the identity provider's sign-ins to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderFlow {
    /// In a browser on this machine, over a loopback redirect (`capsule auth login --oidc`).
    Browser,
    /// On any other device, with a code (`capsule auth login --oidc --device`).
    Device,
}

/// What a begun identity-provider sign-in asks the person to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderPrompt {
    /// Open the authorization address in a browser on this machine.
    Browser {
        /// The provider's authorization address. A [`Secret`]: it carries the ceremony's
        /// `state`.
        authorization_url: Secret,
        /// Whether the platform's opener was started with it.
        opened: bool,
        /// How long the sign-in waits for the redirect, in seconds.
        wait_secs: u64,
    },
    /// Open the verification address on another device and enter the code.
    Device {
        /// Where to enter the code. Provider-authored, its control characters dropped.
        verification_uri: String,
        /// The code to enter. A [`Secret`]: while the ceremony lives, it signs this terminal in.
        user_code: Secret,
        /// The verification address with the code already in it, when the provider gave one.
        verification_uri_complete: Option<Secret>,
        /// How long the sign-in polls for an approval, in seconds.
        wait_secs: u64,
    },
}

/// What the account screen does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthAction {
    /// Show the sign-in form and start typing into it.
    BeginSignIn,
    /// Start a sign-in through the identity provider.
    BeginProvider(ProviderFlow),
    /// Stop waiting on the identity-provider sign-in.
    CancelProvider,
    /// [`AuthEffect::BeginProvider`] finished: what to show, or why the sign-in did not begin.
    ProviderBegun {
        /// The attempt it began.
        attempt: u64,
        /// The prompt, or the reason.
        result: Result<ProviderPrompt, String>,
    },
    /// [`AuthEffect::AwaitProvider`] finished.
    ProviderFinished {
        /// The attempt it waited on.
        attempt: u64,
        /// How far the sign-in got, or why it stopped.
        result: Result<LoginResult, String>,
    },
    /// [`AuthEffect::CancelProvider`] finished.
    ProviderCancelled {
        /// The attempt cancelled.
        attempt: u64,
    },
    /// Ask whether to sign out; [`AuthScreen::on_answer`] takes the answer.
    SignOut,
    /// [`AuthEffect::Login`] finished.
    LoginFinished(Result<LoginResult, String>),
    /// [`AuthEffect::VerifySecondFactor`] finished.
    SecondFactorFinished(Result<(), String>),
    /// [`AuthEffect::Logout`] finished: whether a session was signed out.
    SignOutFinished(Result<bool, String>),
}

/// The work the account screen asks the ports for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthEffect {
    /// Sign in with a password; answered by [`AuthAction::LoginFinished`].
    Login {
        /// The account's email.
        email: String,
        /// Its password.
        password: Secret,
    },
    /// Answer a second-factor challenge; answered by [`AuthAction::SecondFactorFinished`].
    VerifySecondFactor {
        /// The challenge from [`LoginResult::SecondFactor`].
        mfa_token: Secret,
        /// The code the authenticator app shows. A [`Secret`]: it is a live credential for
        /// its validity window, so it never reaches a `Debug` print or `tui.log`.
        code: Secret,
    },
    /// Sign out; answered by [`AuthAction::SignOutFinished`].
    Logout,
    /// Begin an identity-provider sign-in and keep it waiting; answered by
    /// [`AuthAction::ProviderBegun`] once there is something to show.
    BeginProvider {
        /// This sign-in's number, increasing for the life of the screen.
        attempt: u64,
        /// Which sign-in.
        flow: ProviderFlow,
    },
    /// Wait for the begun sign-in to finish; answered by [`AuthAction::ProviderFinished`].
    AwaitProvider {
        /// The attempt to wait on.
        attempt: u64,
    },
    /// Stop the begun sign-in's wait; answered by [`AuthAction::ProviderCancelled`].
    CancelProvider {
        /// The attempt to stop.
        attempt: u64,
    },
}

impl AuthEffect {
    /// The action reporting this effect failing with `reason`.
    #[must_use]
    pub fn failed(self, reason: String) -> AuthAction {
        match self {
            Self::Login { .. } => AuthAction::LoginFinished(Err(reason)),
            Self::VerifySecondFactor { .. } => AuthAction::SecondFactorFinished(Err(reason)),
            Self::Logout => AuthAction::SignOutFinished(Err(reason)),
            Self::BeginProvider { attempt, .. } => AuthAction::ProviderBegun {
                attempt,
                result: Err(reason),
            },
            Self::AwaitProvider { attempt } => AuthAction::ProviderFinished {
                attempt,
                result: Err(reason),
            },
            Self::CancelProvider { attempt } => AuthAction::ProviderCancelled { attempt },
        }
    }
}

/// Which form the screen shows.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Stage {
    /// Email and password.
    Credentials,
    /// The authenticator code, answering this challenge.
    SecondFactor { mfa_token: Secret },
}

/// The email field of the credentials form.
const EMAIL: usize = 0;
/// The password field of the credentials form.
const PASSWORD: usize = 1;
/// The one field of the second-factor form.
const CODE: usize = 0;

fn credentials_form() -> Form {
    Form::new(vec![Field::text("Email"), Field::secret("Password")])
}

fn code_form() -> Form {
    Form::new(vec![Field::secret("Authenticator code")])
}

/// The account screen's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthScreen {
    stage: Stage,
    form: Form,
    editing: bool,
    /// Whether the sign-out question is on screen, waiting for a yes or no.
    confirming: bool,
    status: Status,
    /// The identity-provider sign-in being waited on.
    provider: Option<ProviderWait>,
    /// The last attempt number handed out.
    attempts: u64,
}

/// The identity-provider sign-in in flight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderWait {
    /// Its attempt number.
    pub attempt: u64,
    /// Which sign-in.
    pub flow: ProviderFlow,
    /// What it asks the person to do; `None` until it has begun.
    pub prompt: Option<ProviderPrompt>,
}

impl Default for AuthScreen {
    fn default() -> Self {
        Self {
            stage: Stage::Credentials,
            form: credentials_form(),
            editing: false,
            confirming: false,
            status: Status::Idle,
            provider: None,
            attempts: 0,
        }
    }
}

impl AuthScreen {
    /// Whether keys go into the form.
    #[must_use]
    pub fn is_editing(&self) -> bool {
        self.editing
    }

    /// Whether the sign-out question waits for a yes or no.
    #[must_use]
    pub fn is_confirming(&self) -> bool {
        self.confirming
    }

    /// The form on screen.
    #[must_use]
    pub fn form(&self) -> &Form {
        &self.form
    }

    /// Where the last request stands.
    #[must_use]
    pub fn status(&self) -> &Status {
        &self.status
    }

    /// Whether the form asks for an authenticator code rather than a password.
    #[must_use]
    pub fn awaiting_code(&self) -> bool {
        matches!(self.stage, Stage::SecondFactor { .. })
    }

    /// The identity-provider sign-in being waited on.
    #[must_use]
    pub fn provider(&self) -> Option<&ProviderWait> {
        self.provider.as_ref()
    }

    /// Whether `attempt` is the identity-provider sign-in being waited on.
    fn is_current(&self, attempt: u64) -> bool {
        self.provider
            .as_ref()
            .is_some_and(|wait| wait.attempt == attempt)
    }

    /// Ask for the authenticator code answering `mfa_token`, the first factor (a password or
    /// the identity provider) having verified.
    fn ask_for_code(&mut self, mfa_token: Secret) {
        self.stage = Stage::SecondFactor { mfa_token };
        self.form = code_form();
        self.editing = true;
        self.status = Status::Done(
            "This account has two-factor authentication switched on. Enter the code your \
             authenticator app shows."
                .to_owned(),
        );
    }

    fn begin_provider(&mut self, flow: ProviderFlow) -> Vec<Effect> {
        if self.status.is_working() || self.confirming {
            return Vec::new();
        }
        // A pending challenge is dropped: this starts the sign-in over.
        self.stage = Stage::Credentials;
        self.form = credentials_form();
        self.editing = false;
        self.attempts += 1;
        let attempt = self.attempts;
        self.provider = Some(ProviderWait {
            attempt,
            flow,
            prompt: None,
        });
        self.status = Status::Working("Starting the sign-in with the identity provider…");
        vec![Effect::Auth(AuthEffect::BeginProvider { attempt, flow })]
    }

    fn cancel_provider(&mut self) -> Vec<Effect> {
        let Some(wait) = self.provider.take() else {
            return Vec::new();
        };
        self.status = Status::Failed("Sign-in cancelled.".to_owned());
        vec![Effect::Auth(AuthEffect::CancelProvider {
            attempt: wait.attempt,
        })]
    }

    fn provider_begun(
        &mut self,
        attempt: u64,
        result: Result<ProviderPrompt, String>,
    ) -> Vec<Effect> {
        if !self.is_current(attempt) {
            // Cancelled before it began: make sure the wait it began stops too.
            return match result {
                Ok(_) => vec![Effect::Auth(AuthEffect::CancelProvider { attempt })],
                Err(_) => Vec::new(),
            };
        }
        match result {
            Ok(prompt) => {
                if let Some(wait) = self.provider.as_mut() {
                    wait.prompt = Some(prompt);
                }
                self.status = Status::Working("x cancels the sign-in.");
                vec![Effect::Auth(AuthEffect::AwaitProvider { attempt })]
            }
            Err(reason) => {
                self.provider = None;
                self.status = Status::Failed(format!("Sign-in failed: {reason}"));
                Vec::new()
            }
        }
    }

    fn provider_finished(
        &mut self,
        attempt: u64,
        result: Result<LoginResult, String>,
    ) -> Vec<Effect> {
        if !self.is_current(attempt) {
            // A sign-in approved just as it was cancelled has stored its session, which the
            // header should show. Anything else a cancelled attempt answers is dropped.
            return match result {
                Ok(LoginResult::SignedIn) => vec![Effect::LoadSession],
                Ok(LoginResult::SecondFactor { .. }) | Err(_) => Vec::new(),
            };
        }
        self.provider = None;
        match result {
            Ok(LoginResult::SignedIn) => {
                self.status = Status::Done("Signed in.".to_owned());
                vec![Effect::LoadSession]
            }
            Ok(LoginResult::SecondFactor { mfa_token }) => {
                self.ask_for_code(mfa_token);
                Vec::new()
            }
            Err(reason) => {
                self.status = Status::Failed(format!("Sign-in failed: {reason}"));
                Vec::new()
            }
        }
    }

    /// Apply `action`; pure, as [`super::state::AppState::update`] is.
    pub fn update(&mut self, action: AuthAction) -> Vec<Effect> {
        match action {
            AuthAction::BeginProvider(flow) => self.begin_provider(flow),
            AuthAction::CancelProvider => self.cancel_provider(),
            AuthAction::ProviderBegun { attempt, result } => self.provider_begun(attempt, result),
            AuthAction::ProviderFinished { attempt, result } => {
                self.provider_finished(attempt, result)
            }
            AuthAction::ProviderCancelled { attempt } => {
                tracing::debug!(target: "capsule_cli::tui", attempt, "identity-provider sign-in cancelled");
                Vec::new()
            }
            AuthAction::BeginSignIn => {
                if !self.status.is_working() && !self.confirming {
                    // The email typed last time stays; it is not a secret. A pending challenge
                    // is dropped: this starts the sign-in over.
                    let email = match self.stage {
                        Stage::Credentials => self.form.value(EMAIL).to_owned(),
                        Stage::SecondFactor { .. } => String::new(),
                    };
                    self.stage = Stage::Credentials;
                    self.form = credentials_form();
                    self.form.set(EMAIL, &email);
                    self.editing = true;
                    self.status = Status::Idle;
                }
                Vec::new()
            }
            AuthAction::SignOut => {
                if self.status.is_working() || self.confirming {
                    return Vec::new();
                }
                self.editing = false;
                self.confirming = true;
                self.status = Status::Asking(
                    "Sign out and forget this device's session? y to sign out, n to cancel."
                        .to_owned(),
                );
                Vec::new()
            }
            AuthAction::LoginFinished(Ok(LoginResult::SignedIn))
            | AuthAction::SecondFactorFinished(Ok(())) => {
                self.stage = Stage::Credentials;
                self.status = Status::Done("Signed in.".to_owned());
                vec![Effect::LoadSession]
            }
            AuthAction::LoginFinished(Ok(LoginResult::SecondFactor { mfa_token })) => {
                self.ask_for_code(mfa_token);
                Vec::new()
            }
            AuthAction::LoginFinished(Err(reason))
            | AuthAction::SecondFactorFinished(Err(reason)) => {
                // A challenge is good once: a refused code starts the sign-in over.
                if self.awaiting_code() {
                    self.stage = Stage::Credentials;
                    self.form = credentials_form();
                }
                self.status = Status::Failed(format!("Sign-in failed: {reason}"));
                Vec::new()
            }
            AuthAction::SignOutFinished(result) => {
                self.status = match result {
                    Ok(true) => Status::Done("Signed out.".to_owned()),
                    Ok(false) => Status::Done("You were not signed in.".to_owned()),
                    // The local session is cleared even when the server's revoke failed.
                    Err(reason) => Status::Failed(format!("Sign-out: {reason}")),
                };
                vec![Effect::LoadSession]
            }
        }
    }

    /// Apply the answer to the sign-out question. No question waiting: nothing happens.
    pub fn on_answer(&mut self, answer: Answer) -> Vec<Effect> {
        if !self.confirming {
            return Vec::new();
        }
        self.confirming = false;
        match answer {
            Answer::Yes => {
                self.status = Status::Working("Signing out…");
                vec![Effect::Auth(AuthEffect::Logout)]
            }
            Answer::No => {
                self.status = Status::Idle;
                Vec::new()
            }
        }
    }

    /// Apply a key typed into the form.
    pub fn on_form_key(&mut self, key: FormKey) -> Vec<Effect> {
        if !self.editing {
            return Vec::new();
        }
        match self.form.apply(key) {
            FormOutcome::Editing => Vec::new(),
            FormOutcome::Cancelled => {
                self.editing = false;
                if self.awaiting_code() {
                    self.stage = Stage::Credentials;
                    self.form = credentials_form();
                    self.status = Status::Failed("Sign-in cancelled.".to_owned());
                }
                Vec::new()
            }
            FormOutcome::Submitted => self.submit(),
        }
    }

    fn submit(&mut self) -> Vec<Effect> {
        if self.status.is_working() {
            return Vec::new();
        }
        let effect = match &self.stage {
            Stage::Credentials => {
                let email = self.form.value(EMAIL).to_owned();
                let password = self.form.secret(PASSWORD);
                if email.is_empty() || password.is_empty() {
                    self.status = Status::Failed("Enter an email and a password.".to_owned());
                    return Vec::new();
                }
                self.status = Status::Working("Signing in…");
                AuthEffect::Login { email, password }
            }
            Stage::SecondFactor { mfa_token } => {
                // Whitespace around a pasted code is not part of it.
                let code = Secret::new(self.form.secret(CODE).expose().trim());
                if code.is_empty() {
                    self.status = Status::Failed("Enter the authenticator code.".to_owned());
                    return Vec::new();
                }
                self.status = Status::Working("Checking the code…");
                AuthEffect::VerifySecondFactor {
                    mfa_token: mfa_token.clone(),
                    code,
                }
            }
        };
        self.form.clear_secrets();
        self.editing = false;
        vec![Effect::Auth(effect)]
    }
}

/// Draw the account screen into `area`.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    screen: &AuthScreen,
    session: &Loadable<SessionSummary>,
    title: &'static str,
) {
    let mut lines = vec![match session {
        Loadable::Loading => Line::from("Reading the session…".fg(Color::DarkGray)),
        Loadable::Ready(SessionSummary::SignedOut) => Line::from("Not signed in."),
        Loadable::Ready(SessionSummary::SignedIn { .. }) => {
            Line::from("Signed in.".fg(Color::Green))
        }
        Loadable::Failed(reason) => {
            Line::from(format!("The session could not be read: {reason}").fg(Color::Red))
        }
    }];
    if screen.editing || screen.awaiting_code() {
        lines.push(Line::default());
        lines.extend(screen.form.lines(screen.editing));
    }
    if let Some(prompt) = screen
        .provider
        .as_ref()
        .and_then(|wait| wait.prompt.as_ref())
    {
        lines.push(Line::default());
        lines.extend(prompt_lines(prompt));
    }
    if let Some(status) = screen.status.line() {
        lines.push(Line::default());
        lines.push(status);
    }
    lines.push(Line::default());
    lines.push(Line::from(
        "Identity provider: w signs in with a browser here, d on another device (scriptable: \
         `capsule auth login --oidc [--device] --json`). New account: `capsule auth register`."
            .fg(Color::DarkGray),
    ));
    let block = Block::default().borders(Borders::ALL).title(title);
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// What a begun identity-provider sign-in asks the person to do, as the CLI words it.
fn prompt_lines(prompt: &ProviderPrompt) -> Vec<Line<'static>> {
    match prompt {
        ProviderPrompt::Browser {
            authorization_url,
            opened,
            wait_secs,
        } => {
            let mut lines = vec![
                Line::from("To sign in, open this address in a browser:"),
                Line::from(authorization_url.expose().to_owned().fg(Color::Cyan)),
            ];
            if !opened {
                lines.push(Line::from(
                    "No browser could be opened from here. Open the address above in any browser."
                        .fg(Color::Yellow),
                ));
            }
            lines.push(Line::from(crate::oidc_waiting_line(
                std::time::Duration::from_secs(*wait_secs),
            )));
            lines
        }
        ProviderPrompt::Device {
            verification_uri,
            user_code,
            verification_uri_complete,
            wait_secs,
        } => {
            let mut lines = vec![
                Line::from("On any device with a browser, open this address:"),
                Line::from(verification_uri.clone().fg(Color::Cyan)),
                Line::from(format!("and enter this code: {}", user_code.expose()).bold()),
            ];
            if let Some(complete) = verification_uri_complete {
                lines.push(Line::from(
                    "Or open this address, which already carries the code:",
                ));
                lines.push(Line::from(complete.expose().to_owned().fg(Color::Cyan)));
            }
            lines.push(Line::from(crate::device_waiting_line(
                std::time::Duration::from_secs(*wait_secs),
            )));
            lines
        }
    }
}

#[cfg(test)]
mod tests;
