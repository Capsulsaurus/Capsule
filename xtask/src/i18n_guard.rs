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
//!   A migrated call passes a catalog KEY (`Text("app.settings.title")`); a literal
//!   (`Text("Settings")`) is not a key, so it fails. Slice `S-I4` added two more Swift
//!   detectors: *interpolated* literals in those same positions (`Text("Delete \(n)")`),
//!   which `S-I1` had to skip because they had no catalog mechanism yet, and the key
//!   argument of `String(localized:)`, the mechanism for strings outside a
//!   `LocalizedStringKey` position.
//! - **Compose** (`capsule-android/src/**/*.kt`): the string argument of `Text("…")`
//!   and `contentDescription = "…"`. Migrated code uses
//!   `stringResource(R.string.…)`; a bare literal is not a key, so it fails.
//! - **CLI** (`capsule-cli/src/**/*.rs`, slice `S-I5`): string literals anywhere in the
//!   argument list of a terminal-output or error macro. See the rule below — the CLI is
//!   the one Rust surface that renders prose to a human, and it had never been scanned,
//!   which is how the entire `capsule import` arm printed hardcoded English for months.
//!
//! ## What counts as user-facing in a Rust binary
//!
//! A Rust crate mixes three audiences in one file, so "is it a string literal?" is the
//! wrong question. The rule is **who reads it**:
//!
//! 1. **The user reads it → translatable.** Two syntactic positions carry prose to a
//!    terminal, and both are scanned:
//!    - `print!` / `println!` / `eprint!` / `eprintln!` — anything written to stdout/stderr.
//!      Every literal in the argument list is examined, not just the format string: the
//!      migrated shape is `println!("{}", bundle.format(keys::X, &[]))`, so the prose in
//!      unmigrated code hides in the *arguments* (`println!("{}", "Nothing to import.")`)
//!      as often as in the format string.
//!    - `eyre!` / `bail!` **in this binary** — the CLI's error path ends at `color_eyre`,
//!      which prints the message to the user's terminal. A CLI's printed error is user
//!      output, not a developer diagnostic.
//! 2. **An operator reads it → not translatable.** `tracing::{trace,debug,info,warn,error}!`
//!    is telemetry: structured, queryable, and grepped by whoever is holding the pager. The
//!    i18n design doc makes the same call for the server ("the English detail stays
//!    English"), so log messages are deliberately *not* scanned.
//! 3. **A developer reads it → not translatable.** `#[error("…")]` on a `thiserror` type in
//!    a *library* crate is a `Display` impl consumed by callers, and `#[cfg(test)]` modules
//!    are not shipped. Test modules are cut before scanning; library error types are simply
//!    outside the scanned root.
//!
//! Two carve-outs keep the rule at zero false positives without weakening it: a literal
//! whose text is empty once escapes and `{…}` placeholders are removed is punctuation or a
//! format skeleton, not prose (`println!("{}", …)`, `println!("\n  {} {}", …)`); and a
//! literal in the ICU **argument-name** position (`("email", Value::Str(&email))`) names a
//! placeholder rather than displaying it.
//!
//! **Known blind spot:** clap's `--help` output. Usage text comes from doc comments and
//! `#[arg(...)]` attributes that clap renders itself, with no catalog mechanism to render
//! a key through; localizing it is a separate slice, not something an allowlist entry per
//! flag would express honestly. It is recorded here rather than silently omitted.
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
    scan_surface(
        root,
        &["capsule-cli/src"],
        "rs",
        &|content| rust_cli_findings(content),
        // A migrated CLI call site passes `keys::SOME_CONST`, never a quoted key, so a
        // captured literal is a violation without consulting the catalog (as for web).
        &|_text| false,
        &mut violations,
    )?;

    violations.retain(|v| !allowlist.contains(&(v.file.clone(), v.text.clone())));

    // Runs before the literal verdict so a tree with both problems reports both
    // rather than hiding one behind the other's `bail!`.
    check_swift_error_codes(root, &keys)?;

    if violations.is_empty() {
        println!(
            "i18n-guard: no hardcoded user-facing literals found across web/swift/compose/cli."
        );
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
/// Slice `S-I1` had to skip these: with no way to pass ICU arguments from a catalog key,
/// an interpolated string had nowhere to go, so [`swift_literal_findings`]'s `[^"\\]*`
/// deliberately walks past a `\(`. Slice `S-I4` gave them a mechanism
/// (`String(localized:defaultValue:)` against an ICU catalog message), so the blind spot
/// closes. Every hit is a violation without consulting the catalog: a key can never
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

/// Macros whose arguments are written to the user's terminal.
const RUST_OUTPUT_MACROS: &[&str] = &["print", "println", "eprint", "eprintln"];

/// Macros that build the CLI's *printed* error message. `color_eyre` renders these to
/// stderr as the process's failure output, so they are user-facing, not diagnostics.
const RUST_ERROR_MACROS: &[&str] = &["eyre", "bail"];

/// Detect hardcoded user-facing strings in a `capsule-cli` Rust source.
///
/// See the module doc for the audience rule. In short: every string literal in the
/// argument list of a [`RUST_OUTPUT_MACROS`] or [`RUST_ERROR_MACROS`] invocation, minus
/// the two carve-outs ([`is_prose`] and [`is_icu_argument_name`]). `tracing::*!` is
/// deliberately absent from both lists.
pub(crate) fn rust_cli_findings(content: &str) -> Vec<Finding> {
    let source = strip_test_modules(content);
    let mut findings = Vec::new();
    for (open, kind) in rust_macro_calls(source) {
        for (start, end) in macro_string_literals(source, open) {
            let text = &source[start..end];
            if !is_prose(text) || is_icu_argument_name(source, start, end) {
                continue;
            }
            findings.push(Finding {
                line: line_of(source, start),
                // Rust literals wrap across lines with a trailing `\`; collapsing
                // whitespace keeps a finding on one line so it is allowlistable.
                text: normalize_ws(text),
                kind,
            });
        }
    }
    findings.sort_by_key(|f| f.line);
    findings
}

/// Cut the source at the first `#[cfg(test)]` at column 0. Test modules live last in this
/// tree's Rust files and their strings ship to nobody.
fn strip_test_modules(content: &str) -> &str {
    match content.find("\n#[cfg(test)]") {
        Some(i) => &content[..i],
        None => content,
    }
}

/// Locate watched macro invocations: `(offset_of_open_paren, kind)`.
///
/// The `!` is what separates a macro from a same-named function, and the leading
/// non-identifier byte keeps `my_println!` and `capsule::eyre!`-style suffixes out.
fn rust_macro_calls(content: &str) -> Vec<(usize, &'static str)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?:^|[^A-Za-z0-9_])(print|println|eprint|eprintln|eyre|bail)!\s*\(")
            .expect("static regex is valid")
    });
    let mut calls = Vec::new();
    for caps in re.captures_iter(content) {
        let name = caps.get(1).expect("group 1 exists");
        let whole = caps.get(0).expect("group 0 exists");
        let kind = if RUST_OUTPUT_MACROS.contains(&name.as_str()) {
            "cli-print"
        } else if RUST_ERROR_MACROS.contains(&name.as_str()) {
            "cli-error"
        } else {
            continue;
        };
        // The match ends on the `(` itself.
        calls.push((whole.end() - 1, kind));
    }
    calls
}

