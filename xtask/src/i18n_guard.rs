//! `xtask i18n-guard`: a per-platform regression gate that fails when a NEW
//! user-facing string literal is introduced outside the i18n catalog.
//!
//! Every user-facing literal in the web (JSX), SwiftUI, and Compose surfaces lives
//! as a key in the canonical `locales/` catalog. This gate keeps
//! that from regressing: it scans those three surfaces for hardcoded
//! literals and fails if it finds one that is not a known catalog key (and is not
//! on the documented allowlist).
//!
//! Design (pragmatism over perfection, per the contract): this is an
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
//!   A migrated call passes a catalog KEY (`Text("app.settings.title")`); a literal
//!   (`Text("Settings")`) is not a key, so it fails. Two more Swift
//!   detectors cover *interpolated* literals in those same positions (`Text("Delete \(n)")`),
//!   which a plain-literal scan cannot see, and the key
//!   argument of `String(localized:)`, the mechanism for strings outside a
//!   `LocalizedStringKey` position.
//! - **Compose** (`capsule-android/src/**/*.kt`): the string argument of `Text("…")`
//!   and `contentDescription = "…"`. Migrated code uses
//!   `stringResource(R.string.…)`; a bare literal is not a key, so it fails.
//!
//! The CLI and TUI (`capsule-cli`) are not scanned: they are English-only by decision
//! (ADR-0011), so their inline English is the intended shape rather than a regression.
//!
//! The Swift/Compose surfaces are anchored to the catalog: a captured string passes
//! only if it exactly matches a key in `locales/en.json`. The web surface has no
//! quoted key to compare against (text lives in `<FormattedMessage id=…>`), so any
//! captured web literal is a violation unless allowlisted.
//!
//! Every detector is a pure `&str -> Vec<Finding>` function so the acceptance tests
//! (zero findings on the migrated tree; an injected literal is caught) run without
//! disk I/O.

use std::collections::{BTreeMap, BTreeSet};
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

/// The SwiftUI call and modifier positions whose string argument reaches the screen.
///
/// One list, shared by the plain-literal and the interpolated detectors, because a
/// position watched by one and not the other is a hole nobody notices: `confirmationDialog`
/// was in the interpolation regex and not the literal one for two slices (#394). The
/// leading word boundary at each use keeps a helper that merely *ends* in one of these
/// (`barButton("sf.symbol.name")`) out.
const SWIFT_TEXT_POSITIONS: &str = "Text|Label|Button|Section|Toggle|navigationTitle\
|accessibilityLabel|accessibilityHint|accessibilityValue|alert|confirmationDialog|help\
|searchable|ContentUnavailableView|tabItem";

/// The property and function name stems that mark a `String` member as display text.
///
/// `value` is deliberately absent: `var rawValue: String` appears two dozen times in
/// `capsule-swift/Modules/CapsuleDomain/Sources/` returning identifiers, none of which is
/// display text.
const SWIFT_DISPLAY_STEMS: &str = "title|message|label|name|description|subtitle\
|heading|text|summary|prompt";

