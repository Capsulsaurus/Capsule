use std::cell::Cell;

use super::{BracePair, Value, brace_pairs, format_message, format_message_in, matching_brace};

thread_local! {
    /// Bytes the brace scanners have read on this thread.
    ///
    /// The cost of pairing braces is the whole point of [`brace_pairs`], and a wall
    /// clock cannot pin it: this host runs several builds at once, so a timing budget
    /// would be either flaky or so loose it proves nothing. Counting the bytes the
    /// scanners actually read is deterministic, and it fails if anyone reintroduces a
    /// per-brace rescan. Thread-local because `cargo test` runs tests concurrently on
    /// threads (nextest gives each its own process, which is stricter still).
    static BRACE_SCAN_BYTES: Cell<usize> = const { Cell::new(0) };
}

/// Called by both brace scanners in a test build. Not compiled otherwise.
pub(super) fn record_brace_scan(bytes: usize) {
    BRACE_SCAN_BYTES.with(|counter| counter.set(counter.get() + bytes));
}

/// Bytes scanned since the last call, resetting the counter.
fn take_brace_scan_bytes() -> usize {
    BRACE_SCAN_BYTES.with(Cell::take)
}

/// The shape every plural in `locales/` has today.
const ITEMS: &str = "{count, plural, one {# item} other {# items}}";

#[test]
fn literal_passes_through() {
    assert_eq!(
        format_message("No data available", &[]),
        "No data available"
    );
}

#[test]
fn named_arg_is_substituted() {
    assert_eq!(
        format_message("Hello, {name}!", &[("name", Value::Str("World"))]),
        "Hello, World!"
    );
}

#[test]
fn integer_arg_is_formatted() {
    assert_eq!(
        format_message("{count} selected", &[("count", Value::Int(3))]),
        "3 selected"
    );
}

#[test]
fn missing_arg_keeps_placeholder() {
    assert_eq!(format_message("Hi {name}", &[]), "Hi {name}");
}

#[test]
fn whitespace_inside_placeholder_is_tolerated() {
    assert_eq!(
        format_message("Hi { name }", &[("name", Value::Str("Sam"))]),
        "Hi Sam"
    );
}

#[test]
fn a_plural_selects_its_arm_and_substitutes_the_count() {
    assert_eq!(format_message(ITEMS, &[("count", Value::Int(1))]), "1 item");
    assert_eq!(
        format_message(ITEMS, &[("count", Value::Int(7))]),
        "7 items"
    );
}

#[test]
fn a_plural_may_sit_inside_surrounding_text() {
    let template = "Deleted {count, plural, one {# photo} other {# photos}} today.";
    assert_eq!(
        format_message(template, &[("count", Value::Int(2))]),
        "Deleted 2 photos today."
    );
}

#[test]
fn an_exact_arm_beats_the_category_it_overlaps() {
    let template =
        "{count, plural, =0 {Nothing selected} =1 {Just this one} one {# item} other {# items}}";
    assert_eq!(
        format_message(template, &[("count", Value::Int(0))]),
        "Nothing selected"
    );
    assert_eq!(
        format_message(template, &[("count", Value::Int(1))]),
        "Just this one"
    );
    assert_eq!(
        format_message(template, &[("count", Value::Int(2))]),
        "2 items"
    );
}

#[test]
fn the_locale_chooses_the_arm() {
    // The whole point of threading a locale: Russian's three forms, English's two.
    let ru = "{count, plural, one {# файл} few {# файла} many {# файлов} other {# файла}}";
    assert_eq!(
        format_message_in("ru", ru, &[("count", Value::Int(1))]),
        "1 файл"
    );
    assert_eq!(
        format_message_in("ru", ru, &[("count", Value::Int(3))]),
        "3 файла"
    );
    assert_eq!(
        format_message_in("ru", ru, &[("count", Value::Int(5))]),
        "5 файлов"
    );
    assert_eq!(
        format_message_in("ru", ru, &[("count", Value::Int(21))]),
        "21 файл"
    );
    // French counts zero as singular; English does not.
    let zero = "{count, plural, one {# photo} other {# photos}}";
    assert_eq!(
        format_message_in("fr", zero, &[("count", Value::Int(0))]),
        "0 photo"
    );
    assert_eq!(
        format_message_in("en", zero, &[("count", Value::Int(0))]),
        "0 photos"
    );
    // A region subtag resolves to its language.
    assert_eq!(
        format_message_in("pt-BR", zero, &[("count", Value::Int(0))]),
        "0 photo"
    );
}

