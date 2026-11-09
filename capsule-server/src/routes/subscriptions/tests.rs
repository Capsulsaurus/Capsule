use super::*;

#[test]
fn every_capability_refusal_has_one_answer() {
    assert_eq!(
        refusal_code(&CapabilityError::Expired),
        Some(error_codes::FEDERATION_CAPABILITY_EXPIRED)
    );
    for error in [
        CapabilityError::Unreadable,
        CapabilityError::WrongIssuer,
        CapabilityError::WrongRecipient,
        CapabilityError::NotYetValid,
    ] {
        assert_eq!(
            refusal_code(&error),
            Some(error_codes::FEDERATION_CAPABILITY_INVALID),
            "{error:?} is a capability this server cannot use"
        );
    }
    for error in [
        CapabilityError::Malformed { claim: "aud" },
        CapabilityError::BeyondTtlCeiling,
    ] {
        assert_eq!(
            refusal_code(&error),
            None,
            "{error:?} is a token outside the contract's shape: a 400"
        );
    }
}

#[test]
fn a_view_never_carries_the_token_and_reads_liveness_at_now() {
    let expires_at = jiff::Timestamp::from_second(3600).expect("an instant");
    let subscription = Subscription {
        subscriber: UserId::new("u"),
        peer: PeerId::new("home.test"),
        album: AlbumId::new("a"),
        scope: crate::federation::Scope::ReadDerivativeOnly,
        jti: "jti".to_owned(),
        token: CapabilityToken::new("header.payload.signature"),
        issued_at: jiff::Timestamp::UNIX_EPOCH,
        expires_at,
        min_protocol_version: "2026-06-01".to_owned(),
        subscribed_at: jiff::Timestamp::UNIX_EPOCH,
        updated_at: jiff::Timestamp::UNIX_EPOCH,
    };
    let live = SubscriptionView::of(&subscription, jiff::Timestamp::UNIX_EPOCH);
    assert!(live.live);
    assert_eq!(live.scope, WireScope::ReadDerivativeOnly);
    let rendered = serde_json::to_string(&live).expect("it serializes");
    assert!(!rendered.contains("header.payload.signature"), "{rendered}");
    assert!(!SubscriptionView::of(&subscription, expires_at).live);
}
