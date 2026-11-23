//! The README's generated language regions: the switcher line under the logo and the
//! translations table.
//!
//! Both are pure functions of `locales/config.json` (its locale order and
//! `displayNames`), so README.md changes only when a locale is added or renamed. The
//! table's progress cells are shields.io badges that read the generated
//! `locales/stats.json` live, so catalog edits never touch the README.
//!
//! Each region sits between a `<!-- NAME:begin -->` and a `<!-- NAME:end -->` line;
//! [`sync`] rewrites what lies between them and the drift gate compares the result.

use eyre::{Result, bail, eyre};
use serde_json::Value;

/// The switcher region's marker name.
pub(crate) const SWITCHER: &str = "languages";
/// The translations-table region's marker name.
pub(crate) const TABLE: &str = "translations";

/// Where the README badges read the per-locale progress from (the committed
/// `locales/stats.json` on the default branch).
const STATS_URL: &str =
    "https://raw.githubusercontent.com/Capsulsaurus/Capsule/master/locales/stats.json";

/// The supported locales and their endonyms, in `config.json` order.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Languages {
    /// The source locale tag; its README is `README.md`.
    source: String,
    /// `(tag, endonym)` for every supported locale, source included.
    entries: Vec<(String, String)>,
}

impl Languages {
    /// Parse `locales/config.json`. Every supported locale must have a non-empty
    /// `displayNames` entry, and `displayNames` may name no other locale.
    pub(crate) fn from_config(config: &Value) -> Result<Self> {
        let source = config
            .get("sourceLocale")
            .and_then(Value::as_str)
            .ok_or_else(|| eyre!("locales/config.json: missing string `sourceLocale`"))?;
        let supported = config
            .get("supportedLocales")
            .and_then(Value::as_array)
            .ok_or_else(|| eyre!("locales/config.json: missing array `supportedLocales`"))?;
        let names = config
            .get("displayNames")
            .and_then(Value::as_object)
            .ok_or_else(|| eyre!("locales/config.json: missing object `displayNames`"))?;
        let mut entries = Vec::with_capacity(supported.len());
        for value in supported {
            let tag = value.as_str().ok_or_else(|| {
                eyre!("locales/config.json: `supportedLocales` entries must be strings")
            })?;
            let name = names
                .get(tag)
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .ok_or_else(|| {
                    eyre!("locales/config.json: `displayNames` has no name for `{tag}`")
                })?;
            entries.push((tag.to_string(), name.to_string()));
        }
        for tag in names.keys() {
            if !entries.iter().any(|(t, _)| t == tag) {
                bail!("locales/config.json: `displayNames` names unsupported locale `{tag}`");
            }
        }
        Ok(Self {
            source: source.to_string(),
            entries,
        })
    }

    fn readme(&self, tag: &str) -> String {
        if tag == self.source {
            "README.md".to_string()
        } else {
            format!("README.{tag}.md")
        }
    }

    /// One centred line linking every README by its language's own name.
    pub(crate) fn switcher(&self) -> String {
        let links: Vec<String> = self
            .entries
            .iter()
            .map(|(tag, name)| format!("<a href=\"{}\">{name}</a>", self.readme(tag)))
            .collect();
        format!("<p align=\"center\">{}</p>\n", links.join(" · "))
    }

    /// The translations table: one row per translated locale, its README, and two live
    /// badges (share of strings translated, share reviewed by a fluent reader).
    pub(crate) fn table(&self) -> String {
        let mut s = String::from(
            "| Language | README | App strings translated | Reviewed by a fluent reader |\n\
             | --- | --- | --- | --- |\n",
        );
        let rows = self
            .entries
            .iter()
            .filter(|(tag, _)| *tag != self.source)
            .map(|(tag, name)| {
                let readme = self.readme(tag);
                format!(
                    "| {name} | [{readme}]({readme}) | {} | {} |\n",
                    badge(tag, "translated"),
                    badge(tag, "reviewed"),
                )
            });
        s.extend(rows);
        s
    }
}

/// A shields.io dynamic-JSON badge showing `stats.json`'s `field` for `tag` as a percent.
fn badge(tag: &str, field: &str) -> String {
    let url = format!(
        "https://img.shields.io/badge/dynamic/json?url={}&query={}&suffix=%25&label=&color=blue",
        encode(STATS_URL),
        encode(&format!("$['{tag}'].{field}")),
    );
    format!("![{tag} {field}]({url})")
}

/// Upper-case hex digits for percent-encoding.
const HEX: &[u8; 16] = b"0123456789ABCDEF";

/// Percent-encode everything outside the URL-unreserved set.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(byte >> 4)]));
            out.push(char::from(HEX[usize::from(byte & 0xf)]));
        }
    }
    out
}

/// Replace the body of region `name` in `doc` with `body`, framed by blank lines so the
/// markers and the body segment as separate blocks. Fails when the region's markers are
/// missing, duplicated, or out of order.
pub(crate) fn splice(doc: &str, name: &str, body: &str) -> Result<String> {
    let begin = format!("<!-- {name}:begin -->");
    let end = format!("<!-- {name}:end -->");
    let find = |marker: &str| -> Result<usize> {
        let mut hits = doc.match_indices(marker).map(|(i, _)| i);
        let first = hits
            .next()
            .ok_or_else(|| eyre!("README.md: missing `{marker}`"))?;
        if hits.next().is_some() {
            bail!("README.md: `{marker}` appears more than once");
        }
        Ok(first)
    };
    let start = find(&begin)? + begin.len();
    let stop = find(&end)?;
    if stop < start {
        bail!("README.md: `{end}` comes before `{begin}`");
    }
    Ok(format!("{}\n\n{body}\n{}", &doc[..start], &doc[stop..]))
}

/// Rewrite both generated regions of `doc` from `languages`.
pub(crate) fn sync(doc: &str, languages: &Languages) -> Result<String> {
    let doc = splice(doc, SWITCHER, &languages.switcher())?;
    splice(&doc, TABLE, &languages.table())
}

#[cfg(test)]
mod tests;
