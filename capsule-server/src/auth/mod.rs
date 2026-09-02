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
//! | Who exists, and whether a password is theirs | [`AccountDirectory`] | the one question a sign-in asks |
//! | Bringing an account into existence | [`AccountRegistry`] (`S-C53`) | registration must disclose what authentication must not |
//! | What an account keeps about itself | [`AccountProfiles`] (`S-C54`) | an ordinary authenticated write |
//! | Replacing a password | [`PasswordChange`] (`S-C54`) | a credential rotation, and separate for that reason |
//! | Token minting and reading | [`SessionTokens`] | a pure function of a key and a clock, so a concrete type rather than a port |
//! | Presenting a credential | [`AccessToken`] | a Kynos `SecurityScheme`, which is the only way to guard an operation *and* describe it |
//!
//! # The one bundle a handler injects
//!
//! [`AuthContext`] carries every one of them. It is one injected value rather than nine because a
//! Kynos context provides *types*, and nine `Inject` arguments on every handler would put the
//! module's internal shape in every signature — a change to what authentication needs would then
//! be a change to every operation that authenticates. It is built from
//! [`AuthCollaborators`], a named struct rather than a positional argument list, for the reason
//! [`crate::app::Modules`] is one: a constructor that grows a parameter per ported surface is a
//! constructor that will eventually be got wrong positionally, and two `Arc<dyn …>` swapped at a
//! call site is a compile error only by luck.
//!
//! # Adapters, and the one that is still owed
//!
//! [`InMemoryAccounts`] implements all four account ports over a map, verifying with the
//! [`credential`] helper's Argon2id — so the development profile can register an account and
//! sign in to it — and [`InMemoryTotp`] implements the second factor's. Neither is durable, and
//! neither is reachable without `--memory`
//! ([`Backends::Memory`](crate::config::Backends)), which is an explicit operator act.
//!
//! What is deliberately **not** here is a permissive one. `tests/support/mod.rs` holds a
//! credential directory that "accepts whatever password it was told to accept", and its own docs
//! say why that "belongs in a test binary and nowhere a server could link it". The distinction
//! the port modules were drawing is between a double and an implementation, not between
//! Postgres and everything else.
//!
//! The Postgres adapters are owed (#402), and they are written against these ports and the
//! suites over them. [`SessionTokens`] is not a trait at all, for the reason `credential`
//! records: it is a pure function of a key and a clock.

pub mod accounts_memory;
pub mod accounts_postgres;
pub mod conformance;
pub mod credential;
pub mod directory;
pub mod profile;
pub mod registry;
pub mod scheme;
pub mod tokens;
pub mod totp;

use std::sync::Arc;

pub use self::accounts_memory::InMemoryAccounts;
pub use self::accounts_postgres::PostgresAccounts;
pub use self::credential::{CredentialError, Credentials};
pub use self::directory::{AccountDirectory, Authentication, DirectoryError, DirectoryFuture};
pub use self::profile::{
    AccountProfiles, MAX_DISPLAY_NAME_CHARS, MalformedProfile, PasswordChange, PasswordChanged,
    ProfileRecord, ProfileUpdate, admissible_display_name,
};
pub use self::registry::{AccountRegistry, MIN_PASSWORD_LENGTH, Registration, new_user_id};
pub use self::scheme::{AccessToken, AuthenticatedSession, TOUCH_INTERVAL};
pub use self::tokens::{
    ACCESS_TOKEN_TTL, ChallengeId, ISSUER, IssuedChallenge, IssuedTokens, SessionTokens,
    TokenError, TokenKind, VerifiedChallenge, VerifiedToken,
};
pub use self::totp::{
    ActivateOutcome, BeginOutcome, CHALLENGE_TTL, ConsumeOutcome, EnrollmentState, InMemoryTotp,
    TotpCodes, TotpContext, TotpEnrollment, TotpSecret, TotpStore, UnusableSecret,
};
use crate::store::{AuthStateStore, Clock};

/// The collaborators an [`AuthContext`] is assembled from.
///
/// Named rather than ordered; see the module docs.
#[derive(Debug)]
pub struct AuthCollaborators {
    /// The session-state store (`S-C29`).
    pub sessions: Arc<dyn AuthStateStore>,
    /// Who exists, and whether a presented password is theirs.
    pub accounts: Arc<dyn AccountDirectory>,
    /// Where accounts are created (`S-C53`).
    pub registry: Arc<dyn AccountRegistry>,
    /// The facts an account keeps about itself (`S-C54`).
    pub profiles: Arc<dyn AccountProfiles>,
    /// Where a password is replaced (`S-C54`).
    pub passwords: Arc<dyn PasswordChange>,
    /// The single-use revoke-all challenges (`S-C23`, `S-C29`).
    pub challenges: Arc<dyn crate::store::ChallengeStore>,
    /// The durable device-cohort map (`S-C13`).
    pub cohorts: Arc<dyn crate::store::CohortStore>,
    /// The token signer.
    pub tokens: Arc<SessionTokens>,
    /// The clock every record and every deadline is stamped from.
    pub clock: Arc<dyn Clock>,
}

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
    profiles: Arc<dyn AccountProfiles>,
    passwords: Arc<dyn PasswordChange>,
    challenges: Arc<dyn crate::store::ChallengeStore>,
    cohorts: Arc<dyn crate::store::CohortStore>,
    tokens: Arc<SessionTokens>,
    clock: Arc<dyn Clock>,
}

impl AuthContext {
    /// Assembles the module from its nine collaborators.
    ///
    /// `clock` is passed separately rather than read back out of `tokens` because the session
    /// records and the token deadlines must be stamped from the *same* instant source; handing
    /// it in once is what makes "the same clock" a fact about construction rather than a
    /// convention two call sites have to keep.
    pub fn new(collaborators: AuthCollaborators) -> Self {
        let AuthCollaborators {
            sessions,
            accounts,
            registry,
            profiles,
            passwords,
            challenges,
            cohorts,
            tokens,
            clock,
        } = collaborators;
        Self {
            sessions,
            accounts,
            registry,
            profiles,
            passwords,
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

    /// The facts an account keeps about itself (`S-C54`).
    pub fn profiles(&self) -> &dyn AccountProfiles {
        self.profiles.as_ref()
    }

    /// Where a password is replaced (`S-C54`).
    pub fn passwords(&self) -> &dyn PasswordChange {
        self.passwords.as_ref()
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