/// A display-text literal: a capital followed by a **lowercase letter**.
///
/// This is the rule #394 was filed about. It used to be "a capital, then a space
/// somewhere", which excluded every single-word string — including `case .places:
/// "Places"`, the example the detector's own doc comment gave as the shape it catches.
/// Requiring a lowercase letter in position two keeps out exactly what the space was
/// there to keep out (`"HEIC"`, `"HDR10"`, `"HLG"`, an SF Symbol name like `"key.fill"`),
/// and lets a single capitalized word through.
///
/// The trade: prose whose second character is neither lowercase nor a space
/// (`"E-mail sent"`, `"AI Insights"`) is no longer caught. Measured across
/// `capsule-swift/{App,Modules}` at the time of the change, in every position this
/// detector scans: **zero** strings are lost and one is gained.
const SWIFT_DISPLAY_LITERAL: &str = r#"[A-Z][a-z][^"\\]*"#;

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
    // `Text("…")`, `.navigationTitle("…")`, `Label("…", …)` and the rest of
    // [`SWIFT_TEXT_POSITIONS`]. The leading `(?:^|[^A-Za-z0-9_])` is a word boundary so
    // helper names that merely END in one of these (`barButton("sf.symbol.name")`) don't
    // match; a leading `.` (method syntax) is still allowed. The `"` must immediately
    // follow `(` so `Text(verbatim: "…")` and `Text(dynamicVar)` are not matched.
    // `[^"\\]*` keeps it to simple literals — an interpolation contains `\(` and is
    // caught by [`swift_interpolation_findings`] instead.
    let re = RE.get_or_init(|| {
        Regex::new(&format!(
            r#"(?:^|[^A-Za-z0-9_])(?:{SWIFT_TEXT_POSITIONS})\(\s*"([^"\\]*[A-Za-z][^"\\]*)""#
        ))
        .expect("static regex is valid")
    });
    let mut findings = matched_findings(content, re, "swift-literal");
    findings.extend(swift_key_parameter_findings(content));
    findings.extend(swift_computed_property_findings(content));
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
    // interpolated keys (`"app.x.\(raw).title"`) stay a documented blind spot.
    let re = RE.get_or_init(|| {
        Regex::new(r#"(?:^|[^A-Za-z0-9_.])[A-Za-z]*Key:\s*"([^"\\]*[A-Za-z][^"\\]*)""#)
            .expect("static regex is valid")
    });
    matched_findings(content, re, "swift-key-param")
}

