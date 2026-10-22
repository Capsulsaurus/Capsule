//! The CLI's one seam onto the shared `locales/` catalogs.
//!
//! The CLI and TUI are English-only (ADR-0011): every line they print, every prompt, every
//! error and every `--help` page is inline English, and the process locale (`LANG`,
//! `LC_ALL`, `LC_MESSAGES`) is never consulted. The one catalog surface they still render is
//! the server's stable `error.*` codes: a failure that carries one is described with the
//! English catalog's message for it, so the CLI words a server refusal exactly as the apps'
//! English catalog does instead of re-wording it.

use std::sync::LazyLock;

use capsule_i18n::Bundle;
pub use capsule_i18n::error_codes;

/// The source-locale bundle, pinned: the CLI renders English whatever the environment says.
static ENGLISH: LazyLock<Bundle> = LazyLock::new(|| Bundle::for_locale("en"));

/// The English catalog message for a stable `error.*` code. An unknown code renders as the
/// code itself (and is logged), so it surfaces visibly rather than as an empty line.
#[must_use]
pub fn error_message(code: &str) -> String {
    ENGLISH.format(code, &[])
}

#[cfg(test)]
mod tests;
