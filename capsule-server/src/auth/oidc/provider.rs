//! [`IdentityProvider`] — the port the OIDC routes drive, and its one HTTP adapter.
//!
//! # Two methods, not three
//!
//! Discovery is not a caller-visible operation; it is how both of these are answered. The port
//! is what the routes need — a URL to send the person to, and an identity for the code they
//! come back with — and nothing about how the adapter gets there.
//!
//! # The only thing doubled
//!
//! Everything else in the OIDC module is pure or is a store with an in-memory adapter. The
//! identity provider is the feature's one external boundary, so it is the one place the
//! mocking rule applies: the routes are tested against a double of this trait, and
//! [`HttpIdentityProvider`] is tested against an in-process mock provider that speaks the real
//! wire — discovery JSON, a JWK Set, a form-encoded token `POST`, a signed compact JWS.
//!
//! # The redirect URI is client-supplied and allow-listed
//!
//! A native client's loopback port is ephemeral (RFC 8252 §7.3), and the value sent to the token
//! endpoint must byte-match the one sent to the authorization endpoint (RFC 6749 §4.1.3). So the
//! client names its redirect, [`RedirectPolicy`] admits it or refuses it, and the admitted value
//! is stored with the ceremony and replayed verbatim. This is the one field that makes the CLI
//! and iOS flows possible without a second server surface.
//!
//! # PKCE, and no client secret by default
//!
//! Every ceremony carries an S256 code challenge. A client secret is optional: RFC 8252 §8.5
//! says a native application cannot keep one, and PKCE is what makes a public client sound.
//! When a deployment configures one it is sent as HTTP Basic credentials (RFC 6749 §2.3.1,
//! the default `client_secret_basic` method every provider supports).

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;

use super::claims::{ClaimRejection, Expectations, VerifiedIdentity, verify_id_token};
use super::discovery::{DiscoveryError, MetadataCache};
use super::jwks::{KeyCache, KeyError, Refresh};
use crate::store::{AuthorizationCode, Clock, OidcNonce, OidcState, PkceVerifier};

/// The scopes every authorization request asks for.
///
/// `openid` is what makes it an OIDC request at all. `email` is the one claim the relying party
/// reads, for the one decision it makes with it. **Not `profile`**: design/authentication.md
/// makes the display name something the person sets, and asking the provider for it would have
/// the server store a fact it declined to collect at registration.
pub const SCOPES: &str = "openid email";

/// How long an outbound request to the provider may take.
///
/// Ten seconds, end to end. A provider that takes longer to answer a discovery or token request
/// is one the person should be told is down, rather than one whose slowness holds a request
/// worker open indefinitely.
pub const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The future every port operation returns.
pub type ProviderFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ProviderError>> + Send + 'a>>;

/// What the relying party sends the person to the provider with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationRequest<'a> {
    /// Where the provider should send the person back. Already admitted by the policy.
    pub redirect_uri: &'a str,
    /// The ceremony's state.
    pub state: &'a OidcState,
    /// The nonce the ID token must echo.
    pub nonce: &'a OidcNonce,
    /// The S256 challenge of the ceremony's verifier.
    pub code_challenge: &'a str,
}

/// What the relying party redeems at the provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redemption<'a> {
    /// The code the provider handed back through the redirect.
    pub code: &'a AuthorizationCode,
    /// The verifier whose challenge the authorization request carried.
    pub verifier: &'a PkceVerifier,
    /// The redirect URI the authorization request named, byte for byte.
    pub redirect_uri: &'a str,
    /// The nonce the authorization request carried.
    pub nonce: &'a OidcNonce,
}

