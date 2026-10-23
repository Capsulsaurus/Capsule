use super::*;

#[test]
fn the_configured_redirect_is_admitted_exactly() {
    let policy = RedirectPolicy::new(Some("https://app.example.test/cb".to_owned()), false);
    assert!(policy.admits("https://app.example.test/cb"));
    assert!(!policy.admits("https://app.example.test/cb/"));
    assert!(!policy.admits("https://app.example.test/cb?x=1"));
    assert!(
        !policy.admits("http://127.0.0.1:4242/cb"),
        "loopback is off"
    );
}

#[test]
fn loopback_is_admitted_on_any_port_and_only_as_an_ip_literal() {
    let policy = RedirectPolicy::new(None, true);
    assert!(policy.admits("http://127.0.0.1:4242/cb"));
    assert!(policy.admits("http://127.0.0.1/cb"));
    assert!(policy.admits("http://[::1]:9/"));
    assert!(!policy.admits("http://localhost:4242/cb"), "RFC 8252 §8.3");
    assert!(
        !policy.admits("https://127.0.0.1:4242/cb"),
        "loopback is plain http"
    );
    assert!(
        !policy.admits("http://127.0.0.1:4242/cb#frag"),
        "RFC 6749 §3.1.2"
    );
    assert!(!policy.admits("http://10.0.0.1:4242/cb"));
    assert!(!policy.admits("not a url"));
}

#[test]
fn nothing_is_admitted_by_an_empty_policy() {
    let policy = RedirectPolicy::new(None, false);
    assert!(!policy.admits("http://127.0.0.1:4242/cb"));
    assert!(!policy.admits(""));
}

#[test]
fn ceremony_material_is_fresh_and_the_challenge_is_s256() {
    assert_ne!(fresh_state(), fresh_state());
    assert_ne!(fresh_nonce(), fresh_nonce());
    let verifier = fresh_verifier();
    assert_eq!(
        verifier.as_str().len(),
        43,
        "RFC 7636 §4.1: 43 to 128 characters"
    );
    // RFC 7636 appendix B's worked example.
    let example = PkceVerifier::new("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
    assert_eq!(
        code_challenge(&example),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
}

#[test]
fn a_client_secret_never_prints_itself() {
    let settings = OidcSettings {
        issuer: "https://idp.example.test".to_owned(),
        client_id: "capsule".to_owned(),
        client_secret: Some(ClientSecret::new("hunter2")),
        redirects: RedirectPolicy::new(None, true),
    };
    let printed = format!("{settings:?}");
    assert!(!printed.contains("hunter2"), "{printed}");
    assert!(printed.contains("<redacted>"), "{printed}");
}

fn device_body() -> DeviceAuthorizationResponse {
    DeviceAuthorizationResponse {
        device_code: "device-code".to_owned(),
        user_code: "WDJB-MJHT".to_owned(),
        verification_uri: "https://idp.example.test/device".to_owned(),
        verification_uri_complete: Some(
            "https://idp.example.test/device?user_code=WDJB-MJHT".to_owned(),
        ),
        expires_in: 900,
        interval: Some(8),
    }
}

#[test]
fn a_device_authorization_is_admitted_with_its_interval_floored() {
    let issuer = "https://idp.example.test";
    let admitted = admit_device_authorization(device_body(), issuer).expect("admitted");
    assert_eq!(admitted.device_code, DeviceCode::new("device-code"));
    assert_eq!(admitted.user_code, "WDJB-MJHT");
    assert_eq!(admitted.expires_in, SignedDuration::from_secs(900));
    assert_eq!(admitted.interval, SignedDuration::from_secs(8));
    assert!(
        !format!("{admitted:?}").contains("WDJB"),
        "the user code never prints itself"
    );

    let mut quick = device_body();
    quick.interval = Some(1);
    assert_eq!(
        admit_device_authorization(quick, issuer)
            .expect("admitted")
            .interval,
        DEVICE_POLL_FLOOR,
        "a provider naming less than the floor is raised to it"
    );
    let mut silent = device_body();
    silent.interval = None;
    silent.verification_uri_complete = None;
    let silent = admit_device_authorization(silent, issuer).expect("admitted");
    assert_eq!(
        silent.interval, DEVICE_POLL_FLOOR,
        "RFC 8628 §3.2's default"
    );
    assert_eq!(silent.verification_uri_complete, None);
}

#[test]
fn a_device_authorization_that_would_mislead_the_person_is_refused() {
    let issuer = "https://idp.example.test";
    let mut cases = Vec::new();
    let mut plain = device_body();
    plain.verification_uri = "http://idp.example.test/device".to_owned();
    cases.push(("a plain-http verification address", plain));
    let mut complete = device_body();
    complete.verification_uri_complete = Some("javascript:alert(1)".to_owned());
    cases.push(("a complete address that is not https", complete));
    let mut empty = device_body();
    empty.user_code = String::new();
    cases.push(("an empty user code", empty));
    let mut long = device_body();
    long.user_code = "A".repeat(MAX_USER_CODE_LENGTH + 1);
    cases.push(("an over-long user code", long));
    let mut control = device_body();
    control.user_code = "WDJB\u{1b}[2J".to_owned();
    cases.push(("a user code carrying a terminal escape", control));
    let mut expired = device_body();
    expired.expires_in = 0;
    cases.push(("a code already expired", expired));
    for (case, body) in cases {
        assert!(
            matches!(
                admit_device_authorization(body, issuer),
                Err(ProviderError::Unavailable { .. })
            ),
            "{case} is refused"
        );
    }
}

#[test]
fn every_device_poll_refusal_is_its_own_answer() {
    for (error, expected) in [
        ("authorization_pending", DevicePoll::Pending),
        ("slow_down", DevicePoll::SlowDown),
        ("access_denied", DevicePoll::Denied),
        ("expired_token", DevicePoll::Expired),
    ] {
        assert_eq!(
            device_poll_refusal(error, String::new()),
            Ok(expected),
            "{error}"
        );
    }
    assert_eq!(
        device_poll_refusal("invalid_grant", "invalid_grant".to_owned()),
        Err(ProviderError::ExchangeRefused {
            detail: "invalid_grant".to_owned()
        })
    );
}

#[tokio::test]
async fn the_disabled_provider_answers_not_configured() {
    assert_eq!(
        Disabled.begin_device().await,
        Err(ProviderError::NotConfigured)
    );
    assert_eq!(
        Disabled.poll_device(&DeviceCode::new("code")).await,
        Err(ProviderError::NotConfigured)
    );
    let state = fresh_state();
    let nonce = fresh_nonce();
    let request = AuthorizationRequest {
        redirect_uri: "http://127.0.0.1:1/cb",
        state: &state,
        nonce: &nonce,
        code_challenge: "x",
    };
    assert_eq!(
        Disabled.authorization_url(&request).await,
        Err(ProviderError::NotConfigured)
    );
    let code = AuthorizationCode::new("code");
    let verifier = fresh_verifier();
    let redemption = Redemption {
        code: &code,
        verifier: &verifier,
        redirect_uri: "http://127.0.0.1:1/cb",
        nonce: &nonce,
    };
    assert_eq!(
        Disabled.redeem(&redemption).await,
        Err(ProviderError::NotConfigured)
    );
}
