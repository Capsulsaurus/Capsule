//! The protocol handshake, as one declaration for the whole router (issue #404).
//!
//! # The contract
//!
//! [Threat Model — Protocol and Capability
//! Negotiation](../../capsule-docs/src/content/docs/design/threat-model/validation.md) puts six
//! headers on the wire and says they are applied "by shared Kynos middleware to every public
//! route": three the client sends — `X-Capsule-Protocol`, `X-Capsule-Crypto-Suite`,
//! `X-Capsule-Sidecar-Schema` — and three the server answers with on **every** response of
//! every operation —
//! `X-Capsule-Protocol-Min`, `X-Capsule-Protocol-Max`, `X-Capsule-Min-Client-Build`. A
//! protocol outside the window is `426` with `error.protocol.version_unsupported`; a suite the
//! inventory does not name or a sidecar schema newer than this build knows is `400`.
//!
//! # Where it was broken, and why
//!
//! Before this module the handshake lived in `routes/upload.rs` as a per-route helper: four
//! operations read the request header, and the response window rode as problem *extension
//! members* because a Kynos `ApiError` "has no seam for a response header". Meanwhile
//! `capsule-sdk/src/upload.rs` reads the window **from headers** — and got `None` every time.
//! The `426` recovery path the design promises was dead on both ends, and no route outside the
//! upload surface advertised anything at all.
//!
//! Kynos *does* have the seam; it is just not on the error type. An [`Interceptor`]'s three
//! associated types are its declaration — `Reads` contributes request parameters, `Short`
//! contributes responses, `Adds` contributes response headers — and an interceptor sees every
//! response the chain beneath it produces, a short-circuit included. So the window belongs on an
//! interceptor mounted outside everything that can refuse, not on each refusal.
//!
//! # Two interceptors, deliberately
//!
//! - [`Negotiation`] **advertises**. `Reads = ()`, `Short = Infallible`, `Adds` the three
//!   response headers. Mounted on the router, outside the body-size limit, so a `413`, a
//!   `401`, a `426` and a `200` all leave with the window on them. It cannot refuse anything.
//!   What it cannot reach is a response the router produced *before* choosing an operation —
//!   an unrouted `404` or `405` — because Kynos runs interceptors per operation, after routing.
//! - [`ProtocolGate`] **refuses**. `Reads` the three request headers, `Adds = ()`, `Short` is
//!   [`NegotiationRejection`]. Mounted on a `Group`, which is how an exemption is spelled:
//!   an operation outside the group is not gated and still carries the response headers.
//!
//! One interceptor doing both would make the exemption impossible to express — the response
//! headers are wanted everywhere and the gate is not — and Kynos's conflict check would refuse
//! a second copy of either at a narrower scope.
//!
//! # What the gate reads, and how strictly
//!
//! `X-Capsule-Protocol` is required: absent is a `400`, not a date is a `400`, outside the
//! window is a `426`. The other two are validated **when present** — a suite the inventory does
//! not implement and a sidecar schema above [`MAX_KNOWN_SIDECAR_SCHEMA`] are each a `400` — and
//! their absence is not refused. The design scopes `X-Capsule-Crypto-Suite` to writes and
//! `X-Capsule-Sidecar-Schema` to metadata updates, every write already carries its suite in a
//! body the envelope gate checks, and a gate that demanded a header on a read that has no use
//! for it would refuse every client for a value nobody reads.
//!
//! All three are read as strings and parsed here rather than typed by the framework, so a
//! malformed value is *this* module's coded `400` and not the framework's uncoded one.
//!
//! # `X-Capsule-Min-Client-Build` is advisory
//!
//! The design says "advisory unless the path is hard-deprecated", and no path is. The header is
//! sent, the value is the policy's, and nothing refuses on it. A deployment that has announced
//! no cutoff publishes `0.0.0`, which every build satisfies — the honest spelling of "no cutoff",
//! rather than an absent header a client could not tell from a server that never speaks it.
//!
//! # The document
//!
//! `Reads` and `Short` describe themselves through the interceptor's types. `Adds` describes
//! itself only on success responses (Kynos attaches an interceptor's response headers at
//! `StatusPattern::Success`, `kynos/src/middleware/erased.rs`), so
//! [`crate::openapi`] walks the emitted document once and files the same three headers under
//! every other response. The names and schemas both come from [`response_header_declarations`],
//! so the document and the wire cannot disagree about what a header is called.

