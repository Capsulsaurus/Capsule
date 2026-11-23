//! The hand-written platform adapters beside the generated bindings.
//!
//! `capsule-core-kotlin` and `capsule-core-swift` implement the `capsule_sdk` callback
//! interfaces (`FfiHardwareSigner`, `FfiHardwareKeyAgreement`) over each platform's key store:
//! StrongBox, the Secure Enclave, and a software reference for hosts with neither. The apps
//! construct them directly, so they are part of the surface a native client compiles against,
//! and the SDK pages document them after the generated declarations.
//!
//! Unlike the generated sources, these are formatted by ktlint and swift-format, so the scan
//! reads that layout: a type's header may span lines (a Kotlin primary constructor, a wrapped
//! supertype list), as may a member's parameter list or a wrapped expression body, and an
//! attribute may share the declaration's line.
//!
//! ## What is kept, and what is refused
//!
//! Only what a caller can reach is kept: in Kotlin, anything not `private`, `internal` or
//! `protected` (the scan reads only final classes, so `protected` reaches no caller); in Swift,
//! anything `public` or `open`. Every line at a declaration depth is classified by its keyword,
//! and the scan **fails closed**: a public declaration of a form it does not read (a Kotlin
//! `object`, `enum class`, or `open` or `abstract` class, a Swift `enum`, `protocol` or `extension` with public members, a nested type,
//! a line it cannot classify, a header whose parentheses never balance) is fatal, for the
//! reason [`super::is_export`] gives: a page that silently lost one still looks complete.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use eyre::{Context, ContextCompat, Result, bail};
use regex::Regex;
use tracing::debug;

use super::{Declaration, Line, block, doc_above, lex, tidy_signature};

/// Repo-relative directory of the Kotlin adapters.
pub(crate) const KOTLIN_DIR: &str =
    "capsule-core-kotlin/src/main/kotlin/com/justin13888/capsule/hardware";

/// Repo-relative directory of the Swift adapters.
pub(crate) const SWIFT_DIR: &str = "capsule-core-swift/Sources/CapsuleHardware";

/// The longest header (declaration line through the line its body opens on) the scan accepts.
/// A longer one is an unbalanced parenthesis the scan misread, not a real signature.
const MAX_HEADER_LINES: usize = 40;

/// The language an adapter source is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Language {
    Kotlin,
    Swift,
}

impl Language {
    fn extension(self) -> &'static str {
        match self {
            Self::Kotlin => "kt",
            Self::Swift => "swift",
        }
    }

    /// The keywords that open a declaration. The words before one are its modifiers.
    fn keywords(self) -> &'static [&'static str] {
        match self {
            Self::Kotlin => &[
                "class",
                "interface",
                "object",
                "fun",
                "val",
                "var",
                "typealias",
                "constructor",
                "init",
            ],
            Self::Swift => &[
                "class",
                "struct",
                "enum",
                "protocol",
                "extension",
                "actor",
                "func",
                "init",
                "deinit",
                "var",
                "let",
                "case",
                "subscript",
                "typealias",
                "associatedtype",
            ],
        }
    }
}

/// Every adapter declared in the `.kt` or `.swift` files under `root/dir`, subdirectories
/// included, in path order. A tree that declares none is fatal: the scan no longer matches it.
pub(crate) fn scan_dir(root: &Path, dir: &str, language: Language) -> Result<Vec<Declaration>> {
    let mut files = Vec::new();
    collect_files(&root.join(dir), language, &mut files)?;
    files.sort();
    let mut declarations = Vec::new();
    for file in &files {
        let source =
            fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
        let found =
            scan(&source, language).with_context(|| format!("scanning {}", file.display()))?;
        debug!(file = %file.display(), declarations = found.len(), "scanned a platform adapter source");
        declarations.extend(found);
    }
    if declarations.is_empty() {
        bail!("{dir} declares no public adapter; the scan no longer matches its sources");
    }
    Ok(declarations)
}

