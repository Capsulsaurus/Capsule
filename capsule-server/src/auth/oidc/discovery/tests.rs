use super::*;

fn metadata(issuer: &str, scheme: &str) -> ProviderMetadata {
    ProviderMetadata {
        issuer: issuer.to_owned(),
        authorization_endpoint: format!("{scheme}://idp.example.test/auth"),
        token_endpoint: format!("{scheme}://idp.example.test/token"),
        jwks_uri: format!("{scheme}://idp.example.test/keys"),
        device_authorization_endpoint: None,
    }
}

#[test]
fn a_device_endpoint_is_optional_and_held_to_the_same_scheme_rule() {
    let issuer = "https://idp.example.test";
    assert!(
        admit(metadata(issuer, "https"), issuer).is_ok(),
        "a provider without the grant is still a provider"
    );
    let mut offered = metadata(issuer, "https");
    offered.device_authorization_endpoint = Some(format!("{issuer}/device/code"));
    assert!(admit(offered.clone(), issuer).is_ok());
    offered.device_authorization_endpoint = Some("http://idp.example.test/device".to_owned());
    assert!(matches!(
        admit(offered, issuer).expect_err("refused"),
        DiscoveryError::InsecureEndpoint {
            endpoint: "device_authorization_endpoint",
            ..
        }
    ));

    let parsed: ProviderMetadata = serde_json::from_value(serde_json::json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/auth"),
        "token_endpoint": format!("{issuer}/token"),
        "jwks_uri": format!("{issuer}/keys"),
    }))
    .expect("a document without the member parses");
    assert_eq!(parsed.device_authorization_endpoint, None);
}

#[test]
fn the_discovery_url_hangs_off_the_issuer() {
    assert_eq!(
        discovery_url("https://idp.example.test"),
        "https://idp.example.test/.well-known/openid-configuration"
    );
    assert_eq!(
        discovery_url("https://idp.example.test/realm/"),
        "https://idp.example.test/realm/.well-known/openid-configuration"
    );
}

#[test]
fn a_document_naming_another_issuer_is_refused() {
    let error = admit(
        metadata("https://somebody-else.test", "https"),
        "https://idp.example.test",
    )
    .expect_err("refused");
    assert_eq!(
        error,
        DiscoveryError::IssuerMismatch {
            found: "https://somebody-else.test".to_owned()
        }
    );
}

#[test]
fn an_off_box_endpoint_under_a_loopback_issuer_is_refused() {
    // The carve-out is for a provider on this machine, not for a provider on this machine
    // that sends the code somewhere else in the clear.
    let issuer = "http://127.0.0.1:5556/dex";
    let mut off_box = metadata(issuer, "http");
    off_box.authorization_endpoint = format!("{issuer}/auth");
    off_box.jwks_uri = format!("{issuer}/keys");
    off_box.token_endpoint = "http://idp.example.test/token".to_owned();
    assert!(matches!(
        admit(off_box, issuer).expect_err("refused"),
        DiscoveryError::InsecureEndpoint {
            endpoint: "token_endpoint",
            ..
        }
    ));
    let mut named = metadata(issuer, "http");
    named.authorization_endpoint = format!("{issuer}/auth");
    named.jwks_uri = format!("{issuer}/keys");
    named.token_endpoint = "http://localhost:5556/dex/token".to_owned();
    assert!(
        admit(named, issuer).is_err(),
        "localhost is not loopback either"
    );
}

#[test]
fn plain_http_endpoints_are_refused_unless_the_issuer_is_loopback() {
    let error = admit(
        metadata("https://idp.example.test", "http"),
        "https://idp.example.test",
    )
    .expect_err("refused");
    assert!(matches!(
        error,
        DiscoveryError::InsecureEndpoint {
            endpoint: "authorization_endpoint",
            ..
        }
    ));

    for issuer in ["http://127.0.0.1:5556/dex", "http://[::1]:5556"] {
        assert!(is_loopback_issuer(issuer), "{issuer}");
        // Loopback endpoints under a loopback issuer are the carve-out; https anywhere is
        // still admitted.
        let loopback = ProviderMetadata {
            issuer: issuer.to_owned(),
            authorization_endpoint: format!("{issuer}/auth"),
            token_endpoint: format!("{issuer}/token"),
            jwks_uri: "https://keys.example.test/jwks".to_owned(),
            device_authorization_endpoint: Some(format!("{issuer}/device/code")),
        };
        assert!(admit(loopback, issuer).is_ok(), "{issuer}");
    }
    assert!(
        !is_loopback_issuer("http://localhost:5556"),
        "localhost is a name a resolver answers for; RFC 8252 §8.3"
    );
    assert!(
        !is_loopback_issuer("https://127.0.0.1"),
        "https is not the carve-out"
    );
    assert!(!is_loopback_issuer("http://idp.example.test"));
}
