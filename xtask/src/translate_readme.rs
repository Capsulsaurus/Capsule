//! `xtask translate-readme`: generate the translated `README.<lang>.md` files from
//! the repo-root `README.md`, and (`--check`) the CI drift gate that fails when a
//! source block changed but its translations were not regenerated.
//!
//! # How it works
//!
//! The pipeline is **structure-aware**, not whole-document:
//!
//! 1. [`segment`] splits `README.md` into a tiling sequence of [`Segment`]s (headings,
//!    paragraphs, list items, block quotes, tables — plus *passthrough* blanks, fenced
//!    code, and HTML/comment blocks). Concatenating every segment's `text` reproduces the
//!    source byte-for-byte, so the split is lossless.
//! 2. Only translatable segments are handed to a [`TranslationBackend`]. Passthrough
//!    segments — code fences, HTML, badges, blank separators — are emitted verbatim, so
//!    code blocks, link targets, and badge/image URLs never get translated.
//! 3. A pinned [`Glossary`] keeps product identifiers (`Capsule`, `capsule-core`, slice
//!    IDs, tech names) verbatim across regenerations; [`Glossary::verify`] enforces it.
//! 4. Each `README.<lang>.md` is written with the shared do-not-edit banner, which also
//!    embeds the **source fingerprint** it was generated from (see [`fingerprint`]).
//!
//! # The drift gate (`--check`)
//!
//! `--check` re-segments the current `README.md`, recomputes its [`fingerprint`], and
//! compares it against the fingerprint stored in every committed translation's banner.
//! Mutating any source segment changes the fingerprint, so the stale translations fail
//! the check — the same key-less pattern as `i18n --check`. It is **structural, not
//! semantic**: it reads only committed files and needs no translation API in CI.
//!
//! # The backend seam
//!
//! [`TranslationBackend`] is the seam a future LLM provider drops into. In CI (and by
//! default) the [`FileBackend`] serves human-authored translations from
//! `xtask/translations/readme/<lang>.json`; [`ApiBackend`] documents where an API call
//! goes but is deliberately not invocable offline.

use std::fs;
use std::path::{Path, PathBuf};

use eyre::{Context, Result, bail, eyre};
use regex::Regex;
use serde_json::Value;

/// What `run` should do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Mode {
    /// Regenerate every `README.<lang>.md`. `api` selects the (offline-unavailable) API
    /// backend instead of the default committed-translation file backend.
    Generate { api: bool },
    /// The CI drift gate: fail if any translation drifted from the source.
    Check,
    /// Print the source's translatable blocks as JSON (authoring aid).
    Extract,
}

/// Directory (relative to the repo root) holding the human-authored translation data.
const TRANSLATIONS_DIR: &str = "xtask/translations/readme";
/// The source document, at the repo root.
const SOURCE: &str = "README.md";

/// Entry point dispatched from `main`.
pub(crate) fn run(root: &Path, mode: Mode) -> Result<()> {
    let source =
        fs::read_to_string(root.join(SOURCE)).with_context(|| format!("reading {SOURCE}"))?;
    let locales = non_source_locales(root)?;
    match mode {
        Mode::Extract => {
            print!("{}", extract_json(&source)?);
            Ok(())
        }
        Mode::Generate { api } => {
            let backend: Box<dyn TranslationBackend> = if api {
                Box::new(ApiBackend)
            } else {
                Box::new(FileBackend::new(root))
            };
            generate(root, &source, &locales, backend.as_ref())
        }
        Mode::Check => check(root, &source, &locales),
    }
}

// ─── Locale set (mirrored from locales/config.json) ──────────────────────────────

/// The non-source locales, in `locales/config.json` order — the language list is never
/// declared outside that file (the i18n contract).
fn non_source_locales(root: &Path) -> Result<Vec<String>> {
    let config: Value = {
        let path = root.join("locales/config.json");
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?
    };
    let source = config
        .get("sourceLocale")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre!("locales/config.json: missing string `sourceLocale`"))?;
    let supported = config
        .get("supportedLocales")
        .and_then(Value::as_array)
        .ok_or_else(|| eyre!("locales/config.json: missing array `supportedLocales`"))?;
    let mut out = Vec::new();
    for value in supported {
        let tag = value.as_str().ok_or_else(|| {
            eyre!("locales/config.json: `supportedLocales` entries must be strings")
        })?;
        if tag != source {
            out.push(tag.to_string());
        }
    }
    Ok(out)
}

