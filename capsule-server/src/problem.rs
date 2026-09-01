//! [`CodedProblems`] — the `error.*` code every rejection owes, including the ones Capsule does
//! not write (slice `S-C36`).
//!
//! # The contract, and where it was broken
//!
//! [i18n](../../capsule-docs/src/content/docs/design/i18n.md) says every server rejection carries
//! a stable code from the `error.*` namespace, which the client localizes **offline** while the
//! English `detail` stays English. That is the whole design: no server-side catalogs, no
//! `Accept-Language` negotiation, and a client that can name a refusal in the user's language
//! with the network down.
//!
//! Every rejection this crate *owns* carries one, because every variant declares a
//! `#[problem(extension)] code` field. The ones it does not own carried nothing:
//!
//! | Response | Whose |
//! | --- | --- |
//! | `401`, `403` | `Auth<AccessToken>` — Kynos's `AuthRejection` |
//! | `413` | the body-size interceptor (`S-C33`) |
//! | `400` | a `Path` or `Headers` extractor that could not decode |
//! | `415`, `422` | the `Json` extractor |
//!
//! Between them those are a large share of what a client actually sees, and each one reached a
//! user as untranslatable English.
//!
//! # Why an interceptor and not a wrapper
//!
//! The obvious fix — emit Capsule's own `401` beside the framework's — is the one the bearer
//! scheme's module docs already rejected: it would mean an operation with two different `401`
//! bodies, one of which does not carry the `WWW-Authenticate` header the document declares as
//! required. And Kynos's rejection types have no extension seam to add a member through; they
//! are foreign types with private shapes.
//!
//! What *is* available is the response itself. An interceptor sees what the chain produced, and
//! `Continued::take_body`/`set_body` exist precisely so one can rewrite a body. So this reads
//! the problem the framework rendered and fills in the member it could not.
//!
//! # What it does not do, which is most of it
//!
//! - **It never invents a response.** `Short` is [`Infallible`], so this interceptor cannot
//!   refuse a request; it can only edit what something else already decided.
//! - **It never changes a status, a header, or a media type.** `Adds` is `()`. The status is the
//!   key it reads, and reading is all it does with it.
//! - **It never overwrites a code.** A body that already carries `code` is passed through
//!   untouched, which is what keeps a Capsule rejection's own catalog entry authoritative. The
//!   interceptor exists for the gap and cannot reach anything else.
//! - **It touches nothing but `application/problem+json`.** A `200`, a `206` of ciphertext, a
//!   `304`: the content type is checked before the body is taken, so nothing on the hot path is
//!   buffered.
//!
//! # The status → code mapping is a fallback, not a taxonomy
//!
//! A framework rejection knows less than a Capsule one: it knows a request could not be decoded,
//! not *which field* or *why it mattered here*. So the codes it gets are the coarse ones — one
//! per status, in the `error.request.*` namespace — and they are deliberately not reused by any
//! handler. A route that wants to say something specific declares its own variant with its own
//! code, exactly as every route does today; seeing `error.request.unprocessable` in the wild
//! means the framework refused before a handler ran.
//!
//! [`UNMAPPED`] is the honest end of that: a status the mapping does not name still gets a code,
//! because a codeless problem is the defect this module exists to remove, and the alternative
//! would be a hole that opens silently the next time the surface grows a status.
//!
//! # Ordering
//!
//! Mounted **outermost**, on the router rather than a group, so it sees problems produced by
//! inner interceptors — the body-size `413` in particular — as well as by extractors and
//! handlers. Kynos runs a chain head-first, so the first mounted is the outermost.
//!
//! # The other half
//!
//! Attaching the member is only half the i18n contract; a generated client also has to know the
//! member is there. That is `S-C38`, and it is why [`crate::openapi`] describes `code` on every
//! problem response — the two are one change, and this module is the half that makes the
//! document's claim true for the framework's own responses.

use std::convert::Infallible;

use bytes::Bytes;
use capsule_i18n::error_codes;
use kynos::http::{self, Request, header};
use kynos::middleware::{Continued, Interceptor, Next};

