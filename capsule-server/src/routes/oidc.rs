//! `POST /v1/auth/oidc/authorize`, `POST /v1/auth/oidc/callback` — signing in through an
//! external identity provider (slice `S-N1`).
//!
//! # Two requests, one ceremony
//!
//! The client asks for an authorization URL and gets one back with a `state`; it sends the person
//! there; the provider sends them back to the client's own redirect with a `code`; the client
//! posts `state` and `code` here, and gets exactly what a password sign-in gets — a
//! [`LoginReply`]: a token pair, or a second-factor challenge. The session is opened by the same
//! [`open_session_for`] the password path calls, which is the whole of "identical to the
//! password path" (design/authentication.md, "Choosing an Auth Path").
//!
//! The server never sees the person's browser. The redirect lands at the *client* — a web app's
//! callback route, or a CLI's loopback listener — which is why the redirect URI is a request
//! field the client names and the policy admits, rather than a server route.
//!
//! # What the callback checks, and what it says
//!
//! Every rejection reason is logged with its detail and collapsed on the wire, so the callback
//! is not an oracle over which checks the relying party runs: a foreign signature, a wrong
//! audience, an expired token and a replayed nonce all render `error.auth.oidc_token_invalid`.
//! A burned, expired or unknown `state` is one code, as `error.auth.totp_challenge_invalid` is.
//! What *does* reach the caller distinctly is the remedy: the provider refused the exchange
//! (start again), the provider is down (wait), the address already has an account here (sign in
//! with the password instead).
//!
//! # Not configured
//!
//! Without `OIDC_ISSUER` the authorize answers `404 error.auth.oidc_not_configured`, and
//! `server-info` publishes `auth.oidc: null`, which is how the login chooser decides whether to
//! render the option at all. The callback declares no `404`: an unconfigured deployment holds no
//! pending ceremony, so a callback there is a `401 error.auth.oidc_state_invalid` — the honest
//! answer, and one that adds no oracle.
//!
//! # The second factor is honoured, not bypassed
//!
//! A confirmed TOTP enrollment answers the same `202` here that it does on the password path.
//! Bypassing it would let an account that enrolled a factor be signed into without one through a
//! second door — the `S-C55` defect on a new route.

use std::fmt;

use capsule_i18n::error_codes;
use kynos::prelude::*;
use serde::{Deserialize, Serialize};
use tracing::Instrument as _;

use super::auth::{LoginReply, TokenResponse, open_session_for};
use super::totp::SecondFactorChallenge;
use crate::auth::oidc::{
    AuthorizationRequest, FederatedLink, OidcContext, ProviderError, Redemption, code_challenge,
    fresh_nonce, fresh_state, fresh_verifier,
};
use crate::auth::{AuthContext, DirectoryError, EnrollmentState, TotpContext};
use crate::store::{AuthorizationCode, OidcState, PendingAuthorization, StoreError};

/// The operations that sign in through an external identity provider.
#[derive(Tag)]
#[tag(
    name = "oidc",
    description = "Signing in through an external OpenID Connect identity provider."
)]
pub struct OidcTag;

// ===========================================================================================
// Wire types
// ===========================================================================================

/// The `POST /v1/auth/oidc/authorize` body.
#[derive(Schema, Serialize, Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct OidcAuthorizeRequest {
    /// Where the provider should send the person back: the client's own callback.
    ///
    /// Admitted if it is the deployment's configured redirect URL exactly, or a loopback IP
    /// literal (`http://127.0.0.1:{port}/…`, `http://[::1]:{port}/…`) on any port when the
    /// deployment allows loopback redirects — the shape a CLI's or desktop app's listener has
    /// (RFC 8252 §7.3). Stored with the ceremony and replayed byte for byte to the token endpoint.
    pub redirect_uri: String,
}

/// A begun ceremony: where to send the person, and the `state` that comes back.
#[derive(Schema, Serialize, Deserialize, Clone)]
pub struct OidcAuthorizationResponse {
    /// The provider's authorization endpoint with the whole request in its query: `response_type`,
    /// `client_id`, `redirect_uri`, `scope`, `state`, `nonce`, `code_challenge`,
    /// `code_challenge_method`.
    pub authorization_url: String,
    /// The `state` the provider will echo on the redirect. Present it, with the `code`, to the
    /// callback. Good once, and until `expires_by`.
    pub state: String,
    /// The **absolute** Unix-seconds instant the ceremony stops being redeemable.
    pub expires_by: u64,
}