// ─── Segmentation ────────────────────────────────────────────────────────────────

/// The classification of a markdown block.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    /// A run of blank lines separating blocks. Passthrough.
    Separator,
    /// A fenced code block (``` or ~~~), fences included. Passthrough.
    Code,
    /// An HTML block or `<!-- comment -->` (e.g. badges, layout). Passthrough.
    Html,
    /// An ATX heading line (`#`..`######`). Translatable.
    Heading,
    /// A paragraph (one or more consecutive text lines). Translatable.
    Paragraph,
    /// A single list item (with any indented continuation lines). Translatable.
    ListItem,
    /// A block quote (consecutive `>` lines). Translatable.
    BlockQuote,
    /// A table (consecutive `|` lines, delimiter row included). Translatable.
    Table,
}

impl Kind {
    /// Whether this block's text is sent to the translation backend.
    pub(crate) fn is_translatable(self) -> bool {
        matches!(
            self,
            Kind::Heading | Kind::Paragraph | Kind::ListItem | Kind::BlockQuote | Kind::Table
        )
    }

    /// A stable tag mixed into the fingerprint so a kind change counts as drift.
    fn tag(self) -> &'static str {
        match self {
            Kind::Separator => "sep",
            Kind::Code => "code",
            Kind::Html => "html",
            Kind::Heading => "head",
            Kind::Paragraph => "para",
            Kind::ListItem => "li",
            Kind::BlockQuote => "quote",
            Kind::Table => "table",
        }
    }
}

/// One block of the source. `text` is the exact source slice, trailing newline included,
/// so `segments.map(|s| s.text).concat() == source`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Segment {
    pub(crate) kind: Kind,
    pub(crate) text: String,
}

/// Split `source` into a lossless, tiling sequence of blocks.
pub(crate) fn segment(source: &str) -> Vec<Segment> {
    let lines = split_lines_keep_newline(source);
    let mut segments = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let (kind, end) = if is_blank(line) {
            (Kind::Separator, run_while(&lines, i, is_blank))
        } else if let Some(marker) = fence_marker(line) {
            (Kind::Code, close_fence(&lines, i, marker))
        } else if is_comment_start(line) {
            (Kind::Html, close_comment(&lines, i))
        } else if is_heading(line) {
            (Kind::Heading, i + 1)
        } else if is_quote(line) {
            (Kind::BlockQuote, run_while(&lines, i, is_quote))
        } else if is_table(line) {
            (Kind::Table, run_while(&lines, i, is_table))
        } else if is_list(line) {
            (Kind::ListItem, list_item_end(&lines, i))
        } else if is_html_block(line) {
            (Kind::Html, run_while(&lines, i, |l| !is_blank(l)))
        } else {
            (Kind::Paragraph, paragraph_end(&lines, i))
        };
        segments.push(Segment {
            kind,
            text: lines[i..end].concat(),
        });
        i = end;
    }
    segments
}

/// Split into physical lines, each keeping its trailing `\n` (the last may lack one).
fn split_lines_keep_newline(source: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (idx, &b) in source.as_bytes().iter().enumerate() {
        if b == b'\n' {
            lines.push(&source[start..=idx]);
            start = idx + 1;
        }
    }
    if start < source.len() {
        lines.push(&source[start..]);
    }
    lines
}

/// The first index `>= from` where `pred` stops holding (or the end).
fn run_while(lines: &[&str], from: usize, pred: impl Fn(&str) -> bool) -> usize {
    let mut i = from;
    while i < lines.len() && pred(lines[i]) {
        i += 1;
    }
    i
}

fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

/// The fence marker (```` ``` ```` or `~~~`) opening `line`, if any.
fn fence_marker(line: &str) -> Option<&'static str> {
    let t = line.trim_start();
    if t.starts_with("```") {
        Some("```")
    } else if t.starts_with("~~~") {
        Some("~~~")
    } else {
        None
    }
}

