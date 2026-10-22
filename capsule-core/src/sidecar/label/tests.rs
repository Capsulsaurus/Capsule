use super::*;

#[test]
fn trims_and_preserves_case() {
    assert_eq!(normalize_label("  Beach Day \n").unwrap(), "Beach Day");
}

#[test]
fn composes_to_nfc() {
    // "é" as `e` + combining acute (NFD) normalizes to the single precomposed code point.
    let decomposed = "Cafe\u{301}";
    let label = normalize_label(decomposed).unwrap();
    assert_eq!(label, "Caf\u{e9}");
    assert_eq!(label, normalize_label("Caf\u{e9}").unwrap());
}

#[test]
fn refuses_empty_and_whitespace_only() {
    assert_eq!(normalize_label(""), Err(LabelError::Empty));
    assert_eq!(normalize_label(" \t\n"), Err(LabelError::Empty));
}

#[test]
fn enforces_the_byte_bound_after_normalization() {
    let at_limit = "a".repeat(MAX_LABEL_BYTES);
    assert_eq!(normalize_label(&at_limit).unwrap(), at_limit);
    let over = "a".repeat(MAX_LABEL_BYTES + 1);
    assert_eq!(
        normalize_label(&over),
        Err(LabelError::TooLong(MAX_LABEL_BYTES + 1))
    );
    // Surrounding whitespace does not count against the bound.
    assert!(normalize_label(&format!("  {at_limit}  ")).is_ok());
}
