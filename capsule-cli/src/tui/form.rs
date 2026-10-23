//! Text forms the feature screens collect their input with, and the status line they report on.
//!
//! A [`Form`] is a list of labelled single-line [`Field`]s and the index of the focused one. It
//! changes only through [`Form::apply`], one [`FormKey`] at a time, so a screen's reducer stays
//! pure and a test types into a form the same way the keymap does.
//!
//! Secrets never reach a log line. A field marked secret is drawn masked and its value is
//! redacted from `Debug`, a [`FormKey::Char`] never prints its character (it may be a password),
//! and a value that leaves the form for an effect travels as a [`Secret`], whose `Debug` is
//! redacted too. The reducer and the ports log actions and effects at `trace`/`debug`, so this is
//! what keeps a typed passphrase out of `tui.log`.

use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};

/// The most characters one field holds: far beyond any path, id or passphrase a person types,
/// and a bound on what a stuck key or a paste can grow the state to.
pub const MAX_FIELD_LEN: usize = 4096;

/// A value that must not reach a log line: a password, a passphrase, a one-time token, a share
/// URL carrying its fragment secret. `Debug` prints `<redacted>`.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wrap `value`.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The value, for the one place that needs it: the port handing it to the SDK.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Whether the value is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// One key a form understands, already decoded by the keymap.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FormKey {
    /// Type a character into the focused field.
    Char(char),
    /// Delete the focused field's last character.
    Backspace,
    /// Focus the next field, wrapping to the first.
    Next,
    /// Focus the previous field, wrapping to the last.
    Previous,
    /// Submit the form.
    Submit,
    /// Stop editing without submitting.
    Cancel,
}

impl std::fmt::Debug for FormKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // The character may be part of a password: never printed.
            Self::Char(_) => f.write_str("Char(_)"),
            Self::Backspace => f.write_str("Backspace"),
            Self::Next => f.write_str("Next"),
            Self::Previous => f.write_str("Previous"),
            Self::Submit => f.write_str("Submit"),
            Self::Cancel => f.write_str("Cancel"),
        }
    }
}

/// What [`Form::apply`] did with a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormOutcome {
    /// The form changed (or the key did nothing); editing goes on.
    Editing,
    /// The person submitted the form.
    Submitted,
    /// The person stopped editing without submitting.
    Cancelled,
}

/// One labelled single-line text field.
#[derive(Clone, PartialEq, Eq)]
pub struct Field {
    label: &'static str,
    value: String,
    secret: bool,
}

impl std::fmt::Debug for Field {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value: &dyn std::fmt::Debug = if self.secret {
            &"<redacted>"
        } else {
            &self.value
        };
        f.debug_struct("Field")
            .field("label", &self.label)
            .field("value", value)
            .finish()
    }
}

impl Field {
    /// A plain field.
    #[must_use]
    pub fn text(label: &'static str) -> Self {
        Self {
            label,
            value: String::new(),
            secret: false,
        }
    }

    /// A field drawn masked and redacted from `Debug`.
    #[must_use]
    pub fn secret(label: &'static str) -> Self {
        Self {
            secret: true,
            ..Self::text(label)
        }
    }

    /// The label the form draws beside the value.
    #[must_use]
    pub fn label(&self) -> &'static str {
        self.label
    }

    /// Whether the field is a secret.
    #[must_use]
    pub fn is_secret(&self) -> bool {
        self.secret
    }
}

/// A list of fields and the one with focus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    fields: Vec<Field>,
    focus: usize,
}

impl Form {
    /// A form over `fields`, focused on the first.
    #[must_use]
    pub fn new(fields: Vec<Field>) -> Self {
        Self { fields, focus: 0 }
    }

    /// The fields, in order.
    #[must_use]
    pub fn fields(&self) -> &[Field] {
        &self.fields
    }

    /// The index of the focused field.
    #[must_use]
    pub fn focus(&self) -> usize {
        self.focus
    }

    /// Field `index`'s value with surrounding whitespace dropped, or `""` past the end.
    #[must_use]
    pub fn value(&self, index: usize) -> &str {
        self.fields.get(index).map_or("", |f| f.value.trim())
    }

