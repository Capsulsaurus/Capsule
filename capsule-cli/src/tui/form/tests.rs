//! Form tests: typing, focus, submission, and that a secret never prints.

use super::*;

fn form() -> Form {
    Form::new(vec![Field::text("Email"), Field::secret("Password")])
}

fn type_into(form: &mut Form, text: &str) {
    for c in text.chars() {
        assert_eq!(form.apply(FormKey::Char(c)), FormOutcome::Editing);
    }
}

fn text(lines: &[Line<'_>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
}

#[test]
fn typing_goes_into_the_focused_field_and_backspace_takes_it_out() {
    let mut form = form();
    type_into(&mut form, "ab");
    assert_eq!(form.apply(FormKey::Backspace), FormOutcome::Editing);
    assert_eq!(form.value(0), "a");
    assert_eq!(form.apply(FormKey::Next), FormOutcome::Editing);
    type_into(&mut form, "p w");
    assert_eq!(form.value(0), "a");
    assert_eq!(form.secret(1).expose(), "p w");
}

#[test]
fn focus_wraps_both_ways() {
    let mut form = form();
    form.apply(FormKey::Previous);
    assert_eq!(form.focus(), 1);
    form.apply(FormKey::Next);
    assert_eq!(form.focus(), 0);
}

#[test]
fn submit_and_cancel_are_reported_not_applied() {
    let mut form = form();
    assert_eq!(form.apply(FormKey::Submit), FormOutcome::Submitted);
    assert_eq!(form.apply(FormKey::Cancel), FormOutcome::Cancelled);
    assert_eq!(form, self::form());
}

#[test]
fn value_trims_but_secret_keeps_spaces() {
    let mut form = form();
    form.set(0, "  me@example.com ");
    form.set(1, " pass ");
    assert_eq!(form.value(0), "me@example.com");
    assert_eq!(form.secret(1).expose(), " pass ");
    assert_eq!(form.value(9), "");
}

#[test]
fn control_characters_and_overlong_values_are_refused() {
    let mut form = form();
    form.apply(FormKey::Char('\u{1b}'));
    assert_eq!(form.value(0), "");
    form.set(0, &"x".repeat(MAX_FIELD_LEN + 10));
    form.apply(FormKey::Char('y'));
    assert_eq!(form.value(0).chars().count(), MAX_FIELD_LEN);
    assert!(!form.value(0).contains('y'));
}

#[test]
fn clear_secrets_empties_only_the_secret_fields() {
    let mut form = form();
    form.set(0, "me");
    form.set(1, "pw");
    form.clear_secrets();
    assert_eq!(form.value(0), "me");
    assert!(form.secret(1).is_empty());
}

#[test]
fn debug_never_prints_a_secret_or_a_typed_character() {
    let mut form = form();
    form.set(1, "hunter2");
    let printed = format!(
        "{form:?} {:?} {:?}",
        FormKey::Char('h'),
        Secret::new("hunter2")
    );
    assert!(!printed.contains("hunter2"), "{printed}");
    assert!(!printed.contains("'h'"), "{printed}");
    assert!(printed.contains("Char(_)"), "{printed}");
}

#[test]
fn lines_mask_secrets_and_mark_the_focus_only_while_editing() {
    let mut form = form();
    form.set(0, "me");
    form.set(1, "pw!");
    assert_eq!(
        text(&form.lines(true)),
        vec!["› Email     me▏".to_owned(), "  Password  •••".to_owned()]
    );
    assert_eq!(
        text(&form.lines(false)),
        vec!["  Email     me".to_owned(), "  Password  •••".to_owned()]
    );
}

#[test]
fn focus_can_be_set_and_past_the_end_is_ignored() {
    let mut form = form();
    form.focus_on(1);
    assert_eq!(form.focus(), 1);
    type_into(&mut form, "pw");
    assert_eq!(form.secret(1).expose(), "pw");
    form.focus_on(2);
    assert_eq!(form.focus(), 1);
}

#[test]
fn status_reports_whether_a_request_is_in_flight() {
    assert!(Status::Working("Doing it…").is_working());
    assert!(!Status::Done("done".to_owned()).is_working());
    assert!(Status::Idle.line().is_none());
    assert!(Status::Failed("no".to_owned()).line().is_some());
}
