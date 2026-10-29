use std::collections::BTreeMap;

// A glob: this module tests most of the file, and naming each item was a
// running edit every time a renderer gained a helper.
use super::*;

#[test]
fn android_qualifier_uses_bcp47_for_script_subtags() {
    // A script subtag has no legacy spelling: `values-zh-rHANS` is what the
    // old mapping produced and `aapt2` rejects it as an invalid resource
    // directory name, failing the entire Android build.
    assert_eq!(android_qualifier("zh-Hans"), "b+zh+Hans");
    assert_eq!(android_qualifier("zh-Hant"), "b+zh+Hant");
}

#[test]
fn android_qualifier_uses_the_legacy_form_for_regions() {
    assert_eq!(android_qualifier("pt-BR"), "pt-rBR");
    assert_eq!(android_qualifier("en-us"), "en-rUS");
}

#[test]
fn android_qualifier_passes_bare_languages_through() {
    assert_eq!(android_qualifier("fr"), "fr");
    assert_eq!(android_qualifier("ar"), "ar");
}

/// The two mappings are a pair: a qualifier the inverse cannot read is a
/// directory the tests would mis-attribute rather than fail on.
#[test]
fn every_supported_locale_round_trips_through_its_android_qualifier() {
    for locale in ["en", "fr", "ar", "pt-BR", "zh-Hans", "zh-Hant"] {
        assert_eq!(android_locale(&android_qualifier(locale)), locale);
    }
}

#[test]
fn script_subtags_are_four_letters_regions_are_not() {
    assert!(is_script_subtag("Hans"));
    assert!(is_script_subtag("Latn"));
    assert!(!is_script_subtag("BR"));
    assert!(!is_script_subtag("419"));
}

/// XML forbids `--` inside a comment, and aapt2 enforces it: a banner
/// containing one fails the entire Android resource merge, not just the file.
#[test]
fn android_documents_contain_no_double_hyphen_in_comments() {
    let mut catalog = BTreeMap::new();
    catalog.insert("app_name".to_string(), "Capsule".to_string());
    catalog.insert(
        "plural_key".to_string(),
        "{count, plural, other {# photos}}".to_string(),
    );
    let catalogs = Catalogs {
        source: "en".to_string(),
        supported: vec!["en".to_string()],
        messages: BTreeMap::from([("en".to_string(), catalog.clone())]),
    };
    let xml = catalogs.android_xml("en", &catalog).expect("renders");
    for line in xml.lines() {
        let Some(body) = line.trim().strip_prefix("<!--") else {
            continue;
        };
        let body = body.trim_end_matches("-->");
        assert!(
            !body.contains("--"),
            "XML comment contains a forbidden `--`: {line}"
        );
    }
}

/// The per-app language list names every supported locale, in order, as a BCP-47 tag,
/// and its banner obeys the same no-`--` rule as every other Android document.
#[test]
fn android_locale_config_lists_every_supported_locale() {
    let catalogs = Catalogs {
        source: "en".to_string(),
        supported: owned(&["en", "zh-Hant", "pt-BR"]),
        messages: BTreeMap::new(),
    };
    let xml = catalogs.android_locale_config();
    let names: Vec<&str> = xml
        .lines()
        .filter_map(|line| line.trim().strip_prefix("<locale android:name=\""))
        .filter_map(|rest| rest.strip_suffix("\" />"))
        .collect();
    assert_eq!(names, ["en", "zh-Hant", "pt-BR"]);
    assert!(xml.contains("<locale-config xmlns:android="));
    for line in xml.lines() {
        if let Some(body) = line.trim().strip_prefix("<!--") {
            assert!(!body.trim_end_matches("-->").contains("--"), "{line}");
        }
    }
}

