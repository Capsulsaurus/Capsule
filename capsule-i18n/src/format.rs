//! ICU MessageFormat formatting.
//!
//! The runtime currently supports the subset Capsule's catalog uses today: literal
//! text and simple `{name}` argument interpolation. Full ICU `plural` / `select` /
//! `number` / `date` formatting is a documented follow-up (see the i18n design doc);
//! until then a complex placeholder is copied through verbatim rather than
//! mis-rendered, so the limitation is visible rather than silently wrong.

use std::fmt::{self, Write as _};

/// A value substituted into a `{name}` placeholder.
#[derive(Debug, Clone, Copy)]
pub enum Value<'a> {
    /// A string argument.
    Str(&'a str),
    /// An integer argument.
    Int(i64),
}

impl fmt::Display for Value<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Str(s) => f.write_str(s),
            Value::Int(n) => write!(f, "{n}"),
        }
    }
}

/// Format `template` by substituting `{name}` placeholders from `args`.
///
/// A placeholder whose body is a bare identifier is replaced with the matching arg
/// (or left intact, braces and all, if no such arg was supplied so the gap is
/// visible). Any other `{...}` — e.g. an ICU `plural` block — is copied through
/// unchanged; see the module docs.
#[must_use]
pub fn format_message(template: &str, args: &[(&str, Value<'_>)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '{' {
            out.push(c);
            continue;
        }
        // Collect the placeholder body up to the matching '}'.
        let mut body = String::new();
        let mut closed = false;
        for b in chars.by_ref() {
            if b == '}' {
                closed = true;
                break;
            }
            body.push(b);
        }
        let name = body.trim();
        if closed && is_identifier(name) {
            if let Some((_, value)) = args.iter().find(|(key, _)| *key == name) {
                let _ = write!(out, "{value}");
            } else {
                // Unknown arg: keep the placeholder so the missing value is obvious.
                let _ = write!(out, "{{{body}}}");
            }
        } else {
            // Complex (plural/select) or unterminated placeholder: emit verbatim.
            out.push('{');
            out.push_str(&body);
            if closed {
                out.push('}');
            }
        }
    }
    out
}

/// Whether `s` is a non-empty ASCII identifier (letters, digits, underscore).
fn is_identifier(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::{Value, format_message};

    #[test]
    fn literal_passes_through() {
        assert_eq!(
            format_message("No data available", &[]),
            "No data available"
        );
    }

    #[test]
    fn named_arg_is_substituted() {
        assert_eq!(
            format_message("Hello, {name}!", &[("name", Value::Str("World"))]),
            "Hello, World!"
        );
    }

    #[test]
    fn integer_arg_is_formatted() {
        assert_eq!(
            format_message("{count} selected", &[("count", Value::Int(3))]),
            "3 selected"
        );
    }

    #[test]
    fn missing_arg_keeps_placeholder() {
        assert_eq!(format_message("Hi {name}", &[]), "Hi {name}");
    }

    #[test]
    fn whitespace_inside_placeholder_is_tolerated() {
        assert_eq!(
            format_message("Hi { name }", &[("name", Value::Str("Sam"))]),
            "Hi Sam"
        );
    }

    #[test]
    fn complex_placeholder_is_left_verbatim() {
        // Known limitation: the MVP runtime does not evaluate plural/select blocks.
        let template = "{count, plural, one {# item} other {# items}}";
        assert_eq!(
            format_message(template, &[("count", Value::Int(2))]),
            template
        );
    }
}
