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
//! # The three refusals, and why one of them is a 403
//!
//! | What arrived | Answer |
//! | --- | --- |
//! | No `Authorization` header, or not a `Bearer` one | 401 |
//! | A token that does not verify, or has expired | 401 |
//! | A token naming a session the ledger no longer holds | **401** |
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
//! # The ledger is on the request path (slice `S-C48`)
//!
//! The third row is the one this slice added, and it is a deliberate purchase rather than a
//! refinement. Before it, `authenticate` checked the JWT's signature, issuer, kind and deadline
//! and never touched [`AuthStateStore`](crate::store::AuthStateStore) — so closing a session,
//! one or all of them, killed *refresh* immediately and left every already-issued access token
//! usable for the remainder of its fifteen minutes. Revocation was only ever as fast as the
//! TTL, which is why the TTL is short, but the revoke-all ceremony (`S-C23`) exists to deny an
//! attacker **now**, and a fifteen-minute window is not now.
//!
//! The price is one store round trip per authenticated request. That is real, and it is stated
//! here rather than discovered in a flame graph: [Authentication — Explicit
//! Revocation](../../capsule-docs/src/content/docs/design/authentication.md#explicit-revocation)
//! promises revocation by invalidating a session token, and a promise nothing reads is not a
//! promise. Valkey is a required service (`filesystem/server.md`, and the process refuses to
//! boot without it), so this is a round trip to a store the request already depends on.
//!
//! ## The store-unavailable answer is **closed**, and that is the uncomfortable half
//!
//! Failing open would turn a cache outage into an authentication bypass — the ledger would stop
//! being consulted at exactly the moment an attacker wanted it not to be, and an attacker who
//! can load the store chooses that moment. So an unreadable ledger refuses.
//!
//! **The status it refuses with is wrong, knowingly.** The honest answer is `503`: the server
//! cannot say the credential is invalid, only that it could not check. [`AuthRejection`] is
//! Kynos's type and carries `401` and `403` and nothing else, so `401` is what an
//! [`Authenticator`] can render. Two things keep that from being a lie a client acts on badly:
//!
//! 1. Every refusal here logs at `error` with the store's own reason, so an operator sees an
//!    outage rather than a spike in bad credentials.
//! 2. A client that answers a `401` by refreshing reaches [`crate::routes::auth::refresh`],
//!    which is Capsule's own route with Capsule's own error enum, and *that* renders `500`
//!    with `error.auth.unavailable` — so the client learns the truth one hop later and backs
//!    off instead of prompting for a password.
//!
//! Recorded on `S-C48` and pointed at the same upstream seam `S-C36` and `S-C38` want: a
//! framework rejection that can carry a Capsule status and a Capsule `error.*` code.
//!
//! ## The touch is coalesced, and the coalescing window is a staleness window
//!
//! [`AuthStateStore::touch_session`](crate::store::AuthStateStore::touch_session) had no
//! production caller at all before this slice — the port declared it, the conformance suite
//! exercised it, and no request path used it, so `last_active_at` never moved and the devices
//! listing's "last used" was the sign-in time forever. It is called here, which is where the
//! design always meant it to be called.
//!
//! But a touch on every request is a **write** on every request, and the listing that reads it
//! is refreshed by a human at human intervals. So it is coalesced: at most one write per
//! [`TOUCH_INTERVAL`] per session. That makes the listing stale by up to a minute, and the
//! choice is stated rather than picked — a minute of staleness in a screen a user opens to see
//! which devices are signed in is invisible, and one Valkey write per request per device is
//! not.
//!
//! A touch that *fails* does not fail the request. The authoritative read already succeeded, so
//! the credential is good; losing the activity stamp degrades a listing and nothing else.
//!
//! # What these refusals do not carry
//!
//! An `error.*` code. `AuthRejection` renders a problem document with a `detail` and nothing
//! else, and the type is Kynos's — there is no seam for an extension member. Every rejection
//! this crate *owns* carries its code; these two are the framework's and do not. Recorded here
//! rather than worked around, because the workaround — emitting Capsule's own 401 beside the
//! framework's — would mean an operation with two different 401 bodies, one of which would not
//! carry the `WWW-Authenticate` header the document declares as required.

use jiff::SignedDuration;
use kynos::error::rejection::AuthRejection;
use kynos::prelude::*;
use kynos::security::Authenticator;
use kynos::security::carrier::BearerToken;

use super::{AuthContext, TokenError, TokenKind};

/// How stale a session's `last_active_at` may get before a request writes it forward.
///
/// See the module docs: this is simultaneously the write-amplification bound and the staleness
/// bound of the devices listing, and it is one constant because they are one decision.
pub const TOUCH_INTERVAL: SignedDuration = SignedDuration::from_secs(60);