use std::convert::Infallible;

use capsule_core::validation::protocol::{
    HandshakeReject, check_sidecar_schema, check_suite, protocol_gate,
};
use capsule_i18n::error_codes;
use kynos::di::Provides;
use kynos::error::rejection::HeaderRejection;
use kynos::extract::params::header::{DecodeHeaders, EncodeHeaders, HeaderParams};
use kynos::http::{HeaderMap, HeaderName, HeaderValue, Request};
use kynos::middleware::{Continued, Interceptor, Next};
use kynos::openapi::{Header, Parameter, Schema};
use kynos::prelude::*;
use kynos::schema::registry::Registry;

use crate::upload::{UploadContext, UploadPolicy};

/// The request header carrying the `YYYY-MM-DD` protocol version the request is written against.
pub const PROTOCOL: &str = "X-Capsule-Protocol";
/// The request header carrying the `u16` crypto suite id, on writes.
pub const CRYPTO_SUITE: &str = "X-Capsule-Crypto-Suite";
/// The request header carrying the `u16` sidecar schema version, on metadata updates.
pub const SIDECAR_SCHEMA: &str = "X-Capsule-Sidecar-Schema";
/// The response header carrying the oldest protocol version this server accepts.
pub const PROTOCOL_MIN: &str = "X-Capsule-Protocol-Min";
/// The response header carrying the newest protocol version this server accepts.
pub const PROTOCOL_MAX: &str = "X-Capsule-Protocol-Max";
/// The response header carrying the advisory semver deprecation cutoff.
pub const MIN_CLIENT_BUILD: &str = "X-Capsule-Min-Client-Build";

/// The newest sidecar schema this build indexes.
///
/// `capsule-core` keeps `SIDECAR_SCHEMA_V1` crate-private behind its frozen barrel (`#399`), so
/// the server states the number it will acknowledge here. The server never parses a sidecar —
/// this is the Postel cross-version closure the threat model asks for: a write whose schema
/// number this build cannot index is refused rather than acknowledged and lost.
pub const MAX_KNOWN_SIDECAR_SCHEMA: u16 = 1;

// ===========================================================================================
// The request half
// ===========================================================================================

/// The three request headers the handshake reads.
///
/// Every field is optional at the *type* level so that a missing or unreadable one is this
/// module's coded rejection rather than the framework's uncoded `HeaderRejection`; whether a
/// header is required is decided by [`negotiate`] and declared by [`HeaderParams::parameters`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProtocolRequestHeaders {
    /// `X-Capsule-Protocol`, verbatim.
    pub protocol: Option<String>,
    /// `X-Capsule-Crypto-Suite`, verbatim.
    pub crypto_suite: Option<String>,
    /// `X-Capsule-Sidecar-Schema`, verbatim.
    pub sidecar_schema: Option<String>,
}

