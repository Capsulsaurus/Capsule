//! CLDR cardinal plural rules for the locales Capsule ships.
//!
//! An ICU `plural` block names its arms by CLDR *category* (`one`, `few`, …), and which
//! category a number selects is a property of the language, not of the message. The
//! per-platform renderers never needed this table: Apple and Android compile a plural
//! ahead of time into a native resource and let the platform's own CLDR data pick the arm
//! at display time (see `xtask i18n`). The Rust runtime has no platform underneath it, so
//! it is the one target that must carry the rules itself — which is why [`crate::format`]
//! refused plurals outright until this module existed.
//!
//! # Scope
//!
//! **Integer cardinals only.** Every plural in `locales/` selects on a count, and
//! [`crate::Value`] has no decimal variant, so the CLDR operands reduce to `n = i`,
//! `v = 0`, `f = 0`, `e = 0` and the rules collapse to arithmetic on the absolute value.
//! Ordinals (`selectordinal`) are not implemented and are still refused by the formatter.
//!
//! The table covers exactly the twelve language subtags of `locales/config.json`. An
//! unknown language resolves to [`Category::Other`] rather than panicking — a locale
//! outside the config cannot reach a bundle with translated messages anyway
//! ([`crate::Bundle::for_locale`] falls back to the source catalog) — while
//! [`selectable`] reports `None` for it, so the *generator* can still fail loudly instead
//! of guessing a rule set.

use std::fmt;

/// A CLDR plural category, in CLDR's canonical order.
///
/// The order is meaningful: it is the order an Apple String Catalog, an Android
/// `<plurals>` resource and this crate all list arms in, so `Ord` sorts arms the way
/// every consumer expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Category {
    /// `zero` — a language with a dedicated form for none (Arabic).
    Zero,
    /// `one` — the singular. Not always "exactly 1": French counts 0 as `one`.
    One,
    /// `two` — the dual (Arabic).
    Two,
    /// `few` — the paucal (Arabic, Russian).
    Few,
    /// `many` — Russian's third form, and the large-number form of the Romance languages.
    Many,
    /// `other` — the universal fallback, selectable in every language.
    Other,
}

impl Category {
    /// Every category, in CLDR's canonical order.
    pub const ALL: &'static [Self] = &[
        Self::Zero,
        Self::One,
        Self::Two,
        Self::Few,
        Self::Many,
        Self::Other,
    ];

    /// The CLDR keyword, as it is spelled in an ICU message and in every generated
    /// platform resource.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Zero => "zero",
            Self::One => "one",
            Self::Two => "two",
            Self::Few => "few",
            Self::Many => "many",
            Self::Other => "other",
        }
    }

    /// Parse a CLDR keyword, or `None` if `s` is not one.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|c| c.as_str() == s)
    }
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One language's cardinal rules: which categories it can select, and how.
///
/// The two halves live together deliberately. `selectable` is what the ahead-of-time
/// generator uses to drop unreachable arms; `select` is what the runtime uses to pick
/// one. Holding them in the same row is what lets a test assert the pair is consistent —
/// that `select` never returns a category `selectable` omits — instead of leaving two
/// tables in two crates to drift, which is exactly what they did before this module.
struct Rules {
    /// The language subtag, lowercase.
    language: &'static str,
    /// The categories this language can select, in CLDR order.
    selectable: &'static [Category],
    /// The rule itself, over the absolute value of an integer count.
    select: fn(u64) -> Category,
}

/// CLDR cardinal rules for the twelve language subtags in `locales/config.json`.
///
/// Adding a locale there means adding a row here. Sources are the CLDR
/// `plurals.xml` cardinal rules; each `select` below is the integer specialization
/// (`v = 0`, so `i = n`) of that language's conditions, tried in CLDR order.
const RULES: &[Rules] = &[
    Rules {
        language: "ar",
        selectable: &[
            Category::Zero,
            Category::One,
            Category::Two,
            Category::Few,
            Category::Many,
            Category::Other,
        ],
        select: arabic,
    },
    Rules {
        language: "de",
        selectable: &[Category::One, Category::Other],
        select: exactly_one,
    },
    Rules {
        language: "en",
        selectable: &[Category::One, Category::Other],
        select: exactly_one,
    },
    Rules {
        language: "es",
        selectable: &[Category::One, Category::Many, Category::Other],
        select: romance_one,
    },
    Rules {
        language: "fr",
        selectable: &[Category::One, Category::Many, Category::Other],
        select: romance_zero_or_one,
    },
    Rules {
        language: "hi",
        selectable: &[Category::One, Category::Other],
        select: zero_or_one,
    },
    Rules {
        language: "it",
        selectable: &[Category::One, Category::Many, Category::Other],
        select: romance_one,
    },
    Rules {
        language: "ja",
        selectable: &[Category::Other],
        select: always_other,
    },
    Rules {
        language: "ko",
        selectable: &[Category::Other],
        select: always_other,
    },
    Rules {
        language: "pt",
        selectable: &[Category::One, Category::Many, Category::Other],
        select: romance_zero_or_one,
    },
    Rules {
        language: "ru",
        selectable: &[
            Category::One,
            Category::Few,
            Category::Many,
            Category::Other,
        ],
        select: russian,
    },
    Rules {
        language: "zh",
        selectable: &[Category::Other],
        select: always_other,
    },
];

