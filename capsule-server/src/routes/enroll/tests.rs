use super::*;

#[test]
fn both_spellings_mint_issues_are_shaped_like_codes() {
    // The predicate has to admit exactly what `mint` produces, or a legitimate redemption
    // would be counted in the malformed bucket and throttled deployment-wide.
    // And it has to tell the two apart, or a flood of short guesses would spend the QR
    // payload's bucket too.
    for _ in 0..64 {
        let uuid = Uuid::new_v4().to_string();
        assert_eq!(spelling_of(&uuid), Some(EnrollmentSpelling::Full), "{uuid}");
        let fallback = format!(
            "{:0width$}",
            Uuid::new_v4().as_u128() % 100_000_000,
            width = TEXT_FALLBACK_DIGITS
        );
        assert_eq!(
            spelling_of(&fallback),
            Some(EnrollmentSpelling::Fallback),
            "{fallback}"
        );
    }
    // Including the leading-zero fallback, which is where an "is it a number" check breaks.
    assert_eq!(spelling_of("00000042"), Some(EnrollmentSpelling::Fallback));
    // And an upper-cased UUID, which is a real code a client re-spelled.
    assert_eq!(
        spelling_of(&Uuid::new_v4().to_string().to_ascii_uppercase()),
        Some(EnrollmentSpelling::Full)
    );
}

#[test]
fn the_key_does_not_depend_on_the_guess() {
    // Issue #528: the key used to be the presented string, so every wrong guess was a fresh
    // key with a fresh budget. Two different guesses of one spelling must be one key.
    let key = |raw: &str| spelling_of(raw).map(CounterKey::EnrollmentRedemption);
    assert_eq!(key("00000001"), key("99999999"));
    assert_eq!(
        key(&Uuid::new_v4().to_string()),
        key(&Uuid::new_v4().to_string())
    );
    assert_ne!(key("00000001"), key(&Uuid::new_v4().to_string()));
}

#[test]
fn nothing_else_gets_a_counter_key_of_its_own() {
    for candidate in [
        "",
        "   ",
        "1234567",                             // one digit short
        "123456789",                           // one digit long
        "0000000a",                            // right length, not digits
        "not-a-uuid-at-all-not-even-close-xx", // right length, not a uuid
        "0193d2f4a1b74c3e8f5a6b7c8d9e0f10",    // simple uuid: not the spelling minted
        "urn:uuid:0193d2f4-a1b7-4c3e-8f5a-6b7c8d9e0f10",
        "{0193d2f4-a1b7-4c3e-8f5a-6b7c8d9e0f10}",
    ] {
        assert_eq!(spelling_of(candidate), None, "{candidate:?}");
    }
    // The point of the bound: an arbitrarily long string cannot mint a key.
    let long = "a".repeat(64 * 1024);
    assert_eq!(spelling_of(&long), None);
}

#[test]
fn the_shape_check_reads_no_store() {
    // It is a shape check and must never become an existence check: it takes only a string,
    // so it cannot distinguish a pending code from an absent one, and the charge-before-
    // resolve ordering that stops this route being an existence oracle is untouched.
    let minted = Uuid::new_v4().to_string();
    let never_issued = "0193d2f4-a1b7-4c3e-8f5a-6b7c8d9e0f10";
    assert_eq!(
        spelling_of(&minted),
        spelling_of(never_issued),
        "a code that exists and one that never will are shaped the same"
    );
}
