//! Enforces the unit-test layout: a module's tests live in a sibling file, never inline.
//!
//! `foo.rs` ends with `#[cfg(test)] mod tests;` and the tests sit in `foo/tests.rs` (a `mod.rs`
//! or crate root keeps its `tests.rs` beside it). An inline test module — `mod tests { … }`, or
//! any inline module gated on `cfg(test)` — is a violation. Only `src/` of each workspace member
//! is read: `tests/`, `benches/` and `examples/` hold whole test targets, not unit-test modules.
//!
//! The scan runs over a small Rust lexer rather than a text search, so a `mod tests {` inside a
//! comment, a string or a raw string is not a module, and a `'{'` char literal never unbalances
//! the braces that decide which inline module encloses another.

use std::fs;
use std::path::{Path, PathBuf};

use eyre::{ContextCompat, Result, WrapErr, bail};
use tracing::debug;

use crate::architecture::{cargo_metadata, walk_files};

pub(crate) fn run(root: &Path) -> Result<()> {
    let metadata = cargo_metadata(root)?;
    let members: Vec<&str> = metadata["workspace_members"]
        .as_array()
        .context("cargo metadata workspace_members missing")?
        .iter()
        .filter_map(|member| member.as_str())
        .collect();
    let mut source_dirs = Vec::new();
    for package in metadata["packages"]
        .as_array()
        .context("cargo metadata packages missing")?
    {
        let id = package["id"].as_str().context("package id missing")?;
        if !members.contains(&id) {
            continue;
        }
        let manifest = package["manifest_path"]
            .as_str()
            .context("package manifest_path missing")?;
        if let Some(dir) = Path::new(manifest).parent() {
            source_dirs.push(dir.join("src"));
        }
    }

    let violations = check_source_dirs(root, &source_dirs)?;
    if violations.is_empty() {
        println!("Unit tests live in sibling files");
        return Ok(());
    }
    bail!(
        "Inline test modules (move each body to its sibling file: `foo.rs` -> `foo/tests.rs`, \
         `mod.rs` or a crate root -> `tests.rs` beside it):\n- {}",
        violations.join("\n- ")
    )
}

/// Every violation under `source_dirs`, sorted, one line each. A tree that holds no Rust source
/// at all is itself a violation: a guard that finds nothing to read must not report clean.
fn check_source_dirs(root: &Path, source_dirs: &[PathBuf]) -> Result<Vec<String>> {
    let mut violations = Vec::new();
    let mut scanned = 0usize;
    for dir in source_dirs.iter().filter(|dir| dir.is_dir()) {
        walk_files(dir, &mut |path| {
            if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
                return Ok(());
            }
            scanned += 1;
            let relative = path.strip_prefix(root).unwrap_or(path).display();
            let source = fs::read_to_string(path).with_context(|| format!("reading {relative}"))?;
            match inline_test_modules(&source) {
                Ok(found) => violations.extend(found.into_iter().map(|module| {
                    format!(
                        "{relative}:{}: inline test module `{}`",
                        module.line, module.path
                    )
                })),
                Err(error) => violations.push(format!("{relative}: could not be scanned: {error}")),
            }
            Ok(())
        })?;
    }
    debug!(
        scanned,
        violations = violations.len(),
        "test layout scanned"
    );
    if scanned == 0 {
        violations.push(
            "the test-layout check found no Rust source under any workspace member's `src/`; \
             it checked nothing"
                .into(),
        );
    }
    violations.sort();
    Ok(violations)
}

/// An inline test module: where its `mod` keyword sits and its path from the file's own module.
#[derive(Debug, PartialEq, Eq)]
struct InlineTestModule {
    line: usize,
    path: String,
}

/// The tokens the scan needs: identifiers and punctuation. Comments, literals, lifetimes and
/// numbers are consumed by the lexer and never emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token<'a> {
    Ident(&'a str),
    Punct(char),
}

#[derive(Debug, Clone, Copy)]
struct Spanned<'a> {
    token: Token<'a>,
    offset: usize,
}