/// Detect display text returned from a `String`-typed computed property or function.
///
/// The blind spot that hid twenty-two English strings from this gate. A view
/// that writes `Text("Places")` is caught by [`swift_literal_findings`]; a view that
/// writes `Text(category.title)` is not, and neither is the property behind it:
///
/// ```swift
/// var title: String {
///     switch self {
///     case .places: "Places"          // never seen by the gate
///     }
/// }
/// ```
///
/// The literal reaches the screen through a variable, so no call site carries
/// it and no argument label ends in `Key`. Non-English users read those in
/// English, and the gate reported zero findings the whole time.
///
/// # What is scanned
///
/// A member whose name ends in one of [`SWIFT_DISPLAY_STEMS`] and whose type is
/// `String` — a `var`, or (since #394) a `func` such as `hdrName(_:) -> String`. The
/// function form reads the signature with `\([^)]*\)`, so one whose parameters contain a
/// nested `)` (a closure type) is not matched: a remaining blind spot, but a narrowing
/// one, never a false positive. Inside
/// its body, four literal positions, because a property returns display text in more
/// shapes than a `switch`: a `case` arm, an explicit `return`, a bare literal on its own
/// line (an implicit return, or an `if` branch), and a dictionary value. Hits are keyed by
/// absolute offset, so a literal two positions both match is reported once.
///
/// A `String` member is not automatically display text — a symbol name or a raw value is
/// not — which is what [`SWIFT_DISPLAY_LITERAL`] filters on. Its history is #394: the rule
/// used to require a space, which excluded the single-word example this doc comment gives.
fn swift_computed_property_findings(content: &str) -> Vec<Finding> {
    static MEMBERS: OnceLock<Vec<Regex>> = OnceLock::new();
    static LITERALS: OnceLock<Vec<Regex>> = OnceLock::new();
    let members = MEMBERS.get_or_init(|| {
        [
            // `var displayName: String {` — the stem ends the name.
            format!(r"var\s+[A-Za-z]*(?i:{SWIFT_DISPLAY_STEMS})\s*:\s*String\s*\{{"),
            // `static func hdrName(_ format: HDRFormat) -> String {` — the stem may sit
            // anywhere in the name, since a function reads `label(for:)` as often as
            // `formattedLabel()`.
            format!(
                r"func\s+[A-Za-z]*(?i:{SWIFT_DISPLAY_STEMS})[A-Za-z]*\s*\([^)]*\)\s*->\s*String\s*\{{"
            ),
        ]
        .iter()
        .map(|pattern| Regex::new(pattern).expect("static regex is valid"))
        .collect()
    });
    let literals = LITERALS.get_or_init(|| {
        [
            // `case .places: "Places"`
            format!(r#"case\s+\.[A-Za-z0-9_]+:\s*"({SWIFT_DISPLAY_LITERAL})""#),
            // `return "Places"`
            format!(r#"return\s+"({SWIFT_DISPLAY_LITERAL})""#),
            // A bare literal statement: an implicit return, or an `if`/`else` branch.
            format!(r#"(?m)^\s*"({SWIFT_DISPLAY_LITERAL})"\s*$"#),
            // A dictionary or array value: `[.places: "Places"]`. The key must start
            // with `.`, so a *labelled argument* is not mistaken for one — in
            // particular `String(localized:defaultValue:)`, whose `defaultValue:` is the
            // English source text and is deliberately never captured (see
            // [`swift_localized_key_findings`]).
            format!(r#"\.[A-Za-z0-9_]+\s*:\s*"({SWIFT_DISPLAY_LITERAL})"\s*[,\]]"#),
        ]
        .iter()
        .map(|pattern| Regex::new(pattern).expect("static regex is valid"))
        .collect()
    });

    // Keyed by absolute offset: the dictionary and `case` patterns overlap, and a member
    // nested inside another member's body is scanned twice. Either way the literal is one
    // finding, and `BTreeMap` also puts them back in source order.
    let mut hits: BTreeMap<usize, String> = BTreeMap::new();
    for member in members {
        for member_match in member.find_iter(content) {
            let open = member_match.end() - 1;
            let Some(body) = brace_body(content, open) else {
                continue;
            };
            for literal in literals {
                for capture in literal.captures_iter(body) {
                    let group = capture.get(1).expect("group 1 exists");
                    // Offsets are into `body`, which starts one byte past the
                    // brace — so the absolute position is that plus the local one.
                    hits.entry(open + 1 + group.start())
                        .or_insert_with(|| group.as_str().to_string());
                }
            }
        }
    }
    hits.into_iter()
        .map(|(offset, text)| Finding {
            line: line_of(content, offset),
            text,
            kind: "swift-computed-property",
        })
        .collect()
}

/// The text between the brace at `open` and its match, or `None` if unbalanced.
fn brace_body(content: &str, open: usize) -> Option<&str> {
    let bytes = content.as_bytes();
    debug_assert_eq!(bytes[open], b'{');
    let mut depth = 0usize;
    for (offset, byte) in bytes[open..].iter().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return content.get(open + 1..open + offset);
                }
            }
            _ => {}
        }
    }
    None
}

/// Detect *interpolated* literals in the same watched positions — `Text("Delete \(n)")`.
///
/// [`swift_literal_findings`]'s `[^"\\]*` deliberately walks past a `\(`, so plain-literal
/// scanning cannot see these. An interpolated string has a catalog mechanism
/// (`String(localized:defaultValue:)` against an ICU catalog message), so this detector
/// closes that blind spot. Every hit is a violation without consulting the catalog: a key can never
/// contain an interpolation, and a migrated call site passes `String(...)`, not a literal.
fn swift_interpolation_findings(content: &str) -> Vec<Finding> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(&format!(
            r#"(?:^|[^A-Za-z0-9_])(?:{SWIFT_TEXT_POSITIONS})\(\s*"([^"]*\\\([^"]*)""#
        ))
        .expect("static regex is valid")
    });
    matched_findings(content, re, "swift-interpolation")
}

/// Detect the key argument of `String(localized:)`, the mechanism outside SwiftUI's
/// `LocalizedStringKey` positions (`LAContext` reasons, view-model strings, enum labels).
///
/// The capture is the *key*, checked against the catalog by the runner, so
/// `String(localized: "Photos")` fails while `String(localized: "app.media.photo")`
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
mod tests;