/// Collect `(start, end)` byte ranges of the *contents* of every string literal inside the
/// argument list whose opening `(` sits at `open`, including literals nested in an inner
/// call such as `format!(…)`.
///
/// This is a small lexer rather than a regex because finding the matching `)` requires
/// knowing which brackets are inside a string. It understands ordinary and raw string
/// literals, char literals (so a `'}'` cannot unbalance the scan), lifetimes, and comments.
fn macro_string_literals(content: &str, open: usize) -> Vec<(usize, usize)> {
    let b = content.as_bytes();
    let mut literals = Vec::new();
    let mut depth = 1usize;
    let mut i = open + 1;
    while i < b.len() && depth > 0 {
        match b[i] {
            b'(' | b'[' | b'{' => {
                depth += 1;
                i += 1;
            }
            b')' | b']' | b'}' => {
                depth -= 1;
                i += 1;
            }
            b'"' => {
                let start = i + 1;
                let mut j = start;
                while j < b.len() && b[j] != b'"' {
                    j += if b[j] == b'\\' { 2 } else { 1 };
                }
                let end = j.min(b.len());
                literals.push((start, end));
                i = end + 1;
            }
            b'r' if matches!(b.get(i + 1), Some(b'"' | b'#'))
                && (i == 0 || !is_ident_byte(b[i - 1])) =>
            {
                match raw_string_literal(b, i) {
                    Some((start, end, next)) => {
                        literals.push((start, end));
                        i = next;
                    }
                    None => i += 1,
                }
            }
            b'\'' => i += char_literal_len(b, i),
            b'/' if b.get(i + 1) == Some(&b'/') => {
                i = b[i..]
                    .iter()
                    .position(|&c| c == b'\n')
                    .map_or(b.len(), |n| i + n + 1);
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = content[i + 2..]
                    .find("*/")
                    .map_or(b.len(), |n| i + 2 + n + 2);
            }
            _ => i += 1,
        }
    }
    literals
}

fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// Parse a raw string starting at `i` (`r`, `r#"`, `r##"`, …), returning the content range
/// and the offset just past the closing delimiter.
fn raw_string_literal(b: &[u8], i: usize) -> Option<(usize, usize, usize)> {
    let mut hashes = 0usize;
    let mut j = i + 1;
    while b.get(j) == Some(&b'#') {
        hashes += 1;
        j += 1;
    }
    if b.get(j) != Some(&b'"') {
        return None;
    }
    let start = j + 1;
    let mut k = start;
    while k < b.len() {
        if b[k] == b'"'
            && b[k + 1..]
                .iter()
                .take(hashes)
                .filter(|&&c| c == b'#')
                .count()
                == hashes
        {
            return Some((start, k, k + 1 + hashes));
        }
        k += 1;
    }
    Some((start, b.len(), b.len()))
}

/// Bytes to skip past a `'` at `i`: a char literal (`'a'`, `'\n'`) is consumed whole so its
/// contents cannot unbalance the bracket scan; a lifetime (`'a`) advances by one.
fn char_literal_len(b: &[u8], i: usize) -> usize {
    if b.get(i + 1) == Some(&b'\\') {
        // `'\n'`, `'\''`, … — the shortest form is four bytes.
        if b.get(i + 3) == Some(&b'\'') {
            return 4;
        }
    } else if b.get(i + 2) == Some(&b'\'') {
        return 3;
    }
    1
}

/// Whether a literal's *source text* still says something once formatting is removed.
///
/// Escape sequences and `{…}` placeholders are stripped, then at least one ASCII letter
/// must remain. This is what separates `"Nothing to import."` from `"{}"`, `"\n  {} {}"`,
/// and `"{metadata:#?}"` — the last of which reads like prose only because a *variable
/// name* sits inside the braces.
fn is_prose(raw: &str) -> bool {
    let mut unescaped = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            chars.next();
        } else {
            unescaped.push(c);
        }
    }
    let doubled = unescaped.replace("{{", "").replace("}}", "");
    let mut depth = 0usize;
    let mut visible = String::with_capacity(doubled.len());
    for c in doubled.chars() {
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ if depth == 0 => visible.push(c),
            _ => {}
        }
    }
    visible.chars().any(|c| c.is_ascii_alphabetic())
}

