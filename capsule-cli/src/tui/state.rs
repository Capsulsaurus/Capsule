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
//! - A feature screen lives in its own module (its state, its actions and effects, its reducer
//!   and its render), and the root holds one variant per screen in each enum, delegating to it.
//! - A screen that takes text exposes it as a [`Form`](super::form::Form); while one is being
//!   edited, [`AppState::is_editing`] is true and keys arrive as [`Action::Form`].

use std::path::{Path, PathBuf};

use super::auth::{AuthAction, AuthEffect, AuthScreen};
use super::form::FormKey;
use super::import::{ImportAction, ImportEffect, ImportScreen};
use super::label_editor::{LabelAction, LabelEditor, LabelEffect};
use super::library::{LibraryAction, LibraryEffect, LibraryScreen, Row};
use super::metadata_repair::{MetadataRepair, RepairAction, RepairEffect};
use super::push::{PushAction, PushEffect, PushScreen};
use super::restore::{RestoreAction, RestoreEffect, RestoreScreen};
use super::share::{ShareAction, ShareEffect, ShareScreen};
use super::sync::{SyncAction, SyncEffect, SyncScreen};
use super::transfer::{Stamped, TransferBoard};
use crate::edit::LabelTarget;

/// A library path as typed, with the canonical path it was resolved to outside the reducer.
///
/// The import and push screens refuse each other on the same library, and two spellings
/// (`./lib`, `lib`, an absolute path, a symlink) name one library. The reducer cannot touch the
/// filesystem, so the resolution is made by [`AppState::resolve_libraries`] before the submit
/// reaches it and handed in as this value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedLibrary {
    typed: String,
    canonical: PathBuf,
}

impl ResolvedLibrary {
    pub(super) fn new(typed: &str, canonical: PathBuf) -> Self {
        Self {
            typed: typed.to_owned(),
            canonical,
        }
    }

    /// The path `typed` names for comparison: the canonical form when it was resolved for exactly
    /// this text, else the text itself.
    pub(super) fn identity(resolved: Option<&Self>, typed: &str) -> PathBuf {
        match resolved {
            Some(resolved) if resolved.typed == typed => resolved.canonical.clone(),
            _ => PathBuf::from(typed),
        }
    }
}

/// The canonical form of `path`: symlinks and `.`/`..` resolved when it exists, else made
/// absolute, else the path as given.
#[must_use]
pub fn canonical_library(path: &Path) -> PathBuf {
    std::fs::canonicalize(path)
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

/// One screen on the navigation stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// The landing screen: the session summary and the way into every other screen.
    Home,
    /// The key bindings, generated from the keymap table.
    Help,
    /// Every upload and fetch in real time, drawn from the transfer event stream.
    Transfers,
    /// Sign in and out ([`super::auth`]).
    Auth,
    /// Sync the server's feed into the local store ([`super::sync`]).
    Sync,
    /// Restore originals from the server ([`super::restore`]).
    Restore,
    /// Issue and revoke share links ([`super::share`]).
    Share,
    /// Pick sources and import them into a library ([`super::import`]).
    Import,
    /// Push a library and watch each asset reach custody ([`super::push`]).
    Push,
    /// Browse a library's timeline, albums and stacks ([`super::library`]).
    Library,
    /// Edit the labels and regions of what the browser has selected ([`super::label_editor`]).
    Labels,
    /// Correct the selected asset's capture time and location ([`super::metadata_repair`]).
    Repair,
}

/// The answer to a confirmation a screen asked for before an act it cannot take back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// Go ahead.
    Yes,
    /// Leave things as they are.
    No,
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
    /// A key typed into the form of the screen on top.
    Form(FormKey),
    /// The answer to the confirmation the screen on top is asking.
    Answer(Answer),
    /// The account screen.
    Auth(AuthAction),
    /// The sync screen.
    Sync(SyncAction),
    /// The restore screen.
    Restore(RestoreAction),
    /// The share-links screen.
    Share(ShareAction),
    /// The import screen.
    Import(ImportAction),
    /// The push screen.
    Push(PushAction),
    /// The library browser.
    Library(LibraryAction),
    /// Open the label editor on the browser's selected asset, album or stack.
    EditLabels,
    /// Open metadata repair on the browser's selected asset.
    RepairMetadata,
    /// The label editor.
    Labels(LabelAction),
    /// The metadata repair screen.
    Repair(RepairAction),
}