/// The CLDR category `n` selects in `locale`.
///
/// `locale` may be a full tag (`pt-BR`, `zh-Hans`) — only its language subtag matters.
/// Selection uses the **absolute value** of `n`, as CLDR's `n` operand does, so `-1`
/// selects the same category as `1` and `i64::MIN` cannot overflow. A language with no
/// row in [`RULES`] yields [`Category::Other`], the category every language selects and
/// every well-formed ICU plural carries.
#[must_use]
pub fn category(locale: &str, n: i64) -> Category {
    rules(locale).map_or(Category::Other, |r| (r.select)(n.unsigned_abs()))
}

/// The categories `locale`'s language can actually select, in CLDR order, or `None` when
/// the language has no rules here.
///
/// `None` is a distinct answer from "only `other`" on purpose: the ahead-of-time
/// generator (`xtask i18n`) must refuse a locale it has no rules for rather than emit a
/// single-arm resource for a language that may well need six, while the runtime is happy
/// to fall back. Both callers read this one table.
#[must_use]
pub fn selectable(locale: &str) -> Option<&'static [Category]> {
    rules(locale).map(|r| r.selectable)
}

/// The rule row for `locale`'s language subtag, matched case-insensitively.
fn rules(locale: &str) -> Option<&'static Rules> {
    let language = locale.split(['-', '_']).next().unwrap_or(locale);
    RULES
        .iter()
        .find(|r| r.language.eq_ignore_ascii_case(language))
}

/// `one` for exactly 1 — English, German.
fn exactly_one(n: u64) -> Category {
    if n == 1 {
        Category::One
    } else {
        Category::Other
    }
}

/// `one` for 0 and 1 — Hindi (`i = 0 or n = 1`).
fn zero_or_one(n: u64) -> Category {
    if n <= 1 {
        Category::One
    } else {
        Category::Other
    }
}

/// Spanish, Italian: `one` for exactly 1, `many` for a non-zero multiple of a million.
fn romance_one(n: u64) -> Category {
    if n == 1 {
        Category::One
    } else {
        romance_many(n)
    }
}

/// French, Portuguese: as [`romance_one`], but 0 is also `one`.
fn romance_zero_or_one(n: u64) -> Category {
    if n <= 1 {
        Category::One
    } else {
        romance_many(n)
    }
}

/// The Romance `many`: `e = 0 and i != 0 and i % 1000000 = 0 and v = 0`.
///
/// This exists for the compact-decimal spellings ("2 millones de fotos"), but CLDR states
/// it over the *integer* operands, so a plain `2000000` selects it too — which is why the
/// generator lists `many` as selectable for these languages and why this is a real arm and
/// not a decorative one.
fn romance_many(n: u64) -> Category {
    if n != 0 && n.is_multiple_of(1_000_000) {
        Category::Many
    } else {
        Category::Other
    }
}

/// Russian: `one` for …1 except …11; `few` for …2-4 except …12-14; `many` otherwise.
fn russian(n: u64) -> Category {
    let last = n % 10;
    let last_two = n % 100;
    if last == 1 && last_two != 11 {
        Category::One
    } else if (2..=4).contains(&last) && !(12..=14).contains(&last_two) {
        Category::Few
    } else {
        Category::Many
    }
}

/// Arabic: `zero` 0, `one` 1, `two` 2, `few` …3-10, `many` …11-99, else `other`.
fn arabic(n: u64) -> Category {
    let last_two = n % 100;
    match n {
        0 => Category::Zero,
        1 => Category::One,
        2 => Category::Two,
        _ if (3..=10).contains(&last_two) => Category::Few,
        _ if (11..=99).contains(&last_two) => Category::Many,
        _ => Category::Other,
    }
}

/// `other` for every count — Japanese, Korean, Chinese.
fn always_other(_: u64) -> Category {
    Category::Other
}

#[cfg(test)]
mod tests {
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
}
