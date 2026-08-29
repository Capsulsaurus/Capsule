//! [`SessionTokens`] — minting and reading the two EdDSA tokens a session is worked through.
//!
//! # Why this is a concrete type and not a port
//!
//! [`AccountDirectory`](super::AccountDirectory) and
//! [`AuthStateStore`](crate::store::AuthStateStore) are traits because their real
//! implementations are a database and a cache: infrastructure that a test cannot have and a
//! double must stand in for. Signing a token is neither. It is a pure function of a key and a
//! clock, it runs identically in a test and in production, and a "deterministic token issuer"
//! double would be a **forgery oracle living in `src/`** — an object whose entire job is to
//! hand out credentials that were never signed. So there is no double here: the tests drive the
//! real signer over a freshly generated key pair, which means every refusal below is produced
//! by a genuinely bad token rather than by a flag on a fake.
//!
//! # The claim set, and what it deliberately drops
//!
//! ```text
//! { "sub": <user id>, "sid": <session id>, "kind": "access" | "refresh",
//!   "iss": "capsule-api", "iat": <unix seconds>, "exp": <unix seconds> }
//! ```
//!
//! The Salvo tokens additionally carried `jti`, `role` and a `scopes` array. `jti` had no
//! consumer, `role` was `User` on every path these three operations reach, and `scopes` was
//! carrying one bit — which token this is — inside a list that had to be searched. That bit is
//! [`TokenKind`], a required claim with two values, and the difference is not cosmetic: the
//! Salvo refresh handler never checked the scope list at all, so an **access** token rotated a
//! session there. Here [`SessionTokens::verify`] takes the kind it expects, so the check cannot
//! be the thing a handler forgets.
//!
//! Tokens minted here are therefore not interchangeable with the Salvo server's. Nothing
//! requires them to be: a client talks to one server, and the parity window is a swap rather
//! than a mixed fleet.
//!
//! # Expiry is read from the injected clock, not from the system
//!
//! `jsonwebtoken` validates `exp` itself, against `SystemTime::now()` and with 60 seconds of
//! leeway. Both are wrong here. The leeway made the Salvo tree disagree with itself — its
//! decode allowed a minute of grace and its own `validate()` allowed none — and a system clock
//! cannot be moved by a test, which is why the Salvo suite had no expiry case at all. So
//! `validate_exp` is off and the deadline is compared against [`Clock`], the same clock the
//! session store expires records on. One clock, one rule, and
//! [`ManualClock`](crate::store::memory::ManualClock) can walk over it.

use std::fmt;
use std::sync::Arc;

use jiff::{SignedDuration, Timestamp};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

use crate::store::{Clock, SessionId, UserId};

/// The issuer every Capsule token carries and every Capsule token is checked against.
///
/// The literal, like `capsule-api` in the version response, is the *wire* identity and not this
/// crate's name: the rebuild must not rename the issuer out from under tokens a client already
/// holds because an internal directory moved.
pub const ISSUER: &str = "capsule-api";

/// How long an access token is good for.
///
/// Fifteen minutes, matching the Salvo constant. It is short because the access token is the
/// credential presented on every request and the only revocation it has is running out.
pub const ACCESS_TOKEN_TTL: SignedDuration = SignedDuration::from_mins(15);

/// Which of the two tokens this is.
///
/// A required claim rather than an entry in a scope list, so "is this the kind of token this
/// operation accepts" is a comparison the type system makes the caller perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenKind {
    /// Presented as a bearer credential on an authenticated request.
    Access,
    /// Presented in the body of a refresh, and nowhere else.
    Refresh,
}

impl TokenKind {
    /// The name this kind travels under, for a log field.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Access => "access",
            Self::Refresh => "refresh",
        }
    }
}

impl fmt::Display for TokenKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The claims Capsule signs. See the module docs for what is here and what is not.
#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    /// The account the token authenticates.
    sub: String,
    /// The session it was issued against.
    sid: String,
    /// Which of the two tokens this is.
    kind: TokenKind,
    /// Always [`ISSUER`]; checked on the way back in.
    iss: String,
    /// When it was issued, in Unix seconds.
    iat: i64,
    /// When it stops being honoured, in Unix seconds.
    exp: i64,
}