/// Every inline module in `source` that is named `tests` or gated on a `cfg` requiring `test`,
/// nested ones included, in source order.
fn inline_test_modules(source: &str) -> Result<Vec<InlineTestModule>, String> {
    let tokens = tokenize(source)?;
    let mut found = Vec::new();
    // The inline modules enclosing the cursor, each with the brace depth inside it.
    let mut open: Vec<(&str, usize)> = Vec::new();
    let mut depth = 0usize;
    for (index, spanned) in tokens.iter().enumerate() {
        match spanned.token {
            Token::Punct('{') => {
                depth += 1;
                if let Some(name) = inline_module_name(&tokens, index) {
                    open.push((name, depth));
                    let module_start = index - 2;
                    let gated = attributes_before(&tokens, item_start(&tokens, module_start))
                        .iter()
                        .any(|attribute| cfg_requires_test(attribute));
                    if name == "tests" || gated {
                        let path: Vec<&str> = open.iter().map(|(name, _)| *name).collect();
                        found.push(InlineTestModule {
                            line: line_of(source, tokens[module_start].offset),
                            path: path.join("::"),
                        });
                    }
                }
            }
            Token::Punct('}') => {
                if open.last().is_some_and(|(_, inner)| *inner == depth) {
                    open.pop();
                }
                depth = depth.checked_sub(1).ok_or_else(|| {
                    format!(
                        "unbalanced `}}` on line {}",
                        line_of(source, spanned.offset)
                    )
                })?;
            }
            _ => {}
        }
    }
    if depth != 0 {
        return Err("unbalanced `{`".into());
    }
    Ok(found)
}

/// The module name when the `{` at `brace` opens `mod NAME {`.
fn inline_module_name<'a>(tokens: &[Spanned<'a>], brace: usize) -> Option<&'a str> {
    let keyword = tokens.get(brace.checked_sub(2)?)?;
    let name = tokens.get(brace - 1)?;
    match (keyword.token, name.token) {
        (Token::Ident("mod"), Token::Ident(name)) => Some(name),
        _ => None,
    }
}