    /// Field `index`'s value exactly as typed, as a [`Secret`]: a passphrase keeps its spaces.
    #[must_use]
    pub fn secret(&self, index: usize) -> Secret {
        Secret::new(self.fields.get(index).map_or("", |f| f.value.as_str()))
    }

    /// Set field `index`'s value (for a test, or to prefill).
    pub fn set(&mut self, index: usize, value: &str) {
        if let Some(field) = self.fields.get_mut(index) {
            field.value = value.chars().take(MAX_FIELD_LEN).collect();
        }
    }

    /// Focus field `index`; past the end, focus stays where it is.
    pub fn focus_on(&mut self, index: usize) {
        if index < self.fields.len() {
            self.focus = index;
        }
    }

    /// Empty every secret field, so a submitted passphrase does not outlive its submission.
    pub fn clear_secrets(&mut self) {
        for field in self.fields.iter_mut().filter(|f| f.secret) {
            field.value.clear();
        }
    }

    /// Apply one key.
    pub fn apply(&mut self, key: FormKey) -> FormOutcome {
        let count = self.fields.len().max(1);
        match key {
            FormKey::Char(c) => {
                if let Some(field) = self.fields.get_mut(self.focus)
                    && field.value.chars().count() < MAX_FIELD_LEN
                    && !c.is_control()
                {
                    field.value.push(c);
                }
                FormOutcome::Editing
            }
            FormKey::Backspace => {
                if let Some(field) = self.fields.get_mut(self.focus) {
                    field.value.pop();
                }
                FormOutcome::Editing
            }
            FormKey::Next => {
                self.focus = (self.focus + 1) % count;
                FormOutcome::Editing
            }
            FormKey::Previous => {
                self.focus = (self.focus + count - 1) % count;
                FormOutcome::Editing
            }
            FormKey::Submit => FormOutcome::Submitted,
            FormKey::Cancel => FormOutcome::Cancelled,
        }
    }

    /// The form as lines: one per field, `Label: value`, secrets masked, and the focused field
    /// marked and given a cursor while `editing`.
    #[must_use]
    pub fn lines(&self, editing: bool) -> Vec<Line<'static>> {
        let width = self
            .fields
            .iter()
            .map(|f| f.label.chars().count())
            .max()
            .unwrap_or(0);
        self.fields
            .iter()
            .enumerate()
            .map(|(index, field)| {
                let focused = editing && index == self.focus;
                let value = if field.secret {
                    "•".repeat(field.value.chars().count())
                } else {
                    field.value.clone()
                };
                let mut spans = vec![
                    Span::raw(if focused { "› " } else { "  " }),
                    Span::styled(
                        format!("{:<width$}  ", field.label),
                        Style::new().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(value),
                ];
                if focused {
                    spans.push("▏".fg(Color::Cyan));
                }
                Line::from(spans)
            })
            .collect()
    }
}

/// Where a screen's last request stands: the line drawn under its form.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Status {
    /// Nothing asked yet, or the last outcome was dismissed.
    #[default]
    Idle,
    /// A request is in flight; the phrase says which.
    Working(&'static str),
    /// The screen is waiting for a yes or no; the question.
    Asking(String),
    /// The last request finished; what it did.
    Done(String),
    /// The last request, or the form's own check, failed; why.
    Failed(String),
}

impl Status {
    /// Whether a request is in flight, which a second submission must wait for.
    #[must_use]
    pub fn is_working(&self) -> bool {
        matches!(self, Self::Working(_))
    }

    /// The status as a line, or `None` while idle.
    #[must_use]
    pub fn line(&self) -> Option<Line<'static>> {
        match self {
            Self::Idle => None,
            Self::Working(what) => Some(Line::from(what.fg(Color::DarkGray))),
            Self::Asking(question) => Some(Line::from(question.clone().fg(Color::Yellow))),
            Self::Done(what) => Some(Line::from(what.clone().fg(Color::Green))),
            Self::Failed(why) => Some(Line::from(why.clone().fg(Color::Red))),
        }
    }
}

#[cfg(test)]
mod tests;
