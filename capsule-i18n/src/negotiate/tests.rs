use super::negotiate;

const SUPPORTED: &[&str] = &["en", "fr", "de"];

#[test]
fn exact_match_wins() {
    assert_eq!(negotiate("fr", SUPPORTED, "en"), "fr");
}

#[test]
fn primary_subtag_matches_region() {
    assert_eq!(negotiate("fr-CA", SUPPORTED, "en"), "fr");
}

#[test]
fn highest_weight_is_preferred() {
    assert_eq!(negotiate("fr;q=0.5, de;q=0.9", SUPPORTED, "en"), "de");
}

#[test]
fn unmatched_falls_back_to_source() {
    assert_eq!(negotiate("es-MX, es;q=0.9", SUPPORTED, "en"), "en");
}

#[test]
fn empty_request_falls_back_to_source() {
    assert_eq!(negotiate("", SUPPORTED, "en"), "en");
}

#[test]
fn matching_is_case_insensitive() {
    assert_eq!(negotiate("FR-fr", SUPPORTED, "en"), "fr");
}
