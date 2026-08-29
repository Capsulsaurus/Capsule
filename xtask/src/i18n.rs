//! `xtask i18n`: compile the canonical `locales/` catalogs into every platform's
//! native localization format.
//!
//! The repo-root `locales/` directory is the single source of truth for user-facing
//! strings (ICU MessageFormat JSON). This generator reads it and emits:
//!
//! - `capsule-i18n/src/bundles/<locale>.json` + `generated.rs` — the Rust runtime bundle.
//! - `capsule-web/src/i18n/messages/<locale>.json` — the FormatJS-consumable web catalog.
//! - `capsule-android/.../res/values[-<qualifier>]/strings.xml` — Android resources.
//! - `capsule-swift/Generated/Localizable.xcstrings` — the iOS app's Apple String Catalog.
//! - `capsule-swift/Generated/InfoPlist.xcstrings` — the `Info.plist` usage descriptions.
//!
//! Every renderer is a pure function of the parsed catalogs, so `--check` can
//! re-render in memory and diff against the committed files — the CI drift gate.
//!
//! The two Apple catalogs are *compiled*, not copied: a String Catalog is not an ICU
//! consumer, so `{name}` becomes a `%@`/`%lld` specifier and a whole-message
//! `{n, plural, …}` becomes a `variations.plural` block. Anything outside that supported
//! subset fails the generator rather than reaching a user verbatim.

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
        outputs.push((
            PathBuf::from("capsule-swift/Generated/InfoPlist.xcstrings"),
            self.infoplist_xcstrings()?,
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
        s.push_str(
            "<!-- GENERATED by `cargo run -p xtask -- i18n` from locales/. Do not edit by hand. -->\n",
        );
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

    /// The app's Apple String Catalog: every key except the `ios.infoplist.*` namespace,
    /// which ships in its own catalog ([`Self::infoplist_xcstrings`]).
    fn xcstrings(&self) -> Result<String> {
        let pairs: Vec<(String, String)> = self
            .all_keys()
            .into_iter()
            .filter(|k| !k.starts_with(INFO_PLIST_NAMESPACE))
            .map(|k| (k.clone(), k))
            .collect();
        self.string_catalog(&pairs)
    }

    /// The `InfoPlist.xcstrings` catalog — the platform mechanism for localizing the
    /// usage-description strings the system shows in permission prompts. The catalog key
    /// is in the `ios.infoplist.*` namespace; the *output* key must be the literal
    /// `Info.plist` key, so the mapping is spelled out in [`INFO_PLIST_KEYS`] rather than
    /// derived, and a missing catalog key fails the generator.
    fn infoplist_xcstrings(&self) -> Result<String> {
        let known = self.all_keys();
        let mut pairs = Vec::new();
        for (catalog_key, plist_key) in INFO_PLIST_KEYS {
            if !known.iter().any(|k| k == catalog_key) {
                bail!("locales/: INFO_PLIST_KEYS references missing key `{catalog_key}`");
            }
            pairs.push(((*catalog_key).to_string(), (*plist_key).to_string()));
        }
        self.string_catalog(&pairs)
    }

    /// Every key across every locale, sorted.
    fn all_keys(&self) -> Vec<String> {
        let mut keys: BTreeSet<&String> = BTreeSet::new();
        for map in self.messages.values() {
            keys.extend(map.keys());
        }
        keys.into_iter().cloned().collect()
    }

    /// Render an Apple String Catalog over `pairs` of `(catalog key, catalog entry name)`.
    ///
    /// Each ICU message is compiled to Apple's own format — `%@` / `%lld` specifiers and
    /// `variations.plural` — because a String Catalog is **not** an ICU consumer: an
    /// uncompiled `{count, plural, …}` would be shown to the user verbatim. Argument
    /// positions and types come from the source locale ([`ArgPlan`]) so every translation
    /// numbers its specifiers identically even when it reorders them.
    fn string_catalog(&self, pairs: &[(String, String)]) -> Result<String> {
        let mut strings = Map::new();
        for (key, name) in pairs {
            let plan = match self.messages.get(&self.source).and_then(|m| m.get(key)) {
                Some(source_message) => ArgPlan::of(source_message)
                    .with_context(|| format!("key `{key}` ({} locale)", self.source))?,
                // A key absent from the source locale has no plan to inherit; treat every
                // argument as a string, which is the only safe default.
                None => ArgPlan::default(),
            };
            let mut localizations = Map::new();
            for (locale, map) in &self.messages {
                let Some(message) = map.get(key) else {
                    continue;
                };
                let compiled = AppleMessage::compile(message, &plan)
                    .with_context(|| format!("key `{key}` ({locale} locale)"))?;
                localizations.insert(locale.clone(), compiled.into_localization());
            }
            strings.insert(
                name.clone(),
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

/// The catalog namespace whose keys localize `Info.plist` entries rather than app UI.
const INFO_PLIST_NAMESPACE: &str = "ios.infoplist.";

/// `(catalog key, Info.plist key)` for every localized usage description the iOS app
/// declares. Spelled out rather than derived from the key name: the `Info.plist` side is a
/// fixed Apple vocabulary, and an unreviewed mechanical mapping could silently produce a
/// prompt the system ignores. Adding a usage description means adding a row here *and* the
/// key to `Project.swift`'s `infoPlist` (the base declaration the system requires).
const INFO_PLIST_KEYS: &[(&str, &str)] = &[
    (
        "ios.infoplist.photo_library_usage",
        "NSPhotoLibraryUsageDescription",
    ),
    ("ios.infoplist.face_id_usage", "NSFaceIDUsageDescription"),
];

/// The CLDR plural categories an Apple String Catalog accepts, in Apple's own order.
const PLURAL_CATEGORIES: &[&str] = &["zero", "one", "two", "few", "many", "other"];

/// How one ICU argument is spelled in an Apple format string: its 1-based position and
/// its conversion, `@` (string) or `lld` (integer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ArgSpec {
    position: usize,
    conversion: &'static str,
}

/// The argument plan for one catalog key, derived from the **source** locale.
///
/// Apple resolves a String Catalog entry by key and then formats it with the arguments the
/// Swift call site supplies, so every locale's format string must number and type its
/// specifiers the same way — including a translation that reorders them. Deriving the plan
/// once from the source message and reusing it for every locale is what guarantees that.
#[derive(Debug, Default, Clone)]
struct ArgPlan {
    args: BTreeMap<String, ArgSpec>,
}

impl ArgPlan {
    /// Derive the plan from a source-locale ICU message.
    ///
    /// A whole-message plural types its selector as an integer (`%lld`); every other
    /// `{name}` is a string (`%@`). Author numeric arguments as plurals — a bare `{count}`
    /// compiles to `%@`, which does not match the `%lld` a Swift `Int` interpolation
    /// supplies.
    fn of(message: &str) -> Result<Self> {
        let mut plan = Self::default();
        if let Some(plural) = IcuPlural::parse(message)? {
            plan.insert(&plural.selector, "lld");
            for body in plural.categories.values() {
                for name in simple_args(body)? {
                    plan.insert(&name, "@");
                }
            }
        } else {
            for name in simple_args(message)? {
                plan.insert(&name, "@");
            }
        }
        Ok(plan)
    }

    fn insert(&mut self, name: &str, conversion: &'static str) {
        let position = self.args.len() + 1;
        self.args.entry(name.to_string()).or_insert(ArgSpec {
            position,
            conversion,
        });
    }

    /// The format specifier for `name`. A single-argument message uses the unnumbered form
    /// (`%@`) — what Apple's own tooling emits; two or more use explicit positions so a
    /// translation may reorder them.
    fn specifier(&self, name: &str) -> Result<String> {
        let spec = self
            .args
            .get(name)
            .with_context(|| format!("argument `{{{name}}}` is not in the source message"))?;
        Ok(if self.args.len() == 1 {
            format!("%{}", spec.conversion)
        } else {
            format!("%{}${}", spec.position, spec.conversion)
        })
    }
}

/// A whole-message ICU `plural` block: `{name, plural, one {…} other {…}}`.
struct IcuPlural {
    selector: String,
    /// CLDR category -> the category's raw ICU body.
    categories: BTreeMap<String, String>,
}

impl IcuPlural {
    /// Parse `message` as a whole-message plural, or `Ok(None)` if it is not one.
    ///
    /// Only the whole-message form is supported: a plural embedded in surrounding text has
    /// no Apple equivalent short of `substitutions`, so it is rejected loudly rather than
    /// emitted wrong. `select`, `selectordinal`, and `offset:` are likewise rejected.
    fn parse(message: &str) -> Result<Option<Self>> {
        let text = message.trim();
        if !text.starts_with('{') || matching_brace(text, 0)? != text.len() - 1 {
            reject_embedded_block(text)?;
            return Ok(None);
        }
        let inner = &text[1..text.len() - 1];
        let mut head = inner.splitn(3, ',');
        let selector = head.next().unwrap_or_default().trim().to_string();
        let Some(kind) = head.next().map(str::trim) else {
            // `{name}` — a plain argument, not a plural.
            return Ok(None);
        };
        if kind != "plural" {
            bail!(
                "unsupported ICU block `{kind}`: only whole-message `plural` compiles to an Apple String Catalog"
            );
        }
        let body = head.next().unwrap_or_default().trim();
        if body.starts_with("offset:") {
            bail!("unsupported ICU `plural` with `offset:`");
        }
        let mut categories = BTreeMap::new();
        let mut rest = body;
        while !rest.trim().is_empty() {
            let trimmed = rest.trim_start();
            let split = trimmed
                .find('{')
                .with_context(|| format!("malformed ICU plural body near `{trimmed}`"))?;
            let category = trimmed[..split].trim().to_string();
            if !PLURAL_CATEGORIES.contains(&category.as_str()) {
                bail!(
                    "unsupported ICU plural category `{category}`: an Apple String Catalog \
                     accepts only {PLURAL_CATEGORIES:?}"
                );
            }
            let close = matching_brace(trimmed, split)?;
            categories.insert(category, trimmed[split + 1..close].to_string());
            rest = &trimmed[close + 1..];
        }
        if !categories.contains_key("other") {
            bail!("ICU plural is missing the required `other` category");
        }
        Ok(Some(Self {
            selector,
            categories,
        }))
    }
}

/// Fail if `text` contains an ICU `{name, kind, …}` block that is not the whole message —
/// the shape [`IcuPlural::parse`] cannot compile and must never emit verbatim.
fn reject_embedded_block(text: &str) -> Result<()> {
    let mut i = 0;
    while let Some(offset) = text[i..].find('{') {
        let open = i + offset;
        let close = matching_brace(text, open)?;
        if text[open + 1..close].contains(',') {
            bail!(
                "unsupported embedded ICU block `{}`: only a whole-message `plural` compiles \
                 to an Apple String Catalog",
                &text[open..=close]
            );
        }
        i = close + 1;
    }
    Ok(())
}

/// The `{name}` arguments of a message with no ICU blocks, in order of first appearance.
fn simple_args(text: &str) -> Result<Vec<String>> {
    let mut names = Vec::new();
    let mut i = 0;
    while let Some(offset) = text[i..].find('{') {
        let open = i + offset;
        let close = matching_brace(text, open)?;
        let name = text[open + 1..close].trim();
        if name.contains(',') {
            bail!("unsupported nested ICU block `{}`", &text[open..=close]);
        }
        if !names.iter().any(|n| n == name) {
            names.push(name.to_string());
        }
        i = close + 1;
    }
    Ok(names)
}

/// The index of the `}` matching the `{` at `open`.
fn matching_brace(text: &str, open: usize) -> Result<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    for (i, b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(i);
                }
            }
            _ => {}
        }
    }
    bail!("unbalanced `{{` in ICU message `{text}`")
}