impl fmt::Debug for OidcAuthorizationResponse {
    /// Redacted: the state is the key to the pending ceremony, and the URL carries it and the
    /// nonce.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OidcAuthorizationResponse")
            .field("authorization_url", &"<redacted>")
            .field("state", &"<redacted>")
            .field("expires_by", &self.expires_by)
            .finish()
    }
}

/// The `POST /v1/auth/oidc/callback` body: what the provider's redirect carried, plus the two
/// advisory identifiers a client may volunteer for the session this request opens.
#[derive(Schema, Serialize, Deserialize, Clone)]
pub struct OidcCallbackRequest {
    /// The `state` the authorize answered with, as the redirect echoed it.
    pub state: String,
    /// The authorization `code` the redirect carried.
    pub code: String,
    /// An advisory device-cohort hash (slice `S-C13`). Legibility metadata only; an unusable
    /// value is dropped rather than refused.
    pub cohort_hash: Option<String>,
    /// The directory device the client claims to be (slice `S-N3`), as a UUID. Dropped, not
    /// refused, when it is not a usable UUID.
    pub device_id: Option<String>,
}

impl fmt::Debug for OidcCallbackRequest {
    /// Redacted: the state and the code are the two halves of a live credential.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OidcCallbackRequest")
            .field("state", &"<redacted>")
            .field("code", &"<redacted>")
            .field("cohort_hash", &self.cohort_hash)
            .field("device_id", &self.device_id)
            .finish()
    }
}

// ===========================================================================================
// Rejections
// ===========================================================================================

/// Why a ceremony could not begin.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum OidcAuthorizeRejection {
    /// The redirect URI is neither the configured one nor an admitted loopback address.
    #[error("the redirect URI is not one this server will send a person back to")]
    #[problem(status = 400, title = "Redirect not admitted")]
    RedirectInvalid {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// This deployment has no identity provider.
    #[error("single sign-on is not configured on this server")]
    #[problem(status = 404, title = "Single sign-on not configured")]
    NotConfigured {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The provider could not be reached, or a store could not answer.
    ///
    /// One variant carrying one of two codes: `error.auth.oidc_unavailable` when the identity
    /// provider is the collaborator that failed, `error.auth.unavailable` when a Capsule store
    /// is. "Your identity provider is down" and "our session store is down" are different
    /// operator actions, and the code is what tells them apart.
    #[error("the sign-in could not be started")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Why a callback did not open a session.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum OidcCallbackRejection {
    /// The `state` is unknown, already redeemed, or expired. One answer for all three.
    #[error("that sign-in has expired or was already completed; start again")]
    #[problem(status = 401, title = "Sign-in expired")]
    StateInvalid {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The provider refused to exchange the code.
    #[error("the identity provider did not accept the sign-in")]
    #[problem(status = 401, title = "Exchange refused")]
    ExchangeFailed {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The provider's ID token failed a check. One answer for every check; see the module docs.
    #[error("the identity provider's answer could not be verified")]
    #[problem(status = 401, title = "ID token refused")]
    TokenInvalid {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The asserted address already belongs to an account here, and this identity is not it.
    ///
    /// The disclosure this makes is the one `error.auth.user_already_exists` already makes at
    /// registration, so it adds no new oracle; see `auth::oidc::accounts`.
    #[error("an account with that address already exists here; sign in with its password")]
    #[problem(status = 409, title = "Address already registered")]
    AddressTaken {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The provider could not be reached, or a collaborator could not answer.
    ///
    /// Two codes, as on the authorize: `error.auth.oidc_unavailable` for the provider,
    /// `error.auth.unavailable` for a Capsule store.
    #[error("the sign-in could not be completed")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

impl OidcAuthorizeRejection {
    fn redirect_invalid() -> Self {
        Self::RedirectInvalid {
            code: error_codes::AUTH_OIDC_REDIRECT_INVALID,
        }
    }

    fn not_configured() -> Self {
        Self::NotConfigured {
            code: error_codes::AUTH_OIDC_NOT_CONFIGURED,
        }
    }

    /// The identity provider could not answer.
    fn provider_unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::AUTH_OIDC_UNAVAILABLE,
        }
    }

    /// A Capsule store could not answer.
    fn store_unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::AUTH_UNAVAILABLE,
        }
    }
}

impl OidcCallbackRejection {
    fn state_invalid() -> Self {
        Self::StateInvalid {
            code: error_codes::AUTH_OIDC_STATE_INVALID,
        }
    }

    fn exchange_failed() -> Self {
        Self::ExchangeFailed {
            code: error_codes::AUTH_OIDC_EXCHANGE_FAILED,
        }
    }

    fn token_invalid() -> Self {
        Self::TokenInvalid {
            code: error_codes::AUTH_OIDC_TOKEN_INVALID,
        }
    }

    fn address_taken() -> Self {
        Self::AddressTaken {
            code: error_codes::AUTH_OIDC_ADDRESS_TAKEN,
        }
    }

    fn provider_unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::AUTH_OIDC_UNAVAILABLE,
        }
    }

    fn store_unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::AUTH_UNAVAILABLE,
        }
    }
}