impl HeaderParams for ProtocolRequestHeaders {
    // Lower-case, because these are what the conflict check compares and what a decoder looks
    // up; the document spells them in their canonical case below.
    const NAMES: &'static [&'static str] = &[
        "x-capsule-protocol",
        "x-capsule-crypto-suite",
        "x-capsule-sidecar-schema",
    ];

    fn parameters(registry: &mut Registry) -> Vec<Parameter> {
        let _ = registry;
        vec![
            Parameter::header(PROTOCOL, date_schema())
                .required(true)
                .with_description(
                    "The `YYYY-MM-DD` protocol version this request is written against. \
                     Outside the server's `[X-Capsule-Protocol-Min, X-Capsule-Protocol-Max]` \
                     window the request is refused with `426`.",
                ),
            Parameter::header(CRYPTO_SUITE, u16_schema())
                .required(false)
                .with_description(
                    "The crypto suite id from the primitives inventory. Sent on writes; a suite \
                     this server does not implement is refused with `400`.",
                ),
            Parameter::header(SIDECAR_SCHEMA, u16_schema())
                .required(false)
                .with_description(
                    "The sidecar schema version declared at `sidecar_schema` field 0. Sent on \
                     metadata updates; a schema newer than this server indexes is refused with \
                     `400`.",
                ),
        ]
    }
}

impl DecodeHeaders for ProtocolRequestHeaders {
    fn decode(headers: &HeaderMap) -> Result<Self, HeaderRejection> {
        Ok(Self {
            protocol: read(headers, PROTOCOL)?,
            crypto_suite: read(headers, CRYPTO_SUITE)?,
            sidecar_schema: read(headers, SIDECAR_SCHEMA)?,
        })
    }
}

/// One header as text, or `None` when absent.
///
/// The only failure is a value that is not visible ASCII, which is the one thing that cannot be
/// turned into a coded rejection here because it cannot be turned into a `String` at all.
fn read(headers: &HeaderMap, name: &str) -> Result<Option<String>, HeaderRejection> {
    headers
        .get(name)
        .map(|value| {
            value
                .to_str()
                .map(str::to_owned)
                .map_err(|_| HeaderRejection::Invalid {
                    name: name.to_owned(),
                    detail: "the value is not printable ASCII".to_owned(),
                })
        })
        .transpose()
}

