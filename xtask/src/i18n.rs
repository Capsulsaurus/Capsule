//! `xtask i18n`: compile the canonical `locales/` catalogs into every platform's
//! native localization format.
//!
//! The repo-root `locales/` directory is the single source of truth for user-facing
//! strings (ICU MessageFormat JSON). This generator reads it and emits:
//!
//! - `capsule-i18n/src/bundles/<locale>.json` + `generated.rs` — the Rust runtime bundle.
//! - `capsule-web/src/i18n/messages/<locale>.json` — the FormatJS-consumable web catalog.
//! - `capsule-android/.../res/values[-<qualifier>]/strings.xml` — Android resources.
//! - `capsule-swift/Generated/Localizable.xcstrings` — an Apple String Catalog (EXPERIMENTAL).
//!
//! Every renderer is a pure function of the parsed catalogs, so `--check` can
//! re-render in memory and diff against the committed files — the CI drift gate.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use eyre::{Context, ContextCompat, Result, bail};
use regex::Regex;
use serde_json::{Map, Value};

/// Render every target and either write the files or, in `check` mode, verify the
/// committed files match (failing if any drifted).
pub(crate) fn run(root: &Path, check: bool) -> Result<()> {
    let catalogs = Catalogs::load(root)?;
    let outputs = catalogs.render()?;
    if check {
        check_outputs(root, &outputs)
    } else {
        write_outputs(root, &outputs)
    }
}

/// The parsed catalogs plus the locale configuration.
struct Catalogs {
    /// The source (authoring) locale — the fallback for every other locale.
    source: String,
    /// Supported locales, in `config.json` order.
    supported: Vec<String>,
    /// `locale -> (key -> message)`; both maps are sorted for deterministic output.
    messages: BTreeMap<String, BTreeMap<String, String>>,
}