/// Push every source file of `language` under `dir` onto `files`.
fn collect_files(dir: &Path, language: Language, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("listing {}", dir.display()))? {
        let path = entry
            .with_context(|| format!("listing {}", dir.display()))?
            .path();
        if path.is_dir() {
            collect_files(&path, language, files)?;
        } else if path
            .extension()
            .is_some_and(|ext| ext == language.extension())
        {
            files.push(path);
        }
    }
    Ok(())
}

/// One declaration line, classified: its modifiers, its keyword, and what follows.
#[derive(Debug)]
struct Classified<'a> {
    modifiers: Vec<&'a str>,
    keyword: &'a str,
    /// The declared name (`""` for an `init`).
    name: String,
}

impl Classified<'_> {
    /// Whether a caller outside the module can reach the declaration.
    fn public(&self, language: Language) -> bool {
        match language {
            Language::Kotlin => !self
                .modifiers
                .iter()
                .any(|m| matches!(*m, "private" | "internal" | "protected")),
            Language::Swift => self
                .modifiers
                .iter()
                .any(|m| matches!(*m, "public" | "open")),
        }
    }

    fn has(&self, modifier: &str) -> bool {
        self.modifiers.contains(&modifier)
    }
}

/// `text` with its leading attributes (`@Throws(E::class)`, `@MainActor`, `@objc(name)`)
/// removed.
fn strip_attributes(text: &str) -> &str {
    let mut rest = text.trim_start();
    while let Some(after) = rest.strip_prefix('@') {
        let name_end = after
            .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.' || c == ':'))
            .unwrap_or(after.len());
        let mut tail = &after[name_end..];
        if tail.starts_with('(') {
            let Some(close) = matching(tail, '(', ')') else {
                return rest;
            };
            tail = &tail[close + 1..];
        }
        rest = tail.trim_start();
    }
    rest
}

/// Classify `text`: `None` when it holds no declaration keyword (a continuation line, or code
/// the scan does not read).
fn classify(text: &str, language: Language) -> Option<Classified<'_>> {
    let text = strip_attributes(text);
    let words: Vec<&str> = text.split_whitespace().collect();
    let keywords = language.keywords();
    let at = words.iter().enumerate().position(|(i, word)| {
        let word = word.split(['(', '<', ':', '{']).next().unwrap_or(word);
        keywords.contains(&word)
            // Swift's `class func`/`class var` is a modifier, not a nested class.
            && !(language == Language::Swift
                && word == "class"
                && words.get(i + 1).is_some_and(|next| matches!(*next, "func" | "var")))
    })?;
    let modifiers = words[..at].to_vec();
    // Modifiers are lowercase words (`public`, `private(set)`); anything else means the
    // keyword was found inside an expression, not opening a declaration.
    if !modifiers
        .iter()
        .all(|m| m.chars().next().is_some_and(|c| c.is_ascii_lowercase()))
    {
        return None;
    }
    let raw = words[at];
    let keyword = keywords
        .iter()
        .find(|k| raw.starts_with(**k))
        .copied()
        .unwrap_or(raw);
    let name = declared_name(&words[at..].join(" ")[keyword.len()..]);
    Some(Classified {
        modifiers,
        keyword,
        name,
    })
}

/// The name declared by `after`, the text following a declaration keyword: past a generic
/// parameter list (`fun <T> wrap(…)`) and a receiver (`fun String.shout()`), backticks
/// removed.
fn declared_name(after: &str) -> String {
    let mut rest = after.trim_start();
    if rest.starts_with('<')
        && let Some(close) = matching(rest, '<', '>')
    {
        rest = rest[close + 1..].trim_start();
    }
    let path: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '`'))
        .collect();
    path.rsplit('.')
        .next()
        .unwrap_or_default()
        .trim_matches('`')
        .to_string()
}