/// The line after the closing fence for a code block opened at `open`.
fn close_fence(lines: &[&str], open: usize, marker: &str) -> usize {
    let mut i = open + 1;
    while i < lines.len() {
        let done = lines[i].trim_start().starts_with(marker);
        i += 1;
        if done {
            break;
        }
    }
    i
}

fn is_comment_start(line: &str) -> bool {
    line.trim_start().starts_with("<!--")
}

/// The line after the one closing an HTML comment opened at `open`.
fn close_comment(lines: &[&str], open: usize) -> usize {
    let mut i = open;
    while i < lines.len() {
        let done = lines[i].contains("-->");
        i += 1;
        if done {
            break;
        }
    }
    i
}

fn is_heading(line: &str) -> bool {
    let t = line.trim_start();
    if !t.starts_with('#') {
        return false;
    }
    let rest = t.trim_start_matches('#');
    rest.is_empty() || rest.starts_with([' ', '\t'])
}

fn is_quote(line: &str) -> bool {
    line.trim_start().starts_with('>')
}

fn is_table(line: &str) -> bool {
    line.trim_start().starts_with('|')
}

/// An HTML block (a line starting with `<` that is not a comment) — passthrough.
fn is_html_block(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with('<') && !t.starts_with("<!--")
}

/// A bullet or ordered-list item start (`- `, `* `, `+ `, `1. `, `1) `).
fn is_list(line: &str) -> bool {
    list_re().is_match(line)
}

fn list_re() -> Regex {
    Regex::new(r"^\s{0,3}([-*+]|\d+[.)])\s+").expect("static regex is valid")
}

/// The end of a list item: its start line plus any indented continuation lines
/// (non-blank, not themselves a new bullet).
fn list_item_end(lines: &[&str], start: usize) -> usize {
    let mut i = start + 1;
    while i < lines.len() {
        let l = lines[i];
        if is_blank(l) || is_list(l) || !l.starts_with([' ', '\t']) {
            break;
        }
        i += 1;
    }
    i
}

/// The end of a paragraph: consecutive lines that do not open another block kind.
fn paragraph_end(lines: &[&str], start: usize) -> usize {
    let mut i = start + 1;
    while i < lines.len() {
        let l = lines[i];
        if is_blank(l)
            || fence_marker(l).is_some()
            || is_comment_start(l)
            || is_heading(l)
            || is_quote(l)
            || is_table(l)
            || is_list(l)
            || is_html_block(l)
        {
            break;
        }
        i += 1;
    }
    i
}

// ─── Fingerprint ─────────────────────────────────────────────────────────────────

/// A stable 64-bit fingerprint of the source's block structure *and* content: any
/// change to a segment's kind or text changes it. FNV-1a — deterministic and
/// dependency-free (this is drift detection, not security).
pub(crate) fn fingerprint(segments: &[Segment]) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    let mut mix = |bytes: &[u8]| {
        for &b in bytes {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(PRIME);
        }
    };
    for seg in segments {
        mix(seg.kind.tag().as_bytes());
        mix(&[0x1f]);
        mix(seg.text.as_bytes());
        mix(&[0x1e]);
    }
    format!("{hash:016x}")
}

// ─── Inline protection (link targets / badge URLs / code spans) ──────────────────

/// The inline spans inside translatable text that must pass through untranslated:
/// link/image URLs, autolinks, and inline `code`. A future API backend masks these
/// before translating; the goldens assert they are identified.
pub(crate) fn protected_spans(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for caps in link_re().captures_iter(text) {
        out.push(caps["url"].to_string());
    }
    for m in inline_code_re().find_iter(text) {
        out.push(m.as_str().to_string());
    }
    for m in autolink_re().find_iter(text) {
        out.push(m.as_str().to_string());
    }
    out
}

/// Fail if any protected inline span (link/badge URL, autolink, or inline code) present
/// in `source` is missing from `target` — link targets and code identifiers must pass
/// through translation untouched.
pub(crate) fn verify_protected(source: &str, target: &str) -> Result<()> {
    let missing: Vec<String> = protected_spans(source)
        .into_iter()
        .filter(|span| !target.contains(span.as_str()))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        bail!(
            "translation dropped protected span(s): {}",
            missing.join(", ")
        );
    }
}