/// `locales_config.xml` is one of the rendered outputs, so `i18n --check` fails when
/// it drifts from `config.json`.
#[test]
fn android_locale_config_is_a_generated_output() {
    let catalogs = Catalogs::load(&repo_root()).expect("the real catalogs load");
    let outputs = catalogs.render().expect("the real catalogs compile");
    let (_, xml) = outputs
        .iter()
        .find(|(path, _)| path == Path::new(ANDROID_LOCALE_CONFIG))
        .expect("locales_config.xml is rendered");
    assert_eq!(xml, &catalogs.android_locale_config());
}

const PREFIX: &str = "pub(crate) const SUPPORTED_LOCALES: &[&str] = ";

fn owned(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

/// A short list (the source-only build) stays on one line — matching rustfmt and
/// the historical single-locale output.
#[test]
fn single_line_when_it_fits() {
    let out = str_slice_const(PREFIX, &owned(&["en"]));
    assert_eq!(out, format!("{PREFIX}&[\"en\"];"));
    assert_eq!(out.lines().count(), 1);
}

/// The full twelve-locale set overflows `max_width`, so it breaks the brackets but
/// keeps every element on one indented line — the exact shape rustfmt produces.
#[test]
fn block_form_when_elements_fit_on_one_indented_line() {
    let out = str_slice_const(
        PREFIX,
        &owned(&[
            "en", "zh-Hans", "zh-Hant", "ja", "ko", "fr", "de", "es", "pt-BR", "it", "ru", "hi",
            "ar",
        ]),
    );
    assert_eq!(
        out,
        format!(
            "{PREFIX}&[\n    \"en\", \"zh-Hans\", \"zh-Hant\", \"ja\", \"ko\", \"fr\", \"de\", \
                 \"es\", \"pt-BR\", \"it\", \"ru\", \"hi\", \"ar\",\n];"
        )
    );
    // Every rendered line stays within rustfmt's max_width.
    assert!(out.lines().all(|l| l.len() <= 100));
}

/// If even the single indented line would overflow, fall back to one per line.
#[test]
fn one_per_line_when_too_wide() {
    let items = owned(&[
        "aa-very-long-locale-tag",
        "bb-very-long-locale-tag",
        "cc-very-long-locale-tag",
        "dd-very-long-locale-tag",
        "ee-very-long-locale-tag",
    ]);
    let out = str_slice_const(PREFIX, &items);
    assert!(out.contains("\n    \"aa-very-long-locale-tag\",\n"));
    assert!(out.lines().all(|l| l.len() <= 100));
}

// ── ICU -> Apple String Catalog compilation ───────────────────────────────
//
// A String Catalog is not an ICU consumer: whatever these emit is what the user
// reads. Each case pins one shape the catalogs actually author.

/// Compile one message the way [`Catalogs::string_catalog`] does: plan from the
/// source text, then render.
fn compile(source: &str) -> CompiledMessage {
    compile_as(source, APPLE)
}

/// Compile one message in an arbitrary dialect, planning from its own text.
fn compile_as(source: &str, dialect: Dialect) -> CompiledMessage {
    let plan = ArgPlan::of(source).expect("source message plans");
    CompiledMessage::compile(source, &plan, dialect).expect("source message compiles")
}

#[test]
fn literal_text_passes_through_unchanged() {
    assert_eq!(compile("Recover"), CompiledMessage::Unit("Recover".into()));
}

#[test]
fn a_single_argument_becomes_an_unnumbered_string_specifier() {
    assert_eq!(
        compile("Clear {facet}"),
        CompiledMessage::Unit("Clear %@".into())
    );
}

#[test]
fn several_arguments_are_numbered_so_a_translation_may_reorder_them() {
    assert_eq!(
        compile("{done} of {total} uploaded"),
        CompiledMessage::Unit("%1$@ of %2$@ uploaded".into())
    );
}

#[test]
fn a_translation_inherits_the_source_argument_numbering() {
    let plan = ArgPlan::of("{done} of {total} uploaded").expect("plans");
    // A locale that reorders the two arguments must still address them by their
    // source positions, or the platform formatter reads the wrong argument.
    assert_eq!(
        CompiledMessage::compile("{total} ← {done}", &plan, APPLE).expect("compiles"),
        CompiledMessage::Unit("%2$@ ← %1$@".into())
    );
}

#[test]
fn a_literal_percent_is_escaped() {
    assert_eq!(
        compile("Uploading… {percent}%"),
        CompiledMessage::Unit("Uploading… %@%%".into())
    );
}

#[test]
fn a_whole_message_plural_becomes_a_plural_variation() {
    let CompiledMessage::Plural(categories) =
        compile("{count, plural, one {# Photo} other {# Photos}}")
    else {
        panic!("expected a plural variation");
    };
    assert_eq!(categories["one"], "%lld Photo");
    assert_eq!(categories["other"], "%lld Photos");
}

#[test]
fn a_plural_selector_is_typed_as_an_integer_not_a_string() {
    // Swift interpolates an `Int` as `%lld`; a `%@` here would mis-read the va_list.
    let plan = ArgPlan::of("{count, plural, one {# item} other {# items}}").expect("plans");
    assert_eq!(
        plan.specifier("count", APPLE).expect("known argument"),
        "%lld"
    );
}

#[test]
fn an_embedded_plural_is_rejected_rather_than_emitted_verbatim() {
    let err = ArgPlan::of("Delete {count, plural, one {# item} other {# items}} now?")
        .expect_err("an embedded block has no Apple equivalent");
    assert!(format!("{err}").contains("embedded ICU block"), "{err}");
}

#[test]
fn select_blocks_are_rejected() {
    let err = ArgPlan::of("{gender, select, male {He} other {They}}")
        .expect_err("`select` does not compile to a String Catalog");
    assert!(format!("{err}").contains("unsupported ICU block"), "{err}");
}

#[test]
fn a_plural_without_other_is_rejected() {
    let err = ArgPlan::of("{count, plural, one {# item}}")
        .expect_err("Apple requires the `other` category");
    assert!(format!("{err}").contains("`other`"), "{err}");
}

#[test]
fn an_explicit_value_category_is_rejected() {
    let err = ArgPlan::of("{count, plural, =0 {None} other {# items}}")
        .expect_err("`=N` is not a CLDR category");
    assert!(format!("{err}").contains("category `=0`"), "{err}");
}

#[test]
fn plural_variations_carry_only_categories_a_string_catalog_understands() {
    let json = compile("{count, plural, other {# items} one {# item}}").into_localization();
    let emitted: Vec<&str> = json["variations"]["plural"]
        .as_object()
        .expect("plural object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(emitted, ["one", "other"]);
    assert!(emitted.iter().all(|c| PLURAL_CATEGORIES.contains(c)));
}

// ── ICU -> Android resource compilation ───────────────────────────────────
//
// Android resources are not an ICU consumer either, and the guard this replaces
// (`\{[^{}]*,[^{}]*\}`) could not match a plural — `[^{}]*` cannot span the nested
// braces every plural contains — so it never fired once and every shape below
// shipped its ICU source verbatim to the user. The cases, in order:
//
//  1. a whole-message plural becomes `<plurals>` with per-category `<item>`s
//  2. `#` in a plural arm becomes the quantity substitution Android expects
//  3. `{name}` becomes an Android format specifier
//  4. two or more arguments are numbered, from the source locale
//  5. a translation inherits the source numbering even when it reorders
//  6. a literal `%` is escaped in a formatted value, left alone in a bare one
//  7. an arm the locale cannot select is dropped (`one` in Japanese)
//  8. ... and likewise `few` authored for English
//  9. a locale with no `capsule_i18n::plural` row fails rather than guessing
// 10. embedded plural, `select`, and `offset:` fail the renderer
// 11. Android quoting: apostrophe, quote, backslash, XML, a leading `@`/`?`
// 12. end to end: no committed Android value contains ICU syntax

/// A [`Catalogs`] over one key: `entries` is `(locale, message)`, source `en`.
fn catalogs(key: &str, entries: &[(&str, &str)]) -> Catalogs {
    let mut messages = BTreeMap::new();
    for (locale, message) in entries {
        let mut map = BTreeMap::new();
        map.insert(key.to_string(), (*message).to_string());
        messages.insert((*locale).to_string(), map);
    }
    Catalogs {
        source: "en".to_string(),
        supported: entries.iter().map(|(l, _)| (*l).to_string()).collect(),
        messages,
    }
}

/// Render one locale's `strings.xml` for a single-key catalog.
fn android_xml(key: &str, entries: &[(&str, &str)], locale: &str) -> String {
    let catalogs = catalogs(key, entries);
    let map = catalogs
        .messages
        .get(locale)
        .expect("locale is in the catalog");
    catalogs.android_xml(locale, map).expect("renders")
}

/// Render the source locale of a one-message, one-locale catalog.
fn android_en(message: &str) -> String {
    android_xml("k", &[("en", message)], "en")
}

/// The error from rendering a message the Android renderer must refuse.
fn android_err(message: &str) -> String {
    let catalogs = catalogs("k", &[("en", message)]);
    let map = catalogs
        .messages
        .get("en")
        .expect("locale is in the catalog");
    let err = catalogs
        .android_xml("en", map)
        .expect_err("this shape has no faithful Android form");
    format!("{err:#}")
}

#[test]
fn a_whole_message_plural_becomes_a_plurals_resource() {
    let xml = android_en("{count, plural, one {# item} other {# items}}");
    assert!(xml.contains("<plurals name=\"k\">"), "{xml}");
    assert!(
        xml.contains("<item quantity=\"one\">%d item</item>"),
        "{xml}"
    );
    assert!(
        xml.contains("<item quantity=\"other\">%d items</item>"),
        "{xml}"
    );
    // A plural is never also a flat <string> — that is the shape that shipped ICU.
    assert!(!xml.contains("<string name=\"k\">"), "{xml}");
}

#[test]
fn a_plural_selector_is_the_first_argument_the_call_site_passes() {
    // `getQuantityString(id, count, count)`: the count is argument 1, typed `%d`.
    let plan = ArgPlan::of("{count, plural, one {# item} other {# items}}").expect("plans");
    assert_eq!(
        plan.specifier("count", ANDROID).expect("known argument"),
        "%d"
    );
}

#[test]
fn a_plural_arm_may_carry_its_own_arguments_after_the_count() {
    let xml = android_en("{count, plural, one {# of {total}} other {# of {total}}}");
    assert!(
        xml.contains("<item quantity=\"other\">%1$d of %2$s</item>"),
        "{xml}"
    );
}

#[test]
fn a_named_placeholder_becomes_an_android_format_specifier() {
    assert!(android_en("Clear {facet}").contains("<string name=\"k\">Clear %s</string>"));
}

#[test]
fn several_android_arguments_are_numbered_so_a_translation_may_reorder_them() {
    let xml = android_en("{done} of {total} uploaded");
    assert!(
        xml.contains("<string name=\"k\">%1$s of %2$s uploaded</string>"),
        "{xml}"
    );
}

#[test]
fn an_android_translation_inherits_the_source_argument_numbering() {
    // Android formats by position, so a locale that reorders the two arguments must
    // still address them by their *source* positions.
    let xml = android_xml(
        "k",
        &[
            ("en", "{done} of {total} uploaded"),
            ("de", "{total} ← {done}"),
        ],
        "de",
    );
    assert!(
        xml.contains("<string name=\"k\">%2$s ← %1$s</string>"),
        "{xml}"
    );
}

#[test]
fn a_literal_percent_is_escaped_in_a_formatted_value() {
    let xml = android_en("Uploading… {percent}%");
    assert!(
        xml.contains("<string name=\"k\">Uploading… %s%%</string>"),
        "{xml}"
    );
}

#[test]
fn a_literal_percent_is_left_alone_when_there_is_nothing_to_format() {
    // `getString(id)` with no arguments returns the resource verbatim, so an escaped
    // `%%` here would put two percent signs on screen.
    let xml = android_en("100% offline");
    assert!(
        xml.contains("<string name=\"k\">100% offline</string>"),
        "{xml}"
    );
}

#[test]
fn an_arm_the_locale_cannot_select_is_dropped() {
    // Japanese has one CLDR category, `other`; a `one` arm can never be selected.
    let xml = android_xml(
        "k",
        &[
            ("en", "{count, plural, one {# item} other {# items}}"),
            ("ja", "{count, plural, one {#件} other {#件}}"),
        ],
        "ja",
    );
    assert!(
        xml.contains("<item quantity=\"other\">%d件</item>"),
        "{xml}"
    );
    assert!(!xml.contains("quantity=\"one\""), "{xml}");
}

#[test]
fn a_category_english_cannot_select_is_dropped() {
    let xml = android_en("{count, plural, one {# item} few {# items} other {# items}}");
    assert!(xml.contains("quantity=\"one\""), "{xml}");
    assert!(xml.contains("quantity=\"other\""), "{xml}");
    assert!(!xml.contains("quantity=\"few\""), "{xml}");
}

#[test]
fn a_locale_with_no_plural_rules_row_fails_rather_than_guessing() {
    let catalogs = catalogs(
        "k",
        &[("en", "{count, plural, one {# item} other {# items}}")],
    );
    let map = catalogs
        .messages
        .get("en")
        .expect("locale is in the catalog");
    let err = catalogs
        .android_xml("xx", map)
        .expect_err("an unknown language has no rules to filter by");
    assert!(
        format!("{err:#}").contains("capsule_i18n::plural"),
        "{err:#}"
    );
}

#[test]
fn arms_are_emitted_in_cldr_order_not_catalog_order() {
    let xml = android_en("{count, plural, other {# items} one {# item}}");
    let one = xml.find("quantity=\"one\"").expect("one arm");
    let other = xml.find("quantity=\"other\"").expect("other arm");
    assert!(one < other, "{xml}");
}

#[test]
fn an_embedded_plural_fails_the_android_renderer_rather_than_shipping_icu() {
    // The exact shape the old regex guard could not match.
    let err = android_err("Delete {count, plural, one {# item} other {# items}} now?");
    assert!(err.contains("embedded ICU block"), "{err}");
}

#[test]
fn a_select_block_fails_the_android_renderer() {
    let err = android_err("{gender, select, male {He} other {They}}");
    assert!(err.contains("unsupported ICU block"), "{err}");
}

#[test]
fn a_plural_offset_fails_the_android_renderer() {
    let err = android_err("{count, plural, offset:1 one {# other} other {# others}}");
    assert!(err.contains("offset:"), "{err}");
}

#[test]
fn an_android_plural_without_other_is_rejected() {
    let err = android_err("{count, plural, one {# item}}");
    assert!(err.contains("`other`"), "{err}");
}

#[test]
fn android_quoting_covers_xml_and_the_platforms_own_rules() {
    assert_eq!(android_escape("Don't"), "Don\\'t");
    assert_eq!(android_escape("say \"hi\""), "say \\\"hi\\\"");
    assert_eq!(android_escape("a & b < c > d"), "a &amp; b &lt; c &gt; d");
    assert_eq!(android_escape("back\\slash"), "back\\\\slash");
    assert_eq!(android_escape("line\nbreak"), "line\\nbreak");
    // A leading `@` or `?` is a resource / attribute reference unless escaped.
    assert_eq!(android_escape("@home"), "\\@home");
    assert_eq!(android_escape("?maybe"), "\\?maybe");
    // ... but only in the first position.
    assert_eq!(android_escape("me@home"), "me@home");
}

#[test]
fn a_format_specifier_survives_escaping() {
    // `%` must not be touched by the escaper — `format_string` already decided.
    assert_eq!(android_escape("%1$s of %2$s"), "%1$s of %2$s");
}

/// The repository root, from which the real catalogs load.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask/ has a parent")
        .to_path_buf()
}

/// Every rendered Android `strings.xml`, as `(path, content)`.
fn rendered_android() -> Vec<(PathBuf, String)> {
    let catalogs = Catalogs::load(&repo_root()).expect("the real catalogs load");
    catalogs
        .render()
        .expect("the real catalogs compile")
        .into_iter()
        .filter(|(path, _)| {
            path.to_string_lossy().contains("androidMain")
                && path.file_name().is_some_and(|name| name == "strings.xml")
        })
        .collect()
}

#[test]
fn no_android_resource_ships_icu_syntax_to_a_user() {
    let rendered = rendered_android();
    assert!(!rendered.is_empty(), "no Android output was rendered");
    for (path, xml) in &rendered {
        for (number, line) in xml.lines().enumerate() {
            assert!(
                !line.contains("plural,") && !line.contains('{'),
                "{}:{}: ICU syntax reached a user-visible resource: {line}",
                path.display(),
                number + 1,
            );
        }
    }
}

#[test]
fn the_plural_keys_render_as_plurals_resources() {
    for (path, xml) in &rendered_android() {
        assert!(
            xml.contains("<plurals name=\"common_item_count\">"),
            "{}: the plural catalog keys must reach <plurals>",
            path.display(),
        );
    }
}

/// The `quantity` attribute of every `<item>` in one rendered `strings.xml`.
fn android_quantities(xml: &str) -> Vec<&str> {
    xml.lines()
        .filter(|line| line.contains("<item quantity="))
        .map(|line| {
            line.split("quantity=\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .expect("a quantity attribute")
        })
        .collect()
}

/// The locale a rendered Android resource path belongs to.
fn android_path_locale(path: &Path) -> String {
    let dir = path
        .parent()
        .and_then(Path::file_name)
        .map(|d| d.to_string_lossy().into_owned())
        .expect("a values directory");
    // `values/` is the source locale; `values-<qualifier>/` names its own.
    dir.strip_prefix("values-")
        .map_or_else(|| "en".to_string(), android_locale)
}

/// The `<item quantity=…>` arms of one `<plurals name=…>` block in a rendered
/// `strings.xml`, keyed by quantity.
fn android_plural_block(xml: &str, name: &str) -> BTreeMap<String, String> {
    let mut arms = BTreeMap::new();
    let open = format!("<plurals name=\"{name}\">");
    let Some(start) = xml.find(&open) else {
        return arms;
    };
    for line in xml[start..].lines().skip(1) {
        if line.contains("</plurals>") {
            break;
        }
        let Some((quantity, rest)) = line
            .split_once("quantity=\"")
            .and_then(|(_, rest)| rest.split_once("\">"))
        else {
            continue;
        };
        let body = rest.trim_end().trim_end_matches("</item>");
        arms.insert(quantity.to_string(), body.to_string());
    }
    arms
}

/// The Rust runtime renders exactly the string the Android resource carries.
///
/// The agreement that matters, and the one nothing checked before: the generator
/// lowers an ICU arm into a `%d` format string ahead of time, the runtime substitutes
/// `#` at display time, and the two are supposed to produce the same sentence. Run
/// over the **real** catalogs, in every locale, at the CLDR boundary counts, with the
/// arm picked by the same rules Android will pick it with — so a divergence in either
/// the lowering or the runtime's substitution fails here.
///
/// Compared in Android's escaped spelling rather than un-escaping the resource:
/// `android_escape` is the function under test on that side, so applying it to the
/// runtime's output compares like with like.
#[test]
fn the_runtime_renders_what_the_android_resource_carries() {
    let catalogs = Catalogs::load(&repo_root()).expect("the real catalogs load");
    let rendered: BTreeMap<String, String> = rendered_android()
        .into_iter()
        .map(|(path, xml)| (android_path_locale(&path), xml))
        .collect();
    let counts: [i64; 8] = [0, 1, 2, 3, 5, 11, 21, 101];
    let mut compared = 0usize;

    for (locale, messages) in &catalogs.messages {
        let xml = rendered.get(locale).expect("every locale renders");
        let bundle = capsule_i18n::Bundle::for_locale(locale);
        for (key, message) in messages {
            if !message.contains("plural,") {
                continue;
            }
            let arms = android_plural_block(xml, &android_name(key));
            assert!(!arms.is_empty(), "{locale}/{key}: no <plurals> block");
            for n in counts {
                let category = plural::category(locale, n).as_str();
                let arm = arms
                    .get(category)
                    .or_else(|| arms.get("other"))
                    .expect("every plural resource carries `other`");
                // Single-argument plurals use the unnumbered specifier; a message with
                // more arguments numbers them. `%%` is Android's escaped literal `%`.
                let expected = arm
                    .replacen("%1$d", &n.to_string(), 1)
                    .replacen("%d", &n.to_string(), 1)
                    .replace("%%", "%");
                let runtime = bundle.format(key, &[("count", capsule_i18n::Value::Int(n))]);
                assert_eq!(
                    android_escape(&runtime),
                    expected,
                    "{locale}/{key} at n={n}: the runtime and `strings.xml` disagree"
                );
                compared += 1;
            }
        }
    }
    assert!(compared > 1000, "only {compared} renderings were compared");
}

#[test]
fn every_emitted_quantity_is_selectable_in_its_locale() {
    for (path, xml) in &rendered_android() {
        let locale = android_path_locale(path);
        let selectable = plural::selectable(&locale)
            .unwrap_or_else(|| panic!("{}: no `capsule_i18n::plural` row", path.display()));
        for quantity in android_quantities(xml) {
            assert!(
                selectable.iter().any(|c| c.as_str() == quantity),
                "{}: `{quantity}` is not selectable in `{locale}`",
                path.display(),
            );
        }
    }
}

// ── Translation contract: placeholder parity and provenance ───────────────
//
//  1. a translation carrying the source's arguments, reordered, passes
//  2. a dropped `{name}` fails, as does an added one
//  3. a plural flattened to text fails, as does text turned into a plural
//  4. a plural on a different selector fails
//  5. a plural whose arms differ from the source's still passes: arms are per-locale
//  6. a translation of a key the source lacks fails, and every violation is reported
//  7. a translation must record `origin`, from the closed set; the source must not
//  8. end to end: every committed translation passes both

/// The error from [`placeholder_parity`], or `None` when it passes.
fn parity(source: &str, translation: &str) -> Option<String> {
    placeholder_parity(source, translation)
        .err()
        .map(|e| format!("{e:#}"))
}

#[test]
fn a_translation_may_reorder_the_source_arguments() {
    assert_eq!(parity("{a} then {b}", "{b} avant {a}"), None);
    assert_eq!(parity("No arguments", "Aucun argument"), None);
}

#[test]
fn a_dropped_or_added_argument_fails_parity() {
    let dropped = parity("Hello, {name}!", "Bonjour !").expect("dropped `{name}` must fail");
    assert!(dropped.contains("{name}"), "{dropped}");
    let added = parity("Hello!", "Bonjour, {name} !").expect("added `{name}` must fail");
    assert!(added.contains("{name}"), "{added}");
    assert!(parity("{a} and {b}", "{a} et {c}").is_some());
}

#[test]
fn a_plural_must_stay_a_plural_and_text_must_stay_text() {
    let source = "{count, plural, one {# item} other {# items}}";
    let flattened = parity(source, "{count} éléments").expect("a flattened plural must fail");
    assert!(flattened.contains("plural"), "{flattened}");
    assert!(parity("{count} items", "{count, plural, other {# 件}}").is_some());
}

#[test]
fn a_plural_on_another_selector_fails_parity() {
    assert!(
        parity(
            "{count, plural, one {# item} other {# items}}",
            "{n, plural, other {# 件}}"
        )
        .is_some()
    );
}

#[test]
fn plural_arms_are_the_locales_own() {
    // Russian selects `few` and `many`, Japanese only `other`: the arm set is the target
    // language's, so parity compares arguments, never categories.
    let source = "{count, plural, one {# photo of {album}} other {# photos of {album}}}";
    assert_eq!(
        parity(
            source,
            "{count, plural, one {# фото из {album}} few {# фото из {album}} \
             many {# фото из {album}} other {# фото из {album}}}"
        ),
        None
    );
    assert_eq!(
        parity(source, "{count, plural, other {{album} の写真 # 枚}}"),
        None
    );
    assert!(parity(source, "{count, plural, other {# 枚}}").is_some());
}

#[test]
fn every_parity_violation_is_reported_at_once() {
    let mut messages = BTreeMap::new();
    messages.insert(
        "en".to_string(),
        BTreeMap::from([("greet".to_string(), "Hello, {name}!".to_string())]),
    );
    messages.insert(
        "fr".to_string(),
        BTreeMap::from([
            ("greet".to_string(), "Bonjour !".to_string()),
            ("orphan".to_string(), "Orphelin".to_string()),
        ]),
    );
    let catalogs = Catalogs {
        source: "en".to_string(),
        supported: vec!["en".to_string(), "fr".to_string()],
        messages,
    };
    let err = format!(
        "{:#}",
        catalogs
            .check_placeholder_parity()
            .expect_err("both violations must fail")
    );
    assert!(err.contains("fr: key `greet`"), "{err}");
    assert!(
        err.contains("fr: key `orphan` is not in the en catalog"),
        "{err}"
    );
}

#[test]
fn a_key_the_source_lacks_has_no_argument_plan() {
    let mut messages = BTreeMap::new();
    messages.insert("en".to_string(), BTreeMap::new());
    messages.insert(
        "fr".to_string(),
        BTreeMap::from([("orphan".to_string(), "Orphelin".to_string())]),
    );
    let catalogs = Catalogs {
        source: "en".to_string(),
        supported: vec!["en".to_string(), "fr".to_string()],
        messages,
    };
    let err = format!(
        "{:#}",
        catalogs
            .plan_for("orphan")
            .expect_err("no source message, no plan")
    );
    assert!(
        err.contains("key `orphan` is not in the en catalog"),
        "{err}"
    );
}

#[test]
fn a_translation_records_its_origin_from_the_closed_set() {
    let entry = |origin: Option<&str>| match origin {
        Some(o) => serde_json::json!({ "message": "m", "origin": o }),
        None => serde_json::json!({ "message": "m" }),
    };
    assert!(check_origin(&entry(Some("machine")), false).is_ok());
    assert!(check_origin(&entry(Some("human")), false).is_ok());
    assert!(check_origin(&entry(None), false).is_err());
    assert!(check_origin(&entry(Some("reviewed")), false).is_err());
    assert!(check_origin(&serde_json::json!({ "message": "m", "origin": 1 }), false).is_err());
}

#[test]
fn the_source_catalog_carries_no_origin() {
    assert!(check_origin(&serde_json::json!({ "message": "m" }), true).is_ok());
    assert!(
        check_origin(
            &serde_json::json!({ "message": "m", "origin": "human" }),
            true
        )
        .is_err()
    );
}

#[test]
fn every_committed_translation_matches_its_source_arguments() {
    let catalogs = Catalogs::load(&repo_root()).expect("the real catalogs load");
    catalogs
        .check_placeholder_parity()
        .expect("every committed translation carries its source's arguments");
}

#[test]
fn every_info_plist_key_is_namespaced_so_it_stays_out_of_localizable() {
    for (catalog_key, _) in INFO_PLIST_KEYS {
        assert!(
            catalog_key.starts_with(INFO_PLIST_NAMESPACE),
            "{catalog_key} must live in {INFO_PLIST_NAMESPACE}*"
        );
    }
}