/// One catalog message compiled into the Apple String Catalog shape.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AppleMessage {
    /// A single format string.
    Unit(String),
    /// A per-plural-category format string, keyed by CLDR category.
    Plural(BTreeMap<String, String>),
}

impl AppleMessage {
    /// Compile one locale's ICU message against the key's source-derived [`ArgPlan`].
    fn compile(message: &str, plan: &ArgPlan) -> Result<Self> {
        match IcuPlural::parse(message)? {
            Some(plural) => {
                let hash = plan.specifier(&plural.selector)?;
                let mut categories = BTreeMap::new();
                for (category, body) in &plural.categories {
                    categories.insert(category.clone(), format_string(body, plan, Some(&hash))?);
                }
                Ok(Self::Plural(categories))
            }
            None => Ok(Self::Unit(format_string(message, plan, None)?)),
        }
    }

    /// The `localizations` value for one locale.
    fn into_localization(self) -> Value {
        match self {
            Self::Unit(value) => serde_json::json!({
                "stringUnit": { "state": "translated", "value": value }
            }),
            Self::Plural(categories) => {
                // Driven by `PLURAL_CATEGORIES` rather than by the parsed map so only
                // categories a String Catalog understands can reach the file. (The JSON
                // object itself serializes sorted — object order is not meaningful here.)
                let mut plural = Map::new();
                for category in PLURAL_CATEGORIES {
                    if let Some(value) = categories.get(*category) {
                        plural.insert(
                            (*category).to_string(),
                            serde_json::json!({
                                "stringUnit": { "state": "translated", "value": value }
                            }),
                        );
                    }
                }
                serde_json::json!({ "variations": { "plural": Value::Object(plural) } })
            }
        }
    }
}