/// A line that declares nothing: blank, a comment, an import, a closing brace, a compiler
/// directive, or attributes alone (the declaration they apply to is on a later line).
fn is_trivia(text: &str) -> bool {
    let text = text.trim();
    text.is_empty()
        || text == "}"
        || text.starts_with("//")
        || text.starts_with("/*")
        || text.starts_with('*')
        || text.starts_with('#')
        || text.starts_with("package ")
        || text.starts_with("import ")
        || strip_attributes(text).is_empty()
}

/// The public types one adapter source declares, with their public members.
pub(crate) fn scan(source: &str, language: Language) -> Result<Vec<Declaration>> {
    let lines = lex(source)?;
    let mut declarations = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = &lines[i];
        if line.depth != 0 || line.in_comment || is_trivia(line.text) {
            i += 1;
            continue;
        }
        let Some(declared) = classify(line.text, language) else {
            bail!(
                "line {}: unrecognized top-level line `{}`",
                i + 1,
                line.text.trim()
            );
        };
        let end = header_end(&lines, i, language)?;
        let body = block(&lines, end);
        let next = body.as_ref().map_or(end + 1, |range| range.end);
        let readable = match language {
            Language::Kotlin => {
                declared.keyword == "class"
                    && declared
                        .modifiers
                        .iter()
                        .all(|m| KOTLIN_CLASS_MODIFIERS.contains(m))
            }
            Language::Swift => matches!(declared.keyword, "class" | "struct"),
        };
        if declared.keyword == "extension" {
            // Members an extension adds are the extended type's surface, which the scan does
            // not merge; a public one is refused rather than dropped.
            refuse_public_members(&lines, body.clone(), i, &declared, language)?;
        } else if declared.public(language) {
            if !readable {
                bail!(
                    "line {}: public `{}` `{}` is a declaration the platform adapter scan does \
                     not read; teach xtask/src/bindings_surface/platform.rs to read it",
                    i + 1,
                    declared.keyword,
                    declared.name
                );
            }
            declarations.push(read_type(&lines, i, end, body, &declared, language)?);
        }
        i = next;
    }
    Ok(declarations)
}

/// Kotlin class modifiers the scan reads. `enum`, `sealed`, `annotation`, `inner`, `value`
/// and `inline` classes have a surface it does not. `open` and `abstract` classes are refused
/// too: a subclass reaches their `protected` members, which the scan drops as unreachable on
/// the grounds that every adapter it reads is final.
const KOTLIN_CLASS_MODIFIERS: &[&str] = &["public", "final", "data"];