/// What a guarded handler is handed: the account, the session, and the session's own facts.
///
/// **Never the token itself.** A handler that could reach the raw credential could log it.
///
/// The two timestamps are here rather than re-read because `S-C48` already read the record to
/// decide whether to admit the request at all. A handler that read it again would pay a second
/// round trip for an answer it was already given, and — worse — could get a *different* answer,
/// because a concurrent close or re-authentication can land between the two reads. One read,
/// one truth, carried forward.
///
/// Field names match [`VerifiedToken`]'s deliberately: this replaced it as the scheme's
/// credential type, and every `credential.user` in every handler kept meaning what it meant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedSession {
    /// The account the token authenticates.
    pub user: crate::store::UserId,
    /// The session it was issued against, confirmed live in the ledger.
    pub session: crate::store::SessionId,
    /// When the user last presented a credential on this session's lineage.
    ///
    /// What a freshness gate reads (`S-C7`). Deliberately not moved by a refresh, so a gate
    /// built on it cannot be satisfied by a client that does nothing but rotate.
    pub authenticated_at: jiff::Timestamp,
    /// When the session was last seen, as of the read that admitted this request.
    ///
    /// Stale by up to [`TOUCH_INTERVAL`] — see the module docs. Nothing gates on it.
    pub last_active_at: jiff::Timestamp,
}

/// A Capsule access token, presented as an RFC 6750 bearer credential.
///
/// The credential a handler receives is an [`AuthenticatedSession`] — the account, the session
/// the token named, and the ledger's own facts about it — and never the token itself.
#[derive(SecurityScheme)]
#[security(bearer(format = "JWT"))]
#[security(
    name = "bearer",
    credential = AuthenticatedSession,
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
    ) -> Result<AuthenticatedSession, AuthRejection> {
        let verified = match self.tokens().verify(presented.as_str(), TokenKind::Access) {
            Ok(verified) => verified,

            // Valid, live, and the wrong kind: a credential that is insufficient rather than
            // absent. See the module docs.
            Err(TokenError::WrongKind { .. }) => return Err(AuthRejection::Forbidden),

            // Everything else is indistinguishable on purpose. `TokenError` already refuses to
            // carry any part of the credential, and which check failed is exactly what an
            // attacker probing a token would like to be told.
            Err(reason) => {
                tracing::debug!(%reason, "a request presented a credential that did not verify");
                return Err(AuthRejection::unauthenticated());
            }
        };

        // `S-C48`: the signature says the token was minted here; the ledger says the session it
        // names is still open. Without this second question a revoked session survives for the
        // access token's whole TTL.
        let record = match self.sessions().read_session(&verified.session).await {
            Ok(Some(record)) => record,

            Ok(None) => {
                tracing::debug!(
                    user_id = %verified.user,
                    session_id = %verified.session,
                    "a request presented a token for a session the ledger no longer holds"
                );
                return Err(AuthRejection::unauthenticated());
            }

            // Fail closed. An authenticator that stops consulting the ledger when the ledger is
            // unreachable is an authenticator an attacker turns off. The status is `401` because
            // it is the only refusal this trait can render — see the module docs, and `S-C48`.
            Err(error) => {
                tracing::error!(
                    %error,
                    user_id = %verified.user,
                    session_id = %verified.session,
                    "the session ledger could not be read, so the request was refused closed"
                );
                return Err(AuthRejection::unauthenticated());
            }
        };

        // Coalesced, and deliberately after the decision: a failed touch must not turn a good
        // credential into a refusal.
        let now = self.clock().now();
        if now.duration_since(record.last_active_at) >= TOUCH_INTERVAL
            && let Err(error) = self.sessions().touch_session(&verified.session, now).await
        {
            tracing::warn!(
                %error,
                session_id = %verified.session,
                "a session's activity stamp could not be written forward"
            );
        }

        tracing::trace!(
            user_id = %verified.user,
            session_id = %verified.session,
            "a request presented a valid access token for a live session"
        );
        Ok(AuthenticatedSession {
            user: verified.user,
            session: verified.session,
            authenticated_at: record.authenticated_at,
            last_active_at: record.last_active_at,
        })
    }

    async fn authorize(
        &self,
        _credential: &AuthenticatedSession,
        _scopes: &'static [&'static str],
        _context: &C,
    ) -> Result<(), AuthRejection> {
        // Bearer access tokens carry no scopes — [`TokenKind`] is the whole of what a Capsule
        // token grants, and it is checked in `authenticate`. This exists because the trait
        // requires it; `Auth<S>` never calls it, and no operation here takes a `Scoped<S, R>`.
        Ok(())
    }
}
