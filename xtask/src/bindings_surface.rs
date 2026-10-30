//! The committed surface dump of the `capsule_sdk` uniffi bindings.
//!
//! `mise-tasks/gen-bindings` generates the Swift and Kotlin sources into the gitignored
//! `target/`, so the bun-only documentation build cannot read them (`design/developer-docs.md`:
//! artifacts cross the boundary, not toolchains). uniffi 0.31 offers no stable machine-readable
//! description of a namespace either: its one dump, `print_repr`, is Rust `Debug` of an IR with
//! no serde support. So this task reads the generated sources themselves and writes the
//! declarations a caller compiles against to the committed `capsule-sdk/bindings-surface.json`,
//! in a format this repository owns. With `--check` it fails when the committed copy is stale.
//!
//! ## What is read, and how
//!
//! Not a Swift or Kotlin parser. The sources are uniffi's templates rendered over one
//! namespace, so their layout is fixed by the pinned uniffi version, and this reads only that
//! layout:
//!
//! - A small lexer gives every line the brace depth it starts at, skipping comments, string
//!   literals, and character literals, so a `{` in a doc comment or a string never moves it. A
//!   file whose braces do not balance under it is refused: the depths would be wrong.
//! - A **top-level declaration** is a depth-0 line declaring a type whose name carries the
//!   `Ffi` prefix every `capsule_sdk` export has (minus uniffi's own `FfiConverter*`), or a
//!   free function (in Kotlin, one whose name uniffi backtick-quoted, which it does only for
//!   generated names). A public top-level type that is neither an `Ffi…` export nor one of
//!   uniffi's own runtime types is fatal: the prefix is a convention, and a type outside it
//!   would otherwise vanish from both languages without the parity check noticing.
//! - A type's **members** are the depth-1 declarations of its block: an object's methods come
//!   from the `…Protocol` (Swift) or `…Interface` (Kotlin) uniffi pairs it with, its
//!   constructors (a zero-argument one included) from the class itself, minus uniffi's
//!   handle plumbing; a record's fields, an enum's cases, and a sealed
//!   class's variants from the type's own block.
//! - A declaration's **doc comment** is the `/** … */` block directly above it.
//!
//! The sources are generated with `--no-format`. uniffi formats Kotlin with ktlint when one is
//! on `PATH`, and the Rust CI job has none while a developer's machine usually does, so a
//! formatted read would make the dump depend on the machine that wrote it.
//!
//! ## What it asserts
//!
//! Three things, each fatal, because a dump that silently lost a declaration would publish a
//! reference page that looks complete:
//!
//! - every declaration in [`REQUIRED`] — the verbs the native clients drive — is present;
//! - Swift and Kotlin declare the same top-level names and the same methods (Kotlin spells a
//!   Rust error type `…Exception` where Swift keeps `…Error`); and
//! - each language yields at least one declaration.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::LazyLock;

use eyre::{Context, ContextCompat, Result, bail};
use regex::Regex;
use serde::Serialize;
use tracing::{debug, info};

/// Repo-relative directory `gen-bindings` writes the unformatted sources to.
pub(crate) const SOURCES: &str = "target/bindings-surface";

/// Swift source, relative to [`SOURCES`].
const SWIFT_SOURCE: &str = "swift/capsule_sdk.swift";

/// Kotlin source, relative to [`SOURCES`].
const KOTLIN_SOURCE: &str = "kotlin/uniffi/capsule_sdk/capsule_sdk.kt";

/// Repo-relative path of the committed dump.
pub(crate) const DUMP: &str = "capsule-sdk/bindings-surface.json";

/// Dump format version. Bump it, and `BINDINGS_SCHEMA` in `capsule-docs/scripts/gen-reference.mjs`
/// with it, on any change a reader written against the previous one would misread.
const SCHEMA: u32 = 1;

/// The uniffi namespace dumped.
const NAMESPACE: &str = "capsule_sdk";

