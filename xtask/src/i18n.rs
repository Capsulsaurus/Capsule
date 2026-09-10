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
//! The Apple and Android outputs are *compiled*, not copied: neither platform is an ICU
//! consumer, so `{name}` becomes a positional format specifier (`%1$@`/`%1$s`) and a
//! whole-message `{n, plural, …}` becomes the platform's own plural mechanism — Apple's
//! `variations.plural`, Android's `<plurals>`. Anything outside that supported subset
//! (an embedded plural, `select`, `offset:`) fails the generator rather than reaching a
//! user verbatim: an uncompiled message is not a missing translation, it is raw ICU
//! syntax on screen.
//!
//! Argument *positions* come from the source locale for every platform, so a translation
//! may reorder its arguments and still address them by the position the call site passes.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use capsule_i18n::plural;
use eyre::{Context, ContextCompat, Result, bail};
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
            outputs.push((
                android_path(&self.source, locale),
                self.android_xml(locale, map)?,
            ));
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
    ///
    /// Android resources are not an ICU consumer, so every message is compiled: a
    /// whole-message plural becomes `<plurals>` with one `<item quantity=…>` per CLDR
    /// category the *locale* can select, and everything else becomes a `<string>` whose
    /// `{name}` arguments are `java.util.Formatter` specifiers. A message this cannot
    /// compile faithfully fails the generator — the alternative is shipping the user
    /// `{count, plural, one {# item} other {# items}}` verbatim.
    fn android_xml(&self, locale: &str, map: &BTreeMap<String, String>) -> Result<String> {
        let mut strings = String::new();
        let mut plurals = String::new();
        for (key, message) in map {
            let name = android_name(key);
            let plan = self.plan_for(key)?;
            let compiled = CompiledMessage::compile(message, &plan, ANDROID)
                .with_context(|| format!("key `{key}` ({locale} locale)"))?;
            match compiled {
                CompiledMessage::Unit(value) => {
                    let _ = writeln!(
                        strings,
                        "    <string name=\"{name}\">{}</string>",
                        android_escape(&value)
                    );
                }
                CompiledMessage::Plural(categories) => {
                    let arms = android_plural_arms(locale, &categories)
                        .with_context(|| format!("key `{key}` ({locale} locale)"))?;
                    let _ = writeln!(plurals, "    <plurals name=\"{name}\">");
                    for (category, value) in arms {
                        let _ = writeln!(
                            plurals,
                            "        <item quantity=\"{category}\">{}</item>",
                            android_escape(value)
                        );
                    }
                    let _ = writeln!(plurals, "    </plurals>");
                }
            }
        }
        let mut s = String::new();
        // The banner names the task, not the raw cargo command: XML forbids `--`
        // anywhere inside a comment, and `cargo run -p xtask -- i18n` contains
        // one. aapt2 enforces that and fails the whole Android resource merge.
        s.push_str("<!-- GENERATED by `mise run i18n` from locales/. Do not edit by hand. -->\n");
        s.push_str("<resources>\n");
        s.push_str(&strings);
        if !plurals.is_empty() {
            if !strings.is_empty() {
                s.push('\n');
            }
            s.push_str(&plurals);
        }
        s.push_str("</resources>\n");
        Ok(s)
    }

    /// The source-derived [`ArgPlan`] for one catalog key.
    ///
    /// Every platform numbers its specifiers from the source message, so a translation
    /// that reorders its arguments still addresses them by the positions the call site
    /// passes.
    fn plan_for(&self, key: &str) -> Result<ArgPlan> {
        match self.messages.get(&self.source).and_then(|m| m.get(key)) {
            Some(source_message) => ArgPlan::of(source_message)
                .with_context(|| format!("key `{key}` ({} locale)", self.source)),
            // A key absent from the source locale has no plan to inherit; treat every
            // argument as text, which is the only safe default.
            None => Ok(ArgPlan::default()),
        }
    }

    /// The app's Apple String Catalog: every key except the `app.infoplist.*` namespace,
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
    /// is in the `app.infoplist.*` namespace; the *output* key must be the literal
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
            let plan = self.plan_for(key)?;
            let mut localizations = Map::new();
            for (locale, map) in &self.messages {
                let Some(message) = map.get(key) else {
                    continue;
                };
                let compiled = CompiledMessage::compile(message, &plan, APPLE)
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
const INFO_PLIST_NAMESPACE: &str = "app.infoplist.";

/// `(catalog key, Info.plist key)` for every localized usage description the iOS app
/// declares. Spelled out rather than derived from the key name: the `Info.plist` side is a
/// fixed Apple vocabulary, and an unreviewed mechanical mapping could silently produce a
/// prompt the system ignores. Adding a usage description means adding a row here *and* the
/// key to `Project.swift`'s `infoPlist` (the base declaration the system requires).
const INFO_PLIST_KEYS: &[(&str, &str)] = &[
    (
        "app.infoplist.photo_library_usage",
        "NSPhotoLibraryUsageDescription",
    ),
    ("app.infoplist.face_id_usage", "NSFaceIDUsageDescription"),
];

/// The CLDR plural categories, in CLDR's canonical order.
///
/// ICU, an Apple String Catalog, and an Android `<plurals>` all speak exactly this
/// vocabulary, so a category outside it (`=0`, a typo) fails the generator rather than
/// reaching a file.
const PLURAL_CATEGORIES: &[&str] = &["zero", "one", "two", "few", "many", "other"];

/// What an ICU argument holds, which decides its conversion in a format string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArgKind {
    /// Interpolated as text.
    Text,
    /// A plural selector, which is an integer.
    Number,
}