// ===========================================================================================
// Operations
// ===========================================================================================

/// Begin a sign-in through the identity provider.
///
/// Unauthenticated: this is how a person *becomes* a session. Nothing about the account is
/// known yet — the ceremony carries fresh random `state`, `nonce` and PKCE material and the
/// admitted redirect URI, and the record behind the `state` lives for ten minutes.
#[kynos::post(
    "/v1/auth/oidc/authorize",
    operation_id = "begin_oidc_login",
    tag = OidcTag
)]
pub async fn begin_oidc_login(
    Inject(oidc): Inject<OidcContext>,
    Json(request): Json<OidcAuthorizeRequest>,
) -> Result<Json<OidcAuthorizationResponse>, OidcAuthorizeRejection> {
    async move {
        let redirect_uri = request.redirect_uri.trim();

        // Fresh per ceremony. The verifier never leaves this server; its challenge goes in the
        // URL, and the verifier itself is redeemed at the token endpoint by the callback.
        let state = fresh_state();
        let nonce = fresh_nonce();
        let verifier = fresh_verifier();
        let challenge = code_challenge(&verifier);

        // The provider is asked first, so a refused redirect or an unconfigured deployment
        // writes nothing to the store.
        let authorization_url = oidc
            .provider()
            .authorization_url(&AuthorizationRequest {
                redirect_uri,
                state: &state,
                nonce: &nonce,
                code_challenge: &challenge,
            })
            .await
            .map_err(|error| match error {
                ProviderError::NotConfigured => {
                    tracing::info!("an OIDC sign-in was requested and no provider is configured");
                    OidcAuthorizeRejection::not_configured()
                }
                ProviderError::RedirectRefused { redirect_uri } => {
                    tracing::info!(%redirect_uri, "an OIDC sign-in named a redirect the policy refuses");
                    OidcAuthorizeRejection::redirect_invalid()
                }
                other => {
                    tracing::error!(error = %other, "the identity provider could not begin a sign-in");
                    OidcAuthorizeRejection::provider_unavailable()
                }
            })?;

        let issued_at = oidc.clock().now();
        oidc.authorizations()
            .begin(
                &state,
                PendingAuthorization {
                    nonce,
                    verifier,
                    redirect_uri: redirect_uri.to_owned(),
                    issued_at,
                },
            )
            .await
            .map_err(|error| {
                store_unavailable(&error, "record a pending OIDC authorization");
                OidcAuthorizeRejection::store_unavailable()
            })?;

        let expires_at = crate::store::deadline(issued_at, oidc.authorizations().ttl());
        tracing::info!("began an OIDC sign-in");
        Ok(Json(OidcAuthorizationResponse {
            authorization_url,
            state: state.as_str().to_owned(),
            expires_by: u64::try_from(expires_at.as_second()).unwrap_or(0),
        }))
    }
    .instrument(tracing::info_span!("oidc.authorize"))
    .await
}