/// Why the handshake refused a request.
///
/// The `426` carries the window in its `detail` for a human and **on the response headers**
/// for a client — [`Negotiation`] sits outside this gate, so the refusal leaves with
/// `X-Capsule-Protocol-Min`/`-Max` on it like every other response. No extension member
/// restates them: two spellings of one fact is the drift the census exists to prevent.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum NegotiationRejection {
    /// `X-Capsule-Protocol` is a date outside `[min, max]`.
    #[error("this server accepts protocol versions [{protocol_min}, {protocol_max}]")]
    #[problem(status = 426, title = "Protocol version unsupported")]
    ProtocolUnsupported {
        /// The lowest version this server accepts.
        protocol_min: String,
        /// The highest version this server accepts.
        protocol_max: String,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A handshake header is missing, unreadable, or names something this server does not
    /// implement. The `detail` says which.
    #[error("{detail}")]
    #[problem(status = 400, title = "Malformed handshake")]
    Malformed {
        /// What was wrong, in English. Reaches the client as the problem's `detail`.
        detail: String,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

impl NegotiationRejection {
    fn malformed(detail: impl Into<String>) -> Self {
        Self::Malformed {
            detail: detail.into(),
            code: error_codes::REQUEST_MALFORMED,
        }
    }
}

/// The one-shot handshake: a client either speaks a version this server accepts, or it is
/// refused before any state is read or written. There is no negotiation and no degrade.
///
/// Pure, so the three outcomes are unit-tested without a router.
///
/// # Errors
///
/// `426` for a protocol outside the window; `400` for a missing or non-date protocol, a suite
/// the inventory does not name, or a sidecar schema above [`MAX_KNOWN_SIDECAR_SCHEMA`].
pub fn negotiate(
    policy: &UploadPolicy,
    headers: &ProtocolRequestHeaders,
) -> Result<(), NegotiationRejection> {
    let Some(protocol) = headers.protocol.as_deref() else {
        return Err(NegotiationRejection::malformed(format!(
            "{PROTOCOL} is required on this operation"
        )));
    };
    match protocol_gate(protocol, policy.protocol_min(), policy.protocol_max()) {
        Ok(()) => {}
        Err(HandshakeReject::ProtocolOutOfRange) => {
            tracing::info!(
                presented = protocol,
                min = policy.protocol_min(),
                max = policy.protocol_max(),
                "a request was refused: protocol version outside the accepted window"
            );
            return Err(NegotiationRejection::ProtocolUnsupported {
                protocol_min: policy.protocol_min().to_owned(),
                protocol_max: policy.protocol_max().to_owned(),
                code: error_codes::PROTOCOL_VERSION_UNSUPPORTED,
            });
        }
        Err(_) => {
            tracing::debug!(
                presented = protocol,
                "a request was refused: protocol is not a date"
            );
            return Err(NegotiationRejection::malformed(format!(
                "{PROTOCOL} is not a YYYY-MM-DD date"
            )));
        }
    }

    if let Some(suite) = headers.crypto_suite.as_deref() {
        let id = suite.trim().parse::<u16>().map_err(|_| {
            NegotiationRejection::malformed(format!("{CRYPTO_SUITE} is not a u16 suite id"))
        })?;
        if check_suite(id).is_err() {
            tracing::debug!(
                suite = id,
                "a request was refused: crypto suite not implemented"
            );
            return Err(NegotiationRejection::malformed(format!(
                "{CRYPTO_SUITE} {id} is not in this server's inventory"
            )));
        }
    }

    if let Some(schema) = headers.sidecar_schema.as_deref() {
        let version = schema.trim().parse::<u16>().map_err(|_| {
            NegotiationRejection::malformed(format!("{SIDECAR_SCHEMA} is not a u16 schema version"))
        })?;
        if check_sidecar_schema(version, MAX_KNOWN_SIDECAR_SCHEMA).is_err() {
            tracing::debug!(
                schema = version,
                max_known = MAX_KNOWN_SIDECAR_SCHEMA,
                "a request was refused: sidecar schema newer than this server indexes"
            );
            return Err(NegotiationRejection::malformed(format!(
                "{SIDECAR_SCHEMA} {version} is newer than this server indexes \
                 ({MAX_KNOWN_SIDECAR_SCHEMA})"
            )));
        }
    }

    Ok(())
}

// ===========================================================================================
// The response half
// ===========================================================================================

/// The three response headers every response carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegotiationResponseHeaders {
    /// `X-Capsule-Protocol-Min`.
    pub protocol_min: String,
    /// `X-Capsule-Protocol-Max`.
    pub protocol_max: String,
    /// `X-Capsule-Min-Client-Build`.
    pub min_client_build: String,
}

impl NegotiationResponseHeaders {
    /// The window a policy advertises — the same values it enforces, read from one place.
    #[must_use]
    pub fn advertise(policy: &UploadPolicy) -> Self {
        Self {
            protocol_min: policy.protocol_min().to_owned(),
            protocol_max: policy.protocol_max().to_owned(),
            min_client_build: policy.min_client_build().to_owned(),
        }
    }
}

/// The response headers, as `(name, schema, description)`, in wire order.
///
/// The one source for the document's two spellings of them — the header parameters Kynos
/// describes on success responses through [`HeaderParams::parameters`], and the response headers
/// the post-emit walk in [`crate::openapi`] files under every other response — so the two cannot
/// disagree about what a header is called or what it carries.
fn declarations() -> [(&'static str, Schema, &'static str); 3] {
    [
        (
            PROTOCOL_MIN,
            date_schema(),
            "The oldest protocol version this server accepts.",
        ),
        (
            PROTOCOL_MAX,
            date_schema(),
            "The newest protocol version this server accepts.",
        ),
        (
            MIN_CLIENT_BUILD,
            semver_schema(),
            "The semver client build below which this server will stop answering. Advisory: \
             `0.0.0` when no cutoff has been announced.",
        ),
    ]
}

/// The response headers as a response's `headers` map declares them: `(name, header)`.
#[must_use]
pub fn response_header_declarations() -> Vec<(&'static str, Header)> {
    declarations()
        .into_iter()
        .map(|(name, schema, description)| {
            (
                name,
                Header::new(schema)
                    .required(true)
                    .with_description(description),
            )
        })
        .collect()
}

impl HeaderParams for NegotiationResponseHeaders {
    const NAMES: &'static [&'static str] = &[
        "x-capsule-protocol-min",
        "x-capsule-protocol-max",
        "x-capsule-min-client-build",
    ];

    fn parameters(registry: &mut Registry) -> Vec<Parameter> {
        let _ = registry;
        declarations()
            .into_iter()
            .map(|(name, schema, description)| {
                Parameter::header(name, schema)
                    .required(true)
                    .with_description(description)
            })
            .collect()
    }
}