/// A freshly minted pair.
///
/// `Debug` is hand-written: these are bearer credentials, and a derived one would publish a
/// live session to any `tracing` field or panic message that formatted the struct.
#[derive(Clone, PartialEq, Eq)]
pub struct IssuedTokens {
    /// The short-lived credential for ordinary requests.
    pub access_token: String,
    /// The long-lived credential that buys new pairs.
    pub refresh_token: String,
    /// When [`Self::access_token`] stops being honoured.
    pub access_expires_at: Timestamp,
}

impl fmt::Debug for IssuedTokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IssuedTokens")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("access_expires_at", &self.access_expires_at)
            .finish()
    }
}

/// What a token turned out to name, once it verified.
///
/// Carries no expiry and no raw token: everything downstream needs is the pair of identifiers,
/// and handing on the credential itself is how one ends up in a log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedToken {
    /// The account the token authenticates.
    pub user: UserId,
    /// The session it was issued against.
    pub session: SessionId,
}

/// Why a presented token was not honoured.
///
/// Every variant is a refusal the *client* caused; a signing failure is
/// [`Self::Unissuable`] and is the server's. Deliberately carries no detail from the token —
/// echoing a fragment of a credential back into a log or a response body is how one leaks.
#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    /// The token did not verify: wrong signature, wrong issuer, malformed, or missing a claim.
    /// One variant, because a client can act on none of the distinctions and an attacker would
    /// like all of them.
    #[error("the token could not be read")]
    Unreadable,

    /// The token verified but its deadline has passed.
    #[error("the token has expired")]
    Expired,

    /// The token verified and is live, but is the other kind.
    #[error("a {found} token was presented where a {expected} token is required")]
    WrongKind {
        /// What the operation requires.
        expected: TokenKind,
        /// What arrived.
        found: TokenKind,
    },

    /// The token could not be signed. The server's fault, never the caller's.
    #[error("the token could not be signed: {detail}")]
    Unissuable {
        /// The signer's own description of the failure.
        detail: String,
    },
}

/// Mints and reads Capsule's session tokens.
///
/// `Debug` is hand-written and prints no key material.
pub struct SessionTokens {
    signing: EncodingKey,
    verifying: DecodingKey,
    validation: Validation,
    clock: Arc<dyn Clock>,
    access_ttl: SignedDuration,
}

