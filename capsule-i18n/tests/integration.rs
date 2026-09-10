//! End-to-end checks against the generated `en` bundle embedded in the crate.

use capsule_i18n::{
    Bundle, Value, error_codes, format_message, format_message_in, negotiate, supported_locales,
};

#[test]
fn source_locale_is_supported() {
    assert!(supported_locales().contains(&"en"));
}

#[test]
fn known_message_is_returned() {
    let bundle = Bundle::for_locale("en");
    assert_eq!(bundle.message("app_name"), Some("Capsule App"));
    assert_eq!(bundle.locale(), "en");
}

#[test]
fn unknown_key_returns_the_key() {
    let bundle = Bundle::for_locale("en");
    assert_eq!(bundle.format("does.not.exist", &[]), "does.not.exist");
}

#[test]
fn error_code_resolves_to_its_message() {
    let bundle = Bundle::for_locale("en");
    let message = bundle.format(error_codes::AUTH_INVALID_CREDENTIALS, &[]);
    assert_eq!(message, "Invalid email or password.");
}

#[test]
fn unknown_locale_falls_back_to_source() {
    // `nl` is outside the official language set, so messages come from the source locale.
    let bundle = Bundle::for_locale("nl");
    assert_eq!(bundle.message("back"), Some("Back"));
}

#[test]
fn negotiation_uses_the_supported_set() {
    // A supported language negotiates to its catalog (S-I2 rolled out the official set)…
    assert_eq!(
        negotiate("es-MX, es;q=0.9", supported_locales(), "en"),
        "es"
    );
    // …while a language outside the set still falls back to the source locale.
    assert_eq!(
        negotiate("nl-BE, nl;q=0.9", supported_locales(), "en"),
        "en"
    );
}

#[test]
fn public_formatter_interpolates() {
    assert_eq!(
        format_message("Hi, {name}!", &[("name", Value::Str("Sam"))]),
        "Hi, Sam!"
    );
}

/// The counts the plural matrix below is asserted over: the CLDR boundaries that separate
/// Russian's three forms and Arabic's five from each other and from `other`.
const COUNTS: &[i64] = &[0, 1, 2, 3, 5, 11, 21, 101];

/// One locale's embedded bundle, read from the same file the crate compiles in.
///
/// The crate exposes no key iterator (a bundle answers look-ups; enumerating its keys is
/// not something a caller needs), so the matrix reads the generated JSON directly. It is
/// the *committed generated* file, so a drifted catalog is `mise run i18n-check`'s
/// failure, not this test's.
fn bundle_messages(locale: &str) -> std::collections::BTreeMap<String, String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/bundles")
        .join(format!("{locale}.json"));
    let json = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_str(&json).unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

/// Every plural the shipped catalogs carry renders, in every locale, at every boundary
/// count — no braces, no `plural,`, and the count itself wherever the arm spells `#`.
///
/// This is the acceptance test for the runtime's plural support: before it, each of these
/// ~1000 renderings returned the ICU message source, which is what a user would have read.
#[test]
fn every_plural_renders_in_every_locale() {
    let mut renderings = 0usize;
    let mut keys_seen = 0usize;
    let source_messages = bundle_messages("en");
    for locale in supported_locales() {
        let bundle = Bundle::for_locale(locale);
        // A key the locale has not translated resolves through the source catalog, so the
        // reader still gets an English plural — under *their* language's rules. That path
        // is live (`app.places.preview.accessibility` is `en`-only), so the matrix covers
        // the union rather than only what the locale itself carries.
        let mut messages = source_messages.clone();
        messages.extend(bundle_messages(locale));
        for (key, source) in messages {
            if !source.contains("plural,") {
                continue;
            }
            keys_seen += 1;
            // The whole matrix assumes one selector name, which is also what the
            // generators' argument plan assumes. Assert it rather than silently
            // rendering the `other` arm for a key that renamed its count.
            assert!(
                source.starts_with("{count, plural,"),
                "{locale}/{key}: unexpected plural selector in `{source}`"
            );
            for n in COUNTS {
                let rendered = bundle.format(&key, &[("count", Value::Int(*n))]);
                assert!(
                    !rendered.contains('{')
                        && !rendered.contains('}')
                        && !rendered.contains("plural,"),
                    "{locale}/{key} at n={n} rendered ICU source: `{rendered}`"
                );
                if source.contains('#') {
                    assert!(
                        rendered.contains(&n.to_string()),
                        "{locale}/{key} at n={n} dropped the count: `{rendered}`"
                    );
                }
                renderings += 1;
            }
        }
    }
    // 13 locales x 10-11 plural keys x 8 counts. A floor, not an equality, so adding a
    // plural to the catalogs does not fail this test — losing one does.
    assert!(
        keys_seen >= 13 * 10,
        "only {keys_seen} plural keys found across the bundles"
    );
    assert!(
        renderings >= 130 * COUNTS.len(),
        "only {renderings} renderings were asserted"
    );
}

/// A bundle formats with **its own** locale's rules, which is the API gap plural support
/// had to close: `Bundle::format` used to drop `self.locale` on the floor.
#[test]
fn a_bundle_selects_the_arm_its_own_locale_asks_for() {
    // French counts zero as singular, English does not — the same catalog message, two
    // different arms, chosen only because the bundle knows which locale it is.
    let count = [("count", Value::Int(0))];
    assert_eq!(
        Bundle::for_locale("fr").format("common.item_count", &count),
        "0 item"
    );
    assert_eq!(
        Bundle::for_locale("en").format("common.item_count", &count),
        "0 items"
    );
}

/// The locale-free `format_message` still exists and still means English.
#[test]
fn the_public_formatter_evaluates_a_plural_with_english_rules() {
    let template = "{count, plural, one {# item} other {# items}}";
    assert_eq!(
        format_message(template, &[("count", Value::Int(1))]),
        "1 item"
    );
    assert_eq!(
        format_message_in("fr", template, &[("count", Value::Int(0))]),
        "0 item"
    );
}