/// Finish a sign-in with what the provider's redirect carried.
///
/// The `state` is burned first and whatever happens next: a ceremony that survived a failed
/// callback would be a ceremony an attacker could retry a stolen code against. Then the code is
/// exchanged and the ID token verified by the provider adapter, the identity is resolved to an
/// account — created on first sight, keyed on `(issuer, subject)`, never linked by address — and
/// the session is opened exactly as a password sign-in opens one, second factor included.
#[kynos::post(
    "/v1/auth/oidc/callback",
    operation_id = "complete_oidc_login",
    tag = OidcTag
)]
pub async fn complete_oidc_login(
    Inject(oidc): Inject<OidcContext>,
    Inject(auth): Inject<AuthContext>,
    Inject(totp): Inject<TotpContext>,
    Json(request): Json<OidcCallbackRequest>,
) -> Result<LoginReply, OidcCallbackRejection> {
    async move {
        // Burned first. `consume` is destructive on every attempt, so a replayed state — and a
        // stolen code arriving on it — finds nothing, and two callbacks racing one state resolve
        // to one winner here.
        let state = OidcState::new(request.state.trim());
        let pending = oidc
            .authorizations()
            .consume(&state)
            .await
            .map_err(|error| {
                store_unavailable(&error, "consume a pending OIDC authorization");
                OidcCallbackRejection::store_unavailable()
            })?;
        let Some(pending) = pending else {
            tracing::info!("an OIDC callback presented an unknown, spent or expired state");
            return Err(OidcCallbackRejection::state_invalid());
        };

        let code = AuthorizationCode::new(request.code.trim());
        let identity = oidc
            .provider()
            .redeem(&Redemption {
                code: &code,
                verifier: &pending.verifier,
                redirect_uri: &pending.redirect_uri,
                nonce: &pending.nonce,
            })
            .await
            .map_err(|error| match error {
                ProviderError::ExchangeRefused { detail } => {
                    tracing::warn!(%detail, "the identity provider refused a code exchange");
                    OidcCallbackRejection::exchange_failed()
                }
                ProviderError::TokenRejected(reason) => {
                    // The specific reason for the operator; one code for the wire.
                    tracing::warn!(%reason, "an ID token was refused");
                    OidcCallbackRejection::token_invalid()
                }
                ProviderError::NotConfigured => {
                    // A pending ceremony exists and no provider does. Unreachable while the two
                    // are configured together, and a fault rather than a refusal if it ever is.
                    tracing::error!("a pending OIDC authorization exists on an unconfigured relying party");
                    OidcCallbackRejection::provider_unavailable()
                }
                other => {
                    tracing::error!(error = %other, "the identity provider could not complete a sign-in");
                    OidcCallbackRejection::provider_unavailable()
                }
            })?;

        // Minted here, as `register_user` mints one: the id is a fact about this server's
        // clock. Discarded unchanged if the identity already has an account.
        let now = auth.clock().now();
        let minted = crate::auth::new_user_id();
        let user = match oidc
            .accounts()
            .resolve_or_create(&identity, &minted, now)
            .await
            .map_err(|error: DirectoryError| {
                tracing::error!(%error, "the federated account directory could not answer");
                OidcCallbackRejection::store_unavailable()
            })? {
            FederatedLink::Linked(user) => user,
            FederatedLink::Created(user) => {
                tracing::info!(user_id = %user, issuer = %identity.issuer, "created an account for a federated sign-in");
                user
            }
            FederatedLink::AddressTaken => {
                tracing::info!(issuer = %identity.issuer, "a federated sign-in asserted an address another account holds");
                return Err(OidcCallbackRejection::address_taken());
            }
        };

        // The same second factor the password path honours, read after the identity is
        // established and failing closed on a store outage, for the same reasons `login_user`
        // records.
        let second_factor = totp
            .enrollments()
            .read(&user)
            .await
            .map_err(|error: DirectoryError| {
                tracing::error!(%error, user_id = %user, "the second-factor store could not answer");
                OidcCallbackRejection::store_unavailable()
            })?
            .is_some_and(|held| held.state == EnrollmentState::Active);
        if second_factor {
            let challenge = crate::auth::ChallengeId::generate();
            let issued = auth
                .tokens()
                .issue_second_factor(&user, &challenge, crate::auth::CHALLENGE_TTL)
                .map_err(|error| {
                    tracing::error!(%error, "a second-factor challenge could not be signed");
                    OidcCallbackRejection::store_unavailable()
                })?;
            tracing::info!(user_id = %user, challenge_id = %challenge, "a federated sign-in needs a second factor");
            return Ok(LoginReply::SecondFactorRequired(SecondFactorChallenge {
                mfa_token: issued.token,
                expires_by: u64::try_from(issued.expires_at.as_second()).unwrap_or(0),
            }));
        }

        let issued = open_session_for(
            &auth,
            &user,
            request.cohort_hash.as_deref(),
            request.device_id.as_deref(),
            now,
        )
        .await
        .map_err(|error| {
            store_unavailable(&error, "open a session for a federated sign-in");
            OidcCallbackRejection::store_unavailable()
        })?;

        tracing::info!(user_id = %user, issuer = %identity.issuer, "opened a session for a federated sign-in");
        Ok(LoginReply::Signed(TokenResponse::from(issued)))
    }
    .instrument(tracing::info_span!("oidc.callback"))
    .await
}

/// One log line for every store failure, so a support report can name the operation.
fn store_unavailable(error: &StoreError, doing: &'static str) {
    tracing::error!(%error, operation = doing, "a store could not answer");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_callback_body_never_prints_its_credentials() {
        let request = OidcCallbackRequest {
            state: "live-state".to_owned(),
            code: "live-code".to_owned(),
            cohort_hash: Some("cohort-1".to_owned()),
            device_id: None,
        };
        let printed = format!("{request:?}");
        assert!(
            !printed.contains("live-state") && !printed.contains("live-code"),
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
    fn every_rejection_publishes_its_catalog_code() {
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
}
