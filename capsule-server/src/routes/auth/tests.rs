use super::*;

#[test]
fn a_login_request_never_prints_its_credentials() {
    let request = LoginRequest {
        email: "somebody@example.test".to_owned(),
        password: "correct horse battery staple".to_owned(),
        cohort_hash: Some("cohort-1".to_owned()),
        device_id: None,
    };

    let printed = format!("{request:?}");
    assert!(
        !printed.contains("correct horse battery staple"),
        "a password must not reach a log through Debug, got {printed}"
    );
    assert!(
        !printed.contains("somebody@example.test"),
        "an account's email must not reach a log through Debug, got {printed}"
    );
    assert!(
        printed.contains("cohort-1"),
        "the advisory fields are not secrets and stay legible, got {printed}"
    );
}

#[test]
fn a_refresh_request_never_prints_its_token() {
    let request = RefreshRequest {
        refresh_token: "a-live-refresh-token".to_owned(),
    };
    assert!(!format!("{request:?}").contains("a-live-refresh-token"));
}

#[test]
fn a_token_response_never_prints_its_tokens() {
    let response = TokenResponse {
        access_token: "an-access-token".to_owned(),
        refresh_token: "a-refresh-token".to_owned(),
        token_type: "Bearer".to_owned(),
        expires_by: 42,
    };

    let printed = format!("{response:?}");
    assert!(!printed.contains("an-access-token") && !printed.contains("a-refresh-token"));
    assert!(printed.contains("Bearer") && printed.contains("42"));
}

#[test]
fn a_cohort_hash_is_trimmed_kept_or_dropped() {
    assert_eq!(normalize_cohort_hash(None), None);
    assert_eq!(normalize_cohort_hash(Some("   ")), None);
    assert_eq!(
        normalize_cohort_hash(Some("  abc  ")),
        Some("abc".to_owned())
    );

    let at_the_limit = "x".repeat(MAX_COHORT_HASH_LEN);
    assert_eq!(
        normalize_cohort_hash(Some(&at_the_limit)),
        Some(at_the_limit.clone())
    );

    let over_the_limit = "x".repeat(MAX_COHORT_HASH_LEN + 1);
    assert_eq!(
        normalize_cohort_hash(Some(&over_the_limit)),
        None,
        "an implausible cohort is dropped, not stored and not refused"
    );
}

#[test]
fn a_device_id_is_parsed_once_or_dropped() {
    assert_eq!(normalize_device_id(None), None);
    assert_eq!(normalize_device_id(Some("not-a-uuid")), None);
    assert_eq!(
        normalize_device_id(Some("00000000-0000-0000-0000-000000000000")),
        None,
        "the nil uuid is what a client sends when it has none"
    );

    // Accepted in any spelling `Uuid::parse_str` reads, and normalized by being a `Uuid`
    // rather than by a second pass over a string.
    let canonical =
        Uuid::parse_str("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e6f").expect("the literal is a uuid");
    for spelling in [
        " 018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e6f ",
        "018F3F1E-4B7A-7C9D-8E2F-1A2B3C4D5E6F",
        "urn:uuid:018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e6f",
        "{018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e6f}",
    ] {
        assert_eq!(
            normalize_device_id(Some(spelling)),
            Some(canonical),
            "{spelling:?} names the same device"
        );
    }
}

#[test]
fn session_ids_are_distinct_and_time_ordered() {
    let first = new_session_id();
    let second = new_session_id();
    assert_ne!(first, second, "two sessions are never one session");

    // UUIDv7 sorts by creation time, which is what makes the session listing's "oldest
    // first" order stable when two records share a `created_at` second.
    assert!(first.as_str() < second.as_str());
}

#[test]
fn every_rejection_publishes_its_catalog_code() {
    // The `error.*` code is the discriminator clients switch on, so a variant that lost one
    // — or gained the wrong one — is a client that cannot localize the failure.
    assert!(matches!(
        LoginRejection::invalid_credentials(),
        LoginRejection::InvalidCredentials { code } if code == error_codes::AUTH_INVALID_CREDENTIALS
    ));
    assert!(matches!(
        LoginRejection::account_locked(),
        LoginRejection::AccountLocked { code } if code == error_codes::AUTH_ACCOUNT_LOCKED
    ));
    assert!(matches!(
        LoginRejection::unavailable(),
        LoginRejection::Unavailable { code } if code == error_codes::AUTH_UNAVAILABLE
    ));
    assert!(matches!(
        RefreshRejection::session_expired(),
        RefreshRejection::SessionExpired { code } if code == error_codes::AUTH_SESSION_EXPIRED
    ));
    assert!(matches!(
        RefreshRejection::unavailable(),
        RefreshRejection::Unavailable { code } if code == error_codes::AUTH_UNAVAILABLE
    ));
    assert_eq!(
        LogoutRejection::unavailable().code,
        error_codes::AUTH_UNAVAILABLE
    );
}
