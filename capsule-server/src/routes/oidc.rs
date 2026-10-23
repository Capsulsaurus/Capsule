//! `POST /v1/auth/oidc/authorize`, `POST /v1/auth/oidc/callback` — signing in through an
//! external identity provider.
//!
//! # Two requests, one ceremony
//!
//! The client asks for an authorization URL and gets one back with a `state`; it sends the person
//! there; the provider sends them back to the client's own redirect with a `code`; the client
//! posts `state` and `code` here, and gets exactly what a password sign-in gets — a
//! [`LoginReply`]: a token pair, or a second-factor challenge. The session is opened by the same
//! `open_session_for` the password path calls, which is the whole of "identical to the
//! password path" (design/authentication.md, "Choosing an Auth Path").
//!
//! The server never sees the person's browser. The redirect lands at the *client* — a web app's
//! callback route, or a CLI's loopback listener — which is why the redirect URI is a request
//! field the client names and the policy admits, rather than a server route.
//!
//! # Bound to the client that began it
//!
//! The server's PKCE verifier never leaves the server, so on its own it binds the code to this
//! relying party and to nothing about the client: whoever posts a `state` and its `code` is
//! signed in. That is login CSRF — an attacker begins a ceremony, signs in at the provider as
//! themselves, and carries their `state` and `code` to a victim's client, which completes it into
//! the attacker's library. So the client binds itself too (issue #517): it sends the S256 hash of
//! a secret it keeps as `binding_challenge` at the authorize, the hash is held with the ceremony,
//! and the callback is refused unless its `binding_verifier` hashes to it. The victim's client
//! does not hold the attacker's secret, and the attacker's secret never reaches the victim.
//!
//! # What the callback checks, and what it says
//!
//! Every rejection reason is logged with its detail and collapsed on the wire, so the callback
//! is not an oracle over which checks the relying party runs: a foreign signature, a wrong
//! audience, an expired token and a replayed nonce all render `error.auth.oidc_token_invalid`.
//! A burned, expired or unknown `state` is one code, as `error.auth.totp_challenge_invalid` is,
//! and a `state` presented without its client's binding is that same code.
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
//! second door.

use std::fmt;

use capsule_i18n::error_codes;
use kynos::prelude::*;
use serde::{Deserialize, Serialize};
use tracing::Instrument as _;

use super::auth::{LoginReply, TokenResponse, open_session_for};
use super::totp::SecondFactorChallenge;
use crate::auth::oidc::{
    AuthorizationRequest, DevicePoll, FederatedLink, OidcContext, ProviderError, Redemption,
    code_challenge, fresh_device_handle, fresh_nonce, fresh_state, fresh_verifier,
};
use crate::auth::{AuthContext, DirectoryError, EnrollmentState, TotpContext};
use crate::counter::{CounterContext, CounterKey, budgets};
use crate::store::{
    AuthorizationCode, DevicePollSlot, OidcDeviceHandle, OidcState, PendingAuthorization,
    PendingDeviceAuthorization, StoreError,
};

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
    /// The client's binding to this ceremony: the S256 hash — unpadded base64url of the SHA-256,
    /// 43 characters — of a fresh secret of at least 256 bits that the client generates, keeps,
    /// and presents as `binding_verifier` at the callback.
    ///
    /// The server's own PKCE verifier binds the code to this server; this binds the callback to
    /// the client that began the ceremony, so a `state` and `code` carried to another client — a
    /// login-CSRF victim's — are refused there. Anything but 43 base64url characters is a `400`.
    pub binding_challenge: String,
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
///
/// Strict, like [`OidcAuthorizeRequest`]: a client that forwards the provider's whole redirect
/// query — `error`, `error_description`, `iss` (RFC 9207) — gets a `422` naming the field rather
/// than a callback that quietly ignored what the provider said.
#[derive(Schema, Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct OidcCallbackRequest {
    /// The `state` the authorize answered with, as the redirect echoed it.
    pub state: String,
    /// The authorization `code` the redirect carried.
    pub code: String,
    /// The secret whose S256 hash the authorize carried as `binding_challenge`. A callback whose
    /// secret does not hash to it is refused as an unknown `state` is, and burns the `state`.
    pub binding_verifier: String,
    /// An advisory device-cohort hash. Legibility metadata only; an unusable
    /// value is dropped rather than refused.
    pub cohort_hash: Option<String>,
    /// The directory device the client claims to be, as a UUID. Dropped, not
    /// refused, when it is not a usable UUID.
    pub device_id: Option<String>,
}