fn link_re() -> Regex {
    Regex::new(r"!?\[[^\]]*\]\((?P<url>[^)]+)\)").expect("static regex is valid")
}

fn inline_code_re() -> Regex {
    Regex::new(r"`[^`]+`").expect("static regex is valid")
}

fn autolink_re() -> Regex {
    Regex::new(r"<https?://[^>]+>").expect("static regex is valid")
}

// ─── Glossary ────────────────────────────────────────────────────────────────────

/// Product identifiers and technical names pinned to their English spelling. Terms that
/// appear in a source block must appear verbatim in its translation, so terminology
/// stays consistent across regenerations (the i18n contract). Common English words that
/// double as product terms (e.g. "just", "album", "drop") are pinned as *renderings* in
/// the design doc but deliberately excluded from the mechanical check to avoid false
/// positives.
pub(crate) struct Glossary;

impl Glossary {
    /// Terms enforced verbatim by [`Glossary::verify`].
    pub(crate) fn verbatim_terms() -> &'static [&'static str] {
        &[
            "Capsule",
            "capsule-api",
            "capsule-web",
            "capsule-core",
            "capsule-cli",
            "capsule-swift",
            "capsule-android",
            "capsule-docs",
            "capsule-core-kotlin",
            "capsule-desktop",
            "capsule-i18n",
            "capsule-sdk",
            "capsule-vision",
            "gRPC",
            "GraphQL",
            "WebSockets",
            "PostgreSQL",
            "MinIO",
            "RabbitMQ",
            "Memcached",
            "Envoy",
            "Istio",
            "AGPL-3.0",
            "mise",
            "lefthook",
            "convco",
            "Matrix",
            "AirDrop",
            "GitHub",
            "CONTRIBUTING.md",
            "io_uring",
        ]
    }

    /// Fail if any pinned term present in `source` is missing from `target`.
    pub(crate) fn verify(source: &str, target: &str) -> Result<()> {
        let mut missing = Vec::new();
        for term in Self::verbatim_terms() {
            if source.contains(term) && !target.contains(term) {
                missing.push(*term);
            }
        }
        if missing.is_empty() {
            Ok(())
        } else {
            bail!(
                "translation dropped glossary term(s): {}",
                missing.join(", ")
            );
        }
    }
}

// ─── Translation backends (the seam) ─────────────────────────────────────────────

/// The seam a translation provider plugs into. Given the ordered translatable blocks of
/// the source, it returns their translations for `locale`.
pub(crate) trait TranslationBackend {
    /// Translate `blocks` (the source's translatable segments, in order) into `locale`.
    fn translate(&self, locale: &str, blocks: &[&str]) -> Result<Vec<String>>;
}

/// A future LLM/API provider. Not invocable offline — it documents where the call goes
/// and keeps CI hermetic. Selected with `--api`.
pub(crate) struct ApiBackend;

impl TranslationBackend for ApiBackend {
    fn translate(&self, _locale: &str, _blocks: &[&str]) -> Result<Vec<String>> {
        bail!(
            "the API translation backend is not available offline/in CI; a real provider \
             would translate each block with the pinned glossary here. Use the default \
             file backend (human-authored translations under `{TRANSLATIONS_DIR}/`)."
        )
    }
}

/// The default, CI-safe backend: human-authored translations committed as
/// `xtask/translations/readme/<lang>.json` — an ordered array of
/// `{ "source": <English block>, "target": <translation> }`. The `source` field is
/// verified against the current segmentation so a stale translation file is caught.
pub(crate) struct FileBackend<'a> {
    root: &'a Path,
}

impl<'a> FileBackend<'a> {
    fn new(root: &'a Path) -> Self {
        Self { root }
    }

    fn path(&self, locale: &str) -> PathBuf {
        self.root.join(format!("{TRANSLATIONS_DIR}/{locale}.json"))
    }
}

