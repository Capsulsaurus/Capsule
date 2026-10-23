use super::*;

#[test]
fn the_callback_body_never_prints_its_credentials() {
    let request = OidcCallbackRequest {
        state: "live-state".to_owned(),
        code: "live-code".to_owned(),
        binding_verifier: "live-binding".to_owned(),
        cohort_hash: Some("cohort-1".to_owned()),
        device_id: None,
    };
    let printed = format!("{request:?}");
    assert!(
        !printed.contains("live-state")
            && !printed.contains("live-code")
            && !printed.contains("live-binding"),
        "{printed}"
    );
    assert!(printed.contains("cohort-1"), "{printed}");

    let response = OidcAuthorizationResponse {
        authorization_url: "https://idp/authorize?state=live-state".to_owned(),
        state: "live-state".to_owned(),
        expires_by: 42,
    };
    let printed = format!("{response:?}");
    assert!(!printed.contains("live-state"), "{printed}");
    assert!(printed.contains("42"), "{printed}");
}

#[test]
fn a_binding_matches_only_the_secret_it_was_hashed_from() {
    let secret = "a-client-held-secret-of-enough-length-000000";
    let challenge = code_challenge(&crate::store::PkceVerifier::new(secret));
    assert!(is_s256_challenge(&challenge), "{challenge}");
    assert!(binding_matches(&challenge, secret));
    assert!(!binding_matches(&challenge, "another-client's-secret"));
    assert!(!binding_matches(&challenge, ""));
    // A challenge is not its own verifier: presenting the hash is not presenting the secret.
    assert!(!binding_matches(&challenge, &challenge));
}

#[test]
fn only_the_shape_of_an_s256_hash_is_a_binding_challenge() {
    assert!(is_s256_challenge(&"A".repeat(S256_CHALLENGE_LEN)));
    // Every character class of the url-safe alphabet: 2 + 40 + 1 = 43.
    assert!(is_s256_challenge(&format!("-_{}0", "z9".repeat(20))));
    assert!(!is_s256_challenge(""));
    assert!(!is_s256_challenge(&"A".repeat(S256_CHALLENGE_LEN - 1)));
    assert!(!is_s256_challenge(&"A".repeat(S256_CHALLENGE_LEN + 1)));
    // Padded or standard-alphabet base64 is not the unpadded url-safe form.
    assert!(!is_s256_challenge(&format!(
        "{}=",
        "A".repeat(S256_CHALLENGE_LEN - 1)
    )));
    assert!(!is_s256_challenge(&format!(
        "{}+",
        "A".repeat(S256_CHALLENGE_LEN - 1)
    )));
    assert!(!is_s256_challenge(&format!(
        "{}é",
        "A".repeat(S256_CHALLENGE_LEN - 2)
    )));
}

#[test]
fn every_rejection_publishes_its_catalog_code() {
    assert!(matches!(
        OidcAuthorizeRejection::binding_malformed(),
        OidcAuthorizeRejection::BindingMalformed { code } if code == error_codes::REQUEST_MALFORMED
    ));
    assert!(matches!(
        OidcAuthorizeRejection::redirect_invalid(),
        OidcAuthorizeRejection::RedirectInvalid { code } if code == error_codes::AUTH_OIDC_REDIRECT_INVALID
    ));
    assert!(matches!(
        OidcAuthorizeRejection::not_configured(),
        OidcAuthorizeRejection::NotConfigured { code } if code == error_codes::AUTH_OIDC_NOT_CONFIGURED
    ));
    assert!(matches!(
        OidcAuthorizeRejection::provider_unavailable(),
        OidcAuthorizeRejection::Unavailable { code } if code == error_codes::AUTH_OIDC_UNAVAILABLE
    ));
    assert!(matches!(
        OidcAuthorizeRejection::store_unavailable(),
        OidcAuthorizeRejection::Unavailable { code } if code == error_codes::AUTH_UNAVAILABLE
    ));
    assert!(matches!(
        OidcAuthorizeRejection::rate_limited(),
        OidcAuthorizeRejection::RateLimited { code } if code == error_codes::AUTH_RATE_LIMITED
    ));
    assert!(matches!(
        OidcAuthorizeRejection::at_capacity(),
        OidcAuthorizeRejection::AtCapacity { code } if code == error_codes::AUTH_OIDC_AT_CAPACITY
    ));
    assert_ne!(
        error_codes::AUTH_OIDC_AT_CAPACITY,
        error_codes::AUTH_UNAVAILABLE,
        "the 503 and the 500 are different conditions and may not share a code"
    );
    assert!(matches!(
        OidcCallbackRejection::state_invalid(),
        OidcCallbackRejection::StateInvalid { code } if code == error_codes::AUTH_OIDC_STATE_INVALID
    ));
    assert!(matches!(
        OidcCallbackRejection::exchange_failed(),
        OidcCallbackRejection::ExchangeFailed { code } if code == error_codes::AUTH_OIDC_EXCHANGE_FAILED
    ));
    assert!(matches!(
        OidcCallbackRejection::token_invalid(),
        OidcCallbackRejection::TokenInvalid { code } if code == error_codes::AUTH_OIDC_TOKEN_INVALID
    ));
    assert!(matches!(
        OidcCallbackRejection::address_taken(),
        OidcCallbackRejection::AddressTaken { code } if code == error_codes::AUTH_OIDC_ADDRESS_TAKEN
    ));
    assert!(matches!(
        OidcCallbackRejection::provider_unavailable(),
        OidcCallbackRejection::Unavailable { code } if code == error_codes::AUTH_OIDC_UNAVAILABLE
    ));
}
