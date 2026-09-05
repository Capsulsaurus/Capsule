//! The OpenID Connect relying party (slice `S-N1`).
//!
//! # What it is, and what it is not
//!
//! Capsule is a **relying party only** (design/authentication.md, "Choosing an Auth Path"): an
//! external identity provider authenticates the *session*, and the master key never derives
//! from, and is never visible to, whatever the provider verified. Account lifecycle policy lives
//! at the provider. What this module does is the authorization-code + PKCE handshake, the checks
//! on what comes back, and the mapping from a provider's stable subject to a Capsule account —
//! after which the session is opened by exactly the function the password path uses.
//!
//! # Shape
//!
//! | Concern | Lives | Why |
//! | --- | --- | --- |
//! | Every check an ID token must pass | [`claims`] | a pure function, so every negative case is a unit test with no socket and no clock |
//! | Provider metadata and its cache | [`discovery`] | one fetch a day, refused if it names a different issuer |
//! | The provider's signing keys | [`jwks`] | refetched on an unknown `kid`, at most once a minute |
//! | The port the routes drive, and its one HTTP adapter | [`provider`] | the only external boundary the feature has, so the only thing doubled |
//! | Which account a verified identity is | [`accounts`] | one atomic operation keyed on `(issuer, subject)` |
//! | The pending ceremony between the two legs | [`crate::store::OidcAuthorizationStore`] | a single-use, short-window credential, which is what the ceremony stores are for |
//!
//! # Hand-written over `jsonwebtoken`, not `openidconnect`
//!
//! `openidconnect` would have pulled in `chrono` and the `log` facade — both banned — and
//! `rsa 0.9` carrying RUSTSEC-2023-0071 with no fixed release, which `deny.toml` would not catch
//! because only the licence check is wired. The hard part, verifying a signature against a JWKS,
//! is already in the workspace's JWT crate; what is left is discovery, a form `POST` and the
//! claim checks, each of which is a security decision this repository wants written where a
//! reader can see it. See the OIDC row in design/dependencies.md.

pub mod accounts;
pub mod claims;
pub mod discovery;
pub mod jwks;
pub mod provider;

use std::sync::Arc;

pub use self::accounts::{FederatedAccounts, FederatedLink, InMemoryFederatedAccounts};
pub use self::claims::{
    ALLOWED_ALGORITHMS, CLOCK_SKEW_SECONDS, ClaimRejection, Expectations, MAX_SUBJECT_LENGTH,
    VerifiedIdentity, verify_id_token,
};
pub use self::provider::{
    AuthorizationRequest, ClientSecret, Disabled, HttpIdentityProvider, IdentityProvider,
    OidcSettings, ProviderError, ProviderFuture, Redemption, RedirectPolicy, SCOPES,
    code_challenge, fresh_nonce, fresh_state, fresh_verifier,
};
use crate::store::{Clock, OidcAuthorizationStore};

/// The collaborators an [`OidcContext`] is assembled from. Named rather than ordered, as
/// [`AuthCollaborators`](crate::auth::AuthCollaborators) is.
#[derive(Debug)]
pub struct OidcCollaborators {
    /// The identity provider — [`Disabled`] when `OIDC_ISSUER` is unset.
    pub provider: Arc<dyn IdentityProvider>,
    /// The pending ceremonies between the two legs.
    pub authorizations: Arc<dyn OidcAuthorizationStore>,
    /// Which account a verified identity is.
    pub accounts: Arc<dyn FederatedAccounts>,
    /// The clock every ceremony and every deadline is stamped from.
    pub clock: Arc<dyn Clock>,
}

/// Everything the OIDC operations reach for, as one injectable value.
#[derive(Debug, Clone)]
pub struct OidcContext {
    provider: Arc<dyn IdentityProvider>,
    authorizations: Arc<dyn OidcAuthorizationStore>,
    accounts: Arc<dyn FederatedAccounts>,
    clock: Arc<dyn Clock>,
}

impl OidcContext {
    /// Assembles the module.
    pub fn new(collaborators: OidcCollaborators) -> Self {
        let OidcCollaborators {
            provider,
            authorizations,
            accounts,
            clock,
        } = collaborators;
        Self {
            provider,
            authorizations,
            accounts,
            clock,
        }
    }

    /// The module for a deployment with no identity provider.
    ///
    /// Every operation answers `error.auth.oidc_not_configured` (the authorize) or finds no
    /// pending ceremony (the callback). The store is real and empty so the shape is the same as
    /// a configured deployment's; nothing ever writes to it.
    pub fn disabled(clock: Arc<dyn Clock>) -> Self {
        Self::new(OidcCollaborators {
            provider: Arc::new(Disabled),
            authorizations: Arc::new(
                crate::store::memory::InMemoryOidcAuthorizations::with_default_ttl(Arc::clone(
                    &clock,
                )),
            ),
            accounts: Arc::new(Disabled),
            clock,
        })
    }

    /// The identity provider.
    pub fn provider(&self) -> &dyn IdentityProvider {
        self.provider.as_ref()
    }

    /// The pending ceremonies.
    pub fn authorizations(&self) -> &dyn OidcAuthorizationStore {
        self.authorizations.as_ref()
    }

    /// Which account a verified identity is.
    pub fn accounts(&self) -> &dyn FederatedAccounts {
        self.accounts.as_ref()
    }

    /// The clock every ceremony is stamped from.
    pub fn clock(&self) -> &dyn Clock {
        self.clock.as_ref()
    }
}