/// The declarations each native client consumes, by the name each language spells them with.
///
/// Carried over from the symbol checks `gen-bindings` made before this dump existed, and now
/// checked as declarations rather than as substrings: a `grep` for `FfiHardwareSignerError`
/// also matched `FfiConverterTypeFfiHardwareSignerError`, which Kotlin emits while naming the
/// type itself `FfiHardwareSignerException`.
///
/// - Client and session: the networked user flows (login, upload, status, sync).
/// - Workspace verbs: enroll (including hardware-signer parity), album, seal and import, the
///   seal-to-upload bridge, verify, sync-apply, escrow, and the device directory.
/// - Alerts: the shared predicate and the per-class pre-arm deadlines a client schedules its
///   alarms from. The arm call must not go missing: without it a client can only ask "what is
///   true now?", which is the question pre-arming exists to stop it having to ask.
/// - The hardware seams `capsule-core-kotlin` and `capsule-core-swift` implement and drive.
pub(crate) const REQUIRED: &[(&str, &str)] = &[
    ("FfiCapsuleClient", "FfiCapsuleClient"),
    ("FfiSession", "FfiSession"),
    ("FfiWorkspace", "FfiWorkspace"),
    ("FfiHardwareSigner", "FfiHardwareSigner"),
    ("FfiSyncEntry", "FfiSyncEntry"),
    ("FfiAssetFacts", "FfiAssetFacts"),
    ("FfiUploadBlob", "FfiUploadBlob"),
    ("FfiAlert", "FfiAlert"),
    ("FfiClassDeadline", "FfiClassDeadline"),
    ("FfiNotifyInput", "FfiNotifyInput"),
    ("evaluateAlerts", "evaluateAlerts"),
    ("preArmDeadlines", "preArmDeadlines"),
    ("nextAlertDeadline", "nextAlertDeadline"),
    ("sealAsset", "sealAsset"),
    ("uploadBlobs", "uploadBlobs"),
    ("applySyncEntry", "applySyncEntry"),
    ("escrowBlob", "escrowBlob"),
    ("signedDeviceDirectory", "signedDeviceDirectory"),
    ("publishDeviceDirectory", "publishDeviceDirectory"),
    ("escrowPut", "escrowPut"),
    ("escrowGet", "escrowGet"),
    (
        "createWithP256HardwareSigner",
        "createWithP256HardwareSigner",
    ),
    ("openWithP256HardwareSigner", "openWithP256HardwareSigner"),
    ("FfiHardwareKeyAgreement", "FfiHardwareKeyAgreement"),
    ("FfiHardwareSignerError", "FfiHardwareSignerException"),
    ("FfiDeviceTier", "FfiDeviceTier"),
    ("createWithHardwareSigner", "createWithHardwareSigner"),
    ("p256HardwareDekRoundTrip", "p256HardwareDekRoundTrip"),
    ("verifyAsset", "verifyAsset"),
];

/// The committed dump.
#[derive(Debug, Serialize)]
pub(crate) struct Dump {
    schema: u32,
    namespace: &'static str,
    uniffi: String,
    languages: Languages,
}

/// Both languages, Swift first: iOS is the first platform tier.
#[derive(Debug, Serialize)]
pub(crate) struct Languages {
    swift: Surface,
    kotlin: Surface,
}

/// One language's surface.
#[derive(Debug, Serialize)]
pub(crate) struct Surface {
    source: &'static str,
    declarations: Vec<Declaration>,
}

/// One declaration, in the shape `gen-reference.mjs` renders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Declaration {
    pub(crate) kind: String,
    pub(crate) name: String,
    pub(crate) signature: String,
    pub(crate) doc: String,
    pub(crate) members: Vec<Declaration>,
}