impl Catalogs {
    /// Read `locales/config.json` and every supported locale's catalog.
    fn load(root: &Path) -> Result<Self> {
        let config = read_json(&root.join("locales/config.json"))?;
        let source = config
            .get("sourceLocale")
            .and_then(Value::as_str)
            .context("locales/config.json: missing string `sourceLocale`")?
            .to_string();
        let supported = config
            .get("supportedLocales")
            .and_then(Value::as_array)
            .context("locales/config.json: missing array `supportedLocales`")?
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .context("locales/config.json: `supportedLocales` entries must be strings")
            })
            .collect::<Result<Vec<_>>>()?;
        if !supported.iter().any(|l| l == &source) {
            bail!("locales/config.json: sourceLocale `{source}` is not in supportedLocales");
        }

        let mut messages = BTreeMap::new();
        for locale in &supported {
            let path = root.join(format!("locales/{locale}.json"));
            let catalog = read_json(&path)?;
            let entries = catalog
                .as_object()
                .with_context(|| format!("{} must be a JSON object", path.display()))?;
            let mut map = BTreeMap::new();
            for (key, entry) in entries {
                let message = entry
                    .get("message")
                    .and_then(Value::as_str)
                    .with_context(|| {
                        format!(
                            "{}: key `{key}` is missing a string `message`",
                            path.display()
                        )
                    })?;
                map.insert(key.clone(), message.to_string());
            }
            messages.insert(locale.clone(), map);
        }
        Ok(Self {
            source,
            supported,
            messages,
        })
    }

    /// Build the `(relative path, content)` list for every generated file.
    fn render(&self) -> Result<Vec<(PathBuf, String)>> {
        let mut outputs = Vec::new();
        for (locale, map) in &self.messages {
            let json = flat_json(map)?;
            outputs.push((
                PathBuf::from(format!("capsule-i18n/src/bundles/{locale}.json")),
                json.clone(),
            ));
            outputs.push((
                PathBuf::from(format!("capsule-web/src/i18n/messages/{locale}.json")),
                json,
            ));
            outputs.push((android_path(&self.source, locale), self.android_xml(map)?));
        }
        outputs.push((
            PathBuf::from("capsule-i18n/src/generated.rs"),
            self.rust_generated(),
        ));
        outputs.push((
            PathBuf::from("capsule-swift/Generated/Localizable.xcstrings"),
            self.xcstrings()?,
        ));
        Ok(outputs)
    }

    /// The committed Rust module: bundle pointers plus the `error.*` code constants.
    fn rust_generated(&self) -> String {
        let mut s = String::new();
        s.push_str(
            "//! GENERATED by `cargo run -p xtask -- i18n` from `locales/`. Do not edit by hand.\n",
        );
        s.push_str("//!\n");
        s.push_str("//! Run `mise run i18n` to regenerate after changing the catalogs.\n\n");

        s.push_str("/// The source (authoring) locale; the final fallback for every lookup.\n");
        let _ = writeln!(
            s,
            "pub(crate) const SOURCE_LOCALE: &str = {:?};\n",
            self.source
        );

        s.push_str("/// Supported locales, in `locales/config.json` order.\n");
        let _ = writeln!(
            s,
            "{}\n",
            str_slice_const(
                "pub(crate) const SUPPORTED_LOCALES: &[&str] = ",
                &self.supported,
            )
        );

        s.push_str(
            "/// `(locale, json)` pairs. Each `json` is a flat `{ key: message }` object.\n",
        );
        if let [only] = self.supported.as_slice() {
            let _ = writeln!(
                s,
                "pub(crate) const BUNDLES: &[(&str, &str)] = &[({only:?}, include_str!(\"bundles/{only}.json\"))];\n"
            );
        } else {
            s.push_str("pub(crate) const BUNDLES: &[(&str, &str)] = &[\n");
            for locale in &self.supported {
                let _ = writeln!(
                    s,
                    "    ({locale:?}, include_str!(\"bundles/{locale}.json\")),"
                );
            }
            s.push_str("];\n\n");
        }

        s.push_str("/// Stable error codes — the `error.*` namespace of the message catalog.\n");
        s.push_str("///\n");
        s.push_str(
            "/// The server attaches one of these as `ApiError.code`; clients map it to a\n",
        );
        s.push_str("/// localized high-level message. Generated from the source catalog.\n");
        let codes = self.error_codes();
        if codes.is_empty() {
            s.push_str("pub mod error_codes {}\n");
        } else {
            s.push_str("pub mod error_codes {\n");
            for (index, key) in codes.iter().enumerate() {
                if index > 0 {
                    s.push('\n');
                }
                let _ = writeln!(s, "    /// `{key}`");
                let _ = writeln!(s, "    pub const {}: &str = {key:?};", const_name(key));
            }
            s.push_str("}\n");
        }
        s
    }

    /// The Android `<resources>` document for one locale's catalog.
    fn android_xml(&self, map: &BTreeMap<String, String>) -> Result<String> {
        let complex = Regex::new(r"\{[^{}]*,[^{}]*\}").expect("static regex is valid");
        let mut s = String::new();
        // The banner names the task, not the raw cargo command: XML forbids `--`
        // anywhere inside a comment, and `cargo run -p xtask -- i18n` contains
        // one. aapt2 enforces that and fails the whole Android resource merge.
        s.push_str("<!-- GENERATED by `mise run i18n` from locales/. Do not edit by hand. -->\n");
        s.push_str("<resources>\n");
        for (key, message) in map {
            let name = android_name(key);
            if complex.is_match(message) {
                // ICU plural/select doesn't map 1:1 to a flat <string>; skip rather
                // than mis-translate. Compiling these is a documented follow-up.
                let _ = writeln!(
                    s,
                    "    <!-- TODO(i18n): ICU plural/select not yet compiled for Android: {name} -->"
                );
                continue;
            }
            let _ = writeln!(
                s,
                "    <string name=\"{name}\">{}</string>",
                android_escape(message)
            );
        }
        s.push_str("</resources>\n");
        Ok(s)
    }

    /// An Apple String Catalog containing every locale (EXPERIMENTAL — not yet wired
    /// into the Xcode project; see the i18n design doc).
    fn xcstrings(&self) -> Result<String> {
        let mut keys: BTreeSet<&String> = BTreeSet::new();
        for map in self.messages.values() {
            keys.extend(map.keys());
        }
        let mut strings = Map::new();
        for key in keys {
            let mut localizations = Map::new();
            for (locale, map) in &self.messages {
                if let Some(message) = map.get(key) {
                    localizations.insert(
                        locale.clone(),
                        serde_json::json!({
                            "stringUnit": { "state": "translated", "value": message }
                        }),
                    );
                }
            }
            strings.insert(
                key.clone(),
                serde_json::json!({ "localizations": Value::Object(localizations) }),
            );
        }
        let catalog = serde_json::json!({
            "sourceLanguage": self.source,
            "strings": Value::Object(strings),
            "version": "1.0",
        });
        let mut s = serde_json::to_string_pretty(&catalog).context("serializing xcstrings")?;
        s.push('\n');
        Ok(s)
    }

    /// Source-locale keys under the `error.*` namespace, sorted.
    fn error_codes(&self) -> Vec<String> {
        let Some(source_map) = self.messages.get(&self.source) else {
            return Vec::new();
        };
        source_map
            .keys()
            .filter(|k| k.starts_with("error."))
            .cloned()
            .collect()
    }
}

