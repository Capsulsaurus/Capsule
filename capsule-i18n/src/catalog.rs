//! Message bundles loaded from the generated catalogs.

use std::collections::BTreeMap;

use crate::format::{Value, format_message};
use crate::generated;

/// A localized message bundle: a primary locale plus the source locale as a fallback.
///
/// Look-ups try the primary locale first, then the source locale, so a key that has
/// not been translated yet still renders in the source language instead of breaking.
#[derive(Debug, Clone)]
pub struct Bundle {
    locale: String,
    primary: BTreeMap<String, String>,
    fallback: BTreeMap<String, String>,
}

impl Bundle {
    /// Build a bundle for `locale`, using the source locale as the fallback.
    ///
    /// `locale` should already be a supported tag (see [`crate::negotiate`]); an
    /// unknown locale yields a bundle backed solely by the source catalog.
    #[must_use]
    pub fn for_locale(locale: &str) -> Self {
        let primary = load_bundle(locale).unwrap_or_default();
        let fallback = load_bundle(generated::SOURCE_LOCALE).unwrap_or_default();
        Self {
            locale: locale.to_string(),
            primary,
            fallback,
        }
    }

    /// The locale this bundle was built for.
    #[must_use]
    pub fn locale(&self) -> &str {
        &self.locale
    }

    /// The raw message template for `key`, trying the primary locale then the source.
    #[must_use]
    pub fn message(&self, key: &str) -> Option<&str> {
        self.primary
            .get(key)
            .or_else(|| self.fallback.get(key))
            .map(String::as_str)
    }

    /// Format `key` with `args`. Returns `key` itself when the key is unknown, so a
    /// missing message surfaces visibly rather than as an empty string.
    #[must_use]
    pub fn format(&self, key: &str, args: &[(&str, Value<'_>)]) -> String {
        if let Some(template) = self.message(key) {
            format_message(template, args)
        } else {
            tracing::warn!(key, locale = %self.locale, "missing i18n message");
            key.to_string()
        }
    }
}

/// The supported locales, in `locales/config.json` order.
#[must_use]
pub fn supported_locales() -> &'static [&'static str] {
    generated::SUPPORTED_LOCALES
}

/// Parse the embedded flat `{ key: message }` bundle for `locale`, if present.
fn load_bundle(locale: &str) -> Option<BTreeMap<String, String>> {
    let json = generated::BUNDLES
        .iter()
        .find(|(tag, _)| *tag == locale)
        .map(|(_, json)| *json)?;
    match serde_json::from_str(json) {
        Ok(map) => Some(map),
        Err(error) => {
            tracing::error!(locale, %error, "failed to parse generated i18n bundle");
            None
        }
    }
}