/// How one ICU argument is spelled in a platform format string: its 1-based position and
/// what it holds. The position is shared across platforms; only the spelling differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ArgSpec {
    position: usize,
    kind: ArgKind,
}

/// One platform's `printf`-style vocabulary.
#[derive(Debug, Clone, Copy)]
struct Dialect {
    /// The conversion for an [`ArgKind::Text`] argument.
    text: &'static str,
    /// The conversion for an [`ArgKind::Number`] argument.
    number: &'static str,
    /// Whether a literal `%` is escaped even in a message with no arguments.
    ///
    /// `%` only needs escaping in a value the platform actually runs through a
    /// formatter. Apple escapes unconditionally, which is its historical output;
    /// Android must not, because `getString` with no arguments returns the resource
    /// verbatim and would show the user `%%`.
    escape_bare_percent: bool,
}

/// Apple's spelling: `%@` for text and `%lld` for the integer a Swift `Int` supplies.
const APPLE: Dialect = Dialect {
    text: "@",
    number: "lld",
    escape_bare_percent: true,
};

/// Android's spelling: `java.util.Formatter` conversions, `%s` and `%d`.
const ANDROID: Dialect = Dialect {
    text: "s",
    number: "d",
    escape_bare_percent: false,
};

/// The argument plan for one catalog key, derived from the **source** locale.
///
/// Both platforms resolve a localized entry by key and then format it with the arguments
/// the call site supplies, so every locale's format string must number and type its
/// specifiers the same way — including a translation that reorders them. Deriving the plan
/// once from the source message and reusing it for every locale is what guarantees that.
#[derive(Debug, Default, Clone)]
struct ArgPlan {
    args: BTreeMap<String, ArgSpec>,
}

impl ArgPlan {
    /// Derive the plan from a source-locale ICU message.
    ///
    /// A whole-message plural types its selector as a number (`%lld` / `%d`); every other
    /// `{name}` is text (`%@` / `%s`). Author numeric arguments as plurals — a bare
    /// `{count}` compiles to a text conversion, which does not match the integer a Swift
    /// `Int` interpolation supplies.
    ///
    /// The selector is inserted first, so a plural's count is always argument 1 — the
    /// order `getQuantityString(id, count, …)` and a Swift interpolation both supply.
    fn of(message: &str) -> Result<Self> {
        let mut plan = Self::default();
        if let Some(plural) = IcuPlural::parse(message)? {
            plan.insert(&plural.selector, ArgKind::Number);
            for body in plural.categories.values() {
                for name in simple_args(body)? {
                    plan.insert(&name, ArgKind::Text);
                }
            }
        } else {
            for name in simple_args(message)? {
                plan.insert(&name, ArgKind::Text);
            }
        }
        Ok(plan)
    }

    fn insert(&mut self, name: &str, kind: ArgKind) {
        let position = self.args.len() + 1;
        self.args
            .entry(name.to_string())
            .or_insert(ArgSpec { position, kind });
    }