#[test]
fn a_category_the_message_omits_falls_back_to_other() {
    // Every translated plural in `locales/` carries only `one` and `other`, including
    // the Russian and Arabic ones. Falling back to `other` is what makes those render
    // at all — without it, `few` in Russian would have nowhere to go.
    assert_eq!(
        format_message_in("ru", ITEMS, &[("count", Value::Int(3))]),
        "3 items"
    );
    assert_eq!(
        format_message_in("ar", ITEMS, &[("count", Value::Int(0))]),
        "0 items"
    );
}

#[test]
fn an_arm_may_contain_placeholders_and_further_plurals() {
    let template = "{count, plural, one {{name} has # photo} \
                        other {{name} has # photos in {albums, plural, \
                        one {# album} other {# albums}}}}";
    assert_eq!(
        format_message(
            template,
            &[
                ("count", Value::Int(1)),
                ("albums", Value::Int(4)),
                ("name", Value::Str("Sam")),
            ]
        ),
        "Sam has 1 photo"
    );
    assert_eq!(
        format_message(
            template,
            &[
                ("count", Value::Int(9)),
                ("albums", Value::Int(4)),
                ("name", Value::Str("Sam")),
            ]
        ),
        // The inner `#` is the inner plural's count, not the outer one.
        "Sam has 9 photos in 4 albums"
    );
}

#[test]
fn a_hash_outside_a_plural_is_a_literal() {
    assert_eq!(
        format_message("Issue #{id}", &[("id", Value::Int(7))]),
        "Issue #7"
    );
}

#[test]
fn a_string_selector_is_parsed_as_a_number() {
    assert_eq!(
        format_message(ITEMS, &[("count", Value::Str("1"))]),
        "1 item"
    );
    // A non-numeric argument has no category, so it takes `other` — but `#` still
    // renders what the caller supplied rather than inventing a number.
    assert_eq!(
        format_message(ITEMS, &[("count", Value::Str("lots"))]),
        "lots items"
    );
}

#[test]
fn a_missing_selector_takes_other_and_keeps_the_gap_visible() {
    assert_eq!(format_message(ITEMS, &[]), "{count} items");
}

/// An ICU construct this runtime cannot evaluate is refused, not printed.
///
/// This test previously targeted `plural`, which is now evaluated; it is retargeted
/// onto `select` rather than deleted, because the property it pins is not about
/// plurals. Emitting a construct verbatim means the **user reads the message source**,
/// which is precisely what Android shipped for as long as its renderer lacked plural
/// support. A limitation that renders as output is not a limitation, it
/// is a defect.
///
/// `cfg(debug_assertions)`, which it was missing: the assertion is compiled out of a
/// release build, so under `cargo test --release` the test asserted a panic that
/// cannot happen and failed.
#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "cannot render the ICU construct")]
fn an_unrenderable_icu_construct_is_refused_in_debug_builds() {
    let template = "{kind, select, photo {Photo} other {Item}}";
    let _ = format_message(template, &[("kind", Value::Str("photo"))]);
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "cannot render the ICU construct")]
fn selectordinal_is_refused() {
    let template = "{n, selectordinal, one {#st} two {#nd} few {#rd} other {#th}}";
    let _ = format_message(template, &[("n", Value::Int(3))]);
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "cannot render the ICU construct")]
fn a_plural_with_an_offset_is_refused() {
    let template = "{count, plural, offset:1 one {# other} other {# others}}";
    let _ = format_message(template, &[("count", Value::Int(3))]);
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "has no `other` arm")]
fn a_plural_without_an_other_arm_is_refused() {
    let _ = format_message("{count, plural, one {# item}}", &[("count", Value::Int(5))]);
}

