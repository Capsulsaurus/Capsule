use super::*;

#[test]
fn an_error_code_renders_the_english_catalog_message() {
    assert_eq!(
        error_message(error_codes::AUTH_INVALID_CREDENTIALS),
        "Invalid email or password."
    );
}

/// The code has translations, and the CLI renders none of them: the bundle is pinned to the
/// source locale rather than negotiated from the environment.
#[test]
fn a_translated_code_still_renders_in_english() {
    let code = error_codes::AUTH_INVALID_CREDENTIALS;
    let french = Bundle::for_locale("fr").format(code, &[]);
    assert_ne!(french, Bundle::for_locale("en").format(code, &[]));
    assert_ne!(error_message(code), french);
}

#[test]
fn an_unknown_code_surfaces_as_itself() {
    assert_eq!(error_message("error.no.such_code"), "error.no.such_code");
}