impl fmt::Debug for OidcCallbackRequest {
    /// Redacted: the state, the code and the binding secret are the parts of a live credential.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OidcCallbackRequest")
            .field("state", &"<redacted>")
            .field("code", &"<redacted>")
            .field("binding_verifier", &"<redacted>")
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

    /// The `binding_challenge` is not an S256 hash: 43 unpadded base64url characters.
    #[error("the binding challenge is not an unpadded base64url SHA-256 hash")]
    #[problem(status = 400, title = "Binding challenge malformed")]
    BindingMalformed {
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

    /// Too many ceremonies were begun for this redirect host in the window.
    #[error("too many sign-ins were started; wait and try again")]
    #[problem(status = 429, title = "Too many sign-ins")]
    RateLimited {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The pending-ceremony store is at its ceiling. Retryable: ceremonies expire in minutes.
    ///
    /// Carries `error.auth.oidc_at_capacity`, its own code and not the `error.auth.unavailable`
    /// the `500` below carries. `design/api-surfaces.md` is explicit that "REST status is coarse;
    /// the stable `error.*` code in the `ApiError` body is the precise discriminator. Clients
    /// switch on the code, never on status alone" — so two conditions that answer with different
    /// statuses may not share one code, or a client obeying that rule cannot tell "the server
    /// said not now, retry in a moment" from "a store could not answer at all". The remedies
    /// differ too: the first is a short timer, the second is an outage to surface.
    #[error("the server is holding too many unfinished sign-ins; try again shortly")]
    #[problem(status = 503, title = "Sign-in capacity reached")]
    AtCapacity {
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

    /// A client bug, not something a person can fix: the generic malformed-request code.
    fn binding_malformed() -> Self {
        Self::BindingMalformed {
            code: error_codes::REQUEST_MALFORMED,
        }
    }

    fn not_configured() -> Self {
        Self::NotConfigured {
            code: error_codes::AUTH_OIDC_NOT_CONFIGURED,
        }
    }

    fn rate_limited() -> Self {
        Self::RateLimited {
            code: error_codes::AUTH_RATE_LIMITED,
        }
    }

    /// The store refused a write because it is full.
    fn at_capacity() -> Self {
        Self::AtCapacity {
            code: error_codes::AUTH_OIDC_AT_CAPACITY,
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
///
/// Bounded twice, because it is an unauthenticated write into a store: a budget per redirect
/// host ([`budgets::OIDC_AUTHORIZE`]) answers `429` before anything is done, and the store's
/// own ceiling answers `503` when it is nevertheless full.
#[kynos::post(
    "/v1/auth/oidc/authorize",
    operation_id = "begin_oidc_login",
    tag = OidcTag
)]
pub async fn begin_oidc_login(
    Inject(oidc): Inject<OidcContext>,
    Inject(counters): Inject<CounterContext>,
    Json(request): Json<OidcAuthorizeRequest>,
) -> Result<Json<OidcAuthorizationResponse>, OidcAuthorizeRejection> {
    async move {
        let redirect_uri = request.redirect_uri.trim();

        // Shape-checked before anything is charged or written: a pure function of the body, like
        // the extractor's own `422`, and a value of fixed length is what the store then holds.
        let binding_challenge = request.binding_challenge.trim();
        if !is_s256_challenge(binding_challenge) {
            tracing::info!("an OIDC sign-in carried a binding challenge that is not an S256 hash");
            return Err(OidcAuthorizeRejection::binding_malformed());
        }

        // The redirect is validated *before* the budget is charged, and the order is the
        // security property rather than a preference. `redirect_uri` is caller-supplied and
        // unbounded; charging a counter keyed on its host before the policy has looked at it
        // would let an unauthenticated caller add one permanent row to the counter store per
        // request — a `400` every time, and a map that only grows. So the key is picked from
        // the policy's verdict: an admitted redirect is keyed on its host (three at most), and
        // every refusal shares one fixed bucket. The look-ahead costs a string comparison; the
        // `authorization_url` below applies the same policy and remains the authority.
        let (key, budget) = if oidc.provider().admits_redirect(redirect_uri) {
            match reqwest::Url::parse(redirect_uri)
                .ok()
                .and_then(|url| url.host_str().map(str::to_owned))
            {
                Some(host) => (CounterKey::OidcAuthorize(host), budgets::OIDC_AUTHORIZE),
                // Unreachable: the policy admits an exactly-configured URL, which config
                // validated as absolute, or a loopback literal it parsed. Bounded anyway,
                // because "unreachable" is not a thing to key a map on.
                None => (
                    CounterKey::OidcAuthorizeRefused,
                    budgets::OIDC_AUTHORIZE_REFUSED,
                ),
            }
        } else {
            (
                CounterKey::OidcAuthorizeRefused,
                budgets::OIDC_AUTHORIZE_REFUSED,
            )
        };
        let verdict = counters.hit(&key, budget).await.map_err(|error| {
            // Fail closed, like every other limiter in this crate.
            tracing::error!(%error, "the OIDC authorize limiter could not be reached");
            OidcAuthorizeRejection::store_unavailable()
        })?;
        if !verdict.admits() {
            tracing::warn!(
                counter = key.as_str(),
                "an OIDC sign-in was refused: the budget for this bucket is spent"
            );
            return Err(OidcAuthorizeRejection::rate_limited());
        }

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
                    binding_challenge: binding_challenge.to_owned(),
                    issued_at,
                },
            )
            .await
            .map_err(|error| match error {
                StoreError::Rejected { .. } => {
                    tracing::warn!(%error, "the pending OIDC authorization store is full");
                    OidcAuthorizeRejection::at_capacity()
                }
                other => {
                    store_unavailable(&other, "record a pending OIDC authorization");
                    OidcAuthorizeRejection::store_unavailable()
                }
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

        // Bound to the client that began the ceremony (issue #517), checked before the provider
        // is asked anything: a `state` and `code` carried to another client — the login-CSRF that
        // would sign a victim into the attacker's account — are refused here, with the code an
        // unknown `state` gets, and the `state` is already burned.
        if !binding_matches(&pending.binding_challenge, &request.binding_verifier) {
            tracing::warn!(
                "an OIDC callback did not present the binding of the client that began it"
            );
            return Err(OidcCallbackRejection::state_invalid());
        }

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

        settle_federated(
            &oidc,
            &auth,
            &totp,
            &identity,
            request.cohort_hash.as_deref(),
            request.device_id.as_deref(),
        )
        .await
        .map_err(|unsettled| match unsettled {
            Unsettled::AddressTaken => OidcCallbackRejection::address_taken(),
            Unsettled::Unavailable => OidcCallbackRejection::store_unavailable(),
        })
    }
    .instrument(tracing::info_span!("oidc.callback"))
    .await
}

/// Why a verified identity did not become a session.
enum Unsettled {
    /// The asserted address already belongs to an account here, and this identity is not it.
    AddressTaken,
    /// A Capsule store could not answer.
    Unavailable,
}

/// Turn a verified identity into what a password sign-in answers: a session, or a second-factor
/// challenge.
///
/// The one tail both federated doors — the callback and the device poll — run, so neither can
/// open a session the other would have refused: the account is resolved or created on
/// `(issuer, subject)` and never linked by address, a confirmed second factor turns the answer
/// into the same `202` the password path gives, and the session is opened by the password path's
/// own `open_session_for`.
async fn settle_federated(
    oidc: &OidcContext,
    auth: &AuthContext,
    totp: &TotpContext,
    identity: &crate::auth::oidc::VerifiedIdentity,
    cohort_hash: Option<&str>,
    device_id: Option<&str>,
) -> Result<LoginReply, Unsettled> {
    // Minted here, as `register_user` mints one: the id is a fact about this server's
    // clock. Discarded unchanged if the identity already has an account.
    let now = auth.clock().now();
    let minted = crate::auth::new_user_id();
    let user = match oidc
        .accounts()
        .resolve_or_create(identity, &minted, now)
        .await
        .map_err(|error: DirectoryError| {
            tracing::error!(%error, "the federated account directory could not answer");
            Unsettled::Unavailable
        })? {
        FederatedLink::Linked(user) => user,
        FederatedLink::Created(user) => {
            tracing::info!(user_id = %user, issuer = %identity.issuer, "created an account for a federated sign-in");
            user
        }
        FederatedLink::AddressTaken => {
            tracing::info!(issuer = %identity.issuer, "a federated sign-in asserted an address another account holds");
            return Err(Unsettled::AddressTaken);
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
            Unsettled::Unavailable
        })?
        .is_some_and(|held| held.state == EnrollmentState::Active);
    if second_factor {
        let challenge = crate::auth::ChallengeId::generate();
        let issued = auth
            .tokens()
            .issue_second_factor(&user, &challenge, crate::auth::CHALLENGE_TTL)
            .map_err(|error| {
                tracing::error!(%error, "a second-factor challenge could not be signed");
                Unsettled::Unavailable
            })?;
        tracing::info!(user_id = %user, challenge_id = %challenge, "a federated sign-in needs a second factor");
        return Ok(LoginReply::SecondFactorRequired(SecondFactorChallenge {
            mfa_token: issued.token,
            expires_by: u64::try_from(issued.expires_at.as_second()).unwrap_or(0),
        }));
    }

    let issued = open_session_for(auth, &user, cohort_hash, device_id, now)
        .await
        .map_err(|error| {
            store_unavailable(&error, "open a session for a federated sign-in");
            Unsettled::Unavailable
        })?;

    tracing::info!(user_id = %user, issuer = %identity.issuer, "opened a session for a federated sign-in");
    Ok(LoginReply::Signed(TokenResponse::from(issued)))
}

// ===========================================================================================
// The device authorization grant (RFC 8628, issue #635)
// ===========================================================================================

/// The `POST /v1/auth/oidc/device` body.
#[derive(Schema, Serialize, Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct OidcDeviceRequest {
    /// The client's binding to this ceremony, exactly as on the authorize: the S256 hash —
    /// unpadded base64url of the SHA-256, 43 characters — of a fresh secret of at least 256 bits
    /// the client keeps and presents as `binding_verifier` on every poll. Anything but 43
    /// base64url characters is a `400`.
    pub binding_challenge: String,
}

/// A begun device authorization: what to show the person, and the handle to poll with.
#[derive(Schema, Serialize, Deserialize, Clone)]
pub struct OidcDeviceAuthorizationResponse {
    /// The handle to poll `POST /v1/auth/oidc/device/poll` with. The server's own, never the
    /// provider's device code. Good until `expires_by`, and ended by the first terminal answer.
    pub handle: String,
    /// The code the person types at `verification_uri`. Show it exactly.
    pub user_code: String,
    /// Where the person goes, on any device with a browser, to type `user_code`.
    pub verification_uri: String,
    /// The same address with the code already in it, when the provider offers one — what a
    /// client renders as a QR code or a link.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_uri_complete: Option<String>,
    /// How many seconds to wait before the first poll and between polls. Add five on every
    /// `error.auth.oidc_device_slow_down` (RFC 8628 §3.5); the server does the same, so the two
    /// stay in step without the new interval being sent.
    pub interval: u64,
    /// The **absolute** Unix-seconds instant the ceremony stops being honoured: the provider's
    /// `expires_in` or the server's fifteen minutes, whichever is sooner.
    pub expires_by: u64,
}

impl fmt::Debug for OidcDeviceAuthorizationResponse {
    /// Redacted: the handle is the key to the pending ceremony, and the user code approves it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OidcDeviceAuthorizationResponse")
            .field("handle", &"<redacted>")
            .field("user_code", &"<redacted>")
            .field("verification_uri", &self.verification_uri)
            .field("interval", &self.interval)
            .field("expires_by", &self.expires_by)
            .finish_non_exhaustive()
    }
}

/// The `POST /v1/auth/oidc/device/poll` body: the handle, the client's binding, and the two
/// advisory identifiers a client may volunteer for the session a successful poll opens.
#[derive(Schema, Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct OidcDevicePollRequest {
    /// The `handle` the begin answered with.
    pub handle: String,
    /// The secret whose S256 hash the begin carried as `binding_challenge`. A poll whose secret
    /// does not hash to it ends the ceremony and is answered as an unknown handle is.
    pub binding_verifier: String,
    /// An advisory device-cohort hash. Legibility metadata only.
    pub cohort_hash: Option<String>,
    /// The directory device the client claims to be, as a UUID.
    pub device_id: Option<String>,
}

impl fmt::Debug for OidcDevicePollRequest {
    /// Redacted: the handle and the binding secret are the parts of a live credential.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OidcDevicePollRequest")
            .field("handle", &"<redacted>")
            .field("binding_verifier", &"<redacted>")
            .field("cohort_hash", &self.cohort_hash)
            .field("device_id", &self.device_id)
            .finish()
    }
}