/// Release builds still pass the construct through rather than crashing on a catalog
/// they previously rendered badly — the assertion above is a developer-facing signal,
/// not a production behaviour change. Asserted on the shape the pass-through produces:
/// each `{…}` segment is reconstructed with its braces, so the output equals the input.
#[test]
#[cfg(not(debug_assertions))]
fn release_builds_pass_the_construct_through_unchanged() {
    for template in [
        "{kind, select, photo {Photo} other {Item}}",
        "{n, selectordinal, one {#st} other {#th}}",
        "{count, plural, offset:1 one {# other} other {# others}}",
    ] {
        assert_eq!(
            format_message(template, &[("count", Value::Int(2))]),
            template
        );
    }
}

/// A malformed plural still renders text in release: the first arm in CLDR order,
/// never nothing and never the message source.
#[test]
#[cfg(not(debug_assertions))]
fn release_builds_render_the_first_arm_of_a_plural_with_no_other() {
    assert_eq!(
        format_message("{count, plural, one {# item}}", &[("count", Value::Int(5))]),
        "5 item"
    );
}

#[test]
fn an_unterminated_brace_is_copied_through_without_asserting() {
    // Not an assertion case: the brace may be literal text in a message that never
    // meant to open a placeholder, and nothing distinguishes the two readings.
    assert_eq!(format_message("50% off {sale", &[]), "50% off {sale");
}

#[test]
fn an_unterminated_brace_does_not_swallow_what_follows_it() {
    // The brace is one character of output, not a decision to stop formatting: a
    // stray `{` must not silently blank every later argument.
    assert_eq!(
        format_message(
            "A { stray brace, then {name}",
            &[("name", Value::Str("Sam"))]
        ),
        "A { stray brace, then Sam"
    );
}

#[test]
fn a_closing_brace_with_no_opener_is_literal_text() {
    assert_eq!(format_message("100% } done", &[]), "100% } done");
}

/// A template nested far past [`MAX_DEPTH`] refuses instead of recursing.
///
/// Without the limit, recursion is bounded only by the input: measured on this
/// formatter, 50 000 levels aborts the process with `stack overflow`, which no caller
/// can catch. `format_message` is public, so the input need not be a catalog message.
fn deeply_nested_plural(depth: usize) -> String {
    format!(
        "{}#{}",
        "{count, plural, other {".repeat(depth),
        "}}".repeat(depth)
    )
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "cannot render the ICU construct")]
fn nesting_past_the_depth_limit_is_refused_rather_than_overflowing_the_stack() {
    let _ = format_message(&deeply_nested_plural(5_000), &[("count", Value::Int(2))]);
}

#[test]
#[cfg(not(debug_assertions))]
fn release_builds_pass_a_too_deeply_nested_plural_through() {
    let template = deeply_nested_plural(5_000);
    let rendered = format_message(&template, &[("count", Value::Int(2))]);
    assert!(rendered.ends_with("}}"), "the refusal is a pass-through");
}

#[test]
fn brace_pairs_agrees_with_matching_brace_at_every_open_brace() {
    // The refactor's correctness condition: pairing every brace in one pass must give
    // the same answer as asking about each brace on its own. The awkward shapes are
    // the point — an unmatched outer brace with a matched one inside (`{ {a} `) is
    // where a naive "if one fails they all fail" shortcut would be wrong.
    for template in [
        "",
        "no braces at all",
        "{a}",
        "{{a}}",
        "{",
        "}",
        "}{a}",
        "{a}}",
        "{ {a} ",
        "{a} } {b}",
        "{a, plural, one {# item} other {# items}}",
        "{a, plural, one {{name} has #} other {{name} has {b, plural, other {#}}}}",
        "50% off {sale",
        "A { stray brace, then {name}",
    ] {
        let pairs = brace_pairs(template);
        let opens: Vec<usize> = template
            .bytes()
            .enumerate()
            .filter(|(_, byte)| *byte == b'{')
            .map(|(index, _)| index)
            .collect();
        let expected: Vec<BracePair> = opens
            .iter()
            .map(|open| BracePair {
                open: *open,
                close: matching_brace(template, *open),
            })
            .collect();
        assert_eq!(pairs, expected, "disagreement on `{template}`");
    }
}