/// Work the reducer asks the runtime to perform through the ports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Read the session summary; answered by [`Action::SessionLoaded`].
    LoadSession,
    /// The account screen's work.
    Auth(AuthEffect),
    /// The sync screen's work.
    Sync(SyncEffect),
    /// The restore screen's work.
    Restore(RestoreEffect),
    /// The share-links screen's work.
    Share(ShareEffect),
    /// The import screen's work.
    Import(ImportEffect),
    /// The push screen's work.
    Push(PushEffect),
    /// The library browser's work.
    Library(LibraryEffect),
    /// The label editor's work.
    Labels(LabelEffect),
    /// The metadata repair screen's work.
    Repair(RepairEffect),
}

impl Effect {
    /// The action reporting this effect failing with `reason` instead of answering: what the
    /// runtime sends when the task performing it panicked or was cancelled, so no state is left
    /// waiting on an outcome that will never come.
    #[must_use]
    pub fn failed(self, reason: String) -> Action {
        match self {
            Self::LoadSession => Action::SessionLoaded(Err(reason)),
            Self::Auth(effect) => Action::Auth(effect.failed(reason)),
            Self::Sync(effect) => Action::Sync(effect.failed(reason)),
            Self::Restore(effect) => Action::Restore(effect.failed(reason)),
            Self::Share(effect) => Action::Share(effect.failed(reason)),
            Self::Import(effect) => Action::Import(effect.failed(reason)),
            Self::Push(effect) => Action::Push(effect.failed(reason)),
            Self::Library(effect) => Action::Library(effect.failed(reason)),
            Self::Labels(effect) => Action::Labels(effect.failed(reason)),
            Self::Repair(effect) => Action::Repair(effect.failed(reason)),
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
    auth: AuthScreen,
    sync: SyncScreen,
    restore: RestoreScreen,
    share: ShareScreen,
    import: ImportScreen,
    push: PushScreen,
    library: LibraryScreen,
    labels: LabelEditor,
    repair: MetadataRepair,
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
            auth: AuthScreen::default(),
            sync: SyncScreen::default(),
            restore: RestoreScreen::default(),
            share: ShareScreen::default(),
            import: ImportScreen::default(),
            push: PushScreen::default(),
            library: LibraryScreen::default(),
            labels: LabelEditor::default(),
            repair: MetadataRepair::default(),
        };
        tracing::debug!(target: "capsule_cli::tui", "TUI state initialised");
        (state, vec![Effect::LoadSession])
    }