/// Write the dump, or in `check` mode verify the committed one is current.
pub(crate) fn run(root: &Path, check: bool) -> Result<()> {
    let read = |rel: &str| {
        let path = root.join(SOURCES).join(rel);
        fs::read_to_string(&path).with_context(|| {
            format!(
                "reading {}; run `mise run gen-bindings`, which generates it",
                path.display()
            )
        })
    };
    let swift = scan_swift(&read(SWIFT_SOURCE)?)?;
    let kotlin = scan_kotlin(&read(KOTLIN_SOURCE)?)?;
    assert_surface(&swift, &kotlin)?;
    debug!(
        swift = swift.len(),
        kotlin = kotlin.len(),
        "scanned the generated binding sources"
    );

    let dump = Dump {
        schema: SCHEMA,
        namespace: NAMESPACE,
        uniffi: uniffi_version(
            &fs::read_to_string(root.join("Cargo.lock")).context("reading Cargo.lock")?,
        )?,
        languages: Languages {
            swift: Surface {
                source: "capsule_sdk.swift",
                declarations: swift,
            },
            kotlin: Surface {
                source: "uniffi/capsule_sdk/capsule_sdk.kt",
                declarations: kotlin,
            },
        },
    };
    // Pretty-printed with a trailing newline: the dump is reviewed as a diff.
    let mut json = serde_json::to_string_pretty(&dump).context("serializing the dump")?;
    json.push('\n');

    let path = root.join(DUMP);
    if check {
        let committed = fs::read_to_string(&path)
            .with_context(|| format!("reading the committed dump {DUMP}"))?;
        if committed != json {
            bail!(
                "{DUMP} is out of sync with the generated capsule_sdk bindings; run `mise run \
                 gen-bindings` and commit the result"
            );
        }
        info!(dump = DUMP, "binding surface dump is up to date");
    } else {
        fs::write(&path, &json).with_context(|| format!("writing {}", path.display()))?;
        info!(dump = DUMP, "wrote the binding surface dump");
    }
    Ok(())
}

/// The `uniffi` version `Cargo.lock` resolves — the one whose templates the sources came from.
pub(crate) fn uniffi_version(lock: &str) -> Result<String> {
    static PACKAGE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?m)^name = "uniffi"\r?\nversion = "([^"]+)""#).expect("valid regex")
    });
    PACKAGE
        .captures(lock)
        .map(|captures| captures[1].to_string())
        .context("Cargo.lock resolves no `uniffi` package")
}

/// Every check the dump makes before it is written. See the module docs.
pub(crate) fn assert_surface(swift: &[Declaration], kotlin: &[Declaration]) -> Result<()> {
    if swift.is_empty() || kotlin.is_empty() {
        bail!(
            "a generated binding source yielded no declarations; the scan no longer matches uniffi's layout"
        );
    }
    let swift_names = all_names(swift);
    let kotlin_names = all_names(kotlin);
    for (swift_name, kotlin_name) in REQUIRED {
        if !swift_names.contains(*swift_name) {
            bail!("binding surface missing `{swift_name}` in the Swift source");
        }
        if !kotlin_names.contains(*kotlin_name) {
            bail!("binding surface missing `{kotlin_name}` in the Kotlin source");
        }
    }

    let spelled = |declarations: &[Declaration], kotlin: bool| -> BTreeSet<String> {
        declarations
            .iter()
            .flat_map(|declaration| {
                let name = if kotlin {
                    declaration.name.clone()
                } else {
                    kotlin_type_name(&declaration.name)
                };
                let methods = declaration
                    .members
                    .iter()
                    .filter(|member| is_callable(&member.kind))
                    // Swift's `init` is Kotlin's `constructor`: one primary constructor.
                    .map(|member| {
                        let member = if member.kind == "constructor" {
                            "init"
                        } else {
                            &member.name
                        };
                        format!("{name}.{member}")
                    })
                    .collect::<Vec<_>>();
                std::iter::once(name.clone()).chain(methods)
            })
            .collect()
    };
    let in_swift = spelled(swift, false);
    let in_kotlin = spelled(kotlin, true);
    let only_swift: Vec<_> = in_swift.difference(&in_kotlin).collect();
    let only_kotlin: Vec<_> = in_kotlin.difference(&in_swift).collect();
    if !only_swift.is_empty() || !only_kotlin.is_empty() {
        bail!(
            "the Swift and Kotlin surfaces disagree; only in Swift: {only_swift:?}; only in \
             Kotlin: {only_kotlin:?}. Both are generated from one cdylib, so this is a scan that \
             no longer matches uniffi's layout"
        );
    }
    Ok(())
}

/// How Kotlin spells a Swift top-level name: uniffi renames a Rust error type's `Error` suffix.
fn kotlin_type_name(swift: &str) -> String {
    swift
        .strip_suffix("Error")
        .filter(|base| base.starts_with("Ffi"))
        .map_or_else(|| swift.to_string(), |base| format!("{base}Exception"))
}

/// Every declaration and member name in `declarations`.
fn all_names(declarations: &[Declaration]) -> BTreeSet<&str> {
    declarations
        .iter()
        .flat_map(|declaration| {
            std::iter::once(declaration.name.as_str()).chain(
                declaration
                    .members
                    .iter()
                    .map(|member| member.name.as_str()),
            )
        })
        .collect()
}

