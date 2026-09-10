//! [`ReadBearer`] — the one bearer carriage the two read primitives accept two principals on.
//!
//! # One component key, two principals
//!
//! design/api-surfaces.md: "Session access tokens and federation capabilities are different token
//! types verified by their owning modules, even though both use the standard HTTP carriage."
//! Kynos registers a scheme under its component name, and `GET /v1/sync` and
//! `GET /v1/blob/{hash}` must keep declaring the same `bearer` requirement every other operation
//! does — the generated SDK client attaches its credential by that key, and a second key would
//! split one carriage into two in the document for a difference the wire does not have. So this
//! scheme registers under **the same name, with a byte-identical description**, as
//! [`AccessToken`]; Kynos accepts a duplicate registration exactly when the two descriptions are
//! equal, and `tests::the_two_schemes_describe_one_component` pins that they are.
//!
//! # Session first, capability second
//!
//! The authenticator asks the session module first, exactly as `Auth<AccessToken>` would — the
//! ledger check of `S-C48` included — and only on *unauthenticated* tries the capability codec.
//! A session token that is live and the wrong kind stays `403`: a refresh token is an
//! insufficient credential on this operation whichever module reads it. A capability that
//! verifies must also be one this server **recorded**: an unknown `jti` under a valid signature
//! is a token this server did not issue, and the record is what carries the member and epoch the
//! route checks membership against.
//!
//! # What this authenticator refuses, and what it deliberately does not
//!
//! Only the structural refusals — does not verify, expired, unknown — are decided here, and they
//! render as the framework's uncoded `401`, the recorded limitation of
//! [`crate::auth::scheme`]. Everything a client can act on with a code — revoked, wrong album,
//! insufficient scope, over budget, blocked peer — is decided by the **route** from the admitted
//! [`VerifiedCapability`] through [`admit`](super::admit), the same way `Membership::Revoked`
//! becomes a route's `403`. The credential never carries the raw token.

use kynos::error::rejection::AuthRejection;
use kynos::prelude::*;
use kynos::security::Authenticator;
use kynos::security::carrier::BearerToken;

use super::FederationContext;
use super::capability::CapabilityGrant;
use super::store::CapabilityRecord;
use crate::app::App;
use crate::auth::{AccessToken, AuthContext, AuthenticatedSession};

/// A capability that verified and that this server recorded.
///
/// The grant is what the token said; the record is what the server knows about it — the member
/// it was minted for, the epoch their membership was granted at, whether it has been revoked.
/// A route decides from both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedCapability {
    /// The token's claims, verified.
    pub grant: CapabilityGrant,
    /// The issued record the `jti` names.
    pub record: CapabilityRecord,
}

/// Who a read is being served to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    /// An account, through a session access token.
    Session(AuthenticatedSession),
    /// A peer server, through a federation capability.
    ///
    /// Boxed: the grant and its record are several hundred bytes against a session's tens, and
    /// every session-authenticated read would otherwise carry the difference.
    Peer(Box<VerifiedCapability>),
}

/// The bearer carriage on the two read primitives a peer may pull through.
///
/// Registered under the same component as [`AccessToken`], with the same description: one
/// `bearer` in the document, one credential key in the SDK. The handler receives a
/// [`Principal`] and never the token.
#[derive(SecurityScheme)]
#[security(bearer(format = "JWT"))]
#[security(
    name = "bearer",
    credential = Principal,
    description = "A short-lived Capsule access token, issued by `POST /v1/auth/login` and \
                   rotated by `POST /v1/auth/refresh`."
)]
pub struct ReadBearer;

impl Authenticator<ReadBearer, App> for FederationContext {
    async fn authenticate(
        &self,
        presented: BearerToken,
        context: &App,
    ) -> Result<Principal, AuthRejection> {
        // The session module first, and its answer is final unless it is "not a session": a
        // live refresh token is `403` here as everywhere, and a session it admits is admitted.
        match <AuthContext as Authenticator<AccessToken, App>>::authenticate(
            context.auth(),
            presented.clone(),
            context,
        )
        .await
        {
            Ok(session) => return Ok(Principal::Session(session)),
            // `AuthRejection` is non-exhaustive; anything that is not "insufficient" is "not a
            // session", which is the one answer that opens the capability path.
            Err(AuthRejection::Forbidden) => return Err(AuthRejection::Forbidden),
            Err(_) => {}
        }

        let grant = self.codec().verify(presented.as_str()).map_err(|reason| {
            // Which check failed is safe to log — it names no part of the credential — and it
            // is the only thing that makes "my capability is refused" actionable.
            tracing::debug!(%reason, "a request presented a credential that is neither a session nor a capability");
            AuthRejection::unauthenticated()
        })?;

        let record = match self.capabilities().find(&grant.jti).await {
            Ok(Some(record)) => record,
            Ok(None) => {
                tracing::info!(
                    peer = %grant.peer,
                    jti = %grant.jti,
                    "a capability verified under this server's key but was never issued here"
                );
                return Err(AuthRejection::unauthenticated());
            }
            // Fail closed, as the session ledger does. `401` is the only refusal this trait can
            // render; the route-level `admit` renders the honest `500` for the same outage.
            Err(error) => {
                tracing::error!(
                    %error,
                    jti = %grant.jti,
                    "the capability store could not be read, so the request was refused closed"
                );
                return Err(AuthRejection::unauthenticated());
            }
        };
        if record.grant() != grant {
            // The token and the record disagree about what was granted. Nothing this server
            // wrote can produce that, so it is refused rather than reconciled.
            tracing::error!(jti = %grant.jti, "a capability's claims do not match its record");
            return Err(AuthRejection::unauthenticated());
        }

        tracing::trace!(
            peer = %grant.peer,
            album = %grant.album,
            jti = %grant.jti,
            "a request presented a recorded federation capability"
        );
        Ok(Principal::Peer(Box::new(VerifiedCapability {
            grant,
            record,
        })))
    }

    async fn authorize(
        &self,
        _credential: &Principal,
        _scopes: &'static [&'static str],
        _context: &App,
    ) -> Result<(), AuthRejection> {
        // Neither token type carries OAuth-style scopes; a capability's `scope` is decided
        // against a blob's role by the route. Exists because the trait requires it.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use kynos::security::SecurityScheme as _;

    use super::ReadBearer;
    use crate::auth::AccessToken;

    #[test]
    fn the_two_schemes_describe_one_component() {
        // What keeps `components.securitySchemes` at one `bearer` entry and every operation's
        // `security` unchanged: Kynos accepts a second registration under a name exactly when
        // its description is byte-identical to the first.
        assert_eq!(ReadBearer::NAME, AccessToken::NAME);
        assert_eq!(ReadBearer::describe(), AccessToken::describe());
        assert_eq!(ReadBearer::challenge(), AccessToken::challenge());
    }
}
