use super::*;
use crate::store::memory::ManualClock;

/// A signer over a freshly generated Ed25519 key pair.
///
/// Generated per call rather than read from a checked-in PEM: a private key in the
/// repository is a key somebody eventually uses, and `ring` — already the workspace's
/// key-generation crate, and here a dev-dependency only — mints one in microseconds.
pub(crate) fn signer(clock: &ManualClock) -> SessionTokens {
    let der = ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
        .expect("the platform can generate an Ed25519 key");
    SessionTokens::from_pkcs8(der.as_ref(), Arc::new(clock.clone()))
        .expect("a key just generated parses")
}

fn ids() -> (UserId, SessionId) {
    (UserId::new("user-1"), SessionId::new("session-1"))
}

#[test]
fn a_minted_access_token_reads_back_as_the_session_it_names() {
    let clock = ManualClock::default();
    let tokens = signer(&clock);
    let (user, session) = ids();

    let issued = tokens
        .issue(&user, &session, SignedDuration::from_hours(24))
        .expect("signing succeeds");
    let verified = tokens
        .verify(&issued.access_token, TokenKind::Access)
        .expect("a freshly minted access token verifies");

    assert_eq!(verified.user, user);
    assert_eq!(verified.session, session);
}

#[test]
fn a_refresh_token_is_not_an_access_token() {
    // The defect this type exists to make unrepresentable: the Salvo refresh handler never
    // looked at the scope list, so an access token rotated a session there.
    let clock = ManualClock::default();
    let tokens = signer(&clock);
    let (user, session) = ids();

    let issued = tokens
        .issue(&user, &session, SignedDuration::from_hours(24))
        .expect("signing succeeds");

    let refused = tokens.verify(&issued.access_token, TokenKind::Refresh);
    assert!(
        matches!(
            refused,
            Err(TokenError::WrongKind {
                expected: TokenKind::Refresh,
                found: TokenKind::Access
            })
        ),
        "an access token must not be usable as a refresh token, got {refused:?}"
    );

    let other_way = tokens.verify(&issued.refresh_token, TokenKind::Access);
    assert!(
        matches!(
            other_way,
            Err(TokenError::WrongKind {
                expected: TokenKind::Access,
                found: TokenKind::Refresh
            })
        ),
        "a refresh token must not be usable as an access token, got {other_way:?}"
    );
}

#[test]
fn expiry_is_read_from_the_injected_clock() {
    let clock = ManualClock::default();
    let tokens = signer(&clock);
    let (user, session) = ids();

    let issued = tokens
        .issue(&user, &session, SignedDuration::from_hours(24))
        .expect("signing succeeds");

    // One second before the deadline it is still good; one second past, it is not. No
    // sleeping, and no 60-second leeway widening the window behind the assertion.
    clock.advance(ACCESS_TOKEN_TTL - SignedDuration::from_secs(1));
    assert!(
        tokens
            .verify(&issued.access_token, TokenKind::Access)
            .is_ok(),
        "a token one second short of its deadline is still honoured"
    );

    clock.advance(SignedDuration::from_secs(2));
    assert!(matches!(
        tokens.verify(&issued.access_token, TokenKind::Access),
        Err(TokenError::Expired)
    ));
}

#[test]
fn the_refresh_token_outlives_the_access_token_by_the_ttl_it_was_given() {
    let clock = ManualClock::default();
    let tokens = signer(&clock);
    let (user, session) = ids();

    let issued = tokens
        .issue(&user, &session, SignedDuration::from_hours(24))
        .expect("signing succeeds");

    clock.advance(SignedDuration::from_hours(1));
    assert!(matches!(
        tokens.verify(&issued.access_token, TokenKind::Access),
        Err(TokenError::Expired)
    ));
    assert!(
        tokens
            .verify(&issued.refresh_token, TokenKind::Refresh)
            .is_ok(),
        "the refresh token lives for the TTL the caller supplied"
    );

    clock.advance(SignedDuration::from_hours(24));
    assert!(matches!(
        tokens.verify(&issued.refresh_token, TokenKind::Refresh),
        Err(TokenError::Expired)
    ));
}

#[test]
fn a_token_signed_by_another_key_is_unreadable() {
    let clock = ManualClock::default();
    let tokens = signer(&clock);
    let (user, session) = ids();

    let issued = tokens
        .issue(&user, &session, SignedDuration::from_hours(24))
        .expect("signing succeeds");

    let stranger = signer(&clock);
    assert!(matches!(
        stranger.verify(&issued.access_token, TokenKind::Access),
        Err(TokenError::Unreadable)
    ));
}

#[test]
fn a_token_from_another_issuer_is_unreadable() {
    // `iss` is checked, so a token minted by some other service that happens to share the
    // key material still does not authenticate anybody here.
    let clock = ManualClock::default();
    let tokens = signer(&clock);
    let (user, session) = ids();

    let foreign = Claims {
        sub: user.to_string(),
        sid: session.to_string(),
        kind: TokenKind::Access,
        iss: "somebody-else".to_owned(),
        iat: clock.now().as_second(),
        exp: clock.now().as_second() + 3600,
    };
    let token = jsonwebtoken::encode(&Header::new(Algorithm::EdDSA), &foreign, &tokens.signing)
        .expect("signing succeeds");

    assert!(matches!(
        tokens.verify(&token, TokenKind::Access),
        Err(TokenError::Unreadable)
    ));
}

#[test]
fn garbage_is_unreadable_rather_than_fatal() {
    let clock = ManualClock::default();
    let tokens = signer(&clock);

    for candidate in ["", "not-a-token", "a.b.c", "Bearer x"] {
        assert!(
            matches!(
                tokens.verify(candidate, TokenKind::Access),
                Err(TokenError::Unreadable)
            ),
            "{candidate:?} must be refused, not accepted or fatal"
        );
    }
}

#[test]
fn neither_a_token_pair_nor_the_signer_prints_its_secrets() {
    let clock = ManualClock::default();
    let tokens = signer(&clock);
    let (user, session) = ids();

    let issued = tokens
        .issue(&user, &session, SignedDuration::from_hours(24))
        .expect("signing succeeds");

    let printed = format!("{issued:?}");
    assert!(
        !printed.contains(&issued.access_token) && !printed.contains(&issued.refresh_token),
        "a bearer credential must not reach a log through Debug, got {printed}"
    );

    let signer_printed = format!("{tokens:?}");
    assert!(
        signer_printed.contains("<redacted>"),
        "the signer must not print key material, got {signer_printed}"
    );
}