/// Whether a member kind is something a caller calls, rather than a field or a case.
pub(crate) fn is_callable(kind: &str) -> bool {
    matches!(
        kind,
        "func" | "static func" | "init" | "fun" | "constructor"
    )
}

/// One source line with the brace depth it starts at.
#[derive(Debug)]
pub(crate) struct Line<'a> {
    pub(crate) text: &'a str,
    /// Brace depth before the line's first character.
    pub(crate) depth: usize,
    /// Whether the line starts inside a block comment.
    pub(crate) in_comment: bool,
}

/// Lex `source` into lines carrying their starting brace depth.
///
/// Comments (`//`, nested `/* */`), string literals (`"…"` with escapes, `"""…"""`) and
/// character literals (`'x'`, `'\x'`) are skipped. A brace that closes more than was opened,
/// or a file that ends with a block open, is fatal: every depth after it would be wrong.
pub(crate) fn lex(source: &str) -> Result<Vec<Line<'_>>> {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Code,
        Block(usize),
        Str,
        RawStr,
    }
    let mut state = State::Code;
    let mut depth: usize = 0;
    let mut lines = Vec::new();
    for (number, text) in source.lines().enumerate() {
        lines.push(Line {
            text,
            depth,
            in_comment: matches!(state, State::Block(_)),
        });
        let chars: Vec<char> = text.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let rest = &chars[i..];
            match state {
                State::Code => {
                    if rest.starts_with(&['/', '/']) {
                        break;
                    } else if rest.starts_with(&['/', '*']) {
                        state = State::Block(1);
                        i += 2;
                        continue;
                    } else if rest.starts_with(&['"', '"', '"']) {
                        state = State::RawStr;
                        i += 3;
                        continue;
                    } else if rest[0] == '"' {
                        state = State::Str;
                    } else if rest[0] == '\'' {
                        // 'x' or '\x': skip the literal whole.
                        let len = if rest.get(1) == Some(&'\\') { 4 } else { 3 };
                        if rest.get(len - 1) == Some(&'\'') {
                            i += len;
                            continue;
                        }
                    } else if rest[0] == '{' {
                        depth += 1;
                    } else if rest[0] == '}' {
                        depth = depth.checked_sub(1).with_context(|| {
                            format!(
                                "line {}: a `}}` closes a block that was never opened",
                                number + 1
                            )
                        })?;
                    }
                }
                State::Block(nesting) => {
                    if rest.starts_with(&['*', '/']) {
                        state = if nesting == 1 {
                            State::Code
                        } else {
                            State::Block(nesting - 1)
                        };
                        i += 2;
                        continue;
                    } else if rest.starts_with(&['/', '*']) {
                        state = State::Block(nesting + 1);
                        i += 2;
                        continue;
                    }
                }
                State::Str => {
                    if rest[0] == '\\' {
                        i += 2;
                        continue;
                    } else if rest[0] == '"' {
                        state = State::Code;
                    }
                }
                State::RawStr => {
                    if rest.starts_with(&['"', '"', '"']) {
                        state = State::Code;
                        i += 3;
                        continue;
                    }
                }
            }
            i += 1;
        }
        // A single-quoted string never spans lines in either language.
        if state == State::Str {
            state = State::Code;
        }
    }
    if depth != 0 || state != State::Code {
        bail!("the source ends inside a block (depth {depth}); its braces do not balance");
    }
    Ok(lines)
}