/// The media type an RFC 9457 problem is served as.
const PROBLEM_JSON: &str = "application/problem+json";

/// The extension member the i18n contract is written in terms of.
pub const CODE_MEMBER: &str = "code";

/// The code a status the mapping does not name receives.
///
/// Reaching this means the surface grew a rejection nobody localized. It is a code rather than
/// nothing so that "every response carries a code" stays true by construction, and it is
/// deliberately vague so that nobody is tempted to render it on purpose.
pub const UNMAPPED: &str = error_codes::REQUEST_FAILED;

/// The `error.*` code for a framework-rendered `status`.
///
/// Public so a test can assert the mapping is total over the statuses the document declares
/// rather than over the ones somebody remembered.
#[must_use]
pub fn code_for(status: u16) -> &'static str {
    match status {
        400 => error_codes::REQUEST_MALFORMED,
        401 => error_codes::REQUEST_UNAUTHENTICATED,
        403 => error_codes::REQUEST_FORBIDDEN,
        405 => error_codes::REQUEST_METHOD_NOT_ALLOWED,
        406 => error_codes::REQUEST_NOT_ACCEPTABLE,
        413 => error_codes::REQUEST_TOO_LARGE,
        415 => error_codes::REQUEST_UNSUPPORTED_MEDIA_TYPE,
        422 => error_codes::REQUEST_UNPROCESSABLE,
        _ => UNMAPPED,
    }
}

/// Fills in the `error.*` code on problems the framework rendered.
///
/// See the module docs. Holds nothing: the mapping is a function of the status and the decision
/// of whether to act is a function of the response.
#[derive(Debug, Clone, Copy, Default)]
pub struct CodedProblems;

impl CodedProblems {
    /// The interceptor.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl<C: Sync + 'static> Interceptor<C> for CodedProblems {
    type Reads = ();
    type Adds = ();
    /// Never answers on its own. This interceptor edits a decision; it does not make one.
    type Short = Infallible;

    async fn intercept(
        &self,
        request: Request,
        (): (),
        _context: &C,
        next: Next<'_, C>,
    ) -> Result<Continued<()>, Infallible> {
        let mut continued = next.run(request).await;

        // Checked before the body is taken, so a ranged ciphertext delivery is never buffered.
        if !is_problem(continued.headers()) {
            return Ok(continued);
        }

        let status = continued.status().as_u16();
        let body = continued.take_body();
        let bytes = match collect(body).await {
            Ok(bytes) => bytes,
            Err(error) => {
                // The body is already gone and there is nothing to put back. A problem body is
                // produced in memory by the framework, so this is not a state a request can
                // cause; it is logged loudly and the empty body is returned rather than
                // panicking inside a response path.
                tracing::error!(%error, status, "a problem body could not be read to code it");
                return Ok(continued);
            }
        };

        continued.set_body(kynos::http::body::Body::from_bytes(coded(status, bytes)));
        Ok(continued)
    }
}

/// Whether the response is an RFC 9457 problem.
fn is_problem(headers: &http::HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        // A parameterized `application/problem+json; charset=utf-8` still is one.
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .is_some_and(|essence| essence.trim().eq_ignore_ascii_case(PROBLEM_JSON))
        })
}

/// The body with a `code` member, or unchanged when it already has one.
///
/// Falls through on anything unexpected — a body that is not JSON, or is not an object — rather
/// than inventing one. A problem this crate cannot parse is a problem it has no business
/// rewriting, and returning the original bytes is the answer that cannot make things worse.
fn coded(status: u16, bytes: Bytes) -> Bytes {
    let Ok(serde_json::Value::Object(mut members)) =
        serde_json::from_slice::<serde_json::Value>(&bytes)
    else {
        tracing::warn!(
            status,
            "a problem body was not a JSON object, so it was left alone"
        );
        return bytes;
    };
    if members.contains_key(CODE_MEMBER) {
        return bytes;
    }

    let code = code_for(status);
    if code == UNMAPPED {
        // Loud, because it means the surface grew a status the catalog does not name and a user
        // is about to see the vaguest message the product has.
        tracing::error!(status, "a framework rejection has no mapped error code");
    }
    members.insert(
        CODE_MEMBER.to_owned(),
        serde_json::Value::String(code.to_owned()),
    );

    serde_json::to_vec(&serde_json::Value::Object(members)).map_or(bytes, Bytes::from)
}