impl TranslationBackend for FileBackend<'_> {
    fn translate(&self, locale: &str, blocks: &[&str]) -> Result<Vec<String>> {
        let path = self.path(locale);
        let text = fs::read_to_string(&path).with_context(|| {
            format!(
                "reading {} (run `mise run translate-readme-extract` to scaffold it)",
                path.display()
            )
        })?;
        let entries: Vec<TranslationEntry> =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        if entries.len() != blocks.len() {
            bail!(
                "{}: has {} translation entries but the source has {} translatable blocks; \
                 re-run `mise run translate-readme-extract`",
                path.display(),
                entries.len(),
                blocks.len()
            );
        }
        let mut out = Vec::with_capacity(blocks.len());
        for (idx, (entry, block)) in entries.iter().zip(blocks).enumerate() {
            if entry.source.trim_end() != block.trim_end() {
                bail!(
                    "{}: entry {idx} is out of date — its `source` no longer matches the README \
                     block. Re-run `mise run translate-readme-extract` and retranslate.",
                    path.display()
                );
            }
            Glossary::verify(block, &entry.target)
                .with_context(|| format!("{} entry {idx} ({locale})", path.display()))?;
            verify_protected(block, &entry.target)
                .with_context(|| format!("{} entry {idx} ({locale})", path.display()))?;
            out.push(entry.target.clone());
        }
        Ok(out)
    }
}

/// One authored translation: the English `source` block and its `target` translation.
#[derive(serde::Deserialize)]
struct TranslationEntry {
    source: String,
    target: String,
}

// ─── Generation ──────────────────────────────────────────────────────────────────

/// Regenerate every `README.<lang>.md` from the source and the chosen backend.
fn generate(
    root: &Path,
    source: &str,
    locales: &[String],
    backend: &dyn TranslationBackend,
) -> Result<()> {
    let segments = segment(source);
    let fp = fingerprint(&segments);
    let blocks = translatable_blocks(&segments);
    for locale in locales {
        let translations = backend
            .translate(locale, &blocks)
            .with_context(|| format!("translating README into {locale}"))?;
        let rendered = render(&segments, &translations, locale, &fp);
        let path = root.join(format!("README.{locale}.md"));
        let changed = fs::read_to_string(&path).map_or(true, |existing| existing != rendered);
        if changed {
            fs::write(&path, &rendered).with_context(|| format!("writing {}", path.display()))?;
            println!("generated README.{locale}.md");
        } else {
            println!("unchanged README.{locale}.md");
        }
    }
    Ok(())
}

/// The translatable blocks of `segments`, in order (the backend's input).
fn translatable_blocks(segments: &[Segment]) -> Vec<&str> {
    segments
        .iter()
        .filter(|s| s.kind.is_translatable())
        .map(|s| s.text.trim_end_matches('\n'))
        .collect()
}

/// Assemble a translated document: the do-not-edit banner (embedding the source
/// fingerprint) followed by the reconstructed body — passthrough segments verbatim,
/// translatable segments replaced by their translation.
fn render(segments: &[Segment], translations: &[String], locale: &str, fp: &str) -> String {
    let mut body = String::new();
    let mut t = 0;
    for seg in segments {
        if seg.kind.is_translatable() {
            body.push_str(translations[t].trim_end_matches('\n'));
            body.push('\n');
            t += 1;
        } else {
            body.push_str(&seg.text);
        }
    }
    format!("{}{body}", banner(locale, fp))
}

/// The two-line do-not-edit banner (matching the other generated i18n artifacts) plus a
/// blank line. Line two carries the machine-readable source fingerprint the drift gate
/// compares against.
fn banner(locale: &str, fp: &str) -> String {
    format!(
        "<!-- GENERATED by `cargo run -p xtask -- translate-readme` from README.md. Do not edit by hand. -->\n\
         <!-- translate-readme: locale={locale} source-fingerprint={fp}. Regenerate with `mise run translate-readme`. -->\n\n"
    )
}

/// Read the `source-fingerprint=<hex>` embedded in a translation's banner.
fn stored_fingerprint(content: &str) -> Option<String> {
    fingerprint_re()
        .captures(content)
        .map(|c| c["fp"].to_string())
}

fn fingerprint_re() -> Regex {
    Regex::new(r"source-fingerprint=(?P<fp>[0-9a-f]{16})").expect("static regex is valid")
}

// ─── The drift gate ──────────────────────────────────────────────────────────────