/// The first token of the item whose `mod` keyword is at `keyword`: the visibility, if any.
fn item_start(tokens: &[Spanned<'_>], keyword: usize) -> usize {
    let previous = |index: usize| index.checked_sub(1).map(|index| tokens[index].token);
    match previous(keyword) {
        Some(Token::Ident("pub")) => keyword - 1,
        Some(Token::Punct(')')) => {
            // `pub(crate)`, `pub(super)`, `pub(in path)`.
            let mut index = keyword - 1;
            while index > 0 && tokens[index].token != Token::Punct('(') {
                index -= 1;
            }
            if previous(index) == Some(Token::Ident("pub")) {
                index - 1
            } else {
                keyword
            }
        }
        _ => keyword,
    }
}

/// The outer attributes immediately before token `start`, each as the tokens inside `#[ … ]`.
fn attributes_before<'t, 'a>(tokens: &'t [Spanned<'a>], start: usize) -> Vec<&'t [Spanned<'a>]> {
    let mut attributes = Vec::new();
    let mut end = start;
    while end >= 1 && tokens[end - 1].token == Token::Punct(']') {
        let mut depth = 0usize;
        let mut open = None;
        for index in (0..end).rev() {
            match tokens[index].token {
                Token::Punct(']') => depth += 1,
                Token::Punct('[') => {
                    depth -= 1;
                    if depth == 0 {
                        open = Some(index);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(open) = open else { break };
        if open == 0 || tokens[open - 1].token != Token::Punct('#') {
            break;
        }
        attributes.push(&tokens[open + 1..end - 1]);
        end = open - 1;
    }
    attributes
}

/// Whether an attribute's tokens are `cfg(PREDICATE)` with a predicate that holds only under
/// `test`: `test` itself, or an `all(…)` with such an argument. `any(test, …)` and `not(test)`
/// also compile outside tests, so they do not count.
fn cfg_requires_test(attribute: &[Spanned<'_>]) -> bool {
    match attribute {
        [cfg, open, predicate @ .., close]
            if cfg.token == Token::Ident("cfg")
                && open.token == Token::Punct('(')
                && close.token == Token::Punct(')') =>
        {
            predicate_requires_test(predicate)
        }
        _ => false,
    }
}

fn predicate_requires_test(predicate: &[Spanned<'_>]) -> bool {
    match predicate {
        [single] => single.token == Token::Ident("test"),
        [all, open, arguments @ .., close]
            if all.token == Token::Ident("all")
                && open.token == Token::Punct('(')
                && close.token == Token::Punct(')') =>
        {
            split_arguments(arguments)
                .into_iter()
                .any(predicate_requires_test)
        }
        _ => false,
    }
}

/// The top-level comma-separated arguments of a predicate list.
fn split_arguments<'t, 'a>(tokens: &'t [Spanned<'a>]) -> Vec<&'t [Spanned<'a>]> {
    let mut arguments = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (index, spanned) in tokens.iter().enumerate() {
        match spanned.token {
            Token::Punct('(') => depth += 1,
            Token::Punct(')') => depth = depth.saturating_sub(1),
            Token::Punct(',') if depth == 0 => {
                arguments.push(&tokens[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    if start < tokens.len() {
        arguments.push(&tokens[start..]);
    }
    arguments
}

fn line_of(source: &str, offset: usize) -> usize {
    source[..offset].matches('\n').count() + 1
}

fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte >= 0x80
}

fn is_ident_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 0x80
}

/// Lex `source` into identifiers and punctuation, skipping comments (nested block comments
/// included), string, byte-string, C-string and raw-string literals, char and byte literals,
/// lifetimes and numbers.
fn tokenize(source: &str) -> Result<Vec<Spanned<'_>>, String> {
    let bytes = source.as_bytes();
    let unterminated = |what: &str, at: usize| {
        format!(
            "unterminated {what} starting on line {}",
            line_of(source, at)
        )
    };
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        let rest = &bytes[index..];
        if byte.is_ascii_whitespace() {
            index += 1;
        } else if rest.starts_with(b"//") {
            index = source[index..]
                .find('\n')
                .map_or(bytes.len(), |newline| index + newline);
        } else if rest.starts_with(b"/*") {
            let mut depth = 0usize;
            let mut cursor = index;
            loop {
                let here = &bytes[cursor..];
                if here.is_empty() {
                    return Err(unterminated("block comment", index));
                } else if here.starts_with(b"/*") {
                    depth += 1;
                    cursor += 2;
                } else if here.starts_with(b"*/") {
                    depth -= 1;
                    cursor += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    cursor += 1;
                }
            }
            index = cursor;
        } else if let Some(end) = raw_string_end(source, index) {
            index = end.ok_or_else(|| unterminated("raw string", index))?;
        } else if byte == b'"'
            || (matches!(byte, b'b' | b'c') && bytes.get(index + 1) == Some(&b'"'))
        {
            let mut cursor = index + if byte == b'"' { 1 } else { 2 };
            loop {
                match bytes.get(cursor) {
                    None => return Err(unterminated("string", index)),
                    Some(b'\\') => cursor += 2,
                    Some(b'"') => break,
                    Some(_) => cursor += 1,
                }
            }
            index = cursor + 1;
        } else if byte == b'\'' || (byte == b'b' && bytes.get(index + 1) == Some(&b'\'')) {
            let quote = if byte == b'b' { index + 1 } else { index };
            index = char_or_lifetime_end(source, quote)
                .ok_or_else(|| unterminated("char literal", index))?;
        } else if is_ident_start(byte) {
            let start = index;
            while index < bytes.len() && is_ident_continue(bytes[index]) {
                index += 1;
            }
            // `r#ident` is one identifier.
            if &source[start..index] == "r"
                && bytes.get(index) == Some(&b'#')
                && bytes
                    .get(index + 1)
                    .is_some_and(|&next| is_ident_start(next))
            {
                index += 1;
                while index < bytes.len() && is_ident_continue(bytes[index]) {
                    index += 1;
                }
            }
            tokens.push(Spanned {
                token: Token::Ident(&source[start..index]),
                offset: start,
            });
        } else if byte.is_ascii_digit() {
            while index < bytes.len() && is_ident_continue(bytes[index]) {
                index += 1;
            }
        } else {
            tokens.push(Spanned {
                token: Token::Punct(char::from(byte)),
                offset: index,
            });
            index += 1;
        }
    }
    Ok(tokens)
}

/// `Some(Some(end))` when a raw string (`r"…"`, `r#"…"#`, `br…`, `cr…`) starts at `index`,
/// `Some(None)` when it starts there but never closes, `None` when none starts there.
fn raw_string_end(source: &str, index: usize) -> Option<Option<usize>> {
    let bytes = source.as_bytes();
    let mut cursor = index;
    if matches!(bytes[cursor], b'b' | b'c') {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'r') {
        return None;
    }
    cursor += 1;
    let hashes = bytes[cursor..]
        .iter()
        .take_while(|&&byte| byte == b'#')
        .count();
    cursor += hashes;
    if bytes.get(cursor) != Some(&b'"') {
        return None;
    }
    let closing = format!("\"{}", "#".repeat(hashes));
    Some(
        source[cursor + 1..]
            .find(&closing)
            .map(|found| cursor + 1 + found + closing.len()),
    )
}

/// The end of the char literal or lifetime whose `'` is at `quote`, or `None` for an
/// unterminated char literal.
fn char_or_lifetime_end(source: &str, quote: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    if bytes.get(quote + 1) == Some(&b'\\') {
        // An escape: skip the escaped character, then find the closing quote.
        return source
            .get(quote + 3..)?
            .find('\'')
            .map(|found| quote + 3 + found + 1);
    }
    let character = source[quote + 1..].chars().next()?;
    let after = quote + 1 + character.len_utf8();
    if bytes.get(after) == Some(&b'\'') {
        return Some(after + 1);
    }
    // A lifetime or label: `'a`, `'static`.
    let mut end = quote + 1;
    while end < bytes.len() && is_ident_continue(bytes[end]) {
        end += 1;
    }
    Some(end)
}

#[cfg(test)]
mod tests;