    /// The format specifier for `name` in `dialect`. A single-argument message uses the
    /// unnumbered form (`%@` / `%s`) — what Apple's own tooling emits, and what Android
    /// accepts; two or more use explicit positions so a translation may reorder them.
    fn specifier(&self, name: &str, dialect: Dialect) -> Result<String> {
        let spec = self
            .args
            .get(name)
            .with_context(|| format!("argument `{{{name}}}` is not in the source message"))?;
        let conversion = match spec.kind {
            ArgKind::Text => dialect.text,
            ArgKind::Number => dialect.number,
        };
        Ok(if self.args.len() == 1 {
            format!("%{conversion}")
        } else {
            format!("%{}${conversion}", spec.position)
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
    /// no faithful equivalent on either platform (Apple would need `substitutions`;
    /// Android's `<plurals>` *is* the whole resource), so it is rejected loudly rather
    /// than emitted wrong. `select`, `selectordinal`, and `offset:` are likewise
    /// rejected — the alternative is the raw ICU reaching a user's screen.
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
                "unsupported ICU block `{kind}`: only a whole-message `plural` compiles to a \
                 native plural resource"
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
                    "unsupported ICU plural category `{category}`: a native plural resource \
                     accepts only the CLDR categories {PLURAL_CATEGORIES:?}"
                );
            }
            let close = matching_brace(trimmed, split)?;
            categories.insert(category, trimmed[split + 1..close].to_string());
            rest = &trimmed[close + 1..];
        }
        if !categories.contains_key("other") {
            // `other` is the universal fallback: every language selects it for some input,
            // and both platforms treat a plural without it as invalid.
            bail!("ICU plural is missing the required `other` category");
        }
        Ok(Some(Self {
            selector,
            categories,
        }))
    }
}