/// Render `prefix&[<items>];` as a `&[&str]` const declaration the way rustfmt would,
/// so the generated file stays `cargo fmt`-clean as the locale set grows (rustfmt's
/// `max_width` is 100). rustfmt keeps the whole declaration on one line when it fits;
/// otherwise it breaks the brackets and keeps the elements on one indented line if
/// they fit, falling back to one element per line. `prefix` ends with `= `.
fn str_slice_const(prefix: &str, items: &[String]) -> String {
    const MAX_WIDTH: usize = 100;
    let quoted: Vec<String> = items.iter().map(|i| format!("{i:?}")).collect();
    let inner = quoted.join(", ");
    let one_line = format!("{prefix}&[{inner}];");
    if one_line.len() <= MAX_WIDTH {
        return one_line;
    }
    // Block form: elements on one indented (4-space) line, comma-terminated.
    if inner.len() + "    ".len() + ",".len() <= MAX_WIDTH {
        return format!("{prefix}&[\n    {inner},\n];");
    }
    // Fallback: one element per line.
    let mut s = format!("{prefix}&[\n");
    for q in &quoted {
        let _ = writeln!(s, "    {q},");
    }
    s.push_str("];");
    s
}

/// Pretty-print a flat `{ key: message }` bundle (2-space indent, trailing newline).
fn flat_json(map: &BTreeMap<String, String>) -> Result<String> {
    let mut s = serde_json::to_string_pretty(map).context("serializing message bundle")?;
    s.push('\n');
    Ok(s)
}

/// The Android `strings.xml` path for `locale` (the source locale has no qualifier).
fn android_path(source: &str, locale: &str) -> PathBuf {
    let base = "capsule-android/src/androidMain/res";
    if locale == source {
        PathBuf::from(format!("{base}/values/strings.xml"))
    } else {
        PathBuf::from(format!(
            "{base}/values-{}/strings.xml",
            android_qualifier(locale)
        ))
    }
}