/// Why the provider could not complete an operation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    /// This deployment has no identity provider.
    ///
    /// Answered by [`Disabled`], the provider an unconfigured deployment runs with, so the routes
    /// have one shape whether or not `OIDC_ISSUER` is set.
    #[error("no identity provider is configured")]
    NotConfigured,
    /// The redirect URI is neither the configured one nor an admitted loopback address.
    #[error("the redirect URI {redirect_uri:?} is not admitted")]
    RedirectRefused {
        /// The URI the client asked for.
        redirect_uri: String,
    },
    /// The provider could not be reached, or its published facts could not be used.
    #[error("the identity provider is unavailable: {detail}")]
    Unavailable {
        /// What went wrong, for the log line.
        detail: String,
    },
    /// The provider refused to exchange the code.
    #[error("the identity provider refused the exchange: {detail}")]
    ExchangeRefused {
        /// The provider's own `error` and `error_description`, when it gave them.
        detail: String,
    },
    /// The provider answered with an ID token this relying party refuses.
    #[error("the ID token was refused: {0}")]
    TokenRejected(#[from] ClaimRejection),
}

impl From<DiscoveryError> for ProviderError {
    fn from(error: DiscoveryError) -> Self {
        Self::Unavailable {
            detail: error.to_string(),
        }
    }
}

impl From<KeyError> for ProviderError {
    fn from(error: KeyError) -> Self {
        Self::Unavailable {
            detail: error.to_string(),
        }
    }
}

/// An external identity provider, as the routes see it.
pub trait IdentityProvider: fmt::Debug + Send + Sync {
    /// The URL to send the person to.
    ///
    /// Refuses a redirect the policy does not admit before anything is fetched, so a refused
    /// request costs no round trip to the provider.
    fn authorization_url<'a>(
        &'a self,
        request: &'a AuthorizationRequest<'a>,
    ) -> ProviderFuture<'a, String>;

    /// Exchange the code for an ID token, verify it, and say who it is.
    fn redeem<'a>(&'a self, redemption: &'a Redemption<'a>)
    -> ProviderFuture<'a, VerifiedIdentity>;
}

/// Which redirect URIs a deployment admits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedirectPolicy {
    configured: Option<String>,
    allow_loopback: bool,
}

impl RedirectPolicy {
    /// Admit `configured` exactly, plus loopback URIs when `allow_loopback`.
    pub fn new(configured: Option<String>, allow_loopback: bool) -> Self {
        Self {
            configured,
            allow_loopback,
        }
    }

    /// Whether `redirect_uri` may be used.
    ///
    /// Exact string equality with the configured URI, or — when loopback is allowed — an
    /// `http` URI whose host is `127.0.0.1` or `[::1]` on **any** port, with no fragment (RFC
    /// 6749 §3.1.2 forbids one). `localhost` is deliberately not a loopback address here: RFC
    /// 8252 §8.3 recommends the IP literals, because a resolver can be made to send `localhost`
    /// elsewhere.
    #[must_use]
    pub fn admits(&self, redirect_uri: &str) -> bool {
        if self.configured.as_deref() == Some(redirect_uri) {
            return true;
        }
        if !self.allow_loopback {
            return false;
        }
        reqwest::Url::parse(redirect_uri).is_ok_and(|url| {
            url.scheme() == "http"
                && url.fragment().is_none()
                && matches!(url.host_str(), Some("127.0.0.1" | "[::1]"))
        })
    }
}

/// A client secret, redacted in `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub struct ClientSecret(String);

impl ClientSecret {
    /// Hold `secret`.
    pub fn new(secret: impl Into<String>) -> Self {
        Self(secret.into())
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ClientSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ClientSecret(<redacted>)")
    }
}

/// Everything an operator decides about the relying party.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OidcSettings {
    /// The issuer, exactly as the ID token must carry it.
    pub issuer: String,
    /// This relying party's `client_id` at the provider.
    pub client_id: String,
    /// The client secret, if the deployment is a confidential client. Absent means PKCE-only.
    pub client_secret: Option<ClientSecret>,
    /// Which redirect URIs are admitted.
    pub redirects: RedirectPolicy,
}

// ===========================================================================================
// Ceremony material
// ===========================================================================================

/// Fresh random bytes, base64url without padding.
fn random_token(bytes: usize) -> String {
    use ring::rand::SecureRandom as _;
    let mut buf = vec![0u8; bytes];
    ring::rand::SystemRandom::new()
        .fill(&mut buf)
        .expect("the platform's random source works");
    URL_SAFE_NO_PAD.encode(buf)
}

