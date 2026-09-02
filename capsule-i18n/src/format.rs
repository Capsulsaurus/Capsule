//! ICU MessageFormat formatting.
//!
//! # The supported subset
//!
//! - **Literal text**, copied through unchanged.
//! - **`{name}` interpolation** from the supplied arguments. An argument that was not
//!   supplied leaves its placeholder intact, braces and all, so the gap is visible in the
//!   output rather than silently empty.
//! - **`{name, plural, …}`**, with `=N` exact arms, CLDR category arms, `#` for the
//!   selected count, and arbitrary nesting: an arm may contain `{name}` placeholders and
//!   further plurals. Category selection comes from [`crate::plural`], so the arm chosen
//!   is the one the locale's CLDR rules select — the same arm the ahead-of-time
//!   generators lower into an Apple String Catalog variation or an Android `<plurals>`.
//!   A category the message does not carry falls back to `other`, which is what lets a
//!   catalog whose plurals are still English `one`/`other` copies render correctly in
//!   Russian or Arabic.
//!
//! ICU's apostrophe **quoting** (`'#'` for a literal `#`) is not implemented, here or in
//! the ahead-of-time generators, so a literal `#`, `{` or `}` inside a plural arm cannot
//! be escaped. No catalog message needs one, and an apostrophe that is not adjacent to ICU
//! syntax — "couldn't" — is ordinary text under ICU's own rule, so the catalogs are
//! unaffected.
//!
//! Numbers are rendered as plain digits — no grouping separators and no locale digit
//! shaping. That is a deliberate omission rather than an oversight: doing it properly is
//! `number` skeleton support, which needs CLDR number data this runtime does not carry.
//!
//! # What is still refused
//!
//! `select`, `selectordinal`, `plural` with `offset:`, a `plural` whose arms do not parse,
//! and any other `{name, kind, …}` block are **refused**: a `debug_assert!` fires where a
//! developer will see it, and the release build copies the construct through verbatim
//! rather than gaining a new crash on a catalog it could previously render badly. Emitting
//! ICU source to a user is the exact failure Android shipped before slice `S-I6`; the
//! refusal exists so the next construct this runtime cannot express is a test failure
//! instead.
//!
//! One malformed shape is **not** passed through verbatim: a `plural` whose arms parse but
//! carry no `other`. It asserts, and then renders its first arm in CLDR order. `xtask
//! i18n` refuses to generate such a message, so the case is unreachable from the catalogs;
//! for a hand-written template that slipped past it, some text beats message source.

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};

use crate::plural::{Category, category};

/// How deeply plurals may nest before the formatter refuses.
///
/// Recursion is otherwise bounded only by the template, and this formatter is public: a
/// template from outside the catalogs could nest thousands deep and overflow the stack,
/// which is an abort no caller can catch. Catalog messages are flat — the deepest today is
/// one plural holding one `{name}` — so the limit is far above anything real.
const MAX_DEPTH: usize = 32;

/// A value substituted into a `{name}` placeholder.
#[derive(Debug, Clone, Copy)]
pub enum Value<'a> {
    /// A string argument.
    Str(&'a str),
    /// An integer argument.
    Int(i64),
}

impl fmt::Display for Value<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Str(s) => f.write_str(s),
            Value::Int(n) => write!(f, "{n}"),
        }
    }
}