/// Fail if the extension body `body` (opened at `lines[at]`) declares anything public at its
/// first depth, its own or by the extension's `public` default.
fn refuse_public_members(
    lines: &[Line<'_>],
    body: Option<std::ops::Range<usize>>,
    at: usize,
    extension: &Classified<'_>,
    language: Language,
) -> Result<()> {
    let Some(range) = body else { return Ok(()) };
    // In a `public extension`, a member with no access modifier of its own is public.
    let public_by_default = extension.public(language);
    let depth = lines[at].depth + 1;
    for j in range {
        let line = &lines[j];
        if line.depth != depth || line.in_comment || is_trivia(line.text) {
            continue;
        }
        if let Some(member) = classify(line.text, language)
            && (member.public(language)
                || public_by_default
                    && !member
                        .modifiers
                        .iter()
                        .any(|m| matches!(*m, "private" | "fileprivate" | "internal")))
        {
            bail!(
                "line {}: `{}` adds a public member the platform adapter scan does not read: `{}`",
                j + 1,
                lines[at].text.trim(),
                line.text.trim()
            );
        }
    }
    Ok(())
}

/// Index of the last line of the declaration header starting at `lines[at]`: the line its
/// body opens on, or the line it ends on. Fails on a header whose parentheses never balance
/// before another declaration starts or the enclosing block ends, rather than swallowing
/// what follows.
fn header_end(lines: &[Line<'_>], at: usize, language: Language) -> Result<usize> {
    let depth = lines[at].depth;
    let mut parens = 0i32;
    let mut end = at;
    loop {
        parens += paren_balance(lines[end].text);
        let next = lines.get(end + 1);
        let opens_body = next.is_some_and(|next| next.depth > depth);
        if parens <= 0 && (opens_body || !continues(lines, end)) {
            return Ok(end);
        }
        let unbalanced = || {
            eyre::eyre!(
                "line {}: the parentheses of `{}` never balance; the scan misread the header",
                at + 1,
                lines[at].text.trim()
            )
        };
        let Some(next) = next else {
            return Err(unbalanced());
        };
        if next.depth < depth || end + 1 - at >= MAX_HEADER_LINES {
            return Err(unbalanced());
        }
        if parens > 0
            && next.depth == depth
            && !is_trivia(next.text)
            && classify(next.text, language).is_some_and(|c| {
                // Never inside a parameter list; `val`/`var` are, as constructor properties.
                matches!(c.keyword, "fun" | "func" | "class" | "struct" | "init")
            })
        {
            // A new declaration while a parenthesis is still open: a `(` the counter saw
            // in something it does not lex (a character literal in a template, say).
            return Err(unbalanced());
        }
        end += 1;
    }
}

/// Whether the header on `lines[at]` continues onto the next line: an expression body ktlint
/// wraps after its `=`, a supertype list wrapped after a `,` or a trailing `:`, or one that
/// starts the next line with `:`.
fn continues(lines: &[Line<'_>], at: usize) -> bool {
    let text = lines[at].text.trim_end();
    text.ends_with('=')
        || text.ends_with(',')
        || text.ends_with(':')
        || lines
            .get(at + 1)
            .is_some_and(|next| next.text.trim_start().starts_with(':'))
}

/// `(` minus `)` on a line, outside string and character literals and `//` comments.
fn paren_balance(text: &str) -> i32 {
    let mut balance = 0;
    let mut in_string = false;
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            match c {
                '\\' => i += 1,
                '"' => in_string = false,
                _ => {}
            }
        } else {
            match c {
                '/' if chars.get(i + 1) == Some(&'/') => break,
                '"' => in_string = true,
                '\'' => {
                    // 'x' or '\x'
                    let len = if chars.get(i + 1) == Some(&'\\') {
                        4
                    } else {
                        3
                    };
                    if chars.get(i + len - 1) == Some(&'\'') {
                        i += len;
                        continue;
                    }
                }
                '(' => balance += 1,
                ')' => balance -= 1,
                _ => {}
            }
        }
        i += 1;
    }
    balance
}

/// Index of the `close` matching the `open` that `text` starts with, outside strings.
fn matching(text: &str, open: char, close: char) -> Option<usize> {
    let mut depth = 0;
    let mut in_string = false;
    for (i, c) in text.char_indices() {
        match c {
            '"' => in_string = !in_string,
            c if !in_string && c == open => depth += 1,
            c if !in_string && c == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The header lines `at..=end` as one display signature: attributes dropped, lines joined,
/// parameter lists tightened, any body or expression cut off.
fn joined_signature(lines: &[Line<'_>], at: usize, end: usize) -> String {
    static OPEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\(\s+").expect("valid regex"));
    static CLOSE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r",?\s+\)").expect("valid regex"));
    let joined = lines[at..=end]
        .iter()
        .map(|line| line.text.trim())
        .collect::<Vec<_>>()
        .join(" ");
    let joined = strip_attributes(&joined);
    let joined = CLOSE
        .replace_all(&OPEN.replace_all(joined, "("), ")")
        .into_owned();
    tidy_signature(cut_body(&joined))
}

/// `signature` up to its body: the `{` or the `=` of an expression body, whichever comes first
/// outside parentheses, generic brackets and strings. The `>` of an arrow (`->`) closes nothing.
fn cut_body(signature: &str) -> &str {
    let mut depth = 0i32;
    let mut in_string = false;
    let mut previous = ' ';
    for (i, c) in signature.char_indices() {
        match c {
            '"' => in_string = !in_string,
            '(' | '<' if !in_string => depth += 1,
            ')' if !in_string => depth -= 1,
            '>' if !in_string && previous != '-' => depth -= 1,
            '{' | '=' if !in_string && depth <= 0 => return signature[..i].trim_end(),
            _ => {}
        }
        previous = c;
    }
    signature
}

/// A Kotlin primary constructor, read off a class signature.
struct PrimaryConstructor {
    /// The signature with the constructor's property keywords and visibility removed.
    signature: String,
    /// `(seed: ByteArray)`.
    parameters: String,
    /// The constructor parameters that are also public properties, as `val name: Type`.
    properties: Vec<String>,
}

/// The primary constructor of the Kotlin class `name` declared by `signature`, or `None` when
/// it declares none. Fails on a constructor the scan does not read (`private constructor(…)`).
fn primary_constructor(signature: &str, name: &str) -> Result<Option<PrimaryConstructor>> {
    // The name as a whole word after `class`, not the `s` inside `class`.
    let Some(name_at) = Regex::new(&format!(r"\bclass\s+{}\b", regex::escape(name)))
        .ok()
        .and_then(|pattern| pattern.find(signature))
        .map(|found| found.end() - name.len())
    else {
        return Ok(None);
    };
    let mut start = name_at + name.len();
    if signature[start..].starts_with('<') {
        let close = matching(&signature[start..], '<', '>')
            .with_context(|| format!("`{signature}`: its generic parameters never close"))?;
        start += close + 1;
    }
    let rest = &signature[start..];
    // Anything between the name and `(`, `:` or `{` is a constructor modifier or annotation
    // (`private constructor`, `@Inject constructor`).
    let before = rest
        .split(['(', ':', '{'])
        .next()
        .unwrap_or_default()
        .trim();
    if !before.is_empty() {
        bail!(
            "`{signature}`: a primary constructor with modifiers or annotations is not read by \
             the platform adapter scan"
        );
    }
    if !rest.starts_with('(') {
        return Ok(None);
    }
    let close = matching(rest, '(', ')')
        .with_context(|| format!("`{signature}`: its constructor never closes"))?;
    let mut shown = Vec::new();
    let mut properties = Vec::new();
    for parameter in split_top_level(&rest[1..close]) {
        let words: Vec<&str> = parameter.split_whitespace().collect();
        let keyword = words.iter().position(|w| matches!(*w, "val" | "var"));
        let Some(keyword) = keyword else {
            shown.push(parameter.trim().to_string());
            continue;
        };
        let declaration = words[keyword + 1..].join(" ");
        shown.push(declaration.clone());
        let hidden = words[..keyword]
            .iter()
            .any(|m| matches!(*m, "private" | "internal" | "protected"));
        if !hidden {
            let property = declaration
                .split_once('=')
                .map_or(declaration.as_str(), |(head, _)| head)
                .trim();
            properties.push(format!("{} {property}", words[keyword]));
        }
    }
    let parameters = format!("({})", shown.join(", "));
    Ok(Some(PrimaryConstructor {
        signature: format!(
            "{}{}{}",
            &signature[..start],
            parameters,
            &rest[close + 1..]
        ),
        parameters,
        properties,
    }))
}

/// `text` split at its commas outside parentheses, brackets, braces, generics and strings.
fn split_top_level(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut previous = ' ';
    let mut start = 0;
    for (i, c) in text.char_indices() {
        match c {
            '"' => in_string = !in_string,
            '(' | '[' | '{' | '<' if !in_string => depth += 1,
            ')' | ']' | '}' if !in_string => depth -= 1,
            '>' if !in_string && previous != '-' => depth -= 1,
            ',' if !in_string && depth == 0 => {
                parts.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        previous = c;
    }
    if !text[start..].trim().is_empty() {
        parts.push(&text[start..]);
    }
    parts
}

/// The type whose header spans `lines[at..=end]`, with its public members.
fn read_type(
    lines: &[Line<'_>],
    at: usize,
    end: usize,
    body: Option<std::ops::Range<usize>>,
    declared: &Classified<'_>,
    language: Language,
) -> Result<Declaration> {
    let name = declared.name.clone();
    let mut signature = joined_signature(lines, at, end);
    let mut members = Vec::new();
    if language == Language::Kotlin
        && let Some(constructor) = primary_constructor(&signature, &name)?
    {
        signature = constructor.signature;
        members.push(Declaration {
            kind: "constructor".to_string(),
            name: name.clone(),
            signature: format!("constructor{}", constructor.parameters),
            doc: String::new(),
            members: Vec::new(),
        });
        for property in constructor.properties {
            let property_name = property
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .trim_end_matches(':')
                .to_string();
            members.push(Declaration {
                kind: "field".to_string(),
                name: property_name,
                signature: property,
                doc: String::new(),
                members: Vec::new(),
            });
        }
    }
    if let Some(range) = body {
        let depth = lines[at].depth + 1;
        let mut j = range.start;
        while j < range.end {
            let line = &lines[j];
            if line.depth != depth || line.in_comment || is_trivia(line.text) {
                j += 1;
                continue;
            }
            let Some(member) = classify(line.text, language) else {
                bail!(
                    "line {}: unrecognized line `{}` in `{name}`",
                    j + 1,
                    line.text.trim()
                );
            };
            let member_end = header_end(lines, j, language)?;
            let skip_to = block(lines, member_end).map_or(member_end + 1, |inner| inner.end);
            if matches!(member.keyword, "init" | "deinit") && language == Language::Kotlin
                || member.keyword == "deinit"
                || !member.public(language)
            {
                // A Kotlin `init { … }` block, a deinitializer, or a member no caller reaches.
                j = skip_to;
                continue;
            }
            let kind = member_kind(&member, language).with_context(|| {
                format!(
                    "line {}: public `{}` `{}` of `{name}` is a member the platform adapter \
                     scan does not read",
                    j + 1,
                    member.keyword,
                    member.name
                )
            })?;
            members.push(Declaration {
                kind: kind.to_string(),
                name: if kind == "init" {
                    "init".to_string()
                } else {
                    member.name.clone()
                },
                signature: joined_signature(lines, j, member_end),
                doc: doc_for(lines, j, language),
                members: Vec::new(),
            });
            j = skip_to;
        }
    }
    Ok(Declaration {
        kind: declared.keyword.to_string(),
        name,
        signature,
        doc: doc_for(lines, at, language),
        members,
    })
}

/// The kind `gen-reference.mjs` lists a member under, or `None` for one the scan does not read
/// (a nested type, a companion object, a subscript, an enum case).
fn member_kind(member: &Classified<'_>, language: Language) -> Option<&'static str> {
    match (language, member.keyword) {
        (Language::Kotlin, "fun") => Some("fun"),
        (Language::Kotlin, "val" | "var") | (Language::Swift, "var" | "let") => Some("field"),
        (Language::Kotlin, "constructor") => Some("constructor"),
        (Language::Swift, "func") if member.has("static") || member.has("class") => {
            Some("static func")
        }
        (Language::Swift, "func") => Some("func"),
        (Language::Swift, "init") => Some("init"),
        _ => None,
    }
}

/// The doc comment above `lines[at]`: KDoc's `/** … */`, or Swift's run of `///` lines.
fn doc_for(lines: &[Line<'_>], at: usize, language: Language) -> String {
    match language {
        Language::Kotlin => doc_above(lines, at),
        Language::Swift => line_doc_above(lines, at),
    }
}

/// The run of `///` lines directly above `lines[at]` (attribute lines stepped over), as
/// Markdown.
fn line_doc_above(lines: &[Line<'_>], at: usize) -> String {
    let mut start = at;
    while start > 0 && lines[start - 1].text.trim().starts_with('@') {
        start -= 1;
    }
    let mut body = Vec::new();
    while start > 0 {
        let Some(text) = lines[start - 1].text.trim().strip_prefix("///") else {
            break;
        };
        body.push(text.strip_prefix(' ').unwrap_or(text).trim_end());
        start -= 1;
    }
    body.reverse();
    body.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests;
