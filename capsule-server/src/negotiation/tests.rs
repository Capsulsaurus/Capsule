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
fn a_protocol_inside_the_window_passes_both_gates() {
    // Both ends are inclusive.
    for presented in ["2026-05-31", "2026-01-01", "2026-12-31"] {
        let read = headers(Some(presented), None, None);
        assert_eq!(negotiate(&policy(), &read), Ok(Verdict::InWindow));
        assert!(negotiate_write(&policy(), &read).is_ok());
        assert!(negotiate_read(&policy(), &read).is_ok());
    }
}

#[test]
fn a_protocol_outside_the_window_is_426_on_a_write_and_admitted_on_a_read() {
    // Past and future alike: the write rule is the window, the read rule is the grammar.
    for presented in ["2025-12-31", "2027-01-01", "1999-01-01", "2099-12-31"] {
        let read = headers(Some(presented), None, None);
        assert_eq!(negotiate(&policy(), &read), Ok(Verdict::OutOfWindow));
        let refused = negotiate_write(&policy(), &read).expect_err("a write is refused");
        assert!(
            matches!(
                &refused,
                NegotiationRejection::ProtocolUnsupported { protocol_min, protocol_max, .. }
                    if protocol_min == "2026-01-01" && protocol_max == "2026-12-31"
            ),
            "{presented}: {refused:?}"
        );
        assert_eq!(code(&refused), error_codes::PROTOCOL_VERSION_UNSUPPORTED);
        assert!(
            negotiate_read(&policy(), &read).is_ok(),
            "{presented}: reads of any version succeed"
        );
    }
}

#[test]
fn a_missing_or_non_date_protocol_is_400_on_every_gate() {
    for presented in [None, Some("yesterday"), Some("2026/05/31"), Some("")] {
        let read = headers(presented, None, None);
        let MalformedHandshake::Malformed { code, .. } =
            negotiate_read(&policy(), &read).expect_err("malformed");
        assert_eq!(code, error_codes::REQUEST_MALFORMED, "{presented:?}");
        let refused = negotiate_write(&policy(), &read).expect_err("malformed");
        assert!(
            matches!(refused, NegotiationRejection::Malformed { .. }),
            "{presented:?}: {refused:?}"
        );
        assert_eq!(self::code(&refused), error_codes::REQUEST_MALFORMED);
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
        let MalformedHandshake::Malformed { code, .. } =
            negotiate(&policy(), &headers(ok, suite, schema)).expect_err("refused");
        assert_eq!(
            code,
            error_codes::REQUEST_MALFORMED,
            "suite {suite:?}, schema {schema:?}"
        );
    }
}

#[test]
fn each_gate_declares_exactly_the_statuses_it_renders() {
    let mut statuses = NegotiationRejection::STATUSES.to_vec();
    statuses.sort_unstable();
    assert_eq!(statuses, [400, 426], "a write gate refuses two ways");
    assert_eq!(
        MalformedHandshake::STATUSES,
        [400],
        "a read gate refuses one way"
    );
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
    let documented: Vec<String> = NegotiationResponseHeaders::parameters(&mut Registry::default())
        .into_iter()
        .map(|parameter| parameter.name.to_ascii_lowercase())
        .collect();
    assert_eq!(documented, NegotiationResponseHeaders::NAMES);
}

/// A policy built past the configuration boundary with a non-header value is a programming
/// error, and is reported as one rather than silently sending a shorter response.
#[test]
#[should_panic(expected = "config validation should have refused")]
fn a_window_value_that_is_not_a_header_value_is_a_programming_error() {
    let window = NegotiationResponseHeaders {
        protocol_min: "2026-01-01".to_owned(),
        protocol_max: "bad\nvalue".to_owned(),
        min_client_build: "0.0.0".to_owned(),
    };
    let _ = window.encode();
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