/// Format `template` with English plural rules.
///
/// The locale-free entry point, for a template that did not come from a bundle. Use
/// [`format_message_in`] — or [`crate::Bundle::format`], which does — whenever the
/// message's locale is known, because the plural arm a count selects depends on it.
#[must_use]
pub fn format_message(template: &str, args: &[(&str, Value<'_>)]) -> String {
    format_message_in("en", template, args)
}

/// Format `template` for `locale`, substituting `args`.
///
/// `locale` may be a full tag (`pt-BR`); only its language subtag reaches the plural
/// rules. See the module docs for the supported ICU subset and for what a construct
/// outside it does.
#[must_use]
pub fn format_message_in(locale: &str, template: &str, args: &[(&str, Value<'_>)]) -> String {
    let mut out = String::with_capacity(template.len());
    render(locale, template, args, None, 0, &mut out);
    out
}

/// Render `template` into `out`.
///
/// `hash` is the text `#` stands for — the enclosing plural's count, or `None` at the top
/// level, where ICU treats `#` as an ordinary character.
fn render(
    locale: &str,
    template: &str,
    args: &[(&str, Value<'_>)],
    hash: Option<&str>,
    depth: usize,
    out: &mut String,
) {
    // Every `{` is paired in one pass up front, rather than by scanning forward from each
    // one. Scanning per brace is quadratic on the unmatched path — an unmatched `{` has to
    // read the whole remainder to learn there is no partner, and then the next one reads
    // it again: measured at n(n+1)/2 bytes, 1.6 s for a template of 80 000 braces. This is
    // a `pub` entry point, so that is the same threat model `MAX_DEPTH` is capped for.
    let pairs = brace_pairs(template);
    let mut next_pair = 0;
    let mut i = 0;
    while let Some(c) = template[i..].chars().next() {
        match c {
            '#' => {
                match hash {
                    Some(count) => out.push_str(count),
                    None => out.push('#'),
                }
                i += 1;
            }
            '{' => {
                // `pairs` lists the braces in source order and `i` only moves forward, so
                // this brace is the entry the cursor is on.
                debug_assert_eq!(pairs.get(next_pair).map(|pair| pair.open), Some(i));
                let close = pairs.get(next_pair).and_then(|pair| pair.close);
                next_pair += 1;
                let Some(close) = close else {
                    // An unterminated `{` is copied through as an ordinary character, with
                    // no assertion: the brace may well be literal text in a message that
                    // never meant to open a placeholder, and nothing distinguishes the two
                    // readings. Scanning **continues** past it — abandoning the remainder
                    // would let one stray brace silently swallow every later placeholder.
                    out.push('{');
                    i += 1;
                    continue;
                };
                render_placeholder(locale, &template[i + 1..close], args, depth, out);
                i = close + 1;
                // The braces inside the body were handled by the recursive render.
                while pairs.get(next_pair).is_some_and(|pair| pair.open < i) {
                    next_pair += 1;
                }
            }
            _ => {
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
}

/// One `{` and the `}` that closes it, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BracePair {
    /// Byte index of the `{`.
    open: usize,
    /// Byte index of the matching `}`, or `None` when the brace is unterminated.
    close: Option<usize>,
}

/// Pair up every `{` in `text` in a single pass, in source order.
///
/// A stack of open braces, so the answer for *all* of them costs one scan of the text
/// rather than one scan per brace. Equivalent to calling [`matching_brace`] at each `{` —
/// pinned by a test — and the reason [`render`] no longer does.
///
/// A `}` with nothing open is ignored, exactly as [`matching_brace`]'s depth counter
/// ignores it: it cannot close a brace that is not there, and ICU has no escape for one.
fn brace_pairs(text: &str) -> Vec<BracePair> {
    #[cfg(test)]
    tests::record_brace_scan(text.len());
    let mut pairs: Vec<BracePair> = Vec::new();
    // Indices into `pairs`, innermost last.
    let mut open = Vec::new();
    for (index, byte) in text.as_bytes().iter().enumerate() {
        match byte {
            b'{' => {
                open.push(pairs.len());
                pairs.push(BracePair {
                    open: index,
                    close: None,
                });
            }
            b'}' => {
                if let Some(slot) = open.pop() {
                    pairs[slot].close = Some(index);
                }
            }
            _ => {}
        }
    }
    pairs
}

/// Render one `{…}` placeholder body (the text between the braces) into `out`.
fn render_placeholder(
    locale: &str,
    body: &str,
    args: &[(&str, Value<'_>)],
    depth: usize,
    out: &mut String,
) {
    let name = body.trim();
    if is_identifier(name) {
        if let Some((_, value)) = args.iter().find(|(key, _)| *key == name) {
            let _ = write!(out, "{value}");
        } else {
            // Unknown arg: keep the placeholder so the missing value is obvious.
            let _ = write!(out, "{{{body}}}");
        }
        return;
    }

    let rendered = render_plural(locale, body, args, depth);
    // Not a hard panic: a release build must not gain a new crash on a catalog it could
    // previously render badly, and every test and debug run is a build where this fires.
    // The pass-through below is what production still does — and what makes the construct
    // a *developer's* problem instead of a user's.
    debug_assert!(
        rendered.is_some(),
        "capsule-i18n cannot render the ICU construct `{{{body}}}` — it would be printed \
         to the user verbatim. A `plural` whose arms parse is evaluated here; `select`, \
         `selectordinal`, `offset:`, a `plural` whose arms do not, and nesting past \
         {MAX_DEPTH} levels are not, because the per-platform renderers compile those \
         ahead of time (`xtask i18n`) and this runtime has no equivalent."
    );
    if let Some(text) = rendered {
        out.push_str(&text);
    } else {
        out.push('{');
        out.push_str(body);
        out.push('}');
    }
}

/// Evaluate `body` as a `name, plural, …` block, or `None` if it is not one this runtime
/// can render.
fn render_plural(
    locale: &str,
    body: &str,
    args: &[(&str, Value<'_>)],
    depth: usize,
) -> Option<String> {
    if depth >= MAX_DEPTH {
        return None;
    }
    // Two splits only, so a comma inside an arm belongs to the arm.
    let mut head = body.splitn(3, ',');
    let selector = head.next()?.trim();
    if !is_identifier(selector) || head.next()?.trim() != "plural" {
        return None;
    }
    let arms_source = head.next()?.trim();
    if arms_source.starts_with("offset:") {
        return None;
    }
    let arms = Arms::parse(arms_source)?;

    let argument = args
        .iter()
        .find(|(key, _)| *key == selector)
        .map(|(_, v)| v);
    // The count as a number, for selection. A missing argument — or a string that is not
    // a number — has no category, so it takes `other`, the arm every message carries.
    let count = argument.and_then(|value| match value {
        Value::Int(n) => Some(*n),
        Value::Str(s) => s.parse::<i64>().ok(),
    });
    // The count as text, for `#`. Whenever a number was found this is that number, so `#`
    // shows exactly the value the arm was selected by (`"007"` selects on 7 and renders
    // `7`). A missing argument keeps its placeholder visible, as a missing `{name}` does.
    let hash = count.map_or_else(
        || argument.map_or_else(|| format!("{{{selector}}}"), ToString::to_string),
        |n| n.to_string(),
    );

    let chosen = arms.select(locale, count);
    // A plural with no `other` is malformed: `xtask i18n` refuses to generate one, and
    // both native resource formats treat it as invalid. Render the first arm in CLDR
    // order rather than nothing, so a hand-written template still produces text.
    debug_assert!(
        chosen.is_some(),
        "ICU plural `{{{body}}}` has no `other` arm; every locale selects `other` for \
         some count, so the message cannot render for all inputs."
    );
    let arm = chosen.or_else(|| arms.first()).unwrap_or_default();

    let mut out = String::new();
    render(locale, arm, args, Some(&hash), depth + 1, &mut out);
    Some(out)
}

/// The arms of one `plural` block: exact `=N` matches and CLDR category matches.
struct Arms<'a> {
    /// `=N {…}` arms, in source order. ICU tries these before any category.
    exact: Vec<(i64, &'a str)>,
    /// Category arms, keyed so iteration is in CLDR order.
    categories: BTreeMap<Category, &'a str>,
}

impl<'a> Arms<'a> {
    /// Parse `source`, the text after `name, plural,`, or `None` if it is malformed.
    fn parse(source: &'a str) -> Option<Self> {
        let mut exact = Vec::new();
        let mut categories = BTreeMap::new();
        let mut rest = source;
        loop {
            let trimmed = rest.trim_start();
            if trimmed.is_empty() {
                break;
            }
            let open = trimmed.find('{')?;
            let close = matching_brace(trimmed, open)?;
            let selector = trimmed[..open].trim();
            let arm = &trimmed[open + 1..close];
            if let Some(literal) = selector.strip_prefix('=') {
                exact.push((literal.trim().parse().ok()?, arm));
            } else {
                // First wins, matching the `=N` arms' `find`. ICU rejects a duplicate
                // arm outright; the two kinds resolving it differently would be worse
                // than either answer.
                categories.entry(Category::parse(selector)?).or_insert(arm);
            }
            rest = &trimmed[close + 1..];
        }
        (!exact.is_empty() || !categories.is_empty()).then_some(Self { exact, categories })
    }

    /// The arm `count` selects in `locale`: an exact `=N` match, then the CLDR category,
    /// then `other`.
    fn select(&self, locale: &str, count: Option<i64>) -> Option<&'a str> {
        if let Some(n) = count {
            if let Some((_, arm)) = self.exact.iter().find(|(value, _)| *value == n) {
                return Some(arm);
            }
            if let Some(arm) = self.categories.get(&category(locale, n)) {
                return Some(arm);
            }
        }
        self.categories.get(&Category::Other).copied()
    }

    /// The first arm in CLDR order, for a malformed message with no `other`.
    fn first(&self) -> Option<&'a str> {
        self.categories
            .values()
            .next()
            .copied()
            .or_else(|| self.exact.first().map(|(_, arm)| *arm))
    }
}

/// The byte index of the `}` matching the `{` at `open`, or `None` if it is unterminated.
///
/// Brace *matching*, not the first `}`: every ICU plural nests braces, and a scan that
/// stops at the first one cannot see past the opening arm — the same defect that let
/// Android's renderer ship raw ICU (slice `S-I6`).
///
/// Used by [`Arms::parse`], where the arms are disjoint so the scans add up to one pass
/// over the body. [`render`] uses [`brace_pairs`] instead, because there the same brace
/// can be asked about repeatedly.
fn matching_brace(text: &str, open: usize) -> Option<usize> {
    #[cfg(test)]
    tests::record_brace_scan(text.len() - open);
    debug_assert_eq!(text.as_bytes().get(open), Some(&b'{'));
    let mut depth = 0usize;
    for (offset, byte) in text.as_bytes()[open..].iter().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + offset);
                }
            }
            _ => {}
        }
    }
    None
}

/// Whether `s` is a non-empty ASCII identifier (letters, digits, underscore).
fn is_identifier(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
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
        let template = "{count, plural, =0 {Nothing selected} =1 {Just this one} one {# item} other {# items}}";
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
    /// support (slice `S-I6`). A limitation that renders as output is not a limitation, it
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
}
