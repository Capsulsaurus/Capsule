//! The TUI's whole state and the pure reducer that changes it.
//!
//! [`AppState::update`] is the only way state changes: it takes one [`Action`], applies it in
//! place, and returns the [`Effect`]s the action asks for. It never performs I/O, never awaits,
//! and never reads a clock, so every transition is a plain function a unit test drives and the
//! next frame can always be drawn from what it returns. Work that needs the outside world is an
//! [`Effect`], performed through a [`Ports`](super::ports::Ports) implementation off the input
//! path, whose outcome comes back as another [`Action`].
//!
//! # Contract the feature screens build on
//!
//! - A new screen is a [`Screen`] variant, rendered by [`super::view`], reached by an
//!   [`Action::Open`] from the keymap or another screen.
//! - A new side effect is an [`Effect`] variant, a method on the `Ports` trait, and an
//!   [`Action`] variant carrying its outcome back.
//! - An action that starts an effect also changes state the next frame shows (a loading marker,
//!   a pending row), so no input waits on the effect for a visible response.

use super::transfer::{Stamped, TransferBoard};

/// One screen on the navigation stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// The landing screen: the session summary and the way into every other screen.
    Home,
    /// The key bindings, generated from the keymap table.
    Help,
    /// Every upload and fetch in real time, drawn from the transfer event stream.
    Transfers,
}

/// A value an [`Effect`] is fetching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Loadable<T> {
    /// The effect is in flight; the screen shows that it is.
    Loading,
    /// The effect returned this value.
    Ready(T),
    /// The effect failed, with the English reason the screen shows.
    Failed(String),
}

/// What the local session store says about the signed-in account.
///
/// A local judgement only, as `capsule auth status` makes it: reading it never calls the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionSummary {
    /// No session is persisted; `capsule auth login` creates one.
    SignedOut,
    /// A session is persisted.
    SignedIn {
        /// The access token's recorded expiry is in the future. An expired access token is not
        /// a dead session: the next networked operation refreshes it.
        access_token_fresh: bool,
    },
}

/// Everything that can change the state.
///
/// Input arrives as an action through the keymap; a finished effect arrives as an action
/// through the effect channel. Both go through [`AppState::update`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Leave the TUI.
    Quit,
    /// Push a screen onto the navigation stack. Opening the screen already on top is a no-op.
    Open(Screen),
    /// Pop the top screen. The root screen never pops.
    Back,
    /// Read the session summary again.
    RefreshSession,
    /// [`Effect::LoadSession`] finished.
    SessionLoaded(Result<SessionSummary, String>),
    /// A transfer event arrived, stamped with when.
    Transfer(Stamped),
    /// The transfer stream's subscriber fell behind and this many events never arrived.
    TransfersLagged(u64),
    /// The frame clock: milliseconds since the transfer feed started. Moves the transfer
    /// board's clock without an event, so throughput decays while nothing moves.
    Tick(u64),
}

/// Work the reducer asks the runtime to perform through the ports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Read the session summary; answered by [`Action::SessionLoaded`].
    LoadSession,
}

impl Effect {
    /// The action reporting this effect failing with `reason` instead of answering: what the
    /// runtime sends when the task performing it panicked or was cancelled, so no state is left
    /// waiting on an outcome that will never come.
    #[must_use]
    pub fn failed(self, reason: String) -> Action {
        match self {
            Self::LoadSession => Action::SessionLoaded(Err(reason)),
        }
    }
}

/// The TUI's state: a navigation stack, the data the screens show, and whether to keep running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppState {
    /// Never empty: the bottom is always [`Screen::Home`].
    stack: Vec<Screen>,
    session: Loadable<SessionSummary>,
    transfers: TransferBoard,
    running: bool,
}

impl AppState {
    /// The state the TUI opens in, and the effects that fill it.
    #[must_use]
    pub fn init() -> (Self, Vec<Effect>) {
        let state = Self {
            stack: vec![Screen::Home],
            session: Loadable::Loading,
            transfers: TransferBoard::default(),
            running: true,
        };
        tracing::debug!(target: "capsule_cli::tui", "TUI state initialised");
        (state, vec![Effect::LoadSession])
    }

    /// The screen on top of the stack: the one drawn.
    #[must_use]
    pub fn screen(&self) -> Screen {
        *self.stack.last().unwrap_or(&Screen::Home)
    }

    /// How many screens are stacked, the root included.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    /// The session summary as last loaded.
    #[must_use]
    pub fn session(&self) -> &Loadable<SessionSummary> {
        &self.session
    }

    /// Every transfer seen on the event stream.
    #[must_use]
    pub fn transfers(&self) -> &TransferBoard {
        &self.transfers
    }

    /// Whether the event loop keeps going. False once [`Action::Quit`] is applied.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Apply `action` and return the effects it asks for.
    ///
    /// Pure: no I/O, no awaiting, no clock. The returned effects are performed by the caller.
    pub fn update(&mut self, action: Action) -> Vec<Effect> {
        tracing::trace!(target: "capsule_cli::tui", ?action, "applying action");
        let effects = match action {
            Action::Quit => {
                self.running = false;
                Vec::new()
            }
            Action::Open(screen) => {
                if self.screen() != screen {
                    self.stack.push(screen);
                }
                Vec::new()
            }
            Action::Back => {
                if self.stack.len() > 1 {
                    self.stack.pop();
                }
                Vec::new()
            }
            Action::RefreshSession => {
                // One read in flight at a time, so two answers can never land out of order.
                if self.session == Loadable::Loading {
                    Vec::new()
                } else {
                    self.session = Loadable::Loading;
                    vec![Effect::LoadSession]
                }
            }
            Action::SessionLoaded(result) => {
                self.session = match result {
                    Ok(summary) => Loadable::Ready(summary),
                    Err(reason) => Loadable::Failed(reason),
                };
                Vec::new()
            }
            Action::Transfer(stamped) => {
                self.transfers.apply(stamped);
                Vec::new()
            }
            Action::TransfersLagged(count) => {
                self.transfers.lagged(count);
                Vec::new()
            }
            Action::Tick(now_ms) => {
                self.transfers.tick(now_ms);
                Vec::new()
            }
        };
        if !effects.is_empty() {
            tracing::debug!(target: "capsule_cli::tui", ?effects, "action requested effects");
        }
        effects
    }
}

#[cfg(test)]
mod tests;
