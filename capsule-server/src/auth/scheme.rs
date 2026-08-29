//! [`AccessToken`] — the bearer scheme every authenticated Capsule operation is guarded by.
//!
//! # Why the credential is read here and not from a header parameter
//!
//! Kynos refuses `Authorization` in a header-parameter group, and refuses it on purpose: a
//! credential read out of a header the description does not mention is a guard the description
//! cannot see. Taking [`Auth<AccessToken>`](kynos::security::auth::Auth) as a handler argument
//! *is* the enforcement and *is* the declaration — it adds the scheme to the operation's
//! `security`, adds 401 and 403 to its responses, and fills the `WWW-Authenticate` challenge
//! from the scheme itself, so the string on the wire and the string in the document are one
//! string. The Salvo tree had none of that: `get_token_from_headers` read the header by hand and
//! the `security(("bearer" = []))` annotation beside it was a second, unchecked statement.
//!
//! # The two refusals, and why one of them is a 403
//!
//! | What arrived | Answer |
//! | --- | --- |
//! | No `Authorization` header, or not a `Bearer` one | 401 |
//! | A token that does not verify, or has expired | 401 |
//! | A live, correctly signed **refresh** token | **403** |
//!
//! The last row is a deliberate change from Salvo, which answered 401 with the message
//! `"Invalid token: Invalid scopes. Expected [AccessToken], got [RefreshToken]"`. A refresh
//! token is not an unauthenticated caller — it is a valid credential that is insufficient for
//! this operation, which is precisely RFC 9110's distinction and precisely what
//! [`AuthRejection::Forbidden`] means. It also makes the 403 that `Auth<S>` declares *reachable*:
//! a status the document promises and nothing can produce is the other half of the `S-C28`
//! defect, and `assert_declared_responses_covered` fails on it.
//!
//! # What these refusals do not carry
//!
//! An `error.*` code. `AuthRejection` renders a problem document with a `detail` and nothing
//! else, and the type is Kynos's — there is no seam for an extension member. Every rejection
//! this crate *owns* carries its code; these two are the framework's and do not. Recorded here
//! rather than worked around, because the workaround — emitting Capsule's own 401 beside the
//! framework's — would mean an operation with two different 401 bodies, one of which would not
//! carry the `WWW-Authenticate` header the document declares as required.

use kynos::error::rejection::AuthRejection;
use kynos::prelude::*;
use kynos::security::Authenticator;
use kynos::security::carrier::BearerToken;

use super::{AuthContext, TokenError, TokenKind, VerifiedToken};

/// A Capsule access token, presented as an RFC 6750 bearer credential.
///
/// The credential a handler receives is a [`VerifiedToken`] — the account and the session the
/// token named — and never the token itself. A handler that could reach the raw credential
/// could log it.
#[derive(SecurityScheme)]
#[security(bearer(format = "JWT"))]
#[security(
    name = "bearer",
    credential = VerifiedToken,
    description = "A short-lived Capsule access token, issued by `POST /v1/auth/login` and \
                   rotated by `POST /v1/auth/refresh`."
)]
pub struct AccessToken;

/// Reads a presented bearer token.
///
/// Implemented for [`AuthContext`] and generic over the application context, so the module that
/// owns the signer is the module that verifies with it — there is no second place holding a key.
impl<C: Sync> Authenticator<AccessToken, C> for AuthContext {
    async fn authenticate(
        &self,
        presented: BearerToken,
        _context: &C,
    ) -> Result<VerifiedToken, AuthRejection> {
        match self.tokens().verify(presented.as_str(), TokenKind::Access) {
            Ok(verified) => {
                tracing::trace!(
                    user_id = %verified.user,
                    session_id = %verified.session,
                    "a request presented a valid access token"
                );
                Ok(verified)
            }

            // Valid, live, and the wrong kind: a credential that is insufficient rather than
            // absent. See the module docs.
            Err(TokenError::WrongKind { .. }) => Err(AuthRejection::Forbidden),

            // Everything else is indistinguishable on purpose. `TokenError` already refuses to
            // carry any part of the credential, and which check failed is exactly what an
            // attacker probing a token would like to be told.
            Err(reason) => {
                tracing::debug!(%reason, "a request presented a credential that did not verify");
                Err(AuthRejection::unauthenticated())
            }
        }
    }

    async fn authorize(
        &self,
        _credential: &VerifiedToken,
        _scopes: &'static [&'static str],
        _context: &C,
    ) -> Result<(), AuthRejection> {
        // Bearer access tokens carry no scopes — [`TokenKind`] is the whole of what a Capsule
        // token grants, and it is checked in `authenticate`. This exists because the trait
        // requires it; `Auth<S>` never calls it, and no operation here takes a `Scoped<S, R>`.
        Ok(())
    }
}
