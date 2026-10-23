//! Terminal input to [`Action`]s, through one binding table.
//!
//! [`BINDINGS`] is the single source for both what a key does and what the help screen and the
//! footer say it does, so the two cannot drift.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::state::{Action, AppState, Screen};

/// Where a binding applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// On every screen.
    Global,
    /// Only while this screen is on top.
    On(Screen),
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
];

/// Whether `binding` applies on `screen`.
#[must_use]
pub fn applies(binding: &Binding, screen: Screen) -> bool {
    match binding.scope {
        Scope::Global => true,
        Scope::On(on) => on == screen,
    }
}

/// Whether `binding` does something in `state`: it applies on the screen on top, it is not
/// [`Action::Back`] on the root screen (nothing to go back to), and it does not open the screen
/// already on top. A binding that is not available neither matches a key nor shows in the
/// footer, so the footer never offers a key that does nothing.
#[must_use]
pub fn available(binding: &Binding, state: &AppState) -> bool {
    let screen = state.screen();
    let pointless = match binding.action {
        Action::Back => state.depth() == 1,
        Action::Open(target) => target == screen,
        _ => false,
    };
    applies(binding, screen) && !pointless
}

/// The action a key press sends in `state`, if any.
#[must_use]
pub fn action_for_key(state: &AppState, key: KeyEvent) -> Option<Action> {
    // Key releases and repeats (reported only where the terminal enables them) are not presses.
    if key.kind != KeyEventKind::Press {
        return None;
    }
    // A shifted character arrives with SHIFT set on some terminals and not on others; the
    // character already says which key it is, so SHIFT is ignored for characters.
    let modifiers = match key.code {
        KeyCode::Char(_) => key.modifiers - KeyModifiers::SHIFT,
        _ => key.modifiers,
    };
    let screen = state.screen();
    let action = BINDINGS
        .iter()
        .find(|b| b.code == key.code && b.modifiers == modifiers && available(b, state))
        .map(|b| b.action.clone());
    tracing::trace!(target: "capsule_cli::tui", ?key, ?screen, ?action, "key mapped");
    action
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