/// The `/** … */` block directly above `lines[at]`, as Markdown, or `""`.
///
/// Blank lines, compiler directives (`#if`) and attributes (`@…`) between the two are stepped
/// over: uniffi's Swift enum template leaves a blank line between an enum's doc and its
/// declaration.
pub(crate) fn doc_above(lines: &[Line<'_>], at: usize) -> String {
    let mut end = at;
    while end > 0 {
        let above = lines[end - 1].text.trim();
        if above.is_empty() || above.starts_with('#') || above.starts_with('@') {
            end -= 1;
        } else {
            break;
        }
    }
    if end == 0 || !lines[end - 1].text.trim_end().ends_with("*/") {
        return String::new();
    }
    let mut start = end - 1;
    while start > 0 && !lines[start].text.trim_start().starts_with("/**") {
        start -= 1;
    }
    let Some(first) = lines[start].text.trim_start().strip_prefix("/**") else {
        return String::new();
    };
    let mut body: Vec<&str> = Vec::new();
    if start == end - 1 {
        body.push(first.trim_end().trim_end_matches("*/").trim());
    } else {
        if !first.trim().is_empty() {
            body.push(first.trim());
        }
        for line in &lines[start + 1..end - 1] {
            let text = line.text.trim_start();
            let text = text.strip_prefix('*').unwrap_or(text);
            body.push(text.strip_prefix(' ').unwrap_or(text).trim_end());
        }
    }
    body.join("\n").trim().to_string()
}

/// A declaration line tidied for display: no trailing `{`/`(`/`:`, Swift's `)async` and
/// `)throws` spaced, runs of spaces collapsed.
pub(crate) fn tidy_signature(line: &str) -> String {
    static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("valid regex"));
    static GLUED: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\)(async|throws)").expect("valid regex"));
    let text = GLUED.replace_all(line, ") $1");
    let text = SPACES.replace_all(text.trim(), " ");
    text.trim_end_matches(|c: char| c == '{' || c == '(' || c == ':' || c.is_whitespace())
        .to_string()
}