/// A fresh `state`: 128 bits, base64url.
#[must_use]
pub fn fresh_state() -> OidcState {
    OidcState::new(random_token(16))
}

/// A fresh `nonce`: 128 bits, base64url.
#[must_use]
pub fn fresh_nonce() -> OidcNonce {
    OidcNonce::new(random_token(16))
}

/// A fresh PKCE verifier: 256 bits, base64url — 43 characters, inside RFC 7636 §4.1's 43–128.
#[must_use]
pub fn fresh_verifier() -> PkceVerifier {
    PkceVerifier::new(random_token(32))
}

/// The S256 challenge of `verifier` (RFC 7636 §4.2).
#[must_use]
pub fn code_challenge(verifier: &PkceVerifier) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, verifier.as_str().as_bytes());
    URL_SAFE_NO_PAD.encode(digest.as_ref())
}

// ===========================================================================================
// The HTTP adapter
// ===========================================================================================

/// The token endpoint's success body. Everything but the ID token is ignored: the relying party
/// calls no other provider API, so an access token to the provider is a credential with no use.
#[derive(Deserialize)]
struct TokenResponse {
    id_token: String,
}

/// The token endpoint's refusal body (RFC 6749 §5.2).
#[derive(Deserialize, Default)]
struct TokenRefusal {
    #[serde(default)]
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

/// The relying party over a real provider.
#[derive(Debug)]
pub struct HttpIdentityProvider {
    settings: OidcSettings,
    http: reqwest::Client,
    metadata: MetadataCache,
    keys: KeyCache,
    clock: Arc<dyn Clock>,
}

impl HttpIdentityProvider {
    /// The egress client every relying-party request is sent with.
    ///
    /// Timeouts on, redirects off: a token endpoint that redirects is sending the client secret
    /// somewhere the discovery document did not name.
    ///
    /// # Errors
    ///
    /// Whatever `reqwest` refuses to build with — in practice nothing.
    pub fn http_client() -> reqwest::Result<reqwest::Client> {
        reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("capsule-server/", env!("CARGO_PKG_VERSION")))
            .build()
    }

    /// A relying party for `settings`, fetching with `http` and judging time by `clock`.
    pub fn new(settings: OidcSettings, http: reqwest::Client, clock: Arc<dyn Clock>) -> Self {
        Self {
            metadata: MetadataCache::new(settings.issuer.clone(), http.clone(), Arc::clone(&clock)),
            keys: KeyCache::new(http.clone(), Arc::clone(&clock)),
            settings,
            http,
            clock,
        }
    }

    /// The settings this relying party runs with.
    pub fn settings(&self) -> &OidcSettings {
        &self.settings
    }

    async fn exchange(&self, redemption: &Redemption<'_>) -> Result<String, ProviderError> {
        let metadata = self.metadata.current().await?;
        let mut form = vec![
            ("grant_type", "authorization_code"),
            ("code", redemption.code.as_str()),
            ("redirect_uri", redemption.redirect_uri),
            ("client_id", self.settings.client_id.as_str()),
            ("code_verifier", redemption.verifier.as_str()),
        ];
        // A public client identifies itself in the body; a confidential one authenticates.
        let mut request = self.http.post(&metadata.token_endpoint);
        if let Some(secret) = &self.settings.client_secret {
            request = request.basic_auth(&self.settings.client_id, Some(secret.expose()));
            form.retain(|(name, _)| *name != "client_id");
        }
        let response =
            request
                .form(&form)
                .send()
                .await
                .map_err(|error| ProviderError::Unavailable {
                    detail: format!("the token endpoint could not be reached: {error}"),
                })?;

        let status = response.status();
        if status.is_success() {
            let body: TokenResponse =
                response
                    .json()
                    .await
                    .map_err(|error| ProviderError::Unavailable {
                        detail: format!("the token endpoint's answer was not usable: {error}"),
                    })?;
            return Ok(body.id_token);
        }
        if status.is_client_error() {
            // RFC 6749 §5.2: a refused grant is a `400` with an `error` member. Anything else in
            // the 4xx range is still the provider saying no to *this* request.
            let refusal: TokenRefusal = response.json().await.unwrap_or_default();
            let detail = match refusal.error_description {
                Some(description) => format!("{} ({description})", refusal.error),
                None if refusal.error.is_empty() => format!("the token endpoint answered {status}"),
                None => refusal.error,
            };
            return Err(ProviderError::ExchangeRefused { detail });
        }
        Err(ProviderError::Unavailable {
            detail: format!("the token endpoint answered {status}"),
        })
    }

