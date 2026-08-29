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
//!   (`Text("Settings")`) is not a key, so it fails. Slice `S-I4` added two more Swift
//!   detectors: *interpolated* literals in those same positions (`Text("Delete \(n)")`),
//!   which `S-I1` had to skip because they had no catalog mechanism yet, and the key
//!   argument of `String(localized:)`, the mechanism for strings outside a
//!   `LocalizedStringKey` position.
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

/// Detect hardcoded user-facing strings in a SwiftUI source: plain literals in a watched
/// API position, *interpolated* literals in the same positions, and the key argument of
/// `String(localized:)`.
pub(crate) fn swift_findings(content: &str) -> Vec<Finding> {
    let mut findings = swift_literal_findings(content);
    findings.extend(swift_interpolation_findings(content));
    findings.extend(swift_localized_key_findings(content));
    findings.sort_by_key(|f| f.line);
    findings
}

/// Detect string-literal arguments to user-facing SwiftUI APIs.
fn swift_literal_findings(content: &str) -> Vec<Finding> {
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
    matched_findings(content, re, "swift-literal")
}

/// Detect *interpolated* literals in the same watched positions — `Text("Delete \(n)")`.
///
/// Slice `S-I1` had to skip these: with no way to pass ICU arguments from a catalog key,
/// an interpolated string had nowhere to go, so [`swift_literal_findings`]'s `[^"\\]*`
/// deliberately walks past a `\(`. Slice `S-I4` gave them a mechanism
/// (`String(localized:defaultValue:)` against an ICU catalog message), so the blind spot
/// closes. Every hit is a violation without consulting the catalog: a key can never
/// contain an interpolation, and a migrated call site passes `String(...)`, not a literal.
fn swift_interpolation_findings(content: &str) -> Vec<Finding> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r#"(?:^|[^A-Za-z0-9_])(?:Text|Label|Button|Section|Toggle|navigationTitle|accessibilityLabel|accessibilityHint|alert|confirmationDialog)\(\s*"([^"]*\\\([^"]*)""#,
        )
        .expect("static regex is valid")
    });
    matched_findings(content, re, "swift-interpolation")
}

/// Detect the key argument of `String(localized:)`, the mechanism outside SwiftUI's
/// `LocalizedStringKey` positions (`LAContext` reasons, view-model strings, enum labels).
///
/// The capture is the *key*, checked against the catalog by the runner, so
/// `String(localized: "Photos")` fails while `String(localized: "ios.media_type.photo")`
/// passes. The `defaultValue:` argument is deliberately not captured — it is the English
/// source text the ICU arguments hang off, and is expected to be a literal.
fn swift_localized_key_findings(content: &str) -> Vec<Finding> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r#"String\(\s*localized:\s*"([^"\\]*[A-Za-z][^"\\]*)""#)
            .expect("static regex is valid")
    });
    matched_findings(content, re, "swift-localized-key")
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
        // name — the word boundary keeps it out, for the interpolation detector as
        // much as for the literal one.
        let src = r#"barButton("square.and.arrow.up", action: share)
            myText("not.matched")
            barButton("Delete \(count) Items", role: .destructive) {}
        "#;
        assert_eq!(swift_findings(src), Vec::new());
    }

    #[test]
    fn swift_flags_an_interpolated_literal_in_a_watched_position() {
        // The `S-I1` blind spot, closed by `S-I4`.
        let src = r#"Text("Delete \(count) Items")
            .confirmationDialog("Delete \(count) Items?", isPresented: $flag) {}
        "#;
        let f = swift_findings(src);
        let t = texts(&f);
        assert!(t.contains(&r"Delete \(count) Items"), "{t:?}");
        assert!(t.contains(&r"Delete \(count) Items?"), "{t:?}");
    }

    #[test]
    fn swift_ignores_a_migrated_interpolated_call_site() {
        // The migrated shape passes `String(localized:defaultValue:)`, so the watched
        // position holds no literal at all and the `defaultValue:` text is not captured.
        let src = r#"Button(
                String(
                    localized: "ios.timeline.delete_selected.confirm",
                    defaultValue: "Delete \(count) Items"
                ),
                role: .destructive
            ) {}
        "#;
        let f = swift_findings(src);
        // Only the catalog key is captured; the runner then matches it against the catalog.
        assert_eq!(texts(&f), vec!["ios.timeline.delete_selected.confirm"]);
    }

    #[test]
    fn swift_flags_a_string_localized_key_that_is_not_a_catalog_key() {
        let src = r#"String(localized: "Photos")
            String(localized: "ios.media_type.photo")
        "#;
        let f = swift_findings(src);
        let t = texts(&f);
        // Both are captured; the runner keeps only the one that is not a catalog key.
        assert!(t.contains(&"Photos"));
        assert!(t.contains(&"ios.media_type.photo"));
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
}
