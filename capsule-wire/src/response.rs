//! The neutral response taxonomy: what a wire outcome *is*, with no framework in it.
//!
//! One [`ResponseSpec`] describes a single outcome of one endpoint — the HTTP status it
//! carries, the shape of its body, and the sentence that documents it. A response enum
//! declares its whole taxonomy as a `&'static [ResponseSpec]`; the per-framework adapters
//! ([`crate::salvo_responses!`] today, the replacement server later) are generated from that
//! table rather than restating it.

use serde::{Deserialize, Serialize};

/// What an outcome puts in the response body, independent of any framework.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BodyShape {
    /// No body at all — the status (and any headers) is the whole answer.
    Empty,
    /// A JSON document. [`ResponseSpec::schema`] names its schema when the payload is typed.
    Json,
    /// A plain-text message.
    Text,
    /// Bytes whose framing this taxonomy does not model (a file range, opaque CBOR, …).
    Opaque,
    /// The body belongs to a nested taxonomy that this outcome delegates to (an error type
    /// with its own status ladder). The delegated statuses appear as their own specs.
    Delegated,
}

/// One outcome of one endpoint.
///
/// `status` is `None` only for the delegating arm of a taxonomy: the arm itself picks no
/// status, the taxonomy it defers to does. `description` is `None` when the outcome is
/// deliberately absent from the published API document — a gap the table now records instead
/// of hiding (see [`ResponseSpec::is_documented`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseSpec {
    /// The HTTP status code, or `None` when a delegated taxonomy decides it.
    pub status: Option<u16>,
    /// The shape of the body.
    pub body: BodyShape,
    /// The sentence published for this status, or `None` when it is undocumented.
    pub description: Option<&'static str>,
    /// The name of the payload schema, when the body is a typed JSON document.
    pub schema: Option<&'static str>,
}

impl ResponseSpec {
    /// Whether this outcome reaches the published API document.
    #[must_use]
    pub const fn is_documented(&self) -> bool {
        self.status.is_some() && self.description.is_some()
    }
}

/// Whether `status` is a syntactically valid HTTP status code.
///
/// Used by the adapter macros as a compile-time guard, so a typo in a taxonomy table is a
/// build error rather than a response that silently ships the wrong status.
#[must_use]
pub const fn is_valid_status(status: u16) -> bool {
    status >= 100 && status < 1000
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOCUMENTED: ResponseSpec = ResponseSpec {
        status: Some(200),
        body: BodyShape::Json,
        description: Some("Success"),
        schema: Some("TokenResponse"),
    };

    #[test]
    fn a_status_with_a_description_is_documented() {
        assert!(DOCUMENTED.is_documented());
    }

    #[test]
    fn an_outcome_without_a_description_is_a_recorded_gap() {
        let spec = ResponseSpec {
            description: None,
            ..DOCUMENTED
        };
        assert!(!spec.is_documented());
    }

    #[test]
    fn a_delegating_arm_documents_nothing_itself() {
        let spec = ResponseSpec {
            status: None,
            body: BodyShape::Delegated,
            description: None,
            schema: None,
        };
        assert!(!spec.is_documented());
    }

    #[test]
    fn status_validity_brackets_the_http_range() {
        assert!(!is_valid_status(99));
        assert!(is_valid_status(100));
        assert!(is_valid_status(200));
        assert!(is_valid_status(599));
        assert!(is_valid_status(999));
        assert!(!is_valid_status(1000));
    }

    #[test]
    fn a_spec_round_trips_through_serde() {
        let json = serde_json::to_string(&DOCUMENTED).expect("serializing a spec");
        assert_eq!(
            json,
            r#"{"status":200,"body":"json","description":"Success","schema":"TokenResponse"}"#
        );
    }
}