/// Render one ICU text run as an Apple format string: `{name}` becomes the planned
/// specifier, `#` becomes `hash` inside a plural body, and a literal `%` is escaped so the
/// platform formatter does not read it as a conversion.
fn format_string(text: &str, plan: &ArgPlan, hash: Option<&str>) -> Result<String> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '%' => out.push_str("%%"),
            '#' if hash.is_some() => out.push_str(hash.unwrap_or_default()),
            '{' => {
                let close = matching_brace(text, i)?;
                let name = text[i + 1..close].trim();
                if name.contains(',') {
                    bail!("unsupported nested ICU block `{}`", &text[i..=close]);
                }
                out.push_str(&plan.specifier(name)?);
                // Skip the argument body, including its closing brace.
                for (j, _) in chars.by_ref() {
                    if j >= close {
                        break;
                    }
                }
            }
            _ => out.push(c),
        }
    }
    Ok(out)
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

/// Map a BCP-47 tag to an Android resource qualifier: `language[-rREGION]`.
fn android_qualifier(locale: &str) -> String {
    let mut parts = locale.split('-');
    let lang = parts.next().unwrap_or(locale).to_ascii_lowercase();
    match parts.next() {
        Some(region) => format!("{lang}-r{}", region.to_ascii_uppercase()),
        None => lang,
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
    use super::*;

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
        assert_eq!(out.lines().count(), 1);
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

    // ── ICU -> Apple String Catalog compilation ───────────────────────────────
    //
    // A String Catalog is not an ICU consumer: whatever these emit is what the user
    // reads. Each case pins one shape the catalogs actually author.

    /// Compile one message the way [`Catalogs::string_catalog`] does: plan from the
    /// source text, then render.
    fn compile(source: &str) -> AppleMessage {
        let plan = ArgPlan::of(source).expect("source message plans");
        AppleMessage::compile(source, &plan).expect("source message compiles")
    }

    #[test]
    fn literal_text_passes_through_unchanged() {
        assert_eq!(compile("Recover"), AppleMessage::Unit("Recover".into()));
    }

    #[test]
    fn a_single_argument_becomes_an_unnumbered_string_specifier() {
        assert_eq!(
            compile("Clear {facet}"),
            AppleMessage::Unit("Clear %@".into())
        );
    }

    #[test]
    fn several_arguments_are_numbered_so_a_translation_may_reorder_them() {
        assert_eq!(
            compile("{done} of {total} uploaded"),
            AppleMessage::Unit("%1$@ of %2$@ uploaded".into())
        );
    }

    #[test]
    fn a_translation_inherits_the_source_argument_numbering() {
        let plan = ArgPlan::of("{done} of {total} uploaded").expect("plans");
        // A locale that reorders the two arguments must still address them by their
        // source positions, or the platform formatter reads the wrong argument.
        assert_eq!(
            AppleMessage::compile("{total} ← {done}", &plan).expect("compiles"),
            AppleMessage::Unit("%2$@ ← %1$@".into())
        );
    }

    #[test]
    fn a_literal_percent_is_escaped() {
        assert_eq!(
            compile("Uploading… {percent}%"),
            AppleMessage::Unit("Uploading… %@%%".into())
        );
    }

    #[test]
    fn a_whole_message_plural_becomes_a_plural_variation() {
        let AppleMessage::Plural(categories) =
            compile("{count, plural, one {# Photo} other {# Photos}}")
        else {
            panic!("expected a plural variation");
        };
        assert_eq!(categories["one"], "%lld Photo");
        assert_eq!(categories["other"], "%lld Photos");
    }

    #[test]
    fn a_plural_selector_is_typed_as_an_integer_not_a_string() {
        // Swift interpolates an `Int` as `%lld`; a `%@` here would mis-read the va_list.
        let plan = ArgPlan::of("{count, plural, one {# item} other {# items}}").expect("plans");
        assert_eq!(plan.specifier("count").expect("known argument"), "%lld");
    }

    #[test]
    fn an_embedded_plural_is_rejected_rather_than_emitted_verbatim() {
        let err = ArgPlan::of("Delete {count, plural, one {# item} other {# items}} now?")
            .expect_err("an embedded block has no Apple equivalent");
        assert!(format!("{err}").contains("embedded ICU block"), "{err}");
    }

    #[test]
    fn select_blocks_are_rejected() {
        let err = ArgPlan::of("{gender, select, male {He} other {They}}")
            .expect_err("`select` does not compile to a String Catalog");
        assert!(format!("{err}").contains("unsupported ICU block"), "{err}");
    }

    #[test]
    fn a_plural_without_other_is_rejected() {
        let err = ArgPlan::of("{count, plural, one {# item}}")
            .expect_err("Apple requires the `other` category");
        assert!(format!("{err}").contains("`other`"), "{err}");
    }

    #[test]
    fn an_explicit_value_category_is_rejected() {
        let err = ArgPlan::of("{count, plural, =0 {None} other {# items}}")
            .expect_err("`=N` is not a CLDR category");
        assert!(format!("{err}").contains("category `=0`"), "{err}");
    }

    #[test]
    fn plural_variations_carry_only_categories_a_string_catalog_understands() {
        let json = compile("{count, plural, other {# items} one {# item}}").into_localization();
        let emitted: Vec<&str> = json["variations"]["plural"]
            .as_object()
            .expect("plural object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(emitted, ["one", "other"]);
        assert!(emitted.iter().all(|c| PLURAL_CATEGORIES.contains(c)));
    }

    #[test]
    fn every_info_plist_key_is_namespaced_so_it_stays_out_of_localizable() {
        for (catalog_key, _) in INFO_PLIST_KEYS {
            assert!(
                catalog_key.starts_with(INFO_PLIST_NAMESPACE),
                "{catalog_key} must live in {INFO_PLIST_NAMESPACE}*"
            );
        }
    }
}