impl EncodeHeaders for NegotiationResponseHeaders {
    fn encode(&self) -> Vec<(HeaderName, HeaderValue)> {
        [
            ("x-capsule-protocol-min", self.protocol_min.as_str()),
            ("x-capsule-protocol-max", self.protocol_max.as_str()),
            ("x-capsule-min-client-build", self.min_client_build.as_str()),
        ]
        .into_iter()
        .filter_map(|(name, value)| match HeaderValue::from_str(value) {
            Ok(value) => Some((HeaderName::from_static(name), value)),
            Err(error) => {
                // A policy value that is not a header value is a deployment fault, not a request
                // fault; the response goes out without it rather than not at all, and the log
                // says why. Config checks only that the window is ordered, not that its ends
                // are header values, so this branch is reachable from a misconfiguration.
                tracing::error!(%error, name, value, "a protocol window value is not a header value");
                None
            }
        })
        .collect()
    }
}

// ===========================================================================================
// The interceptors
// ===========================================================================================

/// Advertises the protocol window on every response.
///
/// Mounted on the whole router and outside the body-size limit, so nothing that refuses a
/// request — the framework's `413`, the bearer scheme's `401`, [`ProtocolGate`]'s `426` — can
/// answer without it. Cannot refuse: `Short` is [`Infallible`].
#[derive(Debug, Clone, Copy, Default)]
pub struct Negotiation;

impl Negotiation {
    /// The interceptor.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl<C> Interceptor<C> for Negotiation
where
    C: Provides<UploadContext> + Sync + 'static,
{
    type Reads = ();
    type Adds = NegotiationResponseHeaders;
    /// Advertising never refuses.
    type Short = Infallible;

    async fn intercept(
        &self,
        request: Request,
        (): (),
        context: &C,
        next: Next<'_, C>,
    ) -> Result<Continued<NegotiationResponseHeaders>, Infallible> {
        let upload: UploadContext = context.provide();
        let window = NegotiationResponseHeaders::advertise(upload.policy());
        Ok(next.run(request).await.with_headers(window))
    }
}

/// Refuses a request whose handshake this server cannot honour, before the handler runs.
///
/// Mounted on a `Group` rather than the router, because the exemptions the design names — the
/// reachability probe, public discovery, share reads, guest deposits — are expressed by mounting
/// those operations outside the group. See `lib.rs::router` for the list and the reasons.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProtocolGate;

impl ProtocolGate {
    /// The interceptor.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl<C> Interceptor<C> for ProtocolGate
where
    C: Provides<UploadContext> + Sync + 'static,
{
    type Reads = ProtocolRequestHeaders;
    type Adds = ();
    type Short = NegotiationRejection;

    async fn intercept(
        &self,
        request: Request,
        reads: ProtocolRequestHeaders,
        context: &C,
        next: Next<'_, C>,
    ) -> Result<Continued<()>, NegotiationRejection> {
        let upload: UploadContext = context.provide();
        negotiate(upload.policy(), &reads)?;
        Ok(next.run(request).await)
    }
}

// ===========================================================================================
// Schemas
// ===========================================================================================

/// A `YYYY-MM-DD` protocol date.
fn date_schema() -> Schema {
    serde_json::from_value(serde_json::json!({
        "type": "string",
        "pattern": "^[0-9]{4}-[0-9]{2}-[0-9]{2}$",
    }))
    .expect("a literal date schema is a schema")
}

/// A `u16`, as a header carries it.
fn u16_schema() -> Schema {
    serde_json::from_value(serde_json::json!({
        "type": "integer",
        "minimum": 0,
        "maximum": 65535,
    }))
    .expect("a literal integer schema is a schema")
}

/// A semver build.
fn semver_schema() -> Schema {
    serde_json::from_value(serde_json::json!({
        "type": "string",
        "pattern": "^[0-9]+\\.[0-9]+\\.[0-9]+$",
    }))
    .expect("a literal semver schema is a schema")
}

#[cfg(test)]
mod tests {
    use kynos::response::ShortCircuit as _;

