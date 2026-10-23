//! Terminal input to [`Action`]s, through one binding table.
//!
//! [`BINDINGS`] is the single source for both what a key does and what the help screen and the
//! footer say it does, so the two cannot drift.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::auth::AuthAction;
use super::form::FormKey;
use super::restore::RestoreAction;
use super::share::ShareAction;
use super::state::{Action, Answer, AppState, Screen};
use super::sync::SyncAction;

/// Where a binding applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// On every screen. While a form is being edited, only the Ctrl bindings among these apply:
    /// every other key is text.
    Global,
    /// Only while this screen is on top and no form is being edited.
    On(Screen),
    /// Only while a form is being edited, on any screen.
    Editing,
    /// Only while a screen waits for a yes or no, on any screen. Every other key is ignored,
    /// so an answer is never a slip of the finger on a neighbouring binding.
    Confirming,
}

/// One key binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// The key.
    pub code: KeyCode,
    /// The modifiers that must be held; any other modifier makes the press not match.
    pub modifiers: KeyModifiers,
    /// Where the binding applies.
    pub scope: Scope,
    /// The key as the help screen and the footer print it.
    pub label: &'static str,
    /// What the binding does, in a few words.
    pub description: &'static str,
    /// The action it sends.
    pub action: Action,
    /// Shown in the footer, not only on the help screen.
    pub in_footer: bool,
}

/// Every key binding, in the order the help screen lists them.
pub const BINDINGS: &[Binding] = &[
    Binding {
        code: KeyCode::Char('q'),
        modifiers: KeyModifiers::NONE,
        scope: Scope::Global,
        label: "q",
        description: "Quit",
        action: Action::Quit,
        in_footer: true,
    },
    Binding {
        code: KeyCode::Char('c'),
        modifiers: KeyModifiers::CONTROL,
        scope: Scope::Global,
        label: "Ctrl-C",
        description: "Quit",
        action: Action::Quit,
        in_footer: false,
    },
    Binding {
        code: KeyCode::Char('?'),
        modifiers: KeyModifiers::NONE,
        scope: Scope::Global,
        label: "?",
        description: "Show the key bindings",
        action: Action::Open(Screen::Help),
        in_footer: true,
    },
    Binding {
        code: KeyCode::Esc,
        modifiers: KeyModifiers::NONE,
        scope: Scope::Global,
        label: "Esc",
        description: "Back to the previous screen",
        action: Action::Back,
        in_footer: true,
    },
    Binding {
        code: KeyCode::Char('r'),
        modifiers: KeyModifiers::NONE,
        scope: Scope::On(Screen::Home),
        label: "r",
        description: "Reload the session status",
        action: Action::RefreshSession,
        in_footer: true,
    },
    // ── The screens Home leads to; Home lists them in its body ──────────
    open(KeyCode::Char('a'), "a", "Account", Screen::Auth),
    open(KeyCode::Char('s'), "s", "Sync", Screen::Sync),
    open(
        KeyCode::Char('d'),
        "d",
        "Restore originals",
        Screen::Restore,
    ),
    open(KeyCode::Char('l'), "l", "Share links", Screen::Share),
    open(
        KeyCode::Char('t'),
        "t",
        "Watch transfers",
        Screen::Transfers,
    ),
    // ── Account ──────────────────────────────────────────────────────────
    on(
        Screen::Auth,
        KeyCode::Char('l'),
        "l",
        "Sign in",
        Action::Auth(AuthAction::BeginSignIn),
    ),
    on(
        Screen::Auth,
        KeyCode::Char('o'),
        "o",
        "Sign out",
        Action::Auth(AuthAction::SignOut),
    ),
    // ── Sync ─────────────────────────────────────────────────────────────
    on(
        Screen::Sync,
        KeyCode::Char('s'),
        "s",
        "Sync now",
        Action::Sync(SyncAction::Run { from_start: false }),
    ),
    on(
        Screen::Sync,
        KeyCode::Char('f'),
        "f",
        "Sync from the start",
        Action::Sync(SyncAction::Run { from_start: true }),
    ),
    // ── Restore ──────────────────────────────────────────────────────────
    on(
        Screen::Restore,
        KeyCode::Char('e'),
        "e",
        "Fill in and run a restore",
        Action::Restore(RestoreAction::Edit),
    ),
    // ── Share links ──────────────────────────────────────────────────────
    on(
        Screen::Share,
        KeyCode::Char('c'),
        "c",
        "Issue a link",
        Action::Share(ShareAction::BeginCreate),
    ),
    on(
        Screen::Share,
        KeyCode::Char('x'),
        "x",
        "Revoke a link",
        Action::Share(ShareAction::BeginRevoke),
    ),
    // ── While a form is being edited ─────────────────────────────────────
    editing(KeyCode::Enter, "Enter", "Submit", FormKey::Submit, true),
    editing(KeyCode::Tab, "Tab", "Next field", FormKey::Next, true),
    editing(KeyCode::Down, "Down", "Next field", FormKey::Next, false),
    editing(
        KeyCode::BackTab,
        "Shift-Tab",
        "Previous field",
        FormKey::Previous,
        false,
    ),
    editing(
        KeyCode::Up,
        "Up",
        "Previous field",
        FormKey::Previous,
        false,
    ),
    editing(
        KeyCode::Backspace,
        "Backspace",
        "Delete a character",
        FormKey::Backspace,
        false,
    ),
    editing(KeyCode::Esc, "Esc", "Stop editing", FormKey::Cancel, true),
    // ── While a screen asks for a yes or no ──────────────────────────────
    confirming(KeyCode::Char('y'), "y", "Yes, go ahead", Answer::Yes, true),
    confirming(KeyCode::Char('n'), "n", "No, cancel", Answer::No, true),
    confirming(KeyCode::Esc, "Esc", "No, cancel", Answer::No, false),
];

