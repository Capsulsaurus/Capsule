//! End-to-end checks against the generated `en` bundle embedded in the crate.

use capsule_i18n::{Bundle, Value, error_codes, format_message, negotiate, supported_locales};

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
    // No `es` bundle exists yet, so messages come from the source locale.
    let bundle = Bundle::for_locale("es");
    assert_eq!(bundle.message("back"), Some("Back"));
}

#[test]
fn negotiation_uses_the_supported_set() {
    assert_eq!(
        negotiate("es-MX, es;q=0.9", supported_locales(), "en"),
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
