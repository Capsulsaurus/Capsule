//! `xtask i18n-guard`: a per-platform regression gate that fails when a NEW
//! user-facing string literal is introduced outside the i18n catalog.
//!
//! Slice S-I1 migrated every user-facing literal in the web (JSX), SwiftUI, and
//! Compose surfaces onto keys in the canonical `locales/` catalog. This gate keeps
//! the migration from regressing: it scans those three surfaces for hardcoded
//! literals and fails if it finds one that is not a known catalog key (and is not
//! on the documented allowlist).
//!
//! Design (pragmatism over perfection, per the slice contract): this is an
//! exact-pattern scanner with a documented allowlist file, NOT a full AST parser.
//! Each surface has a small set of high-signal patterns:
//!
//! - **Web** (`capsule-web/src/{routes,components}/**/*.tsx`): JSX text nodes that
//!   sit directly before a closing tag (`>Some text</`), plus string literals in the
//!   user-facing attributes `placeholder`, `aria-label`, `alt`, `title`. Migrated
//!   code renders text via `<FormattedMessage>` / `intl.formatMessage`, so any bare
//!   JSX text or user-facing attribute literal is a regression.
//! - **SwiftUI** (`capsule-swift/{App,Modules}/**/*.swift`): the string argument of
//!   `Text("…")`, `.navigationTitle("…")`, `Label("…", …)`, `Button("…", …)`,
//!   `Section("…")`, `.accessibilityLabel("…")`, `Toggle("…", …)`, `.alert("…", …)`.
//!   A migrated call passes a catalog KEY (`Text("ios.settings.title")`); a literal
//!   (`Text("Settings")`) is not a key, so it fails.
//! - **Compose** (`capsule-android/src/**/*.kt`): the string argument of `Text("…")`
//!   and `contentDescription = "…"`. Migrated code uses
//!   `stringResource(R.string.…)`; a bare literal is not a key, so it fails.
//!
//! The Swift/Compose surfaces are anchored to the catalog: a captured string passes
//! only if it exactly matches a key in `locales/en.json`. The web surface has no
//! quoted key to compare against (text lives in `<FormattedMessage id=…>`), so any
//! captured web literal is a violation unless allowlisted.
//!
//! Every detector is a pure `&str -> Vec<Finding>` function so the acceptance tests
//! (zero findings on the migrated tree; an injected literal is caught) run without
//! disk I/O.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use eyre::{Context, ContextCompat, Result, bail};
use regex::Regex;
use serde_json::Value;

/// Repo-relative path of the documented allowlist (one `path\tstring` per line;
/// `#` comments and blank lines ignored). Entries suppress a single known,
/// intentionally-untranslated finding at that file for that exact captured string.
const ALLOWLIST_PATH: &str = "locales/i18n-guard-allowlist.txt";

/// The hand-written Swift mirror of the server's stable error codes.
///
/// Its Rust counterpart, [`capsule_i18n::error_codes`], is *generated* from the
/// catalog and therefore cannot drift. This one can, which is the whole reason
/// the check below exists.
const SWIFT_ERROR_CODE_PATH: &str = "capsule-swift/Modules/CapsuleDomain/Sources/ErrorCode.swift";

/// One detector hit: the 1-based line and the exact captured string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Finding {
    pub line: usize,
    pub text: String,
    pub kind: &'static str,
}

/// A confirmed violation, tied to the file it was found in.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Violation {
    file: String,
    line: usize,
    text: String,
    kind: &'static str,
}