    /// Resolve, outside the reducer, the library a submit is about to use, so the cross-screen
    /// guard compares canonical paths. Call it with each action before [`update`](Self::update);
    /// it only acts on a form submit on the import or push screen.
    pub fn resolve_libraries(&mut self, action: &Action, canonicalize: impl Fn(&Path) -> PathBuf) {
        if !matches!(action, Action::Form(FormKey::Submit)) {
            return;
        }
        match self.screen() {
            Screen::Import => self.import.resolve_library(canonicalize),
            Screen::Push => self.push.resolve_library(canonicalize),
            _ => {}
        }
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

    /// The account screen's state.
    #[must_use]
    pub fn auth(&self) -> &AuthScreen {
        &self.auth
    }

    /// The sync screen's state.
    #[must_use]
    pub fn sync(&self) -> &SyncScreen {
        &self.sync
    }

    /// The restore screen's state.
    #[must_use]
    pub fn restore(&self) -> &RestoreScreen {
        &self.restore
    }

    /// The share-links screen's state.
    #[must_use]
    pub fn share(&self) -> &ShareScreen {
        &self.share
    }

    /// The import screen's state.
    #[must_use]
    pub fn import(&self) -> &ImportScreen {
        &self.import
    }

    /// The push screen's state.
    #[must_use]
    pub fn push(&self) -> &PushScreen {
        &self.push
    }

    /// The library browser's state.
    #[must_use]
    pub fn library(&self) -> &LibraryScreen {
        &self.library
    }

    /// The label editor's state.
    #[must_use]
    pub fn labels(&self) -> &LabelEditor {
        &self.labels
    }

    /// The metadata repair screen's state.
    #[must_use]
    pub fn repair(&self) -> &MetadataRepair {
        &self.repair
    }

    /// Whether the screen on top is taking text: keys then go into its form as
    /// [`Action::Form`], and only the editing bindings and the Ctrl bindings apply.
    #[must_use]
    pub fn is_editing(&self) -> bool {
        match self.screen() {
            Screen::Home | Screen::Help | Screen::Transfers | Screen::Sync => false,
            Screen::Auth => self.auth.is_editing(),
            Screen::Restore => self.restore.is_editing(),
            Screen::Share => self.share.is_editing(),
            Screen::Import => self.import.is_editing(),
            Screen::Push => self.push.is_editing(),
            Screen::Library => self.library.is_editing(),
            Screen::Labels => self.labels.is_editing(),
            Screen::Repair => self.repair.is_editing(),
        }
    }

    /// Whether the screen on top is waiting for a yes or no before it acts: only the confirming
    /// bindings and the Ctrl bindings apply then.
    #[must_use]
    pub fn is_confirming(&self) -> bool {
        match self.screen() {
            // Import and Push never ask first: an import copies its sources and a push only adds
            // to the server, so neither can take anything away. The library browser only reads.
            Screen::Home
            | Screen::Help
            | Screen::Transfers
            | Screen::Sync
            | Screen::Restore
            | Screen::Import
            | Screen::Push
            | Screen::Library
            | Screen::Repair => false,
            Screen::Auth => self.auth.is_confirming(),
            Screen::Share => self.share.is_confirming(),
            Screen::Labels => self.labels.is_confirming(),
        }
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
            // Esc on the label editor drops a region being drawn before it leaves the screen.
            Action::Back if self.screen() == Screen::Labels && self.labels.cancel_draft() => {
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
            Action::Form(key) => match self.screen() {
                Screen::Home | Screen::Help | Screen::Transfers | Screen::Sync => Vec::new(),
                Screen::Auth => self.auth.on_form_key(key),
                Screen::Restore => self.restore.on_form_key(key),
                Screen::Share => self.share.on_form_key(key),
                // The workspace has no lock, so each writer refuses a library the other has open.
                Screen::Import => self
                    .import
                    .on_form_key_guarded(key, self.push.running_library()),
                Screen::Push => self
                    .push
                    .on_form_key_guarded(key, self.import.running_library()),
                Screen::Library => self.library.on_form_key(key),
                Screen::Labels => {
                    let busy = self.repair.is_working();
                    self.labels.on_form_key(key, &self.library, busy)
                }
                Screen::Repair => {
                    let busy = self.labels.is_working();
                    self.repair.on_form_key(key, &self.library, busy)
                }
            },
            Action::Answer(answer) => match self.screen() {
                Screen::Home
                | Screen::Help
                | Screen::Transfers
                | Screen::Sync
                | Screen::Restore
                | Screen::Import
                | Screen::Push
                | Screen::Library
                | Screen::Repair => Vec::new(),
                Screen::Auth => self.auth.on_answer(answer),
                Screen::Share => self.share.on_answer(answer),
                Screen::Labels => {
                    let busy = self.repair.is_working();
                    self.labels.on_answer(answer, &self.library, busy)
                }
            },
            // A screen's outcome lands on its state whichever screen is on top, so leaving a
            // screen while its request runs loses nothing.
            Action::Auth(action) => self.auth.update(action),
            Action::Sync(action) => self.sync.update(action),
            Action::Restore(action) => self.restore.update(action),
            Action::Share(action) => self.share.update(action),
            Action::Import(action) => {
                let imported = matches!(action, ImportAction::Finished(Ok(_)));
                let effects = self.import.update(action);
                // The library just imported into is the one to push next.
                if imported {
                    self.push.offer_library(self.import.library());
                }
                effects
            }
            Action::Push(action) => self.push.update(action),
            Action::Library(action) => self.library.update(action),
            Action::EditLabels => {
                let subject = match self.library.selected() {
                    Some(Row::Asset(id)) => Some(LabelTarget::Asset(id)),
                    Some(Row::Album(id)) => Some(LabelTarget::Album(id)),
                    Some(Row::Stack(id)) => Some(LabelTarget::Stack(id)),
                    None => None,
                };
                match subject {
                    Some(subject) if self.library.ready().is_some() => {
                        self.stack.push(Screen::Labels);
                        self.labels.begin(subject, &self.library)
                    }
                    _ => Vec::new(),
                }
            }
            Action::RepairMetadata => match self.library.selected_asset() {
                Some(asset_id) if self.library.ready().is_some() => {
                    self.stack.push(Screen::Repair);
                    self.repair.begin(asset_id, &self.library);
                    Vec::new()
                }
                _ => Vec::new(),
            },
            // A written edit's snapshot goes to the browser first, so the screen that asked
            // for it redraws from the library as it now is.
            Action::Labels(LabelAction::Applied(Ok(edited))) => {
                let effects = self
                    .library
                    .replace_catalogue(&edited.library, edited.catalogue);
                self.labels.finish(Ok(edited.summary));
                effects
            }
            Action::Repair(RepairAction::Applied(Ok(edited))) => {
                let effects = self
                    .library
                    .replace_catalogue(&edited.library, edited.catalogue);
                self.repair.finish(Ok(edited.summary));
                effects
            }
            Action::Labels(action) => {
                let busy = self.repair.is_working();
                self.labels.update(action, &self.library, busy)
            }
            Action::Repair(action) => {
                let busy = self.labels.is_working();
                self.repair.update(action, &self.library, busy)
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
