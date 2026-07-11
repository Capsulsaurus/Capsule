//! Golden and behavioural tests for the README translation pipeline.

use std::path::Path;

use super::*;

/// A fixture exercising every block kind plus inline links, badges, and code.
const FIXTURE: &str = "\
# Title

Intro paragraph with a [link](https://example.com) and `code`.

![badge](https://img.shields.io/badge/x.svg)

- item one
- item two

> a quote

```rust
let x = 1;
```

<!-- a comment -->

| a | b |
| --- | --- |
| 1 | 2 |
";

fn kinds(segments: &[Segment]) -> Vec<Kind> {
    segments.iter().map(|s| s.kind).collect()
}

/// Golden: the fixture splits into exactly this block sequence, and code/HTML/table
/// blocks are captured whole.
#[test]
fn golden_segmentation() {
    let segments = segment(FIXTURE);
    assert_eq!(
        kinds(&segments),
        vec![
            Kind::Heading,
            Kind::Separator,
            Kind::Paragraph,
            Kind::Separator,
            Kind::Paragraph, // the badge image line
            Kind::Separator,
            Kind::ListItem,
            Kind::ListItem,
            Kind::Separator,
            Kind::BlockQuote,
            Kind::Separator,
            Kind::Code,
            Kind::Separator,
            Kind::Html,
            Kind::Separator,
            Kind::Table,
        ]
    );

    // The fenced code block is captured verbatim, fences included, and is passthrough.
    let code = segments.iter().find(|s| s.kind == Kind::Code).unwrap();
    assert_eq!(code.text, "```rust\nlet x = 1;\n```\n");
    assert!(!Kind::Code.is_translatable());

    // The HTML comment (a badge/layout stand-in) is one passthrough block.
    let html = segments.iter().find(|s| s.kind == Kind::Html).unwrap();
    assert_eq!(html.text, "<!-- a comment -->\n");

    // The table keeps its delimiter row inside the one block (3 lines).
    let table = segments.iter().find(|s| s.kind == Kind::Table).unwrap();
    assert_eq!(table.text.lines().count(), 3);
}

/// Golden: segmentation is lossless — concatenating every block reproduces the input.
#[test]
fn segmentation_is_lossless() {
    let rebuilt: String = segment(FIXTURE).iter().map(|s| s.text.clone()).collect();
    assert_eq!(rebuilt, FIXTURE);
}

/// The real repo README also tiles losslessly (guards the segmenter against its actual
/// input).
#[test]
fn repo_readme_tiles_losslessly() {
    let src = read_repo_readme();
    let rebuilt: String = segment(&src).iter().map(|s| s.text.clone()).collect();
    assert_eq!(rebuilt, src);
}

/// Golden: link targets and badge/image URLs pass through — they are identified as
/// protected inline spans (a real API backend would mask them; here we prove detection).
#[test]
fn protected_spans_cover_links_badges_and_code() {
    let para = "Intro paragraph with a [link](https://example.com) and `code`.";
    let spans = protected_spans(para);
    assert!(spans.contains(&"https://example.com".to_string()));
    assert!(spans.contains(&"`code`".to_string()));

    let badge = "![badge](https://img.shields.io/badge/x.svg)";
    assert_eq!(
        protected_spans(badge),
        vec!["https://img.shields.io/badge/x.svg"]
    );

    let autolink = "see <https://example.org/docs> for more";
    assert_eq!(
        protected_spans(autolink),
        vec!["<https://example.org/docs>"]
    );
}

/// Mutating any source segment changes the fingerprint.
#[test]
fn fingerprint_changes_when_a_source_segment_is_mutated() {
    let base = fingerprint(&segment(FIXTURE));
    let mutated = FIXTURE.replace("Intro paragraph", "Different intro");
    assert_ne!(base, fingerprint(&segment(&mutated)));

    // A structural change (dropping the list) also moves it.
    let dropped = FIXTURE.replace("- item one\n- item two\n", "");
    assert_ne!(base, fingerprint(&segment(&dropped)));
}

/// `--check` passes for a freshly rendered translation and fails once the source drifts.
#[test]
fn check_passes_then_fails_after_source_mutation() {
    let translation = render_echo(FIXTURE, "fr");
    let files = [("fr".to_string(), Some(translation.clone()))];

    // In sync: the banner fingerprint matches the source.
    check_translations(FIXTURE, &files).expect("fresh translation is in sync");

    // Mutate a source block: the stored banner fingerprint no longer matches -> stale.
    let mutated = FIXTURE.replace("# Title", "# Renamed Title");
    let err = check_translations(&mutated, &files)
        .unwrap_err()
        .to_string();
    assert!(err.contains("stale"), "unexpected error: {err}");
    assert!(err.contains("README.fr.md"));
}

/// A missing translation and a banner-less file both count as drift.
#[test]
fn check_flags_missing_and_bannerless() {
    let missing = [("de".to_string(), None)];
    assert!(check_translations(FIXTURE, &missing).is_err());

    let bannerless = [("de".to_string(), Some("# Titel\n".to_string()))];
    let err = check_translations(FIXTURE, &bannerless)
        .unwrap_err()
        .to_string();
    assert!(err.contains("no source-fingerprint"), "unexpected: {err}");
}

/// Hand-editing a passthrough block (here the code fence) in a committed translation is
/// caught even though the source (and thus the banner fingerprint) is unchanged.
#[test]
fn check_detects_edited_passthrough_block() {
    let mut translation = render_echo(FIXTURE, "es");
    translation = translation.replace("let x = 1;", "let x = 999;");
    let files = [("es".to_string(), Some(translation))];
    let err = check_translations(FIXTURE, &files).unwrap_err().to_string();
    assert!(
        err.contains("passthrough") || err.contains("Code"),
        "unexpected: {err}"
    );
}

/// The glossary keeps product/tech identifiers verbatim.
#[test]
fn glossary_flags_dropped_terms() {
    let source = "Capsule uses gRPC and GraphQL.";
    Glossary::verify(source, "Capsule utilise gRPC et GraphQL.").expect("all terms kept");

    let err = Glossary::verify(source, "Le produit utilise gRPC.")
        .unwrap_err()
        .to_string();
    assert!(err.contains("Capsule"), "unexpected: {err}");
    assert!(err.contains("GraphQL"), "unexpected: {err}");
}

/// Link targets, badge URLs, and code spans must survive translation verbatim.
#[test]
fn verify_protected_catches_dropped_link_target() {
    let source = "See [docs](https://x.test/a) and `capsule-core`.";
    verify_protected(source, "Voir [docs](https://x.test/a) et `capsule-core`.").expect("kept");

    let err = verify_protected(source, "Voir [docs](https://y.test/b) et `capsule-core`.")
        .unwrap_err()
        .to_string();
    assert!(err.contains("https://x.test/a"), "unexpected: {err}");
}

/// The committed translation data files round-trip: their `source` fields match the
/// current README segmentation, counts line up, and each honours the glossary. This is
/// the same validation `generate` runs, exercised over the real committed data.
#[test]
fn committed_translation_data_matches_source() {
    let root = repo_root();
    let source = std::fs::read_to_string(root.join(SOURCE)).unwrap();
    let segments = segment(&source);
    let blocks = translatable_blocks(&segments);
    let backend = FileBackend::new(&root);
    for locale in non_source_locales(&root).unwrap() {
        // `translate` re-checks per-entry source alignment, count, and glossary.
        backend
            .translate(&locale, &blocks)
            .unwrap_or_else(|e| panic!("{locale} translation data invalid: {e:#}"));
    }
}

/// The API backend is a documented seam, not invocable offline.
#[test]
fn api_backend_refuses_offline() {
    assert!(ApiBackend.translate("fr", &["Capsule"]).is_err());
}

// ─── helpers ─────────────────────────────────────────────────────────────────────

fn repo_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn read_repo_readme() -> String {
    std::fs::read_to_string(repo_root().join(SOURCE)).unwrap()
}

/// Render a translation whose every translatable block echoes the source — enough to
/// exercise the banner + reconstruction + drift machinery without real translations.
fn render_echo(source: &str, locale: &str) -> String {
    let segments = segment(source);
    let fp = fingerprint(&segments);
    let translations: Vec<String> = translatable_blocks(&segments)
        .into_iter()
        .map(str::to_string)
        .collect();
    render(&segments, &translations, locale, &fp)
}