    async fn verify(
        &self,
        raw: &str,
        nonce: &OidcNonce,
    ) -> Result<VerifiedIdentity, ProviderError> {
        let metadata = self.metadata.current().await?;
        let expect = Expectations {
            issuer: self.settings.issuer.clone(),
            client_id: self.settings.client_id.clone(),
            nonce: nonce.as_str().to_owned(),
        };
        let now = self.clock.now();
        let keys = self.keys.current(&metadata.jwks_uri).await?;
        match verify_id_token(raw, &keys, &expect, now) {
            Err(ClaimRejection::UnknownKey { kid }) => {
                // The one rejection that is evidence rather than a verdict: the provider may
                // have rotated. Refetch — once, floored — and judge again against the new set.
                match self.keys.refresh(&metadata.jwks_uri).await? {
                    Refresh::Fetched(keys) => Ok(verify_id_token(raw, &keys, &expect, now)?),
                    Refresh::Suppressed => Err(ClaimRejection::UnknownKey { kid }.into()),
                }
            }
            verdict => Ok(verdict?),
        }
    }
}

impl IdentityProvider for HttpIdentityProvider {
    fn authorization_url<'a>(
        &'a self,
        request: &'a AuthorizationRequest<'a>,
    ) -> ProviderFuture<'a, String> {
        Box::pin(async move {
            if !self.settings.redirects.admits(request.redirect_uri) {
                return Err(ProviderError::RedirectRefused {
                    redirect_uri: request.redirect_uri.to_owned(),
                });
            }
            let metadata = self.metadata.current().await?;
            let mut url =
                reqwest::Url::parse(&metadata.authorization_endpoint).map_err(|error| {
                    ProviderError::Unavailable {
                        detail: format!("the authorization endpoint is not a URL: {error}"),
                    }
                })?;
            url.query_pairs_mut()
                .append_pair("response_type", "code")
                .append_pair("client_id", &self.settings.client_id)
                .append_pair("redirect_uri", request.redirect_uri)
                .append_pair("scope", SCOPES)
                .append_pair("state", request.state.as_str())
                .append_pair("nonce", request.nonce.as_str())
                .append_pair("code_challenge", request.code_challenge)
                .append_pair("code_challenge_method", "S256");
            Ok(url.into())
        })
    }

    fn redeem<'a>(
        &'a self,
        redemption: &'a Redemption<'a>,
    ) -> ProviderFuture<'a, VerifiedIdentity> {
        Box::pin(async move {
            let raw = self.exchange(redemption).await?;
            self.verify(&raw, redemption.nonce).await
        })
    }
}

/// The provider an unconfigured deployment runs with.
///
/// A null object rather than an `Option` on the context, so the routes have one shape and the
/// "not configured" answer is produced where every other provider answer is. It also implements
/// [`FederatedAccounts`](super::accounts::FederatedAccounts), refusing, so an unconfigured
/// context needs no second null object; that path is unreachable — a callback on an unconfigured
/// deployment finds no pending authorization first.
#[derive(Debug, Clone, Copy, Default)]
pub struct Disabled;

impl IdentityProvider for Disabled {
    fn authorization_url<'a>(
        &'a self,
        _request: &'a AuthorizationRequest<'a>,
    ) -> ProviderFuture<'a, String> {
        Box::pin(async { Err(ProviderError::NotConfigured) })
    }

    fn redeem<'a>(
        &'a self,
        _redemption: &'a Redemption<'a>,
    ) -> ProviderFuture<'a, VerifiedIdentity> {
        Box::pin(async { Err(ProviderError::NotConfigured) })
    }
}

#[cfg(test)]
mod tests {
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

    #[tokio::test]
    async fn the_disabled_provider_answers_not_configured() {
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
}