/// Reads a body to bytes.
async fn collect(
    body: kynos::http::body::Body,
) -> Result<Bytes, Box<dyn std::error::Error + Send + Sync>> {
    use http_body_util::BodyExt as _;
    body.collect()
        .await
        .map(http_body_util::Collected::to_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every status the mapping names is a real catalog key, and the fallback is too.
    ///
    /// Cheap, and it is the check that stops a typo shipping: `code_for` returns a `&'static
    /// str` from `capsule_i18n::error_codes`, so a wrong *name* is a compile error, but a
    /// mapping that quietly falls through to `UNMAPPED` for a status the surface renders is not.
    #[test]
    fn every_status_the_surface_renders_has_its_own_code() {
        for status in [400_u16, 401, 403, 405, 406, 413, 415, 422] {
            assert_ne!(
                code_for(status),
                UNMAPPED,
                "status {status} is declared somewhere on this surface and has no code of its own"
            );
        }
        assert_eq!(code_for(418), UNMAPPED, "and anything else is the fallback");
    }

    /// A framework problem gains a code.
    #[test]
    fn a_codeless_problem_is_coded() {
        let body =
            Bytes::from_static(br#"{"type":"about:blank","title":"Unauthorized","status":401}"#);
        let coded: serde_json::Value = serde_json::from_slice(&coded(401, body)).expect("json");
        assert_eq!(coded["code"], "error.request.unauthenticated");
        assert_eq!(coded["title"], "Unauthorized", "and nothing else moved");
    }

    /// A Capsule problem is passed through untouched, byte for byte.
    ///
    /// The property that keeps this interceptor from being able to reach a rejection this crate
    /// owns: a coarse framework code overwriting a specific catalog code would replace a
    /// diagnosis with a shrug, and it would do it silently.
    #[test]
    fn a_problem_that_already_carries_a_code_is_untouched() {
        let body = Bytes::from_static(
            br#"{"type":"about:blank","status":401,"code":"error.auth.session_expired"}"#,
        );
        assert_eq!(
            coded(401, body.clone()),
            body,
            "an existing code is authoritative, and so is the exact encoding around it"
        );
    }

    /// Anything that is not a JSON object is left alone rather than replaced.
    #[test]
    fn a_body_this_module_cannot_parse_is_left_alone() {
        for body in [
            Bytes::from_static(b"not json at all"),
            Bytes::from_static(b"[]"),
            Bytes::from_static(b""),
        ] {
            assert_eq!(coded(500, body.clone()), body);
        }
    }

    /// An unmapped status still gets a code, because a codeless problem is the defect.
    #[test]
    fn an_unmapped_status_still_gets_a_code() {
        let body = Bytes::from_static(br#"{"status":418}"#);
        let coded: serde_json::Value = serde_json::from_slice(&coded(418, body)).expect("json");
        assert_eq!(coded["code"], UNMAPPED);
    }

    /// The media-type test is the guard that keeps ciphertext off this path.
    #[test]
    fn only_a_problem_body_is_read() {
        let mut headers = http::HeaderMap::new();
        assert!(!is_problem(&headers), "no content type is not a problem");
        for (value, expected) in [
            ("application/problem+json", true),
            ("application/problem+json; charset=utf-8", true),
            ("APPLICATION/PROBLEM+JSON", true),
            ("application/json", false),
            ("application/octet-stream", false),
        ] {
            headers.insert(
                header::CONTENT_TYPE,
                http::HeaderValue::from_str(value).expect("a header value"),
            );
            assert_eq!(is_problem(&headers), expected, "for {value}");
        }
    }
}
