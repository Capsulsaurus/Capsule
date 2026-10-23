use super::{MAX_DISPLAY_NAME_CHARS, MalformedProfile, ProfileUpdate, admissible_display_name};

#[test]
fn a_blank_display_name_is_the_absent_one() {
    // The two states a client can express for "no name" collapse to one before storage, so
    // a client rendering the stored value never has to special-case "".
    assert_eq!(admissible_display_name("   "), Ok(None));
    assert_eq!(admissible_display_name(""), Ok(None));
}

#[test]
fn a_display_name_is_trimmed_and_kept_verbatim_otherwise() {
    assert_eq!(
        admissible_display_name("  Ada Lovelace  "),
        Ok(Some("Ada Lovelace".to_owned()))
    );
}

#[test]
fn the_ceiling_counts_characters_and_not_bytes() {
    // The unit a person counts in. A byte ceiling would refuse a name in a script where
    // every character is three or four bytes while accepting a much longer ASCII one.
    let name = "\u{1F600}".repeat(MAX_DISPLAY_NAME_CHARS);
    assert!(admissible_display_name(&name).is_ok());
    let over = "\u{1F600}".repeat(MAX_DISPLAY_NAME_CHARS + 1);
    assert_eq!(
        admissible_display_name(&over),
        Err(MalformedProfile::DisplayNameTooLong {
            chars: MAX_DISPLAY_NAME_CHARS + 1
        })
    );
}

#[test]
fn a_control_character_is_refused_and_not_stripped() {
    assert_eq!(
        admissible_display_name("Ada\nLovelace"),
        Err(MalformedProfile::DisplayNameControlCharacters)
    );
}

#[test]
fn an_absent_field_and_a_cleared_one_are_different_updates() {
    // The whole reason the field is a nested option: a flat one cannot express "leave it
    // alone", so every partial update would clear the name nobody mentioned.
    assert!(ProfileUpdate::default().is_empty());
    assert!(
        !ProfileUpdate {
            display_name: Some(None)
        }
        .is_empty()
    );
}