/// The lines of the block a declaration at `lines[at]` opens, as indices, or `None` when it
/// opens none (a declaration ending in `;`, or a Kotlin record whose body is in parentheses).
fn block(lines: &[Line<'_>], at: usize) -> Option<std::ops::Range<usize>> {
    let depth = lines[at].depth;
    let mut start = at + 1;
    if lines
        .get(start)
        .is_some_and(|line| line.depth == depth && line.text.trim() == "{")
    {
        start += 1;
    }
    if lines.get(start).is_none_or(|line| line.depth <= depth) {
        return None;
    }
    let end = (start..lines.len())
        .find(|&i| lines[i].depth <= depth)
        .unwrap_or(lines.len());
    Some(start..end)
}

/// A member declared on `lines[at]`.
fn member(lines: &[Line<'_>], at: usize, kind: &str, name: &str) -> Declaration {
    Declaration {
        kind: kind.to_string(),
        name: name.to_string(),
        signature: tidy_signature(lines[at].text),
        doc: doc_above(lines, at),
        members: Vec::new(),
    }
}

/// The depth-`depth` lines of `range` matching `pattern`, as members of `kind` named by the
/// pattern's first capture.
fn members_matching(
    lines: &[Line<'_>],
    range: std::ops::Range<usize>,
    depth: usize,
    pattern: &Regex,
    kind: &str,
) -> Vec<Declaration> {
    range
        .filter(|&i| lines[i].depth == depth && !lines[i].in_comment)
        .filter_map(|i| {
            pattern
                .captures(lines[i].text)
                .map(|c| member(lines, i, kind, &c[1]))
        })
        .collect()
}

/// The public top-level types uniffi 0.31's own runtime declares, by name, in either language.
///
/// Listed by name rather than matched by pattern so that a uniffi bump which adds one fails
/// [`is_export`] loudly instead of either dropping it or publishing it.
const UNIFFI_RUNTIME: &[&str] = &[
    "RustBuffer",
    "InternalException",
    "Disposable",
    "UniffiRustCallStatusErrorHandler",
    "UniffiCleaner",
];

/// Whether the top-level type `name` is a `capsule_sdk` export (`Ok(true)`) or uniffi's own
/// runtime (`Ok(false)`): an `FfiConverter*`, an `Ffi…Impl` beside the declared `Ffi…` it
/// implements, or a name in [`UNIFFI_RUNTIME`].
///
/// Anything else is an error. Every export carries the `Ffi` prefix only by convention, so a
/// type that is neither would otherwise be dropped from both languages alike, which the parity
/// check cannot notice and which leaves a page that looks complete.
fn is_export(types: &[(usize, String, String)], name: &str) -> Result<bool> {
    if name.starts_with("FfiConverter")
        || name
            .strip_suffix("Impl")
            .is_some_and(|base| base.starts_with("Ffi") && find(types, base).is_some())
        || UNIFFI_RUNTIME.contains(&name)
    {
        return Ok(false);
    }
    if name.starts_with("Ffi") {
        return Ok(true);
    }
    bail!(
        "top-level type `{name}` is neither a `capsule_sdk` export (named `Ffi…`) nor a known \
         uniffi runtime type; give the export the `Ffi` prefix, or, if uniffi declares it, add it \
         to `UNIFFI_RUNTIME` in xtask/src/bindings_surface.rs"
    )
}

/// Index of the depth-0 type declaration named `name` among `types`.
fn find<'a>(
    types: &'a [(usize, String, String)],
    name: &str,
) -> Option<&'a (usize, String, String)> {
    types.iter().find(|(_, _, declared)| declared == name)
}

/// The Swift surface.
pub(crate) fn scan_swift(source: &str) -> Result<Vec<Declaration>> {
    static TYPE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?:public |open )(protocol|class|struct|enum) (\w+)").expect("valid regex")
    });
    static FREE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^public func (\w+)\(").expect("valid regex"));
    static REQUIREMENT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\s*func (\w+)\(").expect("valid regex"));
    static STATIC: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\s*public static func `?(\w+)`?\(").expect("valid regex"));
    // Any parameter list, an empty one included: a zero-argument `#[uniffi::constructor]` is
    // `public convenience init()`. The plumbing inits are told apart by `convenience` below.
    static INIT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\s*public (convenience )?init\(").expect("valid regex"));
    static FIELD: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\s*public var `?(\w+)`?:").expect("valid regex"));
    static CASE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\s*case `?(\w+)`?").expect("valid regex"));

    let lines = lex(source)?;
    let types: Vec<(usize, String, String)> = top_level(&lines, &TYPE);
    let mut declarations = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if line.depth != 0 || line.in_comment {
            continue;
        }
        if let Some(captures) = FREE.captures(line.text) {
            let name = &captures[1];
            if !name.starts_with("FfiConverter") && !name.starts_with("uniffi") {
                declarations.push(member(&lines, i, "func", name));
            }
            continue;
        }
        let Some(captures) = TYPE.captures(line.text) else {
            continue;
        };
        let (kind, name) = (&captures[1], &captures[2]);
        if !is_export(&types, name)? {
            continue;
        }
        let mut declaration = member(&lines, i, kind, name);
        let body = block(&lines, i);
        match kind {
            "protocol" => {
                // `XProtocol` is the method list of class `X`, read with it below.
                if name
                    .strip_suffix("Protocol")
                    .is_some_and(|base| find(&types, base).is_some())
                {
                    continue;
                }
                declaration.members = body
                    .map(|range| members_matching(&lines, range, 1, &REQUIREMENT, "func"))
                    .unwrap_or_default();
            }
            "class" => {
                if let Some(range) = body {
                    declaration.members.extend(
                        range
                            .clone()
                            .filter(|&j| lines[j].depth == 1 && !lines[j].in_comment)
                            .filter_map(|j| {
                                let text = lines[j].text;
                                if let Some(c) = INIT.captures(text) {
                                    // `init(unsafeFromHandle:)` and `init(noHandle:)` are
                                    // uniffi's plumbing; the convenience init is the caller's.
                                    c.get(1).map(|_| member(&lines, j, "init", "init"))
                                } else {
                                    STATIC
                                        .captures(text)
                                        .map(|c| member(&lines, j, "static func", &c[1]))
                                }
                            }),
                    );
                }
                if let Some((at, _, _)) = find(&types, &format!("{name}Protocol"))
                    && let Some(range) = block(&lines, *at)
                {
                    declaration.members.extend(members_matching(
                        &lines,
                        range,
                        1,
                        &REQUIREMENT,
                        "func",
                    ));
                }
            }
            "struct" => {
                declaration.members = body
                    .map(|range| members_matching(&lines, range, 1, &FIELD, "field"))
                    .unwrap_or_default();
            }
            _ => {
                declaration.members = body
                    .map(|range| members_matching(&lines, range, 1, &CASE, "case"))
                    .unwrap_or_default();
            }
        }
        declarations.push(declaration);
    }
    Ok(declarations)
}

/// The Kotlin surface.
pub(crate) fn scan_kotlin(source: &str) -> Result<Vec<Declaration>> {
    static TYPE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?:public |open )?(interface|class|data class|enum class|sealed class) (\w+)")
            .expect("valid regex")
    });
    static FUN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\s*(?:@Throws\([^)]*\)\s*)?(?:suspend\s+)?fun `(\w+)`\(")
            .expect("valid regex")
    });
    // A first parameter uniffi backtick-quoted, or an empty list delegating to `this(…)`: a
    // zero-argument `#[uniffi::constructor]` is `constructor() :`. The plumbing constructors
    // (`withHandle:`, `noHandle:`) have unquoted parameters and open a block instead.
    static CONSTRUCTOR: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\s*constructor\((?:`|\)\s*:)").expect("valid regex"));
    static FIELD: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\s*(?:val|var) `(\w+)`:").expect("valid regex"));
    static ENTRY: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\s*([A-Z][A-Z0-9_]*)\s*[,;]?\s*$").expect("valid regex"));
    static VARIANT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\s*(?:data class|class|object) (\w+)").expect("valid regex")
    });

    let lines = lex(source)?;
    let types = top_level(&lines, &TYPE);
    let mut declarations = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if line.depth != 0 || line.in_comment {
            continue;
        }
        if let Some(captures) = FUN.captures(line.text) {
            declarations.push(member(&lines, i, "fun", &captures[1]));
            continue;
        }
        let Some(captures) = TYPE.captures(line.text) else {
            continue;
        };
        let (kind, name) = (&captures[1], &captures[2]);
        if !is_export(&types, name)? {
            continue;
        }
        let mut declaration = member(&lines, i, kind, name);
        let body = block(&lines, i);
        match kind {
            "interface" => {
                if name
                    .strip_suffix("Interface")
                    .is_some_and(|base| find(&types, base).is_some())
                {
                    continue;
                }
                declaration.members = body
                    .map(|range| members_matching(&lines, range, 1, &FUN, "fun"))
                    .unwrap_or_default();
            }
            "class" => {
                if let Some(range) = body {
                    for j in range.clone() {
                        if lines[j].depth == 1 && CONSTRUCTOR.is_match(lines[j].text) {
                            declaration.members.push(member(
                                &lines,
                                j,
                                "constructor",
                                "constructor",
                            ));
                        }
                    }
                    // Companion-object functions are the class's named constructors.
                    for j in range.clone() {
                        if lines[j].depth == 1
                            && lines[j].text.trim().starts_with("companion object")
                            && let Some(inner) = block(&lines, j)
                        {
                            declaration
                                .members
                                .extend(members_matching(&lines, inner, 2, &FUN, "fun"));
                        }
                    }
                }
                if let Some((at, _, _)) = find(&types, &format!("{name}Interface"))
                    && let Some(range) = block(&lines, *at)
                {
                    declaration
                        .members
                        .extend(members_matching(&lines, range, 1, &FUN, "fun"));
                }
            }
            "data class" => {
                // The fields are the primary constructor's parameters, in parentheses at
                // depth 0 up to the line that closes them.
                let end = (i + 1..lines.len())
                    .find(|&j| lines[j].depth == 0 && lines[j].text.trim_start().starts_with(')'))
                    .unwrap_or(i + 1);
                declaration.members = members_matching(&lines, i + 1..end, 0, &FIELD, "field");
            }
            "enum class" => {
                declaration.members = body
                    .map(|range| members_matching(&lines, range, 1, &ENTRY, "entry"))
                    .unwrap_or_default();
            }
            _ => {
                declaration.members = body
                    .map(|range| members_matching(&lines, range, 1, &VARIANT, "variant"))
                    .unwrap_or_default();
            }
        }
        declarations.push(declaration);
    }
    // uniffi backtick-quotes every generated Kotlin identifier, keyword or not; on a page that
    // is noise in every signature. Swift quotes only keywords, so its backticks are kept.
    fn unquote(declarations: &mut [Declaration]) {
        for declaration in declarations {
            declaration.signature = declaration.signature.replace('`', "");
            unquote(&mut declaration.members);
        }
    }
    unquote(&mut declarations);
    Ok(declarations)
}

/// Every depth-0 type declaration: `(line index, kind, name)`.
fn top_level(lines: &[Line<'_>], pattern: &Regex) -> Vec<(usize, String, String)> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.depth == 0 && !line.in_comment)
        .filter_map(|(i, line)| {
            pattern
                .captures(line.text)
                .map(|c| (i, c[1].to_string(), c[2].to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests;
