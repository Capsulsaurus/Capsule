//! The translation lifecycle: coverage, freshness, translator context and the glossary.
//!
//! The renderers in the parent module care only about each locale's messages. This module
//! reads the rest of each catalog entry and refuses a catalog set that would ship a gap: a
//! source entry without `context`, a translation missing a source key, a translation whose
//! `source` hash no longer matches the English it was made from, and a translation that
//! drops a `verbatim` glossary term. It also produces the two inputs a translation pass
//! needs, `--todo` (what to translate) and `--stamp` (the one-time migration that records
//! `source` on translations made before it existed).

use std::collections::BTreeMap;
use std::path::Path;

use eyre::{Context, ContextCompat, Result};
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::read_json;

/// The repo-relative glossary path.
pub(super) const GLOSSARY: &str = "locales/glossary.json";

/// The lifecycle fields of one catalog entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Entry {
    pub(super) message: String,
    /// Translator context; required on the source catalog, absent on translations.
    pub(super) context: Option<String>,
    /// [`source_hash`] of the source message a translation was made from.
    pub(super) source: Option<String>,
}

impl Entry {
    fn from_json(value: &Value) -> Result<Self> {
        let text = |field: &str| value.get(field).and_then(Value::as_str).map(str::to_string);
        Ok(Self {
            message: text("message").context("missing a string `message`")?,
            context: text("context"),
            source: text("source"),
        })
    }
}

/// The hash a translation records of the source message it translates: the first 16 hex
/// digits of its SHA-256.
pub(super) fn source_hash(message: &str) -> String {
    let digest = Sha256::digest(message.as_bytes());
    hex::encode(&digest[..8])
}

/// The checked half of `locales/glossary.json`. `terms` (preferred renderings) is a
/// reference for translators and deliberately not parsed: inflection defeats an exact match.
#[derive(Debug, Default)]
pub(super) struct Glossary {
    /// Names never translated or transliterated.
    pub(super) verbatim: Vec<String>,
}

impl Glossary {
    pub(super) fn load(root: &Path) -> Result<Self> {
        let path = root.join(GLOSSARY);
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub(super) fn parse(text: &str) -> Result<Self> {
        let value: Value = serde_json::from_str(text)?;
        let verbatim = value
            .get("verbatim")
            .and_then(Value::as_array)
            .context("missing array `verbatim`")?
            .iter()
            .map(|term| {
                term.as_str()
                    .map(str::to_string)
                    .context("`verbatim` entries must be strings")
            })
            .collect::<Result<_>>()?;
        Ok(Self { verbatim })
    }

    /// The verbatim terms `source` uses as whole words that `translation` does not carry.
    /// The translation side is a plain substring test: CJK scripts have no word boundary.
    fn dropped<'a>(&'a self, source: &str, translation: &str) -> Vec<&'a str> {
        self.verbatim
            .iter()
            .filter(|term| contains_word(source, term) && !translation.contains(term.as_str()))
            .map(String::as_str)
            .collect()
    }
}

/// Whether `text` contains `word` not flanked by a letter, digit, `-` or `_`.
fn contains_word(text: &str, word: &str) -> bool {
    let joins = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '-' || c == '_');
    text.match_indices(word).any(|(at, _)| {
        !joins(text[..at].chars().next_back()) && !joins(text[at + word.len()..].chars().next())
    })
}

/// Read every supported locale's catalog as lifecycle entries.
pub(super) fn load(
    root: &Path,
    supported: &[String],
) -> Result<BTreeMap<String, BTreeMap<String, Entry>>> {
    supported
        .iter()
        .map(|locale| {
            let path = root.join(format!("locales/{locale}.json"));
            let entries = read_json(&path)?
                .as_object()
                .with_context(|| format!("{} must be a JSON object", path.display()))?
                .iter()
                .map(|(key, value)| {
                    Entry::from_json(value)
                        .with_context(|| format!("{}: key `{key}`", path.display()))
                        .map(|entry| (key.clone(), entry))
                })
                .collect::<Result<_>>()?;
            Ok((locale.clone(), entries))
        })
        .collect()
}

/// Every lifecycle violation: the source catalog's first, then each translation's in
/// locale order, each in key order.
pub(super) fn problems(
    source_locale: &str,
    catalogs: &BTreeMap<String, BTreeMap<String, Entry>>,
    glossary: &Glossary,
) -> Vec<String> {
    let Some(source) = catalogs.get(source_locale) else {
        return vec![format!("source locale `{source_locale}` has no catalog")];
    };
    let mut found: Vec<String> = source
        .iter()
        .filter(|(_, entry)| entry.context.as_deref().is_none_or(|c| c.trim().is_empty()))
        .map(|(key, _)| format!("{source_locale}: key `{key}` has no `context`"))
        .collect();
    for (locale, translation) in catalogs {
        if locale == source_locale {
            continue;
        }
        for (key, original) in source {
            let Some(entry) = translation.get(key) else {
                found.push(format!("{locale}: key `{key}` is missing"));
                continue;
            };
            match &entry.source {
                None => found.push(format!("{locale}: key `{key}` has no `source`")),
                Some(hash) if *hash != source_hash(&original.message) => found.push(format!(
                    "{locale}: key `{key}` is stale: its source message changed"
                )),
                Some(_) => {}
            }
            let dropped = glossary.dropped(&original.message, &entry.message);
            if !dropped.is_empty() {
                found.push(format!(
                    "{locale}: key `{key}` drops verbatim term(s): {}",
                    dropped.join(", ")
                ));
            }
        }
    }
    found
}

/// Why a key needs a translation pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum TodoReason {
    Missing,
    Stale,
}

/// One key a locale needs translated, with everything a translator needs to do it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct Todo {
    pub(super) key: String,
    pub(super) reason: TodoReason,
    /// The current source message.
    pub(super) message: String,
    pub(super) context: String,
    /// The stale translation, as a starting point; absent for a missing key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) current: Option<String>,
}

/// The keys `translation` lacks or translated from an older source message, in key order.
/// A translation with no recorded `source` is not listed: `--stamp` is its remedy.
pub(super) fn todo(
    source: &BTreeMap<String, Entry>,
    translation: &BTreeMap<String, Entry>,
) -> Vec<Todo> {
    source
        .iter()
        .filter_map(|(key, original)| {
            let (reason, current) = match translation.get(key) {
                None => (TodoReason::Missing, None),
                Some(entry) => match &entry.source {
                    Some(hash) if *hash != source_hash(&original.message) => {
                        (TodoReason::Stale, Some(entry.message.clone()))
                    }
                    _ => return None,
                },
            };
            Some(Todo {
                key: key.clone(),
                reason,
                message: original.message.clone(),
                context: original.context.clone().unwrap_or_default(),
                current,
            })
        })
        .collect()
}

/// Record `source` on every translation entry that has none, hashing the current source
/// message; returns how many entries were stamped. A recorded hash is never overwritten:
/// that would mark a stale translation current.
pub(super) fn stamp(
    source: &BTreeMap<String, Entry>,
    translation: &mut Map<String, Value>,
) -> usize {
    let mut stamped = 0;
    for (key, value) in translation.iter_mut() {
        let (Some(original), Some(entry)) = (source.get(key), value.as_object_mut()) else {
            continue;
        };
        if !entry.contains_key("source") {
            entry.insert(
                "source".to_string(),
                Value::String(source_hash(&original.message)),
            );
            stamped += 1;
        }
    }
    stamped
}

#[cfg(test)]
mod tests;