/// Whether the literal at `start..end` is an ICU **argument name** rather than displayed
/// text — the `"email"` in `bundle.format(key, &[("email", Value::Str(&email))])`. The
/// shape is exact: an opening `(` before it and `, Value::` after it.
fn is_icu_argument_name(content: &str, start: usize, end: usize) -> bool {
    let before = content[..start.saturating_sub(1)].trim_end();
    if !before.ends_with('(') {
        return false;
    }
    let after = content.get(end + 1..).unwrap_or("").trim_start();
    let Some(rest) = after.strip_prefix(',') else {
        return false;
    };
    rest.trim_start().starts_with("Value::")
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
            Text("app.settings.title")
            Text(verbatim: "raw")
            Text(model.title)
            .navigationTitle("Albums")
        "#;
        let f = swift_findings(src);
        let t = texts(&f);
        assert!(t.contains(&"Settings"));
        assert!(t.contains(&"Albums"));
        // The dotted key IS captured; the runner filters it against the catalog.
        assert!(t.contains(&"app.settings.title"));
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
                    localized: "app.timeline.delete_selected.confirm",
                    defaultValue: "Delete \(count) Items"
                ),
                role: .destructive
            ) {}
        "#;
        let f = swift_findings(src);
        // Only the catalog key is captured; the runner then matches it against the catalog.
        assert_eq!(texts(&f), vec!["app.timeline.delete_selected.confirm"]);
    }

    #[test]
    fn swift_flags_a_string_localized_key_that_is_not_a_catalog_key() {
        let src = r#"String(localized: "Photos")
            String(localized: "app.media.photo")
        "#;
        let f = swift_findings(src);
        let t = texts(&f);
        // Both are captured; the runner keeps only the one that is not a catalog key.
        assert!(t.contains(&"Photos"));
        assert!(t.contains(&"app.media.photo"));
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
    fn swift_computed_properties_are_scanned() {
        // The shape that hid twenty-two English strings: display text returned
        // from a `String` property, reaching the screen through a variable so
        // no call site ever carries the literal.
        let src = r#"
            var title: String {
                switch self {
                case .places: "Places and Trips"
                case .people: "People and Pets"
                }
            }
        "#;
        let texts: Vec<String> = swift_findings(src).into_iter().map(|f| f.text).collect();
        assert!(texts.contains(&"Places and Trips".to_string()));
        assert!(texts.contains(&"People and Pets".to_string()));
    }

    #[test]
    fn swift_computed_properties_ignore_identifier_like_values() {
        // A `String` property is not automatically display text. Symbol names, raw values
        // and file formats do not spell a lowercase letter in position two; a `String`
        // property that is not named like display text is not scanned at all.
        let src = r#"
            var title: String {
                switch self {
                case .heic: "HEIC"
                case .dng: "DNG"
                case .hdr10: "HDR10"
                case .hlg: "HLG"
                case .photo: "app.media.photo"
                case .masterKey: "key.fill"
                }
            }
            var systemImage: String {
                switch self {
                case .places: "Map Pin Icon"
                }
            }
        "#;
        assert_eq!(swift_findings(src), Vec::new());
    }

    #[test]
    fn swift_computed_properties_catch_a_single_word_string() {
        // Issue #394: the detector's own documented example. The literal rule used to
        // require a space, so `case .places: "Places"` — the exact shape the doc comment
        // advertises as the motivating bug — could not be caught.
        let src = r#"
            var title: String {
                switch self {
                case .places: "Places"
                case .people: "People"
                }
            }
        "#;
        let texts: Vec<String> = swift_findings(src).into_iter().map(|f| f.text).collect();
        assert_eq!(texts, vec!["Places".to_string(), "People".to_string()]);
    }

    #[test]
    fn swift_display_functions_are_scanned() {
        // `var` was required, so a `func` returning display text was invisible — the
        // blind spot that hid "Dolby Vision" in `AssetInfoFormatting.hdrName(_:)`.
        let src = r#"
            static func hdrName(_ format: HDRFormat) -> String {
                switch format {
                case .hdr10: "HDR10"
                case .dolbyVision: "Dolby Vision"
                case .hlg: "HLG"
                }
            }
        "#;
        let texts: Vec<String> = swift_findings(src).into_iter().map(|f| f.text).collect();
        assert_eq!(texts, vec!["Dolby Vision".to_string()]);
    }

    #[test]
    fn swift_display_members_are_scanned_beyond_the_switch() {
        // A property returns display text in more shapes than a `switch`: an explicit
        // `return`, an implicit one, and a dictionary value. Only `case` was scanned.
        // `heading`, `summary` and `text` are also new stems — `var heading` was not
        // matched at all before.
        let src = r#"
            var heading: String {
                if isEmpty { return "Nothing here yet" }
                "Your library"
            }
            var summary: String {
                let names: [Kind: String] = [.places: "Places and trips"]
                return names[kind] ?? ""
            }
            var promptText: String {
                "Choose an album"
            }
        "#;
        let texts: Vec<String> = swift_findings(src).into_iter().map(|f| f.text).collect();
        assert_eq!(
            texts,
            vec![
                "Nothing here yet".to_string(),
                "Your library".to_string(),
                "Places and trips".to_string(),
                "Choose an album".to_string(),
            ]
        );
    }

    #[test]
    fn swift_display_members_do_not_report_a_literal_twice() {
        // A `func` nested in a `var` body is scanned by both members, and the dictionary
        // and `case` patterns overlap. Findings are keyed by absolute offset, so the
        // literal is one violation, not two.
        let src = r#"
            var title: String {
                func headingFor(_ k: Kind) -> String {
                    return "Places and trips"
                }
                return headingFor(kind)
            }
        "#;
        let findings = swift_findings(src);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].text, "Places and trips");
    }

    #[test]
    fn swift_display_members_ignore_a_localized_default_value() {
        // `String(localized:defaultValue:)` is the *migrated* shape: the key is checked
        // against the catalog and the default value is the English source the ICU
        // arguments hang off. Neither is a hardcoded literal, and the dictionary-value
        // pattern must not read `defaultValue:` as a dictionary key.
        let src = r#"
            var title: String {
                String(
                    localized: "app.timeline.delete_selected.confirm",
                    defaultValue: "Delete Items",
                    comment: "Confirm button"
                )
            }
        "#;
        let texts: Vec<String> = swift_computed_property_findings(src)
            .into_iter()
            .map(|f| f.text)
            .collect();
        assert_eq!(texts, Vec::<String>::new());
    }

    /// The watched positions, one per alternative of the shared list.
    fn swift_text_positions() -> Vec<&'static str> {
        SWIFT_TEXT_POSITIONS.split('|').collect()
    }

    #[test]
    fn every_watched_api_position_is_a_bare_identifier() {
        // The shared list is spliced into two regexes, so a stray space, an empty
        // alternative or a regex metacharacter in it would silently widen or break both.
        let positions = swift_text_positions();
        assert!(positions.len() >= 15, "{positions:?}");
        for position in &positions {
            assert!(
                !position.is_empty()
                    && position
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_'),
                "`{position}` is not a bare identifier"
            );
        }
        let mut unique = positions.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), positions.len(), "a position is listed twice");
    }

    #[test]
    fn every_watched_api_position_is_caught_in_both_regexes() {
        // The fixture is *derived* from the shared list rather than written out, so a
        // position cannot be added without a case: `confirmationDialog` sat in the
        // interpolation regex and not the literal one for two slices (#394) precisely
        // because the two lists were maintained by hand. `searchable` and `tabItem` had
        // no fixture at all until this test, so a broken alternative would have been
        // silent.
        for position in swift_text_positions() {
            let literal = format!("Watched {position} text");
            let interpolated = format!("Watched {position} \\(count)");
            let src = format!(
                "view\n    {position}(\"{literal}\")\n    {position}(\"{interpolated}\")\n"
            );
            let findings = swift_findings(&src);
            let found = texts(&findings);
            assert!(
                found.contains(&literal.as_str()),
                "{position}: the plain literal was not caught, got {found:?}"
            );
            assert!(
                found.contains(&interpolated.as_str()),
                "{position}: the interpolated literal was not caught, got {found:?}"
            );
            // And the word boundary still holds, for both regexes: a helper that merely
            // ends in a watched name is not a watched position.
            let helper = format!("view\n    my{position}(\"{literal}\")\n");
            assert_eq!(
                swift_findings(&helper),
                Vec::new(),
                "{position}: the word boundary was lost"
            );
        }
    }

    #[test]
    fn swift_watches_every_api_position_in_both_regexes() {
        // The derived fixture above proves each alternative matches; this one proves the
        // real call shapes do, with the arguments SwiftUI actually puts after the string.
        // `confirmationDialog` was in the interpolation regex and not the literal one;
        // `help`, `accessibilityValue`, `ContentUnavailableView` and `tabItem` were in
        // neither. One shared list now, so the two cannot disagree again.
        let src = r#"
            .help("Show the import log")
            ContentUnavailableView("No photos yet", systemImage: "photo")
            .accessibilityValue("Three of ten")
            .confirmationDialog("Delete this?", isPresented: $flag) {}
            .help("Imported \(count) files")
        "#;
        let f = swift_findings(src);
        let t = texts(&f);
        for expected in [
            "Show the import log",
            "No photos yet",
            "Three of ten",
            "Delete this?",
        ] {
            assert!(t.contains(&expected), "{expected} missing from {t:?}");
        }
        assert!(t.contains(&r"Imported \(count) files"), "{t:?}");
    }

    #[test]
    fn rust_flags_prose_in_every_print_macro() {
        let src = r#"
            println!("{}", "Nothing to import.".yellow());
            print!("Enter a value: ");
            eprintln!("Import failed");
            eprint!("{}", "partial".red());
        "#;
        let f = rust_cli_findings(src);
        let t = texts(&f);
        assert!(t.contains(&"Nothing to import."), "{t:?}");
        assert!(t.contains(&"Enter a value:"), "{t:?}");
        assert!(t.contains(&"Import failed"), "{t:?}");
        assert!(t.contains(&"partial"), "{t:?}");
    }

    #[test]
    fn rust_flags_prose_in_the_format_string_and_in_the_arguments() {
        // Unmigrated CLI code hides prose in both positions, so both are scanned.
        let src = r#"
            println!("  Name:            {}", cfg.library_name);
            println!("  {} {}", "Status:".dimmed(), "Not logged in".red());
        "#;
        let f = rust_cli_findings(src);
        let t = texts(&f);
        assert!(t.contains(&"Name: {}"), "{t:?}");
        assert!(t.contains(&"Status:"), "{t:?}");
        assert!(t.contains(&"Not logged in"), "{t:?}");
    }

    #[test]
    fn rust_flags_a_literal_nested_in_an_inner_format_call() {
        let src = r#"println!("{}", format!("Error checking auth status: {e}").red());"#;
        assert_eq!(
            texts(&rust_cli_findings(src)),
            vec!["Error checking auth status: {e}"]
        );
    }

    #[test]
    fn rust_flags_printed_errors_but_not_library_error_attributes() {
        // `eyre!`/`bail!` in the binary end at `color_eyre`, which prints them to the
        // user. `#[error(...)]` is a `Display` impl for developers.
        let src = r#"
            let x = plan(..).map_err(|e| eyre!("Planning failed: {e}"))?;
            bail!("Library version mismatch. Upgrade required.");
            #[error("workspace is sealed")]
            SealedWorkspace,
        "#;
        let f = rust_cli_findings(src);
        let t = texts(&f);
        assert!(t.contains(&"Planning failed: {e}"), "{t:?}");
        assert!(
            t.contains(&"Library version mismatch. Upgrade required."),
            "{t:?}"
        );
        assert!(!t.contains(&"workspace is sealed"), "{t:?}");
    }

    #[test]
    fn rust_ignores_tracing_because_logs_are_operator_telemetry() {
        let src = r#"
            tracing::info!("import: source adapter extraction complete");
            tracing::debug!(count = n, "planned the import");
            tracing::warn!("the library lock is stale");
            tracing::error!("failed to open the library: {e}");
            tracing::trace!("Parsed CLI arguments: {:#?}", cli);
        "#;
        assert_eq!(rust_cli_findings(src), Vec::new());
    }

    #[test]
    fn rust_ignores_a_migrated_call_site() {
        // The migrated shape: the format string is a skeleton, the prose comes from the
        // catalog, and the only literal left is an ICU argument NAME.
        let src = r#"
            println!(
                "{}",
                bundle
                    .format(keys::IMPORT_DONE, &[("imported", Value::Int(n))])
                    .green()
            );
        "#;
        assert_eq!(rust_cli_findings(src), Vec::new());
    }

    #[test]
    fn rust_ignores_format_skeletons_with_no_prose_left() {
        let src = r#"
            println!("{}", value);
            println!("\n{} {}", a, b);
            println!("{metadata:#?}");
            println!("  {} {}", x, y);
            println!("{{}}");
            println!();
        "#;
        assert_eq!(rust_cli_findings(src), Vec::new());
    }

    #[test]
    fn rust_ignores_test_modules() {
        let src = "println!(\"Shipped prose\");\n#[cfg(test)]\nmod tests {\n    println!(\"Test-only prose\");\n}\n";
        let f = rust_cli_findings(src);
        let t = texts(&f);
        assert_eq!(t, vec!["Shipped prose"]);
    }

    #[test]
    fn rust_ignores_helper_macros_and_functions_that_merely_end_in_a_watched_name() {
        let src = r#"
            my_println!("Hello");
            let s = println("not a macro");
            capsule_eyre!("nope");
        "#;
        assert_eq!(rust_cli_findings(src), Vec::new());
    }

    #[test]
    fn rust_survives_char_literals_and_raw_strings_in_the_argument_list() {
        // A `'}'` char literal must not unbalance the bracket scan, or every literal
        // after it in the file would be missed.
        let src = concat!(
            "println!(\"{}\", s.replace('}', \"\").trim());\n",
            "println!(\"{}\", r#\"Raw prose\"#);\n",
            "println!(\"Trailing prose\");\n",
        );
        let f = rust_cli_findings(src);
        let t = texts(&f);
        assert!(t.contains(&"Raw prose"), "{t:?}");
        assert!(t.contains(&"Trailing prose"), "{t:?}");
    }

    #[test]
    fn rust_collapses_a_wrapped_literal_onto_one_line_so_it_is_allowlistable() {
        let src =
            "eyre!(\"No directories specified. Use --all or one of \\\n    --config, --data.\")";
        assert_eq!(
            texts(&rust_cli_findings(src)),
            vec!["No directories specified. Use --all or one of \\ --config, --data."]
        );
    }

    #[test]
    fn rust_line_numbers_point_at_the_literal_not_the_macro() {
        let src = "fn f() {\n    println!(\n        \"{}\",\n        \"Prose here\"\n    );\n}\n";
        let f = rust_cli_findings(src);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].line, 4);
        assert_eq!(f[0].kind, "cli-print");
    }

    #[test]
    fn the_committed_allowlist_parses_and_covers_only_known_files() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask lives in the workspace root");
        let entries = load_allowlist(root).expect("the committed allowlist parses");
        assert!(!entries.is_empty(), "S-I5 left CLI debt on the allowlist");
        for (file, _) in &entries {
            assert!(
                root.join(file).exists(),
                "allowlist references a file that no longer exists: {file} \
                 (delete the entry when the file goes away)"
            );
        }
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
        SettingsRow(titleKey: "app.settings.storage.title", labelKey: "app.common.done")
        EmptyState(emptyDescriptionKey: "app.import.plan.empty.description")
        "#;
        let texts: Vec<String> = swift_findings(src).into_iter().map(|f| f.text).collect();
        assert!(texts.contains(&"app.settings.storage.title".to_string()));
        assert!(texts.contains(&"app.common.done".to_string()));
        assert!(texts.contains(&"app.import.plan.empty.description".to_string()));
    }

    /// The whole point of widening the gate: a key nobody added to the catalog
    /// used to compile, render as its own raw text, and fail nothing.
    #[test]
    fn swift_key_parameters_catch_a_key_that_was_never_added() {
        let src = r#"Row(titleKey: "app.settings.totally.made.up")"#;
        let texts: Vec<String> = swift_findings(src).into_iter().map(|f| f.text).collect();
        assert_eq!(texts, vec!["app.settings.totally.made.up".to_string()]);
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
        Text("app.settings.title")
        // an error.something mentioned in a comment, unquoted
        Label("app.error.banner", systemImage: "x")
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