    use super::*;

    fn headers(
        protocol: Option<&str>,
        suite: Option<&str>,
        schema: Option<&str>,
    ) -> ProtocolRequestHeaders {
        ProtocolRequestHeaders {
            protocol: protocol.map(str::to_owned),
            crypto_suite: suite.map(str::to_owned),
            sidecar_schema: schema.map(str::to_owned),
        }
    }

    fn policy() -> UploadPolicy {
        UploadPolicy::default().with_protocol_window("2026-01-01", "2026-12-31")
    }

    fn code(rejection: &NegotiationRejection) -> &'static str {
        match rejection {
            NegotiationRejection::ProtocolUnsupported { code, .. }
            | NegotiationRejection::Malformed { code, .. } => code,
        }
    }

    #[test]
    fn a_protocol_inside_the_window_passes() {
        assert!(negotiate(&policy(), &headers(Some("2026-05-31"), None, None)).is_ok());
        // Both ends are inclusive.
        assert!(negotiate(&policy(), &headers(Some("2026-01-01"), None, None)).is_ok());
        assert!(negotiate(&policy(), &headers(Some("2026-12-31"), None, None)).is_ok());
    }

    #[test]
    fn a_protocol_outside_the_window_is_426_with_the_window() {
        for presented in ["2025-12-31", "2027-01-01"] {
            let refused = negotiate(&policy(), &headers(Some(presented), None, None))
                .expect_err("outside the window");
            assert!(
                matches!(
                    &refused,
                    NegotiationRejection::ProtocolUnsupported { protocol_min, protocol_max, .. }
                        if protocol_min == "2026-01-01" && protocol_max == "2026-12-31"
                ),
                "{presented}: {refused:?}"
            );
            assert_eq!(code(&refused), error_codes::PROTOCOL_VERSION_UNSUPPORTED);
        }
    }

    #[test]
    fn a_missing_or_non_date_protocol_is_400_not_426() {
        for presented in [None, Some("yesterday"), Some("2026/05/31"), Some("")] {
            let refused =
                negotiate(&policy(), &headers(presented, None, None)).expect_err("malformed");
            assert!(
                matches!(refused, NegotiationRejection::Malformed { .. }),
                "{presented:?}: {refused:?}"
            );
            assert_eq!(code(&refused), error_codes::REQUEST_MALFORMED);
        }
    }

    #[test]
    fn the_suite_and_the_sidecar_schema_are_checked_when_present() {
        let ok = Some("2026-05-31");
        let suite = capsule_core::crypto::primitives::CRYPTO_SUITE_ID.to_string();
        assert!(negotiate(&policy(), &headers(ok, Some(&suite), Some("1"))).is_ok());
        assert!(negotiate(&policy(), &headers(ok, Some(&suite), Some("0"))).is_ok());

        for (suite, schema) in [
            (Some("9999"), None),
            (Some("not a number"), None),
            (None, Some("2")),
            (None, Some("v1")),
        ] {
            let refused = negotiate(&policy(), &headers(ok, suite, schema)).expect_err("refused");
            assert!(
                matches!(refused, NegotiationRejection::Malformed { .. }),
                "suite {suite:?}, schema {schema:?}: {refused:?}"
            );
            assert_eq!(code(&refused), error_codes::REQUEST_MALFORMED);
        }
    }

