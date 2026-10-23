use super::{Category, RULES, category, selectable};

/// The languages `locales/config.json` ships, as their subtags.
const LANGUAGES: &[&str] = &[
    "ar", "de", "en", "es", "fr", "hi", "it", "ja", "ko", "pt", "ru", "zh",
];

/// The full table, cell by cell: one expected category per (language, count).
///
/// Written out rather than derived, because a table derived from the implementation
/// asserts nothing. Counts are the CLDR boundary set: the teens and the `x1`/`x2`
/// tails that separate Russian's three forms, the `x00` tail that separates Arabic's
/// `many` from its `other`, and 0, which four of these twelve treat as singular.
const COUNTS: &[i64] = &[0, 1, 2, 3, 5, 11, 21, 100, 101, 1000];

#[rustfmt::skip]
const EXPECTED: &[(&str, &[Category])] = {
    use Category::{Few, Many, One, Other, Two, Zero};
    &[
        //          0      1     2      3      5      11     21     100    101    1000
        ("ar", &[Zero,  One,  Two,   Few,   Few,   Many,  Many,  Other, Other, Other]),
        ("de", &[Other, One,  Other, Other, Other, Other, Other, Other, Other, Other]),
        ("en", &[Other, One,  Other, Other, Other, Other, Other, Other, Other, Other]),
        ("es", &[Other, One,  Other, Other, Other, Other, Other, Other, Other, Other]),
        ("fr", &[One,   One,  Other, Other, Other, Other, Other, Other, Other, Other]),
        ("hi", &[One,   One,  Other, Other, Other, Other, Other, Other, Other, Other]),
        ("it", &[Other, One,  Other, Other, Other, Other, Other, Other, Other, Other]),
        ("ja", &[Other, Other, Other, Other, Other, Other, Other, Other, Other, Other]),
        ("ko", &[Other, Other, Other, Other, Other, Other, Other, Other, Other, Other]),
        ("pt", &[One,   One,  Other, Other, Other, Other, Other, Other, Other, Other]),
        ("ru", &[Many,  One,  Few,   Few,   Many,  Many,  One,   Many,  One,   Many]),
        ("zh", &[Other, Other, Other, Other, Other, Other, Other, Other, Other, Other]),
    ]
};

#[test]
fn the_rule_table_covers_exactly_the_shipped_languages() {
    let listed: Vec<&str> = RULES.iter().map(|r| r.language).collect();
    assert_eq!(
        listed, LANGUAGES,
        "add a row when a locale joins config.json"
    );
    let expected: Vec<&str> = EXPECTED.iter().map(|(lang, _)| *lang).collect();
    assert_eq!(expected, LANGUAGES);
}

#[test]
fn every_cell_of_the_cldr_table_is_asserted() {
    for (language, row) in EXPECTED {
        assert_eq!(row.len(), COUNTS.len(), "{language} row is the wrong width");
        for (n, want) in COUNTS.iter().zip(*row) {
            assert_eq!(
                category(language, *n),
                *want,
                "{language} at n={n} selected the wrong category"
            );
        }
    }
}

#[test]
fn russian_separates_its_three_forms_past_the_teens() {
    assert_eq!(category("ru", 22), Category::Few);
    assert_eq!(category("ru", 25), Category::Many);
    assert_eq!(category("ru", 111), Category::Many);
    assert_eq!(category("ru", 121), Category::One);
}

#[test]
fn arabic_uses_the_hundreds_tail() {
    assert_eq!(category("ar", 103), Category::Few);
    assert_eq!(category("ar", 111), Category::Many);
    assert_eq!(category("ar", 200), Category::Other);
}

#[test]
fn the_romance_many_is_the_millions_form_and_nothing_smaller() {
    // `many` exists for these four, but nothing below a million reaches it — which is
    // why a catalog carrying only `one`/`other` still renders correctly today.
    for language in ["es", "fr", "it", "pt"] {
        for n in 0i64..=1000 {
            assert_ne!(
                category(language, n),
                Category::Many,
                "{language} reached `many` at n={n}"
            );
        }
        assert_eq!(category(language, 1_000_000), Category::Many);
        assert_eq!(category(language, 2_000_000), Category::Many);
        assert_eq!(category(language, 1_000_001), Category::Other);
    }
}

#[test]
fn the_selectable_set_is_exactly_what_selection_can_produce() {
    // The two halves of a row must agree in both directions, because both are load
    // bearing: `xtask i18n` drops an `<item quantity=…>` the language cannot select,
    // and the runtime picks the arm. A category listed but unreachable is a dead arm
    // the generator would emit; a category reachable but unlisted is an arm it would
    // drop and the runtime would then ask for.
    //
    // `other` is the exception in one direction only: Russian never selects it for an
    // *integer* (its `other` is for fractions), yet every plural resource must carry
    // it, so it is always listed.
    for language in LANGUAGES {
        let selectable = selectable(language).expect("a shipped language has rules");
        assert!(selectable.contains(&Category::Other), "{language}");
        let mut reachable: Vec<Category> = (0i64..=2000)
            .chain([999_999, 1_000_000, 2_000_000, i64::MAX])
            .map(|n| category(language, n))
            .collect();
        reachable.push(Category::Other);
        reachable.sort_unstable();
        reachable.dedup();
        assert_eq!(
            reachable, selectable,
            "{language}: the rules and the selectable set disagree"
        );
    }
}

#[test]
fn a_region_or_script_subtag_resolves_to_its_language() {
    assert_eq!(category("pt-BR", 0), category("pt", 0));
    assert_eq!(category("zh-Hans", 1), Category::Other);
    assert_eq!(category("zh-Hant", 1), Category::Other);
    assert_eq!(category("EN-gb", 1), Category::One);
    assert_eq!(selectable("pt-BR"), selectable("pt"));
}

#[test]
fn negative_counts_use_the_absolute_value() {
    assert_eq!(category("en", -1), Category::One);
    assert_eq!(category("ru", -22), Category::Few);
    // The one input that would panic under a naive `n.abs()`.
    assert_eq!(category("ru", i64::MIN), category("ru", 8));
}

#[test]
fn an_unknown_language_falls_back_to_other_but_reports_no_rules() {
    assert_eq!(category("nl", 1), Category::Other);
    assert_eq!(category("", 1), Category::Other);
    assert_eq!(selectable("nl"), None);
}

#[test]
fn categories_round_trip_through_their_cldr_keyword() {
    for want in Category::ALL {
        assert_eq!(Category::parse(want.as_str()), Some(*want));
        assert_eq!(want.to_string(), want.as_str());
    }
    assert_eq!(Category::parse("=0"), None);
    assert_eq!(Category::parse("One"), None);
}
