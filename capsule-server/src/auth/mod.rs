//! The authentication module: the ports login needs, the tokens a session is worked through,
//! and the bearer scheme every authenticated operation is guarded by.
//!
//! One cohesive module, per design/module-map.md's "Planned Server Modules" — the operations
//! themselves live in [`crate::routes::auth`], because a route is a description of a surface and
//! this is the machinery behind it.
//!
//! # What this module owns, and what it borrows
//!
//! | Concern | Lives | Why |
//! | --- | --- | --- |
//! | Session records | [`crate::store::AuthStateStore`] (`S-C29`) | landed already; consumed, not re-declared |
//! | Accounts and passwords | [`AccountDirectory`] | the only database dependency of these three operations |
//! | Token minting and reading | [`SessionTokens`] | a pure function of a key and a clock, so a concrete type rather than a port |
//! | Presenting a credential | [`AccessToken`] | a Kynos `SecurityScheme`, which is the only way to guard an operation *and* describe it |
//!
//! # The one bundle a handler injects
//!
//! [`AuthContext`] carries all four. It is one injected value rather than four because a Kynos
//! context provides *types*, and four `Inject` arguments on every handler would put the module's
//! internal shape in every signature — a change to what authentication needs would then be a
//! change to every operation that authenticates.
//!
//! # Adapters this slice does not write
//!
//! There is no [`AccountDirectory`] implementation in `src/`, and that is deliberate rather than
//! unfinished: the real one is Postgres, the test one is a double, and a double in `src/` is a
//! fake credential directory shipped inside the server binary. The suite's doubles live in
//! `tests/support/`. Same reasoning, one step further than `S-C29` took it for the session
//! store, and it is why [`SessionTokens`] is not a trait at all.

pub mod directory;
pub mod registry;
pub mod scheme;
pub mod tokens;

use std::sync::Arc;

pub use self::directory::{AccountDirectory, Authentication, DirectoryError, DirectoryFuture};
pub use self::registry::{AccountRegistry, MIN_PASSWORD_LENGTH, Registration, new_user_id};
pub use self::scheme::{AccessToken, AuthenticatedSession, TOUCH_INTERVAL};
pub use self::tokens::{
    ACCESS_TOKEN_TTL, ISSUER, IssuedTokens, SessionTokens, TokenError, TokenKind, VerifiedToken,
};
use crate::store::{AuthStateStore, Clock};

/// Everything the auth operations reach for, as one injectable value.
///
/// `Clone` is cheap and required — a Kynos provider hands out a value per request — and every
/// field is an `Arc`, so cloning shares the one store, the one directory and the one signer the
/// process was built with.
#[derive(Debug, Clone)]
pub struct AuthContext {
    sessions: Arc<dyn AuthStateStore>,
    accounts: Arc<dyn AccountDirectory>,
    registry: Arc<dyn AccountRegistry>,
    challenges: Arc<dyn crate::store::ChallengeStore>,
    cohorts: Arc<dyn crate::store::CohortStore>,
    tokens: Arc<SessionTokens>,
    clock: Arc<dyn Clock>,
}

impl AuthContext {
    /// Assembles the module from its seven collaborators.
    ///
    /// `clock` is passed separately rather than read back out of `tokens` because the session
    /// records and the token deadlines must be stamped from the *same* instant source; handing
    /// it in once is what makes "the same clock" a fact about construction rather than a
    /// convention two call sites have to keep.
    pub fn new(
        sessions: Arc<dyn AuthStateStore>,
        accounts: Arc<dyn AccountDirectory>,
        registry: Arc<dyn AccountRegistry>,
        challenges: Arc<dyn crate::store::ChallengeStore>,
        cohorts: Arc<dyn crate::store::CohortStore>,
        tokens: Arc<SessionTokens>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            sessions,
            accounts,
            registry,
            challenges,
            cohorts,
            tokens,
            clock,
        }
    }

    /// The session-state store (`S-C29`).
    pub fn sessions(&self) -> &dyn AuthStateStore {
        self.sessions.as_ref()
    }

    /// The account directory.
    pub fn accounts(&self) -> &dyn AccountDirectory {
        self.accounts.as_ref()
    }

    /// Where accounts are created (`S-C53`).
    pub fn registry(&self) -> &dyn AccountRegistry {
        self.registry.as_ref()
    }

    /// The single-use revoke-all challenges (`S-C23`, `S-C29`).
    pub fn challenges(&self) -> &dyn crate::store::ChallengeStore {
        self.challenges.as_ref()
    }

    /// The durable device-cohort map (`S-C13`).
    pub fn cohorts(&self) -> &dyn crate::store::CohortStore {
        self.cohorts.as_ref()
    }

    /// The token signer.
    pub fn tokens(&self) -> &SessionTokens {
        self.tokens.as_ref()
    }

    /// The clock every record and every deadline is stamped from.
    pub fn clock(&self) -> &dyn Clock {
        self.clock.as_ref()
    }
}