/// Scan the three UI surfaces and fail if any un-allowlisted literal remains.
pub(crate) fn run(root: &Path) -> Result<()> {
    let keys = load_catalog_keys(root)?;
    let allowlist = load_allowlist(root)?;

    let mut violations = Vec::new();
    scan_surface(
        root,
        &["capsule-web/src/routes", "capsule-web/src/components"],
        "tsx",
        &|content| web_findings(content),
        // Web has no catalog key to compare against — every finding is a candidate.
        &|_text| false,
        &mut violations,
    )?;
    scan_surface(
        root,
        &["capsule-swift/App", "capsule-swift/Modules"],
        "swift",
        &|content| swift_findings(content),
        &|text| keys.contains(text),
        &mut violations,
    )?;
    scan_surface(
        root,
        &["capsule-android/src"],
        "kt",
        &|content| kotlin_findings(content),
        &|text| keys.contains(text),
        &mut violations,
    )?;

    violations.retain(|v| !allowlist.contains(&(v.file.clone(), v.text.clone())));

    // Runs before the literal verdict so a tree with both problems reports both
    // rather than hiding one behind the other's `bail!`.
    check_swift_error_codes(root, &keys)?;

    if violations.is_empty() {
        println!("i18n-guard: no hardcoded user-facing literals found across web/swift/compose.");
        return Ok(());
    }

    eprintln!(
        "i18n-guard: found {} hardcoded user-facing literal(s). Move each to a\n\
         `locales/en.json` key (see the i18n design doc), or, if it is intentionally\n\
         not translatable, add a `<path>\\t<string>` line to {ALLOWLIST_PATH}:\n",
        violations.len()
    );
    for v in &violations {
        eprintln!("  {}:{} [{}] {:?}", v.file, v.line, v.kind, v.text);
    }
    bail!(
        "i18n-guard failed: {} hardcoded literal(s)",
        violations.len()
    );
}

/// Walk `roots` for `*.ext` files, run `detect` on each, and keep findings whose
/// captured text is not `is_key` (an already-migrated catalog reference).
fn scan_surface(
    root: &Path,
    roots: &[&str],
    ext: &str,
    detect: &dyn Fn(&str) -> Vec<Finding>,
    is_key: &dyn Fn(&str) -> bool,
    out: &mut Vec<Violation>,
) -> Result<()> {
    for rel in roots {
        let dir = root.join(rel);
        if !dir.exists() {
            continue;
        }
        for path in collect_files(&dir, ext)? {
            let content =
                fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
            let file = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            for f in detect(&content) {
                if is_key(&f.text) {
                    continue;
                }
                out.push(Violation {
                    file: file.clone(),
                    line: f.line,
                    text: f.text,
                    kind: f.kind,
                });
            }
        }
    }
    Ok(())
}