/// Why a device authorization could not begin.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum OidcDeviceRejection {
    /// The `binding_challenge` is not an S256 hash: 43 unpadded base64url characters.
    #[error("the binding challenge is not an unpadded base64url SHA-256 hash")]
    #[problem(status = 400, title = "Binding challenge malformed")]
    BindingMalformed {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// There is no device sign-in to offer.
    ///
    /// One variant carrying one of two codes: `error.auth.oidc_not_configured` when the
    /// deployment has no identity provider at all, `error.auth.oidc_device_unsupported` when its
    /// provider does not offer the device grant to this client. Both are configuration, and the
    /// person's remedy differs — no single sign-on here, or the browser flow instead.
    #[error("signing in without a browser is not available on this server")]
    #[problem(status = 404, title = "Device sign-in not offered")]
    NotOffered {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// Too many device authorizations were begun in the window.
    #[error("too many sign-ins were started; wait and try again")]
    #[problem(status = 429, title = "Too many sign-ins")]
    RateLimited {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The pending-device store is at its ceiling. Retryable: ceremonies expire in minutes.
    #[error("the server is holding too many unfinished sign-ins; try again shortly")]
    #[problem(status = 503, title = "Sign-in capacity reached")]
    AtCapacity {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The provider could not be reached, or a store could not answer — `error.auth.
    /// oidc_unavailable` and `error.auth.unavailable` respectively, as on the authorize.
    #[error("the sign-in could not be started")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

impl OidcDeviceRejection {
    fn binding_malformed() -> Self {
        Self::BindingMalformed {
            code: error_codes::REQUEST_MALFORMED,
        }
    }

    fn not_configured() -> Self {
        Self::NotOffered {
            code: error_codes::AUTH_OIDC_NOT_CONFIGURED,
        }
    }

    fn unsupported() -> Self {
        Self::NotOffered {
            code: error_codes::AUTH_OIDC_DEVICE_UNSUPPORTED,
        }
    }

    fn rate_limited() -> Self {
        Self::RateLimited {
            code: error_codes::AUTH_RATE_LIMITED,
        }
    }

    fn at_capacity() -> Self {
        Self::AtCapacity {
            code: error_codes::AUTH_OIDC_AT_CAPACITY,
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

/// Why a device poll did not open a session.
///
/// Two of these are not failures: `Pending` and `SlowDown` are RFC 8628's own "keep polling",
/// answered as problems because a success status would have to carry a body that is not a
/// sign-in, and `200`/`202` already mean what the callback means by them.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum OidcDevicePollRejection {
    /// The person has not finished at the provider. Poll again after the interval.
    #[error("waiting for the sign-in to be approved")]
    #[problem(status = 400, title = "Authorization pending")]
    Pending {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// Polled too soon, or the provider asked to slow down. Add five seconds to the interval.
    #[error("polling too often; add five seconds to the interval")]
    #[problem(status = 429, title = "Slow down")]
    SlowDown {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The handle is unknown, finished or expired, its binding did not match, or the provider
    /// says the device code expired. One answer for all of them.
    #[error("that sign-in code has expired or was already used; start again")]
    #[problem(status = 401, title = "Sign-in expired")]
    Expired {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The person declined at the provider.
    #[error("the sign-in was declined at the identity provider")]
    #[problem(status = 401, title = "Sign-in declined")]
    Denied {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The provider refused the poll for a reason RFC 8628 does not name.
    #[error("the identity provider did not accept the sign-in")]
    #[problem(status = 401, title = "Exchange refused")]
    ExchangeFailed {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The provider's ID token failed a check. One answer for every check.
    #[error("the identity provider's answer could not be verified")]
    #[problem(status = 401, title = "ID token refused")]
    TokenInvalid {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The asserted address already belongs to an account here, and this identity is not it.
    #[error("an account with that address already exists here; sign in with its password")]
    #[problem(status = 409, title = "Address already registered")]
    AddressTaken {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The provider could not be reached, or a store could not answer.
    #[error("the sign-in could not be completed")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

impl OidcDevicePollRejection {
    fn pending() -> Self {
        Self::Pending {
            code: error_codes::AUTH_OIDC_DEVICE_PENDING,
        }
    }

    fn slow_down() -> Self {
        Self::SlowDown {
            code: error_codes::AUTH_OIDC_DEVICE_SLOW_DOWN,
        }
    }

    fn expired() -> Self {
        Self::Expired {
            code: error_codes::AUTH_OIDC_DEVICE_EXPIRED,
        }
    }

    fn denied() -> Self {
        Self::Denied {
            code: error_codes::AUTH_OIDC_DEVICE_DENIED,
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

/// Begin a sign-in through the identity provider for a client with no browser of its own.
///
/// Unauthenticated, like the authorize, and bounded the same way: the binding's shape first, then
/// one deployment-wide budget ([`budgets::OIDC_DEVICE`], `429`), then the provider, then the
/// store's own ceiling (`503`). The provider is asked before anything is written, so a provider
/// without the grant, or an unconfigured deployment, writes nothing. The provider's device code
/// stays here; the client gets a handle of the server's own.
#[kynos::post(
    "/v1/auth/oidc/device",
    operation_id = "begin_oidc_device_login",
    tag = OidcTag
)]
pub async fn begin_oidc_device_login(
    Inject(oidc): Inject<OidcContext>,
    Inject(counters): Inject<CounterContext>,
    Json(request): Json<OidcDeviceRequest>,
) -> Result<Json<OidcDeviceAuthorizationResponse>, OidcDeviceRejection> {
    async move {
        let binding_challenge = request.binding_challenge.trim();
        if !is_s256_challenge(binding_challenge) {
            tracing::info!("an OIDC device sign-in carried a binding challenge that is not an S256 hash");
            return Err(OidcDeviceRejection::binding_malformed());
        }

        let key = CounterKey::OidcDevice;
        let verdict = counters.hit(&key, budgets::OIDC_DEVICE).await.map_err(|error| {
            tracing::error!(%error, "the OIDC device limiter could not be reached");
            OidcDeviceRejection::store_unavailable()
        })?;
        if !verdict.admits() {
            tracing::warn!(
                counter = key.as_str(),
                "an OIDC device sign-in was refused: the budget is spent"
            );
            return Err(OidcDeviceRejection::rate_limited());
        }

        let device = oidc.provider().begin_device().await.map_err(|error| match error {
            ProviderError::NotConfigured => {
                tracing::info!("an OIDC device sign-in was requested and no provider is configured");
                OidcDeviceRejection::not_configured()
            }
            ProviderError::DeviceUnsupported { detail } => {
                tracing::warn!(%detail, "the identity provider does not offer the device grant");
                OidcDeviceRejection::unsupported()
            }
            other => {
                tracing::error!(error = %other, "the identity provider could not begin a device sign-in");
                OidcDeviceRejection::provider_unavailable()
            }
        })?;

        let handle = fresh_device_handle();
        let issued_at = oidc.clock().now();
        let lifetime = device.expires_in.min(oidc.devices().ttl());
        oidc.devices()
            .begin(
                &handle,
                PendingDeviceAuthorization {
                    device_code: device.device_code,
                    binding_challenge: binding_challenge.to_owned(),
                    interval: device.interval,
                    issued_at,
                },
            )
            .await
            .map_err(|error| match error {
                StoreError::Rejected { .. } => {
                    tracing::warn!(%error, "the pending OIDC device store is full");
                    OidcDeviceRejection::at_capacity()
                }
                other => {
                    store_unavailable(&other, "record a pending OIDC device authorization");
                    OidcDeviceRejection::store_unavailable()
                }
            })?;

        let expires_at = crate::store::deadline(issued_at, lifetime);
        tracing::info!(
            interval_secs = device.interval.as_secs(),
            lifetime_secs = lifetime.as_secs(),
            "began an OIDC device sign-in"
        );
        Ok(Json(OidcDeviceAuthorizationResponse {
            handle: handle.as_str().to_owned(),
            user_code: device.user_code,
            verification_uri: device.verification_uri,
            verification_uri_complete: device.verification_uri_complete,
            interval: u64::try_from(device.interval.as_secs()).unwrap_or(0),
            expires_by: u64::try_from(expires_at.as_second()).unwrap_or(0),
        }))
    }
    .instrument(tracing::info_span!("oidc.device"))
    .await
}

/// Poll a device authorization once.
///
/// The slot is claimed first, atomically: a poll sooner than one interval after the last is
/// `429 error.auth.oidc_device_slow_down` and never reaches the provider, so a client polling too
/// fast cannot make this server poll the provider any faster. A due poll checks the client's
/// binding, then asks the provider once. Every terminal answer — a session, a challenge, a
/// denial, an expiry, a refusal — ends the ceremony; `authorization_pending`, `slow_down` and an
/// unreachable provider leave it to be polled again. On success the ceremony is ended *before*
/// the session is opened, so two polls collecting one approval open one session.
#[kynos::post(
    "/v1/auth/oidc/device/poll",
    operation_id = "poll_oidc_device_login",
    tag = OidcTag
)]
pub async fn poll_oidc_device_login(
    Inject(oidc): Inject<OidcContext>,
    Inject(auth): Inject<AuthContext>,
    Inject(totp): Inject<TotpContext>,
    Json(request): Json<OidcDevicePollRequest>,
) -> Result<LoginReply, OidcDevicePollRejection> {
    async move {
        let handle = OidcDeviceHandle::new(request.handle.trim());
        let slot = oidc.devices().claim(&handle).await.map_err(|error| {
            store_unavailable(&error, "claim an OIDC device poll");
            OidcDevicePollRejection::store_unavailable()
        })?;
        let pending = match slot {
            DevicePollSlot::Due(pending) => pending,
            DevicePollSlot::TooSoon { interval } => {
                tracing::debug!(
                    interval_secs = interval.as_secs(),
                    "an OIDC device sign-in was polled too soon"
                );
                return Err(OidcDevicePollRejection::slow_down());
            }
            DevicePollSlot::Unknown => {
                tracing::info!("an OIDC device poll presented an unknown, finished or expired handle");
                return Err(OidcDevicePollRejection::expired());
            }
        };

        // Bound to the client that began it, checked before the provider is asked anything, and
        // answered as an unknown handle is, so neither the handle nor the binding is an oracle.
        if !binding_matches(&pending.binding_challenge, &request.binding_verifier) {
            tracing::warn!("an OIDC device poll did not present the binding of the client that began it");
            end_device(&oidc, &handle).await;
            return Err(OidcDevicePollRejection::expired());
        }

        let answer = oidc
            .provider()
            .poll_device(&pending.device_code)
            .await;
        let identity = match answer {
            Ok(DevicePoll::Verified(identity)) => identity,
            Ok(DevicePoll::Pending) => {
                tracing::debug!("an OIDC device sign-in is still pending at the provider");
                return Err(OidcDevicePollRejection::pending());
            }
            Ok(DevicePoll::SlowDown) => {
                let raised = oidc.devices().slow_down(&handle).await.map_err(|error| {
                    store_unavailable(&error, "slow an OIDC device poll down");
                    OidcDevicePollRejection::store_unavailable()
                })?;
                tracing::info!(
                    interval_secs = raised.map(|interval| interval.as_secs()),
                    "the identity provider asked an OIDC device sign-in to slow down"
                );
                return Err(OidcDevicePollRejection::slow_down());
            }
            Ok(DevicePoll::Denied) => {
                tracing::info!("an OIDC device sign-in was declined at the provider");
                end_device(&oidc, &handle).await;
                return Err(OidcDevicePollRejection::denied());
            }
            Ok(DevicePoll::Expired) => {
                tracing::info!("the identity provider says an OIDC device code expired");
                end_device(&oidc, &handle).await;
                return Err(OidcDevicePollRejection::expired());
            }
            Err(ProviderError::ExchangeRefused { detail }) => {
                tracing::warn!(%detail, "the identity provider refused a device poll");
                end_device(&oidc, &handle).await;
                return Err(OidcDevicePollRejection::exchange_failed());
            }
            Err(ProviderError::TokenRejected(reason)) => {
                tracing::warn!(%reason, "a device ID token was refused");
                end_device(&oidc, &handle).await;
                return Err(OidcDevicePollRejection::token_invalid());
            }
            Err(other) => {
                // Transient, or a configuration fault that is not the person's: the ceremony is
                // left, so a provider back within the window still completes it.
                tracing::error!(error = %other, "the identity provider could not answer a device poll");
                return Err(OidcDevicePollRejection::provider_unavailable());
            }
        };

        // Ended before the session is opened, and required to have been live: of two polls that
        // both collected this approval, only the one that ends the ceremony opens a session.
        let ended = oidc.devices().finish(&handle).await.map_err(|error| {
            store_unavailable(&error, "finish an OIDC device authorization");
            OidcDevicePollRejection::store_unavailable()
        })?;
        if ended.is_none() {
            tracing::warn!("an approved OIDC device sign-in was already finished by another poll");
            return Err(OidcDevicePollRejection::expired());
        }

        settle_federated(
            &oidc,
            &auth,
            &totp,
            &identity,
            request.cohort_hash.as_deref(),
            request.device_id.as_deref(),
        )
        .await
        .map_err(|unsettled| match unsettled {
            Unsettled::AddressTaken => OidcDevicePollRejection::address_taken(),
            Unsettled::Unavailable => OidcDevicePollRejection::store_unavailable(),
        })
    }
    .instrument(tracing::info_span!("oidc.device.poll"))
    .await
}

/// End a device ceremony on a terminal answer.
///
/// Best effort: the answer the caller gets is the provider's, and a store that cannot remove the
/// record now leaves it to expire on its own — which is also what stops it being collected, since
/// the provider has already said its last word on the device code.
async fn end_device(oidc: &OidcContext, handle: &OidcDeviceHandle) {
    if let Err(error) = oidc.devices().finish(handle).await {
        store_unavailable(&error, "end an OIDC device authorization");
    }
}

/// One log line for every store failure, so a support report can name the operation.
fn store_unavailable(error: &StoreError, doing: &'static str) {
    tracing::error!(%error, operation = doing, "a store could not answer");
}

/// The length of an S256 hash: 32 bytes of SHA-256, unpadded base64url.
const S256_CHALLENGE_LEN: usize = 43;

/// Whether `challenge` has the shape of an S256 hash: 43 unpadded base64url characters.
///
/// The shape, not the content — any 43-character base64url string could be one, and the
/// callback's comparison is what decides whether it is the hash of the presented secret.
fn is_s256_challenge(challenge: &str) -> bool {
    challenge.len() == S256_CHALLENGE_LEN
        && challenge
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Whether `verifier` hashes to the stored `challenge`, compared in constant time.
///
/// The same S256 derivation PKCE uses (RFC 7636 §4.2), so one function is the definition on
/// both sides of this server. Comparing hashes already denies a timing attacker any byte of the
/// secret; it is constant time anyway, as the drop link's passphrase proof is compared, and
/// both sides are 43-character hashes, so the length check leaks nothing either.
fn binding_matches(challenge: &str, verifier: &str) -> bool {
    use subtle::ConstantTimeEq as _;
    let presented = code_challenge(&crate::store::PkceVerifier::new(verifier));
    bool::from(presented.as_bytes().ct_eq(challenge.as_bytes()))
}

#[cfg(test)]
mod tests;