/// The CI drift gate. Reads the committed translations off disk and delegates the
/// comparison to the pure [`check_translations`].
fn check(root: &Path, source: &str, locales: &[String]) -> Result<()> {
    let mut files = Vec::new();
    for locale in locales {
        let path = root.join(format!("README.{locale}.md"));
        let content = fs::read_to_string(&path).ok();
        files.push((locale.clone(), content));
    }
    check_translations(source, &files)?;
    println!(
        "translate-readme: {} translations up to date",
        locales.len()
    );
    Ok(())
}

/// Pure drift check over in-memory inputs (so it is unit-testable without disk).
///
/// A translation is stale when: it is missing; it carries no fingerprint; its stored
/// fingerprint differs from the freshly computed source fingerprint (a source segment
/// changed); or its own block structure no longer aligns with the source (passthrough
/// blocks must stay byte-identical — this catches hand-edits to the generated file).
pub(crate) fn check_translations(source: &str, files: &[(String, Option<String>)]) -> Result<()> {
    let source_segments = segment(source);
    let source_fp = fingerprint(&source_segments);
    let mut drift = Vec::new();
    for (locale, content) in files {
        let Some(content) = content else {
            drift.push(format!("README.{locale}.md (missing)"));
            continue;
        };
        match stored_fingerprint(content) {
            None => drift.push(format!("README.{locale}.md (no source-fingerprint banner)")),
            Some(fp) if fp != source_fp => drift.push(format!(
                "README.{locale}.md (stale: source changed — banner {fp} != source {source_fp})"
            )),
            Some(_) => {
                if let Err(reason) = structure_matches(&source_segments, content) {
                    drift.push(format!("README.{locale}.md ({reason})"));
                }
            }
        }
    }
    if drift.is_empty() {
        Ok(())
    } else {
        bail!(
            "README translations are stale; run `mise run translate-readme`:\n  {}",
            drift.join("\n  ")
        );
    }
}

/// Verify a translation's block structure lines up with the source: same kind sequence,
/// and every passthrough block byte-identical (translatable blocks may differ in text).
fn structure_matches(source_segments: &[Segment], translation: &str) -> Result<(), String> {
    let body = strip_banner(translation);
    let translated = segment(body);
    let src: Vec<&Segment> = source_segments
        .iter()
        .filter(|s| s.kind != Kind::Separator)
        .collect();
    let tr: Vec<&Segment> = translated
        .iter()
        .filter(|s| s.kind != Kind::Separator)
        .collect();
    if src.len() != tr.len() {
        return Err(format!(
            "block count changed: source has {}, translation has {}",
            src.len(),
            tr.len()
        ));
    }
    for (idx, (s, t)) in src.iter().zip(&tr).enumerate() {
        if s.kind != t.kind {
            return Err(format!(
                "block {idx} kind changed: {:?} -> {:?}",
                s.kind, t.kind
            ));
        }
        if !s.kind.is_translatable() && s.text != t.text {
            return Err(format!("passthrough block {idx} ({:?}) was edited", s.kind));
        }
    }
    Ok(())
}

/// Drop the leading do-not-edit banner (contiguous HTML comments plus following blanks)
/// so the remaining body segments align with the source.
fn strip_banner(content: &str) -> &str {
    let lines = split_lines_keep_newline(content);
    let mut i = 0;
    while i < lines.len() && is_comment_start(lines[i]) {
        i = close_comment(&lines, i);
    }
    while i < lines.len() && is_blank(lines[i]) {
        i += 1;
    }
    let consumed: usize = lines[..i].iter().map(|l| l.len()).sum();
    &content[consumed..]
}

// ─── Extract (authoring aid) ─────────────────────────────────────────────────────

/// Render the source's translatable blocks as a `[{source, target}]` scaffold — the
/// starting point for a `xtask/translations/readme/<lang>.json` file (target seeded to
/// the English source for the author to overwrite).
fn extract_json(source: &str) -> Result<String> {
    let segments = segment(source);
    let entries: Vec<Value> = translatable_blocks(&segments)
        .into_iter()
        .map(|block| serde_json::json!({ "source": block, "target": block }))
        .collect();
    let mut s = serde_json::to_string_pretty(&entries).context("serializing extract")?;
    s.push('\n');
    Ok(s)
}

#[cfg(test)]
mod tests;