/// Recursively collect `*.ext` files under `dir`, skipping test files and build
/// output. Sorted for deterministic output.
fn collect_files(dir: &Path, ext: &str) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in fs::read_dir(&d).with_context(|| format!("reading dir {}", d.display()))? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                if matches!(
                    name.as_str(),
                    "node_modules" | "target" | "build" | "dist" | ".git" | "Generated"
                ) {
                    continue;
                }
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some(ext)
                && !name.contains(".test.")
                && !name.contains("Tests")
            {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

/// Map a byte offset in `content` to its 1-based line number.
fn line_of(content: &str, offset: usize) -> usize {
    content[..offset].bytes().filter(|&b| b == b'\n').count() + 1
}

/// Detect hardcoded literals in a `.tsx` web source.
pub(crate) fn web_findings(content: &str) -> Vec<Finding> {
    static TEXT: OnceLock<Regex> = OnceLock::new();
    static ATTR: OnceLock<Regex> = OnceLock::new();
    // JSX text directly before a closing tag: `>Some text</`. `[^<>{}]` forbids
    // nested tags/expressions and lets the run span newlines (multiline text).
    let text = TEXT.get_or_init(|| {
        Regex::new(r"(?s)>([^<>{}]*[A-Za-z][^<>{}]*)</").expect("static regex is valid")
    });
    // User-facing string attributes. Migrated code passes these via
    // `intl.formatMessage(...)` (a `{}` expression), so a `"..."` literal here is a
    // regression.
    let attr = ATTR.get_or_init(|| {
        Regex::new(r#"(?:placeholder|aria-label|alt|title)\s*=\s*"([^"]*[A-Za-z][^"]*)""#)
            .expect("static regex is valid")
    });

    let mut findings = Vec::new();
    for caps in text.captures_iter(content) {
        let m = caps.get(1).expect("group 1 exists");
        let trimmed = m.as_str().trim();
        if trimmed.is_empty() {
            continue;
        }
        findings.push(Finding {
            line: line_of(content, m.start()),
            text: normalize_ws(trimmed),
            kind: "jsx-text",
        });
    }
    for caps in attr.captures_iter(content) {
        let m = caps.get(1).expect("group 1 exists");
        findings.push(Finding {
            line: line_of(content, m.start()),
            text: m.as_str().to_string(),
            kind: "jsx-attr",
        });
    }
    findings
}

/// Collapse internal whitespace runs (JSX text is reflowed by the bundler, so the
/// exact indentation is not meaningful for allowlisting).
fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Detect string-literal arguments to user-facing SwiftUI APIs.
pub(crate) fn swift_findings(content: &str) -> Vec<Finding> {
    static RE: OnceLock<Regex> = OnceLock::new();
    // `Text("…")`, `.navigationTitle("…")`, `Label("…", …)`, `Button("…", …)`,
    // `Section("…")`, `.accessibilityLabel("…")`, `Toggle("…", …)`, `.alert("…", …)`.
    // The leading `(?:^|[^A-Za-z0-9_])` is a word boundary so helper names that
    // merely END in one of these (`barButton("sf.symbol.name")`) don't match; a
    // leading `.` (method syntax) is still allowed. The `"` must immediately follow
    // `(` so `Text(verbatim: "…")` and `Text(dynamicVar)` are not matched.
    // `[^"\\]*` keeps it to simple literals — interpolations contain `\(` and are
    // skipped (documented blind spot; ICU-argument catalog support for Swift is a
    // follow-up).
    let re = RE.get_or_init(|| {
        Regex::new(
            r#"(?:^|[^A-Za-z0-9_])(?:Text|Label|Button|Section|Toggle|navigationTitle|accessibilityLabel|accessibilityHint|alert)\(\s*"([^"\\]*[A-Za-z][^"\\]*)""#,
        )
        .expect("static regex is valid")
    });
    let mut findings = matched_findings(content, re, "swift-literal");
    findings.extend(swift_key_parameter_findings(content));
    findings
}

/// Detect catalog keys passed to a `…Key:` parameter.
///
/// The app's own convention is that any parameter whose label ends in `Key`
/// carries a catalog key — `titleKey:`, `labelKey:`, `emptyDescriptionKey:`, and
/// a dozen more. [`swift_findings`] cannot see them, because it only knows the
/// stock SwiftUI call shapes, which left several hundred keys outside the gate:
/// a typo'd or never-added key there compiles, renders as its own raw text, and
/// nothing fails.
///
/// The same catalog membership test applies, so a real key passes and a literal
/// or a missing key is reported.
fn swift_key_parameter_findings(content: &str) -> Vec<Finding> {
    static RE: OnceLock<Regex> = OnceLock::new();
    // `[A-Za-z]*Key:` — a labelled argument whose name ends in `Key`. The
    // leading class excludes `.` as well as identifier characters, because an
    // argument label is never preceded by a dot but an enum case in a switch
    // always is: `case .masterKey: "key.fill"` is a pattern returning an SF
    // Symbol name, not a catalog reference. As in
    // `swift_findings`, `[^"\\]*` keeps this to simple literals, so
    // interpolated keys (`"ios.x.\(raw).title"`) stay a documented blind spot.
    let re = RE.get_or_init(|| {
        Regex::new(r#"(?:^|[^A-Za-z0-9_.])[A-Za-z]*Key:\s*"([^"\\]*[A-Za-z][^"\\]*)""#)
            .expect("static regex is valid")
    });
    matched_findings(content, re, "swift-key-param")
}

/// Detect string-literal arguments to user-facing Compose APIs.
pub(crate) fn kotlin_findings(content: &str) -> Vec<Finding> {
    static RE: OnceLock<Regex> = OnceLock::new();
    // `Text("…")` and `contentDescription = "…"`. `stringResource(...)` and
    // `Text(dynamicVar)` have no `"` in the captured position, so they don't match.
    let re = RE.get_or_init(|| {
        Regex::new(r#"(?:Text\(\s*|contentDescription\s*=\s*)"([^"\\]*[A-Za-z][^"\\]*)""#)
            .expect("static regex is valid")
    });
    matched_findings(content, re, "compose-literal")
}

fn matched_findings(content: &str, re: &Regex, kind: &'static str) -> Vec<Finding> {
    let mut findings = Vec::new();
    for caps in re.captures_iter(content) {
        let m = caps.get(1).expect("group 1 exists");
        findings.push(Finding {
            line: line_of(content, m.start()),
            text: m.as_str().to_string(),
            kind,
        });
    }
    findings
}

/// Every key in the source catalog `locales/en.json`.
fn load_catalog_keys(root: &Path) -> Result<BTreeSet<String>> {
    let path = root.join("locales/en.json");
    let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let value: Value =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let obj = value
        .as_object()
        .with_context(|| format!("{} must be a JSON object", path.display()))?;
    Ok(obj.keys().cloned().collect())
}

/// Load the `(path, string)` allowlist. Missing file => empty allowlist.
fn load_allowlist(root: &Path) -> Result<BTreeSet<(String, String)>> {
    let path = root.join(ALLOWLIST_PATH);
    let Ok(text) = fs::read_to_string(&path) else {
        return Ok(BTreeSet::new());
    };
    let mut set = BTreeSet::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim_end();
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let Some((file, string)) = line.split_once('\t') else {
            bail!(
                "{}:{}: allowlist entries must be `<path>\\t<string>` (tab-separated)",
                path.display(),
                i + 1
            );
        };
        set.insert((file.to_string(), string.to_string()));
    }
    Ok(set)
}

/// Fail when Swift's `ErrorCode` enum names an `error.*` code the catalog does
/// not define.
///
/// The server error codes are a stable contract shared by three surfaces. Two of
/// them are generated: `capsule_i18n::error_codes` comes out of the catalog, and
/// the web reads the catalog directly. The Swift side is a hand-written enum
/// whose raw values *are* the catalog keys, so it is the one surface where the
/// contract can rot silently — and it rots into the worst possible symptom, a
/// lookup miss that renders a blank or raw-key message on exactly the screens a
/// user reaches when something has already gone wrong.
///
/// **The two directions are deliberately not symmetric.**
///
/// - A code in Swift that the catalog lacks is **fatal**. There is no message to
///   show; the client is asking for a key that does not exist.
/// - A code in the catalog that no Swift source mentions is **reported, not
///   fatal**. The enum's `unknown(String)` case is load-bearing by design — a
///   newer server may legitimately send a code this build predates, and the raw
///   value *is* the catalog key, so the string still localizes. Making this
///   fatal would force a Swift change for every server-side error addition,
///   which is exactly the coupling `unknown` exists to avoid. It is still worth
///   naming: each is a recovery the client cannot offer an affordance for.
///
/// The second direction is measured against **every Swift source, not just the
/// enum**. Some catalog error strings are client-local and are reached
/// deliberately through `unknown(_)` rather than through a case —
/// `error.client.unclassified` is the generic fallback a screen shows when it
/// has no code at all. Those are handled; calling them "codes with no case"
/// would be a gate that reports noise, and a gate that reports noise is a gate
/// people learn to skim.
fn check_swift_error_codes(root: &Path, catalog_keys: &BTreeSet<String>) -> Result<()> {
    let path = root.join(SWIFT_ERROR_CODE_PATH);
    let Ok(content) = fs::read_to_string(&path) else {
        // Not an error: the Swift client may not be checked out in every
        // context this runs in (a sparse checkout, a docs-only CI lane).
        return Ok(());
    };

    let swift_codes = swift_error_codes(&content);
    if swift_codes.is_empty() {
        bail!(
            "i18n-guard: {SWIFT_ERROR_CODE_PATH} declares no `error.*` raw values. \
             Either the file moved or its shape changed — this check would silently \
             pass forever, so it fails loudly instead."
        );
    }

    let catalog_codes: BTreeSet<&String> = catalog_keys
        .iter()
        .filter(|k| k.starts_with("error."))
        .collect();

    let dangling: Vec<&String> = swift_codes
        .iter()
        .filter(|code| !catalog_keys.contains(*code))
        .collect();

    let referenced = swift_referenced_error_codes(root)?;
    let unhandled: Vec<&&String> = catalog_codes
        .iter()
        .filter(|code| !referenced.contains(**code))
        .collect();

    if !unhandled.is_empty() {
        println!(
            "i18n-guard: {} error code(s) are defined in the catalog but named nowhere \
             in the Swift client. They still localize through `unknown(_)`, but no \
             screen can offer a specific recovery for them:",
            unhandled.len()
        );
        for code in &unhandled {
            println!("  {code}");
        }
    }

    if dangling.is_empty() {
        println!(
            "i18n-guard: Swift `ErrorCode` matches the catalog ({} codes).",
            swift_codes.len()
        );
        return Ok(());
    }

    eprintln!(
        "i18n-guard: Swift `ErrorCode` names {} code(s) that `locales/en.json` does not\n\
         define. Each is a lookup miss — the user sees a blank or raw-key message on a\n\
         failure screen. Add the key to the catalog, or correct the raw value:\n",
        dangling.len()
    );
    for code in &dangling {
        eprintln!("  {SWIFT_ERROR_CODE_PATH}: {code:?}");
    }
    bail!(
        "i18n-guard failed: {} Swift error code(s) missing from the catalog",
        dangling.len()
    );
}

/// Every `error.*` code named anywhere in the Swift client.
///
/// Broader than the enum on purpose — see [`check_swift_error_codes`].
fn swift_referenced_error_codes(root: &Path) -> Result<BTreeSet<String>> {
    let mut codes = BTreeSet::new();
    for rel in ["capsule-swift/App", "capsule-swift/Modules"] {
        let dir = root.join(rel);
        if !dir.exists() {
            continue;
        }
        for path in collect_files(&dir, "swift")? {
            let content =
                fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
            codes.extend(swift_error_codes(&content));
        }
    }
    Ok(codes)
}

/// Every `"error.*"` string literal in a Swift source file.
///
/// A plain literal scan rather than a parse of the `rawValue` switch: the switch
/// is the only place these strings appear, and a scanner cannot be broken by the
/// enum being reorganised into extensions or split across files.
pub(crate) fn swift_error_codes(content: &str) -> BTreeSet<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r#""(error\.[a-z0-9_.]+)""#).expect("valid regex"));
    re.captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(findings: &[Finding]) -> Vec<&str> {
        findings.iter().map(|f| f.text.as_str()).collect()
    }

    #[test]
    fn web_flags_jsx_text_and_attributes() {
        let src = r#"
            <h1>Dashboard</h1>
            <input placeholder="Search photos" />
            <img alt="Asset" />
        "#;
        let f = web_findings(src);
        assert!(texts(&f).contains(&"Dashboard"));
        assert!(texts(&f).contains(&"Search photos"));
        assert!(texts(&f).contains(&"Asset"));
    }

    #[test]
    fn web_catches_multiline_text() {
        let src = "<CardTitle>\n    Storage Used\n</CardTitle>";
        let f = web_findings(src);
        assert_eq!(texts(&f), vec!["Storage Used"]);
        assert_eq!(f[0].line, 1);
    }

    #[test]
    fn web_ignores_migrated_and_expressions() {
        // FormattedMessage self-closes; interpolation lives in `{}`.
        let src = r#"
            <FormattedMessage id="dashboard.title" />
            <p>{intl.formatMessage({ id: 'x' })}</p>
            <Button>{loading ? spinner : label}</Button>
            <div className="text-sm" data-slot="x" />
            <Input placeholder={intl.formatMessage({ id: 'auth.email' })} />
        "#;
        assert_eq!(web_findings(src), Vec::new());
    }

    #[test]
    fn web_ignores_generics_and_comparisons() {
        let src = "const x = useState<string>('');\nif (a > b) { return null; }\n";
        assert_eq!(web_findings(src), Vec::new());
    }

    #[test]
    fn swift_flags_literal_but_not_verbatim_or_dynamic() {
        let src = r#"Text("Settings")
            Text("ios.settings.title")
            Text(verbatim: "raw")
            Text(model.title)
            .navigationTitle("Albums")
        "#;
        let f = swift_findings(src);
        let t = texts(&f);
        assert!(t.contains(&"Settings"));
        assert!(t.contains(&"Albums"));
        // The dotted key IS captured; the runner filters it against the catalog.
        assert!(t.contains(&"ios.settings.title"));
        // `verbatim:` and dynamic args are never captured.
        assert!(!t.contains(&"raw"));
    }

    #[test]
    fn swift_ignores_helper_names_ending_in_a_watched_name() {
        // `barButton` ends in `Button` but is a custom helper taking an SF Symbol
        // name — the word boundary keeps it out. Interpolations (`\(count)`) are a
        // documented blind spot and must not match either.
        let src = r#"barButton("square.and.arrow.up", action: share)
            myText("not.matched")
            Button("Delete \(count) Items", role: .destructive) {}
        "#;
        assert_eq!(swift_findings(src), Vec::new());
    }

    #[test]
    fn kotlin_flags_text_and_content_description() {
        let src = r#"Text("Hello")
            Text(stringResource(R.string.no_data_available))
            Text(obj.title)
            Icon(x, contentDescription = "Back arrow")
            Icon(x, contentDescription = obj.title)
        "#;
        let f = kotlin_findings(src);
        let t = texts(&f);
        assert!(t.contains(&"Hello"));
        assert!(t.contains(&"Back arrow"));
        assert_eq!(t.len(), 2, "stringResource and dynamic args are ignored");
    }

    #[test]
    fn line_numbers_are_one_based() {
        let src = "line1\nline2 Text(\"x\")\n";
        assert_eq!(line_of(src, 0), 1);
        assert_eq!(line_of(src, 6), 2);
    }

    #[test]
    fn swift_key_parameters_are_scanned() {
        let src = r#"
        SettingsRow(titleKey: "ios.settings.storage.title", labelKey: "ios.common.done")
        EmptyState(emptyDescriptionKey: "ios.import.plan.empty.description")
        "#;
        let texts: Vec<String> = swift_findings(src).into_iter().map(|f| f.text).collect();
        assert!(texts.contains(&"ios.settings.storage.title".to_string()));
        assert!(texts.contains(&"ios.common.done".to_string()));
        assert!(texts.contains(&"ios.import.plan.empty.description".to_string()));
    }

    /// The whole point of widening the gate: a key nobody added to the catalog
    /// used to compile, render as its own raw text, and fail nothing.
    #[test]
    fn swift_key_parameters_catch_a_key_that_was_never_added() {
        let src = r#"Row(titleKey: "ios.settings.totally.made.up")"#;
        let texts: Vec<String> = swift_findings(src).into_iter().map(|f| f.text).collect();
        assert_eq!(texts, vec!["ios.settings.totally.made.up".to_string()]);
    }

    /// A switch over an enum whose case ends in `Key` is not a call site. This
    /// exact shape — `case .masterKey: "key.fill"` returning an SF Symbol —
    /// was the first false positive the widened detector produced.
    #[test]
    fn swift_key_parameters_ignore_enum_case_patterns() {
        let src = r#"
        public var symbolName: String {
            switch self {
            case .masterKey: "key.fill"
            case .userIdentityKey: "person.badge.key.fill"
            }
        }
        "#;
        assert_eq!(swift_key_parameter_findings(src), Vec::new());
    }

    /// A parameter that merely contains "key" is not a catalog reference.
    #[test]
    fn swift_key_parameters_require_the_label_to_end_in_key() {
        let src = r#"Signer(publicKeyPem: "-----BEGIN PUBLIC KEY-----", keyring: "default")"#;
        assert_eq!(swift_key_parameter_findings(src), Vec::new());
    }

    #[test]
    fn swift_error_codes_reads_the_raw_value_switch() {
        let src = r#"
        public var rawValue: String {
            switch self {
            case .protocolVersionUnsupported: "error.protocol.version_unsupported"
            case .authInvalidCredentials: "error.auth.invalid_credentials"
            case let .unknown(raw): raw
            }
        }
        "#;
        let codes = swift_error_codes(src);
        assert_eq!(codes.len(), 2);
        assert!(codes.contains("error.protocol.version_unsupported"));
        assert!(codes.contains("error.auth.invalid_credentials"));
    }

    /// The scanner must not mistake an ordinary catalog key for an error code —
    /// the check's whole value is that its two sets are the *same* contract.
    #[test]
    fn swift_error_codes_ignores_ui_keys_and_prose() {
        let src = r#"
        Text("ios.settings.title")
        // an error.something mentioned in a comment, unquoted
        Label("ios.error.banner", systemImage: "x")
        "#;
        assert!(swift_error_codes(src).is_empty());
    }

    /// A real regression this guards: a typo'd raw value still compiles, still
    /// round-trips through `init(rawValue:)`, and fails only at lookup time.
    #[test]
    fn swift_error_codes_captures_a_typo_verbatim() {
        let src = r#"case .authRateLimited: "error.auth.rate_limted""#;
        let codes = swift_error_codes(src);
        assert!(codes.contains("error.auth.rate_limted"));
    }
}