/// Map a BCP-47 tag to an Android resource qualifier.
///
/// Android accepts two spellings and they are not interchangeable:
///
/// - `language[-rREGION]`, where REGION is a two-letter ISO 3166-1 code. This is
///   the only form the legacy qualifier supports.
/// - `b+language+Script[+REGION]`, the BCP-47 form, which is the *only* way to
///   name a script.
///
/// The distinction is load-bearing: a script subtag is four letters
/// (`Hans`, `Hant`, `Latn`), and squeezing it into `-rREGION` yields
/// `values-zh-rHANS`, which `aapt2` rejects outright with "Invalid resource
/// directory name" — the whole Android build fails, not just that locale.
fn android_qualifier(locale: &str) -> String {
    let mut parts = locale.split('-');
    let lang = parts.next().unwrap_or(locale).to_ascii_lowercase();
    let Some(subtag) = parts.next() else {
        return lang;
    };
    if is_script_subtag(subtag) {
        // BCP-47 form, preserving the script's canonical title case.
        let script = title_case(subtag);
        return match parts.next() {
            Some(region) => format!("b+{lang}+{script}+{}", region.to_ascii_uppercase()),
            None => format!("b+{lang}+{script}"),
        };
    }
    format!("{lang}-r{}", subtag.to_ascii_uppercase())
}

/// Whether a BCP-47 subtag is a script: exactly four ASCII letters.
///
/// Regions are two letters or three digits, so the length alone separates them.
fn is_script_subtag(subtag: &str) -> bool {
    subtag.len() == 4 && subtag.chars().all(|c| c.is_ascii_alphabetic())
}

/// `hans` / `HANS` / `Hans` all become `Hans` — the canonical script casing.
fn title_case(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => {
            first.to_ascii_uppercase().to_string() + &chars.as_str().to_ascii_lowercase()
        }
        None => String::new(),
    }
}

/// Sanitize a catalog key into a valid Android resource name (`.`/`-` become `_`).
fn android_name(key: &str) -> String {
    key.replace(['.', '-'], "_")
}

/// Escape a message for an Android `<string>` body (XML plus Android quoting).
fn android_escape(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    for c in message.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\'' => out.push_str("\\'"),
            '"' => out.push_str("\\\""),
            _ => out.push(c),
        }
    }
    out
}

/// Turn an `error.*` key into a constant name (`error.auth.x` -> `AUTH_X`).
fn const_name(key: &str) -> String {
    key.strip_prefix("error.")
        .unwrap_or(key)
        .replace(['.', '-'], "_")
        .to_ascii_uppercase()
}

/// Parse a JSON file into a [`Value`], with path context on failure.
fn read_json(path: &Path) -> Result<Value> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Write every output, creating parent directories and reporting what changed.
fn write_outputs(root: &Path, outputs: &[(PathBuf, String)]) -> Result<()> {
    for (rel, content) in outputs {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let changed = fs::read_to_string(&path).map_or(true, |existing| existing != *content);
        if changed {
            fs::write(&path, content).with_context(|| format!("writing {}", path.display()))?;
            println!("generated {}", rel.display());
        } else {
            println!("unchanged {}", rel.display());
        }
    }
    Ok(())
}

