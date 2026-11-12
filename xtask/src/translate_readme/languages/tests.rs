use serde_json::json;

use super::*;

fn languages() -> Languages {
    Languages::from_config(&json!({
        "sourceLocale": "en",
        "supportedLocales": ["en", "ja", "pt-BR"],
        "displayNames": {"en": "English", "ja": "日本語", "pt-BR": "Português (Brasil)"}
    }))
    .expect("valid config")
}

#[test]
fn every_supported_locale_needs_a_display_name() {
    let err = Languages::from_config(&json!({
        "sourceLocale": "en",
        "supportedLocales": ["en", "ja"],
        "displayNames": {"en": "English"}
    }))
    .expect_err("ja has no name");
    assert!(format!("{err}").contains("no name for `ja`"), "{err}");
}

#[test]
fn a_blank_display_name_is_refused() {
    let err = Languages::from_config(&json!({
        "sourceLocale": "en",
        "supportedLocales": ["en"],
        "displayNames": {"en": "  "}
    }))
    .expect_err("blank name");
    assert!(format!("{err}").contains("no name for `en`"), "{err}");
}

#[test]
fn a_display_name_for_an_unsupported_locale_is_refused() {
    let err = Languages::from_config(&json!({
        "sourceLocale": "en",
        "supportedLocales": ["en"],
        "displayNames": {"en": "English", "xx": "Xx"}
    }))
    .expect_err("xx is not supported");
    assert!(
        format!("{err}").contains("unsupported locale `xx`"),
        "{err}"
    );
}

#[test]
fn the_switcher_links_every_readme_in_config_order() {
    assert_eq!(
        languages().switcher(),
        "<p align=\"center\"><a href=\"README.md\">English</a> · \
         <a href=\"README.ja.md\">日本語</a> · \
         <a href=\"README.pt-BR.md\">Português (Brasil)</a></p>\n"
    );
}

#[test]
fn the_table_has_one_row_per_translated_locale() {
    let table = languages().table();
    let rows: Vec<&str> = table.lines().skip(2).collect();
    assert_eq!(rows.len(), 2, "{table}");
    // Built with `format!` so the docs-truth link checker does not read these
    // expectations as doc references.
    let row = |name: &str, readme: &str| format!("| {name} | [{readme}]({readme}) | ");
    assert!(
        rows[0].starts_with(&row("日本語", "README.ja.md")),
        "{table}"
    );
    assert!(
        rows[1].starts_with(&row("Português (Brasil)", "README.pt-BR.md")),
        "{table}"
    );
    assert!(
        !table.contains("README.md)"),
        "the source locale has no row"
    );
}

/// The badge query must address a hyphenated tag with bracket notation, which a dotted
/// JSONPath would split at the hyphen.
#[test]
fn badges_query_stats_json_with_bracket_notation() {
    let cell = badge("pt-BR", "reviewed");
    assert!(
        cell.contains("query=%24%5B%27pt-BR%27%5D.reviewed"),
        "{cell}"
    );
    assert!(
        cell.contains("url=https%3A%2F%2Fraw.githubusercontent.com%2FCapsulsaurus%2FCapsule%2Fmaster%2Flocales%2Fstats.json"),
        "{cell}"
    );
}

#[test]
fn splice_replaces_only_the_region_body() {
    let doc = "intro\n<!-- x:begin -->\nstale\n<!-- x:end -->\noutro\n";
    assert_eq!(
        splice(doc, "x", "fresh\n").expect("splices"),
        "intro\n<!-- x:begin -->\n\nfresh\n\n<!-- x:end -->\noutro\n"
    );
}

#[test]
fn splice_is_idempotent() {
    let doc = "<!-- x:begin -->\n<!-- x:end -->\n";
    let once = splice(doc, "x", "body\n").expect("splices");
    assert_eq!(splice(&once, "x", "body\n").expect("splices"), once);
}

#[test]
fn splice_refuses_missing_duplicate_and_reversed_markers() {
    let missing = splice("<!-- x:begin -->\n", "x", "b\n").expect_err("no end");
    assert!(format!("{missing}").contains("missing"), "{missing}");
    let twice = "<!-- x:begin -->\n<!-- x:begin -->\n<!-- x:end -->\n";
    let duplicate = splice(twice, "x", "b\n").expect_err("two begins");
    assert!(
        format!("{duplicate}").contains("more than once"),
        "{duplicate}"
    );
    let reversed = splice("<!-- x:end -->\n<!-- x:begin -->\n", "x", "b\n").expect_err("reversed");
    assert!(format!("{reversed}").contains("comes before"), "{reversed}");
}

/// The generated regions segment as passthrough HTML (switcher) and a translatable
/// table, so the table's header is translated and its rows carried verbatim.
#[test]
fn synced_regions_segment_as_html_and_table() {
    let doc = "<!-- languages:begin -->\n<!-- languages:end -->\n\n\
               <!-- translations:begin -->\n<!-- translations:end -->\n";
    let synced = sync(doc, &languages()).expect("syncs");
    let kinds: Vec<_> = crate::translate_readme::segment(&synced)
        .into_iter()
        .filter(|s| s.kind != crate::translate_readme::Kind::Separator)
        .map(|s| s.kind)
        .collect();
    use crate::translate_readme::Kind::{Html, Table};
    assert_eq!(kinds, [Html, Html, Html, Html, Table, Html]);
}

/// The committed README carries both regions exactly as `config.json` renders them.
#[test]
fn the_committed_readme_is_in_sync_with_config() {
    let root = crate::repo_root();
    let config: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("locales/config.json")).expect("config"),
    )
    .expect("json");
    let readme = std::fs::read_to_string(root.join("README.md")).expect("readme");
    let languages = Languages::from_config(&config).expect("valid config");
    assert_eq!(sync(&readme, &languages).expect("syncs"), readme);
}
