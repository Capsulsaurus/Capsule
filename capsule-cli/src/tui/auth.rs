//! The account screen: sign in with a password (and an authenticator code when the account asks
//! for one), sign out, and see the local session's state.
//!
//! The scriptable twins are `capsule auth login --json`, `capsule auth logout --json` and
//! `capsule auth status --json`. Signing in through an identity provider and creating an account
//! stay on their subcommands (`capsule auth login --oidc`, `capsule auth register`), which the
//! screen names.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::form::{Field, Form, FormKey, FormOutcome, Secret, Status};
use super::state::{Effect, Loadable, SessionSummary};

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

/// What the account screen does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthAction {
    /// Show the sign-in form and start typing into it.
    BeginSignIn,
    /// Sign out.
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
        /// The code the authenticator app shows.
        code: String,
    },
    /// Sign out; answered by [`AuthAction::SignOutFinished`].
    Logout,
}

impl AuthEffect {
    /// The action reporting this effect failing with `reason`.
    #[must_use]
    pub fn failed(self, reason: String) -> AuthAction {
        match self {
            Self::Login { .. } => AuthAction::LoginFinished(Err(reason)),
            Self::VerifySecondFactor { .. } => AuthAction::SecondFactorFinished(Err(reason)),
            Self::Logout => AuthAction::SignOutFinished(Err(reason)),
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
    Form::new(vec![Field::text("Authenticator code")])
}

/// The account screen's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthScreen {
    stage: Stage,
    form: Form,
    editing: bool,
    status: Status,
}

impl Default for AuthScreen {
    fn default() -> Self {
        Self {
            stage: Stage::Credentials,
            form: credentials_form(),
            editing: false,
            status: Status::Idle,
        }
    }
}

impl AuthScreen {
    /// Whether keys go into the form.
    #[must_use]
    pub fn is_editing(&self) -> bool {
        self.editing
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

    /// Apply `action`; pure, as [`super::state::AppState::update`] is.
    pub fn update(&mut self, action: AuthAction) -> Vec<Effect> {
        match action {
            AuthAction::BeginSignIn => {
                if !self.status.is_working() {
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
                if self.status.is_working() {
                    return Vec::new();
                }
                self.editing = false;
                self.status = Status::Working("Signing out…");
                vec![Effect::Auth(AuthEffect::Logout)]
            }
            AuthAction::LoginFinished(Ok(LoginResult::SignedIn))
            | AuthAction::SecondFactorFinished(Ok(())) => {
                self.stage = Stage::Credentials;
                self.status = Status::Done("Signed in.".to_owned());
                vec![Effect::LoadSession]
            }
            AuthAction::LoginFinished(Ok(LoginResult::SecondFactor { mfa_token })) => {
                self.stage = Stage::SecondFactor { mfa_token };
                self.form = code_form();
                self.editing = true;
                self.status = Status::Done(
                    "This account has two-factor authentication switched on. Enter the code \
                     your authenticator app shows."
                        .to_owned(),
                );
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
                let code = self.form.value(CODE).to_owned();
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
    if let Some(status) = screen.status.line() {
        lines.push(Line::default());
        lines.push(status);
    }
    lines.push(Line::default());
    lines.push(Line::from(
        "Identity provider: `capsule auth login --oidc` (add `--device` to approve on another \
         device). New account: `capsule auth register`. Scriptable: `capsule auth status --json`."
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

#[cfg(test)]
mod tests;
