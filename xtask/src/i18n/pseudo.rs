//! Development pseudo-locales for the web client: `en-XA` and `ar-XB`.
//!
//! Android ships these two pseudo-locales itself and Apple has Xcode's double-length and
//! right-to-left pseudolanguages, so only the web client needs them generated. Both are
//! derived from the source catalog alone and are never in `supportedLocales`: the web
//! client loads them in a development build only.
//!
//! - `en-XA` (accented): every letter is swapped for an accented look-alike, the text is
//!   padded about 40% longer, and the message is bracketed. A string that renders without
//!   the brackets was never looked up in the catalog — it is hardcoded — and one whose
//!   brackets are clipped did not absorb the expansion.
//! - `ar-XB` (right to left): every literal run is wrapped in a right-to-left override,
//!   so English reads mirrored while the app takes its right-to-left layout.
//!
//! Only literal text changes. `{name}` arguments, a plural's selector and categories,
//! and `#` stay exactly as the source has them, so every pseudo message formats with the
//! arguments the call site passes.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use eyre::{Context, Result};

use super::{IcuPlural, matching_brace};

/// The two pseudo-locales, in the order they are generated.
pub(super) const PSEUDO_LOCALES: &[(&str, Pseudo)] =
    &[("en-XA", Pseudo::Accented), ("ar-XB", Pseudo::Bidi)];

/// How a pseudo-locale rewrites literal text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Pseudo {
    /// `en-XA`: accented, about 40% longer, bracketed.
    Accented,
    /// `ar-XB`: each literal run forced right to left.
    Bidi,
}

/// Opens an `en-XA` message. The render tests read the bracket pair as the mark of a
/// catalogued string.
pub(super) const OPEN: char = '[';
/// Closes an `en-XA` message.
pub(super) const CLOSE: char = ']';
/// Right-to-left mark: gives an `ar-XB` message a right-to-left base direction.
pub(super) const RLM: char = '\u{200F}';
/// Right-to-left override: lays the following text out right to left.
pub(super) const RLO: char = '\u{202E}';
/// Pop directional formatting: ends the override [`RLO`] opened.
pub(super) const PDF: char = '\u{202C}';

/// The words `en-XA` pads with, the same filler Android's own `en-XA` uses.
const PADDING: &[&str] = &[
    "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
];

/// The accented look-alike of each ASCII letter, `a`..`z` then `A`..`Z`.
const ACCENTED_LOWER: [char; 26] = [
    'å', 'ƀ', 'ç', 'ð', 'é', 'ƒ', 'ĝ', 'ĥ', 'î', 'ĵ', 'ķ', 'ļ', 'ɱ', 'ñ', 'ö', 'þ', 'ǫ', 'ŕ', 'š',
    'ţ', 'û', 'ṽ', 'ŵ', 'ẋ', 'ý', 'ž',
];
const ACCENTED_UPPER: [char; 26] = [
    'Å', 'Ɓ', 'Ç', 'Ð', 'É', 'Ƒ', 'Ĝ', 'Ĥ', 'Î', 'Ĵ', 'Ķ', 'Ļ', 'Ṁ', 'Ñ', 'Ö', 'Þ', 'Ǫ', 'Ŕ', 'Š',
    'Ţ', 'Û', 'Ṽ', 'Ŵ', 'Ẋ', 'Ý', 'Ž',
];

impl Pseudo {
    /// Rewrite one source ICU message into this pseudo-locale.
    ///
    /// A whole-message plural keeps its selector and categories and has each category's
    /// body rewritten on its own, so every count still shows a marked string.
    pub(super) fn message(self, message: &str) -> Result<String> {
        match IcuPlural::parse(message)? {
            Some(plural) => {
                let mut out = format!("{{{}, plural,", plural.selector);
                for (category, body) in &plural.categories {
                    let _ = write!(out, " {category} {{{}}}", self.unit(body, true)?);
                }
                out.push('}');
                Ok(out)
            }
            None => self.unit(message, false),
        }
    }

    /// Rewrite one message body. In a plural body `#` is the count and stays as it is.
    fn unit(self, text: &str, in_plural: bool) -> Result<String> {
        let mut out = String::with_capacity(text.len() * 2);
        let mut run = String::new();
        let mut letters = 0usize;
        let mut chars = text.char_indices();
        while let Some((i, c)) = chars.next() {
            match c {
                '{' => {
                    let close = matching_brace(text, i)?;
                    self.flush(&mut run, &mut out);
                    out.push_str(&text[i..=close]);
                    for (j, _) in chars.by_ref() {
                        if j >= close {
                            break;
                        }
                    }
                }
                '#' if in_plural => {
                    self.flush(&mut run, &mut out);
                    out.push('#');
                }
                _ => {
                    if c.is_ascii_alphabetic() {
                        letters += 1;
                    }
                    run.push(c);
                }
            }
        }
        self.flush(&mut run, &mut out);
        Ok(match self {
            Self::Accented => {
                let padding = padding(letters);
                if padding.is_empty() {
                    format!("{OPEN}{out}{CLOSE}")
                } else {
                    format!("{OPEN}{out} {padding}{CLOSE}")
                }
            }
            Self::Bidi => format!("{RLM}{out}{RLM}"),
        })
    }

    /// Append the pending literal `run`, rewritten, to `out`.
    fn flush(self, run: &mut String, out: &mut String) {
        if run.is_empty() {
            return;
        }
        match self {
            Self::Accented => out.extend(run.chars().map(accent)),
            Self::Bidi => {
                out.push(RLO);
                out.push_str(run);
                out.push(PDF);
            }
        }
        run.clear();
    }
}

/// The accented look-alike of `c`, or `c` itself where it is not an ASCII letter.
fn accent(c: char) -> char {
    let index = |base: u8| usize::from(c as u8 - base);
    match c {
        'a'..='z' => ACCENTED_LOWER[index(b'a')],
        'A'..='Z' => ACCENTED_UPPER[index(b'A')],
        _ => c,
    }
}

/// Filler words adding at least 40% of `letters`, or nothing for a message with no letters.
fn padding(letters: usize) -> String {
    let target = (letters * 2).div_ceil(5);
    let mut out = String::new();
    for word in PADDING.iter().cycle() {
        if out.chars().count() >= target {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// Render every pseudo-locale's catalog from the source catalog: `(locale, key -> message)`.
pub(super) fn catalogs(
    source: &BTreeMap<String, String>,
) -> Result<Vec<(&'static str, BTreeMap<String, String>)>> {
    PSEUDO_LOCALES
        .iter()
        .map(|(locale, pseudo)| {
            let map = source
                .iter()
                .map(|(key, message)| {
                    pseudo
                        .message(message)
                        .with_context(|| format!("key `{key}` ({locale} pseudo-locale)"))
                        .map(|rewritten| (key.clone(), rewritten))
                })
                .collect::<Result<BTreeMap<_, _>>>()?;
            Ok((*locale, map))
        })
        .collect()
}

/// Spell the invisible direction controls as JSON escapes, so a generated bundle shows
/// where they are instead of silently reordering the file in an editor or a diff view.
pub(super) fn escape_controls(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    for c in json.chars() {
        match c {
            RLM | RLO | PDF => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests;
