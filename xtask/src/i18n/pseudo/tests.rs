use std::collections::BTreeMap;

use super::*;
use crate::i18n::{ArgPlan, Catalogs, placeholder_parity};

fn accented(message: &str) -> String {
    Pseudo::Accented.message(message).expect("rewrites")
}

fn bidi(message: &str) -> String {
    Pseudo::Bidi.message(message).expect("rewrites")
}

#[test]
fn en_xa_accents_every_letter_and_brackets_the_message() {
    let out = accented("Photos");
    assert!(out.starts_with("[Þĥöţöš"), "{out}");
    assert!(out.ends_with(']'), "{out}");
    assert!(!out.contains("Photos"));
}

#[test]
fn en_xa_is_at_least_forty_percent_longer() {
    for source in ["Photos", "Sign in", "Choose a Capsule album.", "A"] {
        let out = accented(source);
        let grown = out.chars().count() - 2; // less the brackets
        assert!(
            grown * 5 >= source.chars().count() * 7,
            "`{out}` is not 40% longer than `{source}`"
        );
    }
}

#[test]
fn en_xa_leaves_a_message_with_no_letters_unpadded() {
    assert_eq!(accented("{count}"), "[{count}]");
    assert_eq!(accented(""), "[]");
}

#[test]
fn arguments_survive_unchanged() {
    let out = accented("Hello, {name}! You have {count} new");
    assert!(out.contains("{name}") && out.contains("{count}"), "{out}");
    assert!(
        out.starts_with("[Ĥéļļö, {name}! Ýöû ĥåṽé {count} ñéŵ"),
        "{out}"
    );
}

#[test]
fn a_plural_keeps_its_selector_categories_and_count() {
    let out = accented("{count, plural, one {# photo} other {# photos in {album}}}");
    let plural = IcuPlural::parse(&out)
        .expect("parses")
        .expect("still a plural");
    assert_eq!(plural.selector, "count");
    assert_eq!(
        plural.categories.keys().collect::<Vec<_>>(),
        ["one", "other"]
    );
    assert!(plural.categories["one"].starts_with("[# þĥöţö"), "{out}");
    assert!(plural.categories["other"].contains("{album}"), "{out}");
}

#[test]
fn ar_xb_overrides_each_literal_run_and_keeps_arguments_outside_it() {
    assert_eq!(
        bidi("Hi {name}!"),
        format!("{RLM}{RLO}Hi {PDF}{{name}}{RLO}!{PDF}{RLM}")
    );
}

#[test]
fn ar_xb_keeps_the_plural_count_outside_the_override() {
    let out = bidi("{n, plural, other {# items}}");
    assert_eq!(
        out,
        format!("{{n, plural, other {{{RLM}#{RLO} items{PDF}{RLM}}}}}")
    );
}

/// Every pseudo message must carry exactly its source's arguments, or the web client
/// would format it without a value the call site passes.
#[test]
fn every_pseudo_message_of_the_real_catalog_keeps_its_source_arguments() {
    let catalogs = Catalogs::load(&crate::repo_root()).expect("the real catalogs load");
    let source = &catalogs.messages[&catalogs.source];
    for (locale, map) in super::catalogs(source).expect("the real catalog rewrites") {
        assert_eq!(map.len(), source.len(), "{locale} drops a key");
        for (key, message) in &map {
            placeholder_parity(&source[key], message)
                .unwrap_or_else(|e| panic!("{locale} `{key}`: {e:#}"));
            ArgPlan::of(message).unwrap_or_else(|e| panic!("{locale} `{key}`: {e:#}"));
        }
    }
}

#[test]
fn every_en_xa_message_is_bracketed_and_carries_no_plain_ascii_word() {
    let mut source = BTreeMap::new();
    source.insert("k".to_string(), "Delete {count} items now".to_string());
    let (_, map) = super::catalogs(&source)
        .expect("rewrites")
        .into_iter()
        .find(|(locale, _)| *locale == "en-XA")
        .expect("en-XA is generated");
    let out = &map["k"];
    assert!(out.starts_with(OPEN) && out.ends_with(CLOSE));
    for word in ["Delete", "items", "now"] {
        assert!(!out.contains(word), "`{word}` survived in {out}");
    }
}

#[test]
fn direction_controls_are_written_as_json_escapes() {
    let escaped = escape_controls(&format!("\"{RLM}{RLO}a{PDF}\""));
    assert_eq!(escaped, "\"\\u200f\\u202ea\\u202c\"");
    let parsed: String = serde_json::from_str(&escaped).expect("still JSON");
    assert_eq!(parsed, format!("{RLM}{RLO}a{PDF}"));
}

/// The pseudo-locales are development-only: generated for the web client and never
/// added to the supported set, the Rust bundle, Android or Apple.
#[test]
fn pseudo_locales_reach_only_the_web_client() {
    let catalogs = Catalogs::load(&crate::repo_root()).expect("the real catalogs load");
    let outputs = catalogs.render().expect("the real catalogs compile");
    for (locale, _) in PSEUDO_LOCALES {
        assert!(!catalogs.supported.iter().any(|l| l == locale));
        let paths: Vec<_> = outputs
            .iter()
            .filter(|(path, content)| {
                path.to_string_lossy().contains(locale)
                    || content.contains(&format!("\"{locale}\""))
            })
            .map(|(path, _)| path.clone())
            .collect();
        assert_eq!(
            paths,
            [std::path::PathBuf::from(format!(
                "capsule-web/src/i18n/messages/{locale}.json"
            ))],
            "{locale}"
        );
    }
}