/// Fail if `text` contains an ICU `{name, kind, …}` block that is not the whole message —
/// the shape [`IcuPlural::parse`] cannot compile and must never be emitted verbatim.
fn reject_embedded_block(text: &str) -> Result<()> {
    let mut i = 0;
    while let Some(offset) = text[i..].find('{') {
        let open = i + offset;
        let close = matching_brace(text, open)?;
        if text[open + 1..close].contains(',') {
            bail!(
                "unsupported embedded ICU block `{}`: only a whole-message `plural` compiles \
                 to a native plural resource",
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

/// One catalog message compiled out of ICU into a platform's format strings.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CompiledMessage {
    /// A single format string.
    Unit(String),
    /// A per-plural-category format string, keyed by CLDR category.
    Plural(BTreeMap<String, String>),
}

impl CompiledMessage {
    /// Compile one locale's ICU message against the key's source-derived [`ArgPlan`],
    /// in `dialect`'s spelling.
    fn compile(message: &str, plan: &ArgPlan, dialect: Dialect) -> Result<Self> {
        // A `%` only needs escaping in a value the platform will run through a formatter,
        // which is exactly a value with arguments.
        let escape_percent = dialect.escape_bare_percent || !plan.args.is_empty();
        match IcuPlural::parse(message)? {
            Some(plural) => {
                let hash = plan.specifier(&plural.selector, dialect)?;
                let mut categories = BTreeMap::new();
                for (category, body) in &plural.categories {
                    categories.insert(
                        category.clone(),
                        format_string(body, plan, dialect, Some(&hash), escape_percent)?,
                    );
                }
                Ok(Self::Plural(categories))
            }
            None => Ok(Self::Unit(format_string(
                message,
                plan,
                dialect,
                None,
                escape_percent,
            )?)),
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

/// Render one ICU text run as a platform format string: `{name}` becomes the planned
/// specifier, `#` becomes `hash` inside a plural body, and a literal `%` is escaped (when
/// `escape_percent`) so the platform formatter does not read it as a conversion.
fn format_string(
    text: &str,
    plan: &ArgPlan,
    dialect: Dialect,
    hash: Option<&str>,
    escape_percent: bool,
) -> Result<String> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '%' if escape_percent => out.push_str("%%"),
            '#' if hash.is_some() => out.push_str(hash.unwrap_or_default()),
            '{' => {
                let close = matching_brace(text, i)?;
                let name = text[i + 1..close].trim();
                if name.contains(',') {
                    bail!("unsupported nested ICU block `{}`", &text[i..=close]);
                }
                out.push_str(&plan.specifier(name, dialect)?);
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

/// The inverse of [`android_qualifier`]: a `values-<qualifier>` directory name back to
/// the locale that produced it.
///
/// Only the tests need this — nothing generates from a directory name — but they need it
/// to be the *actual* inverse. Reading `b+zh+Hans` as a legacy `zh-rHANS` silently yields
/// the language `b`, and an assertion that then looks up plural rules for `b` fails with
/// a message about plural rules rather than about the spelling that broke.
#[cfg(test)]
fn android_locale(qualifier: &str) -> String {
    match qualifier.strip_prefix("b+") {
        Some(rest) => rest.replace('+', "-"),
        None => qualifier.replace("-r", "-"),
    }
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

/// The `<item quantity=…>` arms to emit for `locale`, in CLDR order.
///
/// Arms the language's CLDR rules can never select are dropped: Android resolves the
/// quantity with those same rules, so such an arm is unreachable — a `few` in English or
/// a `one` in Japanese changes nothing a user sees and trips the platform's
/// `UnusedQuantity` lint. `other` is selectable in every language and required by
/// [`IcuPlural::parse`], so what is left is always a complete resource.
///
/// The rules come from [`capsule_i18n::plural`], which is also what the Rust runtime
/// selects with. They used to be a second table in this file; `S-I7` gave the runtime
/// plural evaluation and the two immediately had to agree, so there is one table. A
/// language with no row there fails the generator rather than guessing a rule set.
fn android_plural_arms<'a>(
    locale: &str,
    categories: &'a BTreeMap<String, String>,
) -> Result<Vec<(&'static str, &'a str)>> {
    let language = locale.split('-').next().unwrap_or(locale);
    let selectable = plural::selectable(locale).with_context(|| {
        format!(
            "no CLDR plural rules for language `{language}`: add a row to \
             `capsule_i18n::plural` before a `{locale}` message uses a plural"
        )
    })?;
    let arms: Vec<(&'static str, &str)> = PLURAL_CATEGORIES
        .iter()
        .filter(|category| selectable.iter().any(|c| c.as_str() == **category))
        .filter_map(|category| Some((*category, categories.get(*category)?.as_str())))
        .collect();
    if !arms.iter().any(|(category, _)| *category == "other") {
        bail!("plural for `{locale}` has no `other` arm after applying its CLDR rules");
    }
    Ok(arms)
}

/// Escape a compiled value for an Android `<string>` / `<item>` body.
///
/// Two rule sets stack. XML's (`&`, `<`, `>`), and Android's own resource quoting: a
/// backslash escapes itself, an apostrophe and a double quote must be escaped or `aapt2`
/// rejects the value, a literal newline or tab has to be spelled as an escape, and a
/// value *starting* with `@` or `?` would otherwise be read as a resource or attribute
/// reference. `%` is deliberately untouched — [`format_string`] has already decided
/// whether this value is a format string.
fn android_escape(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    for (index, c) in message.chars().enumerate() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '@' | '?' if index == 0 => {
                out.push('\\');
                out.push(c);
            }
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
    use std::collections::BTreeMap;

    // A glob: this module tests most of the file, and naming each item was a
    // running edit every time a renderer gained a helper.
    use super::*;

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

    /// The two mappings are a pair: a qualifier the inverse cannot read is a
    /// directory the tests would mis-attribute rather than fail on.
    #[test]
    fn every_supported_locale_round_trips_through_its_android_qualifier() {
        for locale in ["en", "fr", "ar", "pt-BR", "zh-Hans", "zh-Hant"] {
            assert_eq!(android_locale(&android_qualifier(locale)), locale);
        }
    }

    #[test]
    fn script_subtags_are_four_letters_regions_are_not() {
        assert!(is_script_subtag("Hans"));
        assert!(is_script_subtag("Latn"));
        assert!(!is_script_subtag("BR"));
        assert!(!is_script_subtag("419"));
    }

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
        let xml = catalogs.android_xml("en", &catalog).expect("renders");
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
    fn compile(source: &str) -> CompiledMessage {
        compile_as(source, APPLE)
    }

    /// Compile one message in an arbitrary dialect, planning from its own text.
    fn compile_as(source: &str, dialect: Dialect) -> CompiledMessage {
        let plan = ArgPlan::of(source).expect("source message plans");
        CompiledMessage::compile(source, &plan, dialect).expect("source message compiles")
    }

    #[test]
    fn literal_text_passes_through_unchanged() {
        assert_eq!(compile("Recover"), CompiledMessage::Unit("Recover".into()));
    }

    #[test]
    fn a_single_argument_becomes_an_unnumbered_string_specifier() {
        assert_eq!(
            compile("Clear {facet}"),
            CompiledMessage::Unit("Clear %@".into())
        );
    }

    #[test]
    fn several_arguments_are_numbered_so_a_translation_may_reorder_them() {
        assert_eq!(
            compile("{done} of {total} uploaded"),
            CompiledMessage::Unit("%1$@ of %2$@ uploaded".into())
        );
    }

    #[test]
    fn a_translation_inherits_the_source_argument_numbering() {
        let plan = ArgPlan::of("{done} of {total} uploaded").expect("plans");
        // A locale that reorders the two arguments must still address them by their
        // source positions, or the platform formatter reads the wrong argument.
        assert_eq!(
            CompiledMessage::compile("{total} ← {done}", &plan, APPLE).expect("compiles"),
            CompiledMessage::Unit("%2$@ ← %1$@".into())
        );
    }

    #[test]
    fn a_literal_percent_is_escaped() {
        assert_eq!(
            compile("Uploading… {percent}%"),
            CompiledMessage::Unit("Uploading… %@%%".into())
        );
    }

    #[test]
    fn a_whole_message_plural_becomes_a_plural_variation() {
        let CompiledMessage::Plural(categories) =
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
        assert_eq!(
            plan.specifier("count", APPLE).expect("known argument"),
            "%lld"
        );
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

    // ── ICU -> Android resource compilation ───────────────────────────────────
    //
    // Android resources are not an ICU consumer either, and the guard this replaces
    // (`\{[^{}]*,[^{}]*\}`) could not match a plural — `[^{}]*` cannot span the nested
    // braces every plural contains — so it never fired once and every shape below
    // shipped its ICU source verbatim to the user. The cases, in order:
    //
    //  1. a whole-message plural becomes `<plurals>` with per-category `<item>`s
    //  2. `#` in a plural arm becomes the quantity substitution Android expects
    //  3. `{name}` becomes an Android format specifier
    //  4. two or more arguments are numbered, from the source locale
    //  5. a translation inherits the source numbering even when it reorders
    //  6. a literal `%` is escaped in a formatted value, left alone in a bare one
    //  7. an arm the locale cannot select is dropped (`one` in Japanese)
    //  8. ... and likewise `few` authored for English
    //  9. a locale with no `capsule_i18n::plural` row fails rather than guessing
    // 10. embedded plural, `select`, and `offset:` fail the renderer
    // 11. Android quoting: apostrophe, quote, backslash, XML, a leading `@`/`?`
    // 12. end to end: no committed Android value contains ICU syntax

    /// A [`Catalogs`] over one key: `entries` is `(locale, message)`, source `en`.
    fn catalogs(key: &str, entries: &[(&str, &str)]) -> Catalogs {
        let mut messages = BTreeMap::new();
        for (locale, message) in entries {
            let mut map = BTreeMap::new();
            map.insert(key.to_string(), (*message).to_string());
            messages.insert((*locale).to_string(), map);
        }
        Catalogs {
            source: "en".to_string(),
            supported: entries.iter().map(|(l, _)| (*l).to_string()).collect(),
            messages,
        }
    }

    /// Render one locale's `strings.xml` for a single-key catalog.
    fn android_xml(key: &str, entries: &[(&str, &str)], locale: &str) -> String {
        let catalogs = catalogs(key, entries);
        let map = catalogs
            .messages
            .get(locale)
            .expect("locale is in the catalog");
        catalogs.android_xml(locale, map).expect("renders")
    }

    /// Render the source locale of a one-message, one-locale catalog.
    fn android_en(message: &str) -> String {
        android_xml("k", &[("en", message)], "en")
    }

    /// The error from rendering a message the Android renderer must refuse.
    fn android_err(message: &str) -> String {
        let catalogs = catalogs("k", &[("en", message)]);
        let map = catalogs
            .messages
            .get("en")
            .expect("locale is in the catalog");
        let err = catalogs
            .android_xml("en", map)
            .expect_err("this shape has no faithful Android form");
        format!("{err:#}")
    }

    #[test]
    fn a_whole_message_plural_becomes_a_plurals_resource() {
        let xml = android_en("{count, plural, one {# item} other {# items}}");
        assert!(xml.contains("<plurals name=\"k\">"), "{xml}");
        assert!(
            xml.contains("<item quantity=\"one\">%d item</item>"),
            "{xml}"
        );
        assert!(
            xml.contains("<item quantity=\"other\">%d items</item>"),
            "{xml}"
        );
        // A plural is never also a flat <string> — that is the shape that shipped ICU.
        assert!(!xml.contains("<string name=\"k\">"), "{xml}");
    }

    #[test]
    fn a_plural_selector_is_the_first_argument_the_call_site_passes() {
        // `getQuantityString(id, count, count)`: the count is argument 1, typed `%d`.
        let plan = ArgPlan::of("{count, plural, one {# item} other {# items}}").expect("plans");
        assert_eq!(
            plan.specifier("count", ANDROID).expect("known argument"),
            "%d"
        );
    }

    #[test]
    fn a_plural_arm_may_carry_its_own_arguments_after_the_count() {
        let xml = android_en("{count, plural, one {# of {total}} other {# of {total}}}");
        assert!(
            xml.contains("<item quantity=\"other\">%1$d of %2$s</item>"),
            "{xml}"
        );
    }

    #[test]
    fn a_named_placeholder_becomes_an_android_format_specifier() {
        assert!(android_en("Clear {facet}").contains("<string name=\"k\">Clear %s</string>"));
    }

    #[test]
    fn several_android_arguments_are_numbered_so_a_translation_may_reorder_them() {
        let xml = android_en("{done} of {total} uploaded");
        assert!(
            xml.contains("<string name=\"k\">%1$s of %2$s uploaded</string>"),
            "{xml}"
        );
    }

    #[test]
    fn an_android_translation_inherits_the_source_argument_numbering() {
        // Android formats by position, so a locale that reorders the two arguments must
        // still address them by their *source* positions.
        let xml = android_xml(
            "k",
            &[
                ("en", "{done} of {total} uploaded"),
                ("de", "{total} ← {done}"),
            ],
            "de",
        );
        assert!(
            xml.contains("<string name=\"k\">%2$s ← %1$s</string>"),
            "{xml}"
        );
    }

    #[test]
    fn a_literal_percent_is_escaped_in_a_formatted_value() {
        let xml = android_en("Uploading… {percent}%");
        assert!(
            xml.contains("<string name=\"k\">Uploading… %s%%</string>"),
            "{xml}"
        );
    }

    #[test]
    fn a_literal_percent_is_left_alone_when_there_is_nothing_to_format() {
        // `getString(id)` with no arguments returns the resource verbatim, so an escaped
        // `%%` here would put two percent signs on screen.
        let xml = android_en("100% offline");
        assert!(
            xml.contains("<string name=\"k\">100% offline</string>"),
            "{xml}"
        );
    }

    #[test]
    fn an_arm_the_locale_cannot_select_is_dropped() {
        // Japanese has one CLDR category, `other`; a `one` arm can never be selected.
        let xml = android_xml(
            "k",
            &[
                ("en", "{count, plural, one {# item} other {# items}}"),
                ("ja", "{count, plural, one {#件} other {#件}}"),
            ],
            "ja",
        );
        assert!(
            xml.contains("<item quantity=\"other\">%d件</item>"),
            "{xml}"
        );
        assert!(!xml.contains("quantity=\"one\""), "{xml}");
    }

    #[test]
    fn a_category_english_cannot_select_is_dropped() {
        let xml = android_en("{count, plural, one {# item} few {# items} other {# items}}");
        assert!(xml.contains("quantity=\"one\""), "{xml}");
        assert!(xml.contains("quantity=\"other\""), "{xml}");
        assert!(!xml.contains("quantity=\"few\""), "{xml}");
    }

    #[test]
    fn a_locale_with_no_plural_rules_row_fails_rather_than_guessing() {
        let catalogs = catalogs(
            "k",
            &[("en", "{count, plural, one {# item} other {# items}}")],
        );
        let map = catalogs
            .messages
            .get("en")
            .expect("locale is in the catalog");
        let err = catalogs
            .android_xml("xx", map)
            .expect_err("an unknown language has no rules to filter by");
        assert!(
            format!("{err:#}").contains("capsule_i18n::plural"),
            "{err:#}"
        );
    }

    #[test]
    fn arms_are_emitted_in_cldr_order_not_catalog_order() {
        let xml = android_en("{count, plural, other {# items} one {# item}}");
        let one = xml.find("quantity=\"one\"").expect("one arm");
        let other = xml.find("quantity=\"other\"").expect("other arm");
        assert!(one < other, "{xml}");
    }

    #[test]
    fn an_embedded_plural_fails_the_android_renderer_rather_than_shipping_icu() {
        // The exact shape the old regex guard could not match.
        let err = android_err("Delete {count, plural, one {# item} other {# items}} now?");
        assert!(err.contains("embedded ICU block"), "{err}");
    }

    #[test]
    fn a_select_block_fails_the_android_renderer() {
        let err = android_err("{gender, select, male {He} other {They}}");
        assert!(err.contains("unsupported ICU block"), "{err}");
    }

    #[test]
    fn a_plural_offset_fails_the_android_renderer() {
        let err = android_err("{count, plural, offset:1 one {# other} other {# others}}");
        assert!(err.contains("offset:"), "{err}");
    }

    #[test]
    fn an_android_plural_without_other_is_rejected() {
        let err = android_err("{count, plural, one {# item}}");
        assert!(err.contains("`other`"), "{err}");
    }

    #[test]
    fn android_quoting_covers_xml_and_the_platforms_own_rules() {
        assert_eq!(android_escape("Don't"), "Don\\'t");
        assert_eq!(android_escape("say \"hi\""), "say \\\"hi\\\"");
        assert_eq!(android_escape("a & b < c > d"), "a &amp; b &lt; c &gt; d");
        assert_eq!(android_escape("back\\slash"), "back\\\\slash");
        assert_eq!(android_escape("line\nbreak"), "line\\nbreak");
        // A leading `@` or `?` is a resource / attribute reference unless escaped.
        assert_eq!(android_escape("@home"), "\\@home");
        assert_eq!(android_escape("?maybe"), "\\?maybe");
        // ... but only in the first position.
        assert_eq!(android_escape("me@home"), "me@home");
    }

    #[test]
    fn a_format_specifier_survives_escaping() {
        // `%` must not be touched by the escaper — `format_string` already decided.
        assert_eq!(android_escape("%1$s of %2$s"), "%1$s of %2$s");
    }

    /// The repository root, from which the real catalogs load.
    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask/ has a parent")
            .to_path_buf()
    }

    /// Every rendered Android `strings.xml`, as `(path, content)`.
    fn rendered_android() -> Vec<(PathBuf, String)> {
        let catalogs = Catalogs::load(&repo_root()).expect("the real catalogs load");
        catalogs
            .render()
            .expect("the real catalogs compile")
            .into_iter()
            .filter(|(path, _)| path.to_string_lossy().contains("androidMain"))
            .collect()
    }

    #[test]
    fn no_android_resource_ships_icu_syntax_to_a_user() {
        let rendered = rendered_android();
        assert!(!rendered.is_empty(), "no Android output was rendered");
        for (path, xml) in &rendered {
            for (number, line) in xml.lines().enumerate() {
                assert!(
                    !line.contains("plural,") && !line.contains('{'),
                    "{}:{}: ICU syntax reached a user-visible resource: {line}",
                    path.display(),
                    number + 1,
                );
            }
        }
    }

    #[test]
    fn the_plural_keys_render_as_plurals_resources() {
        for (path, xml) in &rendered_android() {
            assert!(
                xml.contains("<plurals name=\"common_item_count\">"),
                "{}: the plural catalog keys must reach <plurals>",
                path.display(),
            );
        }
    }

    /// The `quantity` attribute of every `<item>` in one rendered `strings.xml`.
    fn android_quantities(xml: &str) -> Vec<&str> {
        xml.lines()
            .filter(|line| line.contains("<item quantity="))
            .map(|line| {
                line.split("quantity=\"")
                    .nth(1)
                    .and_then(|rest| rest.split('"').next())
                    .expect("a quantity attribute")
            })
            .collect()
    }

    /// The locale a rendered Android resource path belongs to.
    fn android_path_locale(path: &Path) -> String {
        let dir = path
            .parent()
            .and_then(Path::file_name)
            .map(|d| d.to_string_lossy().into_owned())
            .expect("a values directory");
        // `values/` is the source locale; `values-<qualifier>/` names its own.
        dir.strip_prefix("values-")
            .map_or_else(|| "en".to_string(), android_locale)
    }

    /// The `<item quantity=…>` arms of one `<plurals name=…>` block in a rendered
    /// `strings.xml`, keyed by quantity.
    fn android_plural_block(xml: &str, name: &str) -> BTreeMap<String, String> {
        let mut arms = BTreeMap::new();
        let open = format!("<plurals name=\"{name}\">");
        let Some(start) = xml.find(&open) else {
            return arms;
        };
        for line in xml[start..].lines().skip(1) {
            if line.contains("</plurals>") {
                break;
            }
            let Some((quantity, rest)) = line
                .split_once("quantity=\"")
                .and_then(|(_, rest)| rest.split_once("\">"))
            else {
                continue;
            };
            let body = rest.trim_end().trim_end_matches("</item>");
            arms.insert(quantity.to_string(), body.to_string());
        }
        arms
    }

    /// The Rust runtime renders exactly the string the Android resource carries.
    ///
    /// The agreement that matters, and the one nothing checked before: the generator
    /// lowers an ICU arm into a `%d` format string ahead of time, the runtime substitutes
    /// `#` at display time, and the two are supposed to produce the same sentence. Run
    /// over the **real** catalogs, in every locale, at the CLDR boundary counts, with the
    /// arm picked by the same rules Android will pick it with — so a divergence in either
    /// the lowering or the runtime's substitution fails here.
    ///
    /// Compared in Android's escaped spelling rather than un-escaping the resource:
    /// `android_escape` is the function under test on that side, so applying it to the
    /// runtime's output compares like with like.
    #[test]
    fn the_runtime_renders_what_the_android_resource_carries() {
        let catalogs = Catalogs::load(&repo_root()).expect("the real catalogs load");
        let rendered: BTreeMap<String, String> = rendered_android()
            .into_iter()
            .map(|(path, xml)| (android_path_locale(&path), xml))
            .collect();
        let counts: [i64; 8] = [0, 1, 2, 3, 5, 11, 21, 101];
        let mut compared = 0usize;

        for (locale, messages) in &catalogs.messages {
            let xml = rendered.get(locale).expect("every locale renders");
            let bundle = capsule_i18n::Bundle::for_locale(locale);
            for (key, message) in messages {
                if !message.contains("plural,") {
                    continue;
                }
                let arms = android_plural_block(xml, &android_name(key));
                assert!(!arms.is_empty(), "{locale}/{key}: no <plurals> block");
                for n in counts {
                    let category = plural::category(locale, n).as_str();
                    let arm = arms
                        .get(category)
                        .or_else(|| arms.get("other"))
                        .expect("every plural resource carries `other`");
                    // Single-argument plurals use the unnumbered specifier; a message with
                    // more arguments numbers them. `%%` is Android's escaped literal `%`.
                    let expected = arm
                        .replacen("%1$d", &n.to_string(), 1)
                        .replacen("%d", &n.to_string(), 1)
                        .replace("%%", "%");
                    let runtime = bundle.format(key, &[("count", capsule_i18n::Value::Int(n))]);
                    assert_eq!(
                        android_escape(&runtime),
                        expected,
                        "{locale}/{key} at n={n}: the runtime and `strings.xml` disagree"
                    );
                    compared += 1;
                }
            }
        }
        assert!(compared > 1000, "only {compared} renderings were compared");
    }

    #[test]
    fn every_emitted_quantity_is_selectable_in_its_locale() {
        for (path, xml) in &rendered_android() {
            let locale = android_path_locale(path);
            let selectable = plural::selectable(&locale)
                .unwrap_or_else(|| panic!("{}: no `capsule_i18n::plural` row", path.display()));
            for quantity in android_quantities(xml) {
                assert!(
                    selectable.iter().any(|c| c.as_str() == quantity),
                    "{}: `{quantity}` is not selectable in `{locale}`",
                    path.display(),
                );
            }
        }
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