/// A Home binding opening `screen`, listed in Home's body rather than the footer.
const fn open(
    code: KeyCode,
    label: &'static str,
    description: &'static str,
    screen: Screen,
) -> Binding {
    Binding {
        code,
        modifiers: KeyModifiers::NONE,
        scope: Scope::On(Screen::Home),
        label,
        description,
        action: Action::Open(screen),
        in_footer: false,
    }
}

/// An unmodified key on one screen, shown in its footer.
const fn on(
    screen: Screen,
    code: KeyCode,
    label: &'static str,
    description: &'static str,
    action: Action,
) -> Binding {
    Binding {
        code,
        modifiers: KeyModifiers::NONE,
        scope: Scope::On(screen),
        label,
        description,
        action,
        in_footer: true,
    }
}

/// A key that edits the form being typed into.
const fn editing(
    code: KeyCode,
    label: &'static str,
    description: &'static str,
    key: FormKey,
    in_footer: bool,
) -> Binding {
    Binding {
        code,
        modifiers: KeyModifiers::NONE,
        scope: Scope::Editing,
        label,
        description,
        action: Action::Form(key),
        in_footer,
    }
}

/// A key that answers the question a screen is asking.
const fn confirming(
    code: KeyCode,
    label: &'static str,
    description: &'static str,
    answer: Answer,
    in_footer: bool,
) -> Binding {
    Binding {
        code,
        modifiers: KeyModifiers::NONE,
        scope: Scope::Confirming,
        label,
        description,
        action: Action::Answer(answer),
        in_footer,
    }
}

/// Whether `binding` applies in `state`, before asking whether it would do anything.
#[must_use]
pub fn applies(binding: &Binding, state: &AppState) -> bool {
    let editing = state.is_editing();
    let confirming = state.is_confirming();
    // Typing must reach the field and an answer must reach the question: only a Ctrl chord
    // outranks either.
    let captured = editing || confirming;
    match binding.scope {
        Scope::Global => !captured || binding.modifiers.contains(KeyModifiers::CONTROL),
        Scope::On(on) => !captured && on == state.screen(),
        Scope::Editing => editing,
        Scope::Confirming => confirming,
    }
}

/// Whether `binding` does something in `state`: it applies there, it is not [`Action::Back`] on
/// the root screen (nothing to go back to), and it does not open the screen already on top. A
/// binding that is not available neither matches a key nor shows in the footer, so the footer
/// never offers a key that does nothing.
#[must_use]
pub fn available(binding: &Binding, state: &AppState) -> bool {
    let screen = state.screen();
    let pointless = match binding.action {
        Action::Back => state.depth() == 1,
        Action::Open(target) => target == screen,
        _ => false,
    };
    applies(binding, state) && !pointless
}

/// The action a key press sends in `state`, if any.
#[must_use]
pub fn action_for_key(state: &AppState, key: KeyEvent) -> Option<Action> {
    // Key releases and repeats (reported only where the terminal enables them) are not presses.
    if key.kind != KeyEventKind::Press {
        return None;
    }
    // A shifted character arrives with SHIFT set on some terminals and not on others; the
    // character already says which key it is, so SHIFT is ignored for characters. Shift-Tab is
    // its own key code, reported with SHIFT on some terminals.
    let modifiers = match key.code {
        KeyCode::Char(_) | KeyCode::BackTab => key.modifiers - KeyModifiers::SHIFT,
        _ => key.modifiers,
    };
    let screen = state.screen();
    let bound = BINDINGS
        .iter()
        .find(|b| b.code == key.code && b.modifiers == modifiers && available(b, state))
        .map(|b| b.action.clone());
    if state.is_editing() {
        // Any other unmodified character is text. The key itself is never logged here: it may
        // be part of a passphrase, and `FormKey`'s `Debug` already hides it in the action.
        let action = bound.or(match key.code {
            KeyCode::Char(c) if modifiers.is_empty() => Some(Action::Form(FormKey::Char(c))),
            _ => None,
        });
        tracing::trace!(target: "capsule_cli::tui", ?screen, ?action, "key mapped while editing");
        return action;
    }
    tracing::trace!(target: "capsule_cli::tui", ?key, ?screen, action = ?bound, "key mapped");
    bound
}

/// The action a terminal event sends in `state`, if any. Only key presses send one; a resize
/// or a focus change needs no state change, and the loop redraws after every event anyway.
#[must_use]
pub fn action_for_event(state: &AppState, event: &Event) -> Option<Action> {
    match event {
        Event::Key(key) => action_for_key(state, *key),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