impl fmt::Debug for SessionTokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `finish_non_exhaustive`, not `finish`: the fields left out are the key pair, the
        // `jsonwebtoken` validation it is used with, and the clock, and the first of those is
        // the reason this impl is hand-written at all.
        f.debug_struct("SessionTokens")
            .field("issuer", &ISSUER)
            .field("access_ttl", &self.access_ttl)
            .field("keys", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl SessionTokens {
    /// A signer over an already-loaded key pair, reading `clock` for issuance and expiry.
    pub fn new(signing: EncodingKey, verifying: DecodingKey, clock: Arc<dyn Clock>) -> Self {
        // `validate_exp` off: the deadline is compared against `clock` in `verify`, so a test
        // can walk over an expiry and the 60-second leeway `jsonwebtoken` would otherwise apply
        // does not silently widen the window. `exp` stays *required*, so a token without one is
        // unreadable rather than eternal. `validate_aud` off because Capsule mints no `aud` and
        // the default would reject every token for lacking one.
        let mut validation = Validation::new(Algorithm::EdDSA);
        validation.set_issuer(&[ISSUER]);
        validation.set_required_spec_claims(&["exp", "iss", "sub"]);
        validation.validate_exp = false;
        validation.validate_aud = false;

        Self {
            signing,
            verifying,
            validation,
            clock,
            access_ttl: ACCESS_TOKEN_TTL,
        }
    }

    /// How long an access token this signer mints is good for.
    pub fn access_ttl(&self) -> SignedDuration {
        self.access_ttl
    }

    /// Mint a pair for `session`, which belongs to `user`.
    ///
    /// `refresh_ttl` is supplied by the caller rather than held here, and the caller passes
    /// [`AuthStateStore::ttl`](crate::store::AuthStateStore::ttl). A refresh token that outlives
    /// the session record it names is a credential that verifies and then fails, which reads to
    /// a client as the server losing sessions; making the two one fact removes the way for them
    /// to disagree.
    ///
    /// # Errors
    ///
    /// Returns [`TokenError::Unissuable`] if the claims cannot be signed.
    pub fn issue(
        &self,
        user: &UserId,
        session: &SessionId,
        refresh_ttl: SignedDuration,
    ) -> Result<IssuedTokens, TokenError> {
        let now = self.clock.now();
        let access_expires_at = crate::store::deadline(now, self.access_ttl);
        let refresh_expires_at = crate::store::deadline(now, refresh_ttl);

        let access_token = self.sign(user, session, TokenKind::Access, now, access_expires_at)?;
        let refresh_token =
            self.sign(user, session, TokenKind::Refresh, now, refresh_expires_at)?;

        tracing::debug!(
            user_id = %user,
            session_id = %session,
            access_expires_at = %access_expires_at,
            refresh_expires_at = %refresh_expires_at,
            "issued a token pair"
        );

        Ok(IssuedTokens {
            access_token,
            refresh_token,
            access_expires_at,
        })
    }

    /// Read `presented`, requiring it to be a token of kind `expected`.
    ///
    /// # Errors
    ///
    /// Returns [`TokenError`] if the token does not verify, has expired, or is the other kind.
    pub fn verify(
        &self,
        presented: &str,
        expected: TokenKind,
    ) -> Result<VerifiedToken, TokenError> {
        let claims = jsonwebtoken::decode::<Claims>(presented, &self.verifying, &self.validation)
            .map_err(|error| {
                // The error *kind* is safe to log — it names which check failed, never any part
                // of the credential — and it is the only thing that makes a support report
                // about "it says my token is bad" actionable.
                tracing::debug!(reason = ?error.kind(), "a presented token did not verify");
                TokenError::Unreadable
            })?
            .claims;

        if claims.exp <= self.clock.now().as_second() {
            tracing::debug!(session_id = %claims.sid, "a presented token has expired");
            return Err(TokenError::Expired);
        }

        if claims.kind != expected {
            tracing::warn!(
                session_id = %claims.sid,
                expected = %expected,
                found = %claims.kind,
                "a token of the wrong kind was presented"
            );
            return Err(TokenError::WrongKind {
                expected,
                found: claims.kind,
            });
        }

        Ok(VerifiedToken {
            user: UserId::new(claims.sub),
            session: SessionId::new(claims.sid),
        })
    }

    fn sign(
        &self,
        user: &UserId,
        session: &SessionId,
        kind: TokenKind,
        issued_at: Timestamp,
        expires_at: Timestamp,
    ) -> Result<String, TokenError> {
        let claims = Claims {
            sub: user.to_string(),
            sid: session.to_string(),
            kind,
            iss: ISSUER.to_owned(),
            iat: issued_at.as_second(),
            exp: expires_at.as_second(),
        };

        jsonwebtoken::encode(&Header::new(Algorithm::EdDSA), &claims, &self.signing).map_err(
            |error| TokenError::Unissuable {
                detail: error.to_string(),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::memory::ManualClock;

    /// A signer over a freshly generated Ed25519 key pair.
    ///
    /// Generated per call rather than read from a checked-in PEM: a private key in the
    /// repository is a key somebody eventually uses, and `ring` — already the workspace's
    /// key-generation crate, and here a dev-dependency only — mints one in microseconds.
    pub(crate) fn signer(clock: &ManualClock) -> SessionTokens {
        let (signing, verifying) = key_pair();
        SessionTokens::new(signing, verifying, Arc::new(clock.clone()))
    }

    fn key_pair() -> (EncodingKey, DecodingKey) {
        use ring::signature::KeyPair as _;

        let der = ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
            .expect("the platform can generate an Ed25519 key");
        let pair = ring::signature::Ed25519KeyPair::from_pkcs8_maybe_unchecked(der.as_ref())
            .expect("a key just generated parses");
        (
            EncodingKey::from_ed_der(der.as_ref()),
            DecodingKey::from_ed_der(pair.public_key().as_ref()),
        )
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
}