    #[test]
    fn the_rejection_declares_exactly_the_two_statuses_it_renders() {
        let mut statuses = NegotiationRejection::STATUSES.to_vec();
        statuses.sort_unstable();
        assert_eq!(statuses, [400, 426]);
    }

    #[test]
    fn the_response_group_encodes_what_it_declares() {
        let window = NegotiationResponseHeaders {
            protocol_min: "2026-01-01".to_owned(),
            protocol_max: "2026-12-31".to_owned(),
            min_client_build: "0.0.0".to_owned(),
        };
        let encoded: Vec<(String, String)> = window
            .encode()
            .into_iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    value.to_str().expect("ascii").to_owned(),
                )
            })
            .collect();
        assert_eq!(
            encoded,
            [
                ("x-capsule-protocol-min".to_owned(), "2026-01-01".to_owned()),
                ("x-capsule-protocol-max".to_owned(), "2026-12-31".to_owned()),
                ("x-capsule-min-client-build".to_owned(), "0.0.0".to_owned()),
            ]
        );

        // The declared names, the encoded names and the documented names are one list.
        let declared: Vec<String> = response_header_declarations()
            .into_iter()
            .map(|(name, _)| name.to_ascii_lowercase())
            .collect();
        assert_eq!(declared, NegotiationResponseHeaders::NAMES);
        let documented: Vec<String> =
            NegotiationResponseHeaders::parameters(&mut Registry::default())
                .into_iter()
                .map(|parameter| parameter.name.to_ascii_lowercase())
                .collect();
        assert_eq!(documented, NegotiationResponseHeaders::NAMES);
    }

    #[test]
    fn a_window_value_that_is_not_a_header_value_is_dropped_not_panicked() {
        let window = NegotiationResponseHeaders {
            protocol_min: "2026-01-01".to_owned(),
            protocol_max: "bad\nvalue".to_owned(),
            min_client_build: "0.0.0".to_owned(),
        };
        assert_eq!(window.encode().len(), 2);
    }

    #[test]
    fn the_request_group_documents_the_protocol_as_required_and_the_rest_as_optional() {
        let parameters = ProtocolRequestHeaders::parameters(&mut Registry::default());
        let required: Vec<(&str, Option<bool>)> = parameters
            .iter()
            .map(|parameter| (parameter.name.as_str(), parameter.required))
            .collect();
        assert_eq!(
            required,
            [
                (PROTOCOL, Some(true)),
                (CRYPTO_SUITE, Some(false)),
                (SIDECAR_SCHEMA, Some(false)),
            ]
        );
        let declared: Vec<String> = parameters
            .iter()
            .map(|parameter| parameter.name.to_ascii_lowercase())
            .collect();
        assert_eq!(declared, ProtocolRequestHeaders::NAMES);
    }

    #[test]
    fn decoding_reads_each_header_verbatim_and_tolerates_absence() {
        let mut map = HeaderMap::new();
        assert_eq!(
            ProtocolRequestHeaders::decode(&map).expect("absent is fine"),
            ProtocolRequestHeaders::default()
        );
        map.insert("x-capsule-protocol", HeaderValue::from_static("2026-05-31"));
        map.insert("x-capsule-crypto-suite", HeaderValue::from_static("1"));
        assert_eq!(
            ProtocolRequestHeaders::decode(&map).expect("decodes"),
            headers(Some("2026-05-31"), Some("1"), None)
        );
        map.insert(
            "x-capsule-sidecar-schema",
            HeaderValue::from_bytes(b"\xff").expect("opaque bytes are a header value"),
        );
        assert!(ProtocolRequestHeaders::decode(&map).is_err());
    }
}