/// Verify every committed file matches its freshly rendered content.
fn check_outputs(root: &Path, outputs: &[(PathBuf, String)]) -> Result<()> {
    let mut drift = Vec::new();
    for (rel, content) in outputs {
        match fs::read_to_string(root.join(rel)) {
            Ok(existing) if existing == *content => {}
            Ok(_) => drift.push(format!("{} (out of date)", rel.display())),
            Err(_) => drift.push(format!("{} (missing)", rel.display())),
        }
    }
    if drift.is_empty() {
        println!("i18n: generated files are up to date");
        Ok(())
    } else {
        bail!(
            "i18n generated files are stale; run `mise run i18n`:\n  {}",
            drift.join("\n  ")
        );
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn android_qualifier_uses_bcp47_for_script_subtags() {
        // A script subtag has no legacy spelling: `values-zh-rHANS` is what the
        // old mapping produced and `aapt2` rejects it as an invalid resource
        // directory name, failing the entire Android build.
        assert_eq!(android_qualifier("zh-Hans"), "b+zh+Hans");
        assert_eq!(android_qualifier("zh-Hant"), "b+zh+Hant");
    }

    #[test]
    fn android_qualifier_uses_the_legacy_form_for_regions() {
        assert_eq!(android_qualifier("pt-BR"), "pt-rBR");
        assert_eq!(android_qualifier("en-us"), "en-rUS");
    }

    #[test]
    fn android_qualifier_passes_bare_languages_through() {
        assert_eq!(android_qualifier("fr"), "fr");
        assert_eq!(android_qualifier("ar"), "ar");
    }

    #[test]
    fn script_subtags_are_four_letters_regions_are_not() {
        assert!(is_script_subtag("Hans"));
        assert!(is_script_subtag("Latn"));
        assert!(!is_script_subtag("BR"));
        assert!(!is_script_subtag("419"));
    }

    use std::collections::BTreeMap;

    use super::{Catalogs, android_qualifier, is_script_subtag, str_slice_const};

    /// XML forbids `--` inside a comment, and aapt2 enforces it: a banner
    /// containing one fails the entire Android resource merge, not just the file.
    #[test]
    fn android_documents_contain_no_double_hyphen_in_comments() {
        let mut catalog = BTreeMap::new();
        catalog.insert("app_name".to_string(), "Capsule".to_string());
        catalog.insert(
            "plural_key".to_string(),
            "{count, plural, other {# photos}}".to_string(),
        );
        let catalogs = Catalogs {
            source: "en".to_string(),
            supported: vec!["en".to_string()],
            messages: BTreeMap::from([("en".to_string(), catalog.clone())]),
        };
        let xml = catalogs.android_xml(&catalog).expect("renders");
        for line in xml.lines() {
            let Some(body) = line.trim().strip_prefix("<!--") else {
                continue;
            };
            let body = body.trim_end_matches("-->");
            assert!(
                !body.contains("--"),
                "XML comment contains a forbidden `--`: {line}"
            );
        }
    }

    const PREFIX: &str = "pub(crate) const SUPPORTED_LOCALES: &[&str] = ";

    fn owned(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    /// A short list (the source-only build) stays on one line — matching rustfmt and
    /// the historical single-locale output.
    #[test]
    fn single_line_when_it_fits() {
        let out = str_slice_const(PREFIX, &owned(&["en"]));
        assert_eq!(out, format!("{PREFIX}&[\"en\"];"));
        assert!(out.lines().count() == 1);
    }

    /// The full twelve-locale set overflows `max_width`, so it breaks the brackets but
    /// keeps every element on one indented line — the exact shape rustfmt produces.
    #[test]
    fn block_form_when_elements_fit_on_one_indented_line() {
        let out = str_slice_const(
            PREFIX,
            &owned(&[
                "en", "zh-Hans", "zh-Hant", "ja", "ko", "fr", "de", "es", "pt-BR", "it", "ru",
                "hi", "ar",
            ]),
        );
        assert_eq!(
            out,
            format!(
                "{PREFIX}&[\n    \"en\", \"zh-Hans\", \"zh-Hant\", \"ja\", \"ko\", \"fr\", \"de\", \
                 \"es\", \"pt-BR\", \"it\", \"ru\", \"hi\", \"ar\",\n];"
            )
        );
        // Every rendered line stays within rustfmt's max_width.
        assert!(out.lines().all(|l| l.len() <= 100));
    }

    /// If even the single indented line would overflow, fall back to one per line.
    #[test]
    fn one_per_line_when_too_wide() {
        let items = owned(&[
            "aa-very-long-locale-tag",
            "bb-very-long-locale-tag",
            "cc-very-long-locale-tag",
            "dd-very-long-locale-tag",
            "ee-very-long-locale-tag",
        ]);
        let out = str_slice_const(PREFIX, &items);
        assert!(out.contains("\n    \"aa-very-long-locale-tag\",\n"));
        assert!(out.lines().all(|l| l.len() <= 100));
    }
}