#[test]
fn an_unmatched_brace_costs_one_pass_not_one_per_brace() {
    // The defect: `render` asked `matching_brace` at every `{`, and an unmatched one
    // reads the whole remainder to learn it has no partner — n(n+1)/2 bytes over the
    // template, measured at 1.6 s for 80 000 braces in release. `format_message` is
    // public, so the input need not be a catalog message.
    let template = "{".repeat(100_000);
    take_brace_scan_bytes();
    let rendered = format_message(&template, &[]);
    let scanned = take_brace_scan_bytes();
    assert_eq!(rendered, template, "every unmatched brace is still emitted");
    assert!(
        scanned <= 4 * template.len(),
        "scanned {scanned} bytes for a {}-byte template: the per-brace rescan is back",
        template.len()
    );
}

#[test]
fn many_placeholders_cost_one_pass_too() {
    // The matched path, and the arm parser behind it: the scans must still add up to a
    // constant number of passes over the template, not one per placeholder.
    let template = "{name} ".repeat(10_000) + &"{n, plural, one {#} other {#}} ".repeat(1_000);
    take_brace_scan_bytes();
    let rendered = format_message(
        &template,
        &[("name", Value::Str("Sam")), ("n", Value::Int(2))],
    );
    let scanned = take_brace_scan_bytes();
    assert!(rendered.starts_with("Sam Sam "), "{rendered:.32}");
    assert!(rendered.ends_with("2 "), "{rendered:.32}");
    assert!(
        scanned <= 8 * template.len(),
        "scanned {scanned} bytes for a {}-byte template",
        template.len()
    );
}

#[test]
fn the_locale_free_entry_point_uses_english_rules() {
    // Asserted against English's actual answer, not against `format_message_in("en")`
    // — the latter is how `format_message` is implemented, so it would pass even if
    // the English rules were wrong.
    assert_eq!(
        format_message(ITEMS, &[("count", Value::Int(0))]),
        "0 items"
    );
    assert_eq!(format_message(ITEMS, &[("count", Value::Int(1))]), "1 item");
}

#[test]
fn a_string_count_renders_the_number_it_was_selected_by() {
    // Selection and `#` read the same value, so a padded or spaced string cannot
    // choose one arm and print another.
    assert_eq!(
        format_message(ITEMS, &[("count", Value::Str("007"))]),
        "7 items"
    );
    // Not a number once the spaces count: no category, so `other`, and `#` shows what
    // the caller actually passed.
    assert_eq!(
        format_message(ITEMS, &[("count", Value::Str(" 1 "))]),
        " 1  items"
    );
}

#[test]
fn a_duplicate_arm_resolves_the_same_way_for_both_arm_kinds() {
    // ICU rejects a duplicate arm; this runtime takes the first of each kind, so the
    // two kinds cannot disagree about which duplicate wins.
    assert_eq!(
        format_message(
            "{count, plural, other {first} other {second}}",
            &[("count", Value::Int(2))]
        ),
        "first"
    );
    assert_eq!(
        format_message(
            "{count, plural, =2 {first} =2 {second} other {o}}",
            &[("count", Value::Int(2))]
        ),
        "first"
    );
}

#[test]
fn an_arm_may_be_empty_or_contain_a_comma() {
    assert_eq!(
        format_message(
            "{count, plural, one {} other {one, two, many}}",
            &[("count", Value::Int(1))]
        ),
        ""
    );
    assert_eq!(
        format_message(
            "{count, plural, one {} other {one, two, many}}",
            &[("count", Value::Int(3))]
        ),
        "one, two, many"
    );
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "cannot render the ICU construct")]
fn an_empty_placeholder_is_refused() {
    let _ = format_message("{}", &[]);
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "cannot render the ICU construct")]
fn trailing_junk_after_the_last_arm_is_refused() {
    let _ = format_message(
        "{count, plural, other {items} junk}",
        &[("count", Value::Int(2))],
    );
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "cannot render the ICU construct")]
fn a_spaced_offset_is_refused_like_an_unspaced_one() {
    let _ = format_message(
        "{count, plural, offset : 1 other {# others}}",
        &[("count", Value::Int(3))],
    );
}
