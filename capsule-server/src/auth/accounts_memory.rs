//! [`InMemoryAccounts`] — the deterministic account store the development profile runs on.
//!
//! # Why this exists in `src/` when three port modules say it would not
//!
//! [`directory`](super::directory), [`registry`](super::registry) and
//! [`profile`](super::profile) each record the same reason for having no adapter: *"the real one
//! is Postgres, the test one is a double, and a double in `src/` is a fake credential directory
//! shipped inside the server binary."* That reasoning is about a **double** — specifically
//! `tests/support/mod.rs`'s, which "accepts whatever password it was told to accept" and
//! therefore must never be linkable by a server.
//!
//! This is not that. It verifies with the same Argon2id helper the Postgres adapter will use
//! ([`credential`](super::credential)), it stores PHC strings and no plaintext, it takes the
//! timing-equalized miss, and it locks an account out after enough failures. What it is missing
//! is **durability**, which is what makes it a development profile rather than a deployment:
//! every account registered against it is gone when the process exits. `capsule-server serve`
//! reaches it only through `--memory`, which is an explicit operator act
//! ([`Backends::Memory`](crate::config::Backends)).
//!
//! The alternative was a fail-closed stub: four ports that answer `Unavailable`, so
//! `POST /v1/auth/register` and `POST /v1/auth/login` return their declared refusal until #402
//! lands. That was rejected once the cost was measured — `argon2` is already a workspace
//! dependency with a design-doc row, and the credential helper this needs is the one #402
//! reuses, so nothing is written twice — and the gain is large: a `mise run serve-memory` you
//! can actually sign in to is the difference between a server a client developer can point at
//! and a surface they can only read.
//!
//! # Where the hashing happens relative to the lock
//!
//! Argon2id is deliberately expensive — tens of milliseconds — and this adapter holds a
//! `Mutex`. Every operation therefore computes or checks its hash **outside** the critical
//! section and touches the map only to read a snapshot or to write a result. Holding the lock
//! across a hash would serialize every account operation in the process behind the slowest
//! primitive in it.
//!
//! # The attempt is reserved before it is verified
//!
//! Checking the lockout, then verifying, then recording the failure is a race: every guess that
//! reads the count before any of them records one is admitted, so a burst of parallel guesses
//! walks straight past the ceiling (#510). So an attempt is **reserved** first — the lockout
//! check and the failure charge are one critical section — and only a reserved attempt is
//! verified, outside the lock and off the async runtime. A success then settles the charge by
//! clearing the run. However many guesses arrive at once, at most `lockout_attempts` of them
//! inside a window are ever verified; the rest are told `Locked`.
//!
//! The charge is taken pessimistically and is never refunded on a refusal or a dropped request.
//! A refusal is exactly what the charge already says, and a request cancelled mid-verification
//! must not be a free guess — otherwise disconnecting before the answer is a way to guess
//! without being counted.
//!
//! # Federated accounts are rows here too (#460)
//!
//! [`FederatedAccounts`] is implemented on this same object, over the same rows, because that is
//! the shape the Postgres adapter has — one account table — and the development profile is only
//! worth running if it answers like the deployment does. Two consequences follow, both asserted
//! by the shared suite: an OIDC identity asserting a **verified** address a password account
//! registered with is [`FederatedLink::AddressTaken`], and a federated account has a profile row.
//!
//! A federated row holds **no credential**. A presentation against it — by its verified address
//! or by its id — is answered exactly as an unknown account is: refused, after the
//! timing-equalized miss, and never charged, so it can never read as `Locked` either. A password
//! change against it reports no such account. "No password for OIDC accounts" is the absence of
//! a credential, not a check somebody has to remember.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use jiff::{SignedDuration, Timestamp};

use super::credential::{CredentialError, Credentials};
use super::directory::{AccountDirectory, Authentication, DirectoryError, DirectoryFuture};
use super::oidc::{FederatedAccounts, FederatedLink, VerifiedIdentity, reserved_address};
use super::profile::{
    AccountProfiles, PasswordChange, PasswordChanged, ProfileRecord, ProfileUpdate,
};
use super::registry::{AccountRegistry, Registration};
use crate::store::{Clock, UserId};

/// How many consecutive failures put an account into [`Authentication::Locked`].
///
/// Ten, and it is a **lockout** rather than a rate limit: the port is explicit that `Locked` is
/// account state the adapter owns, and that rate limiting is a counter with no port anywhere in
/// this crate. Cleared by a success and by a password change, which the port requires — leaving
/// it behind would bar somebody from an account they just proved they own.
pub const MAX_FAILED_ATTEMPTS: u32 = 10;

/// One account, as this adapter holds it.
#[derive(Debug, Clone)]
struct Account {
    /// The address it signs in with, verbatim as it was registered — or, for a federated
    /// account, the address its provider asserted **verified**, if it asserted one.
    email: Option<String>,
    /// The id every session and every manifest names.
    user_id: UserId,
    /// The Argon2id PHC string. Never a password. `None` for a federated account, which no
    /// password can sign in to.
    stored: Option<String>,
    /// The name it chose to be shown as.
    display_name: Option<String>,
    /// When it was created.
    created_at: Timestamp,
    /// Consecutive failed credential presentations, since the last success or decay.
    failures: u32,
    /// When the most recent one was, if there has been one.
    ///
    /// The lockout's whole clock. Without it the count is a one-way door: **nothing in this
    /// server can clear a lockout.** `login`, `reauthenticate` and `password` each ask the
    /// directory first and refuse on `Locked` before verifying anything, there is no unlock
    /// operation on any surface, and no operator command reaches this state — so a permanent
    /// lockout is a permanently lost account.
    last_failure_at: Option<Timestamp>,
}

impl Account {
    /// Whether enough recent failures have accumulated to refuse a correct password.
    ///
    /// Recent is the operative word. The window is measured from the last **counted** failure, so
    /// a person who mistyped their password ten times and walked away gets back in.
    fn locked(&self, now: Timestamp, threshold: u32, window: SignedDuration) -> bool {
        self.failures >= threshold
            && self
                .last_failure_at
                .is_some_and(|at| now.duration_since(at) < window)
    }

    /// The profile view of this account.
    ///
    /// Every account has one. A federated account whose provider asserted no verified address
    /// answers with [`ProfileRecord::email`] `None` — the same answer the Postgres adapter gives
    /// for the same row.
    fn profile(&self) -> ProfileRecord {
        ProfileRecord {
            user_id: self.user_id.clone(),
            email: self.email.clone(),
            display_name: self.display_name.clone(),
            created_at: self.created_at,
        }
    }
}

/// Every account, and the two indexes that find one.
#[derive(Debug, Default)]
struct Held {
    /// Every account, by id.
    accounts: BTreeMap<UserId, Account>,
    /// Address → the account holding it: every password account, and every federated account
    /// whose provider asserted its address verified. Unique, as the Postgres index is.
    addresses: BTreeMap<String, UserId>,
    /// `(issuer, subject)` → the federated account it is.
    links: BTreeMap<(String, String), UserId>,
}

impl Held {
    /// The account holding `email`, if any.
    fn by_email_mut(&mut self, email: &str) -> Option<&mut Account> {
        let user = self.addresses.get(email)?;
        self.accounts.get_mut(user)
    }
}

/// Accounts held in this process: password accounts and federated ones, in one map.
///
/// Addresses are compared **verbatim**, exactly as the suite's double compares them. Case
/// folding would be a normalization policy this port does not describe, and a policy invented
/// here is a policy the Postgres adapter would have to guess at: `Foo@example.test` and
/// `foo@example.test` are two accounts until a slice says otherwise, and saying otherwise is a
/// decision about identity rather than about storage.
#[derive(Debug)]
pub struct InMemoryAccounts {
    credentials: Credentials,
    clock: Arc<dyn Clock>,
    lockout_attempts: u32,
    lockout_window: SignedDuration,
    held: Mutex<Held>,
}

impl InMemoryAccounts {
    /// An empty directory over `credentials`, locking an account out for `lockout_window` after
    /// `lockout_attempts` consecutive failures ([`MAX_FAILED_ATTEMPTS`] by default).
    ///
    /// The verifier is passed in rather than constructed here because building one costs an
    /// Argon2id hash (the decoy), and a composition root that builds several adapters should pay
    /// that once. The clock is injected for the reason every other adapter in this crate injects
    /// one: expiry that reads the wall clock directly is expiry a test can only assert by
    /// sleeping.
    pub fn new(
        credentials: Credentials,
        clock: Arc<dyn Clock>,
        lockout_attempts: u32,
        lockout_window: SignedDuration,
    ) -> Self {
        Self {
            credentials,
            clock,
            lockout_attempts,
            lockout_window,
            held: Mutex::new(Held::default()),
        }
    }

    /// How many accounts are held, password and federated alike, for a caller that logs the
    /// profile it came up on.
    pub fn len(&self) -> usize {
        self.held().accounts.len()
    }

    /// Whether no account has been created yet.
    pub fn is_empty(&self) -> bool {
        self.held().accounts.is_empty()
    }

    /// The failure count held for `email`, or `None` when no account holds it.
    ///
    /// Test-only: no port discloses the count. The conformance harness reads it to know an
    /// attempt's charge has landed before it cancels the attempt.
    #[cfg(test)]
    pub(crate) fn recorded_failures(&self, email: &str) -> Option<u32> {
        self.held().by_email_mut(email).map(|held| held.failures)
    }

    /// Take the lock, recovering rather than propagating a poisoned one.
    ///
    /// The same choice [`crate::store::memory`] makes: a panic in one request must not turn
    /// every later account lookup into a second panic. Every write below changes the map and
    /// its indexes inside one critical section with no early return between them, so a
    /// half-finished write is not a state a panic can leave behind.
    fn held(&self) -> MutexGuard<'_, Held> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A snapshot of the account `user` names, if there is one.
    fn by_id(&self, user: &UserId) -> Option<Account> {
        self.held().accounts.get(user).cloned()
    }

    /// Reserve one credential presentation against the account `key` names.
    ///
    /// The lockout check and the failure charge in one critical section, which is the whole fix
    /// for #510: see the module docs. A failure after the window has passed starts a fresh run
    /// rather than tipping a stale count over — otherwise ten mistypes spread over a year would
    /// lock an account on the tenth, which is not a guessing run and not what the ceiling is
    /// counting. A locked account is **not** charged: an attempt made while locked must not
    /// extend the window, or anybody who can reach the endpoint can keep somebody else's account
    /// locked forever.
    ///
    /// A federated account holds no credential, so it is answered as no account at all and is
    /// not charged (module docs): there is nothing to verify, and charging it would let anyone
    /// who knows its address turn the refusal into `Locked`.
    fn reserve(&self, key: Key<'_>) -> Reservation {
        let now = self.clock.now();
        let window = self.lockout_window;
        let mut accounts = self.held();
        let held = match key {
            Key::Email(email) => accounts.by_email_mut(email),
            Key::User(user) => accounts.accounts.get_mut(user),
        };
        let Some(held) = held else {
            return Reservation::NoSuchAccount;
        };
        let Some(stored) = held.stored.clone() else {
            tracing::debug!(user = %held.user_id, "a credential presentation met an account with no password");
            return Reservation::NoSuchAccount;
        };
        if held.locked(now, self.lockout_attempts, window) {
            tracing::debug!(user = %held.user_id, "a credential presentation met a lockout");
            return Reservation::Locked(held.user_id.clone());
        }
        if held
            .last_failure_at
            .is_some_and(|at| now.duration_since(at) >= window)
        {
            held.failures = 0;
        }
        held.failures = held.failures.saturating_add(1);
        held.last_failure_at = Some(now);
        tracing::trace!(
            user = %held.user_id,
            failures = held.failures,
            "a credential presentation was reserved"
        );
        if held.failures == self.lockout_attempts {
            tracing::warn!(
                user = %held.user_id,
                failures = held.failures,
                window = %window,
                "an account reached the failed-attempt ceiling; further attempts are locked out \
                 unless this one succeeds"
            );
        }
        Reservation::Reserved {
            user_id: held.user_id.clone(),
            stored,
        }
    }

    /// Settle a reservation whose password matched: the run of failures is over.
    fn settle_success(&self, user: &UserId) {
        if let Some(held) = self.held().accounts.get_mut(user) {
            held.failures = 0;
            held.last_failure_at = None;
        }
    }

    /// Reserve an attempt against `key`, verify `password` if one was reserved, and settle.
    ///
    /// The shared body of the two [`AccountDirectory`] methods: they differ only in how they
    /// find the account, and that is exactly the difference the port wants them to have.
    async fn decide(&self, key: Key<'_>, password: &str) -> Result<Authentication, DirectoryError> {
        // The guard is dropped inside `reserve`, so no lock is held across the hash below.
        let reservation = self.reserve(key);
        let (user_id, stored) = match reservation {
            Reservation::NoSuchAccount => {
                // The timing-equalized miss. Refusing here without doing the work would leak the
                // difference between an unknown address and a wrong password in the response
                // time, whatever the body said.
                self.credentials.absorb_miss(password).await;
                return Ok(Authentication::Refused(None));
            }
            Reservation::Locked(user_id) => {
                // Still absorbed: a locked account that returned instantly would tell an
                // attacker which addresses they have already spent attempts on.
                self.credentials.absorb_miss(password).await;
                return Ok(Authentication::Locked(user_id));
            }
            Reservation::Reserved { user_id, stored } => (user_id, stored),
        };
        let granted = self
            .credentials
            .verify(password, &stored)
            .await
            .map_err(unavailable)?;
        if granted {
            self.settle_success(&user_id);
            Ok(Authentication::Granted(user_id))
        } else {
            Ok(Authentication::Refused(Some(user_id)))
        }
    }
}

/// How [`InMemoryAccounts::reserve`] finds the account.
#[derive(Debug, Clone, Copy)]
enum Key<'a> {
    /// By the address it signs in with.
    Email(&'a str),
    /// By its id, for re-authentication.
    User(&'a UserId),
}

/// What reserving an attempt found.
#[derive(Debug)]
enum Reservation {
    /// No account answers to the key, or the one that does holds no credential.
    NoSuchAccount,
    /// This account is refusing attempts; nothing was charged.
    Locked(UserId),
    /// One failure was charged, and this is what to verify against.
    Reserved {
        user_id: UserId,
        /// The Argon2id PHC string. Never a password.
        stored: String,
    },
}

/// A credential fault is a directory fault: no decision was reached.
///
/// It is [`DirectoryError::Unavailable`] rather than a refusal because a stored hash this server
/// cannot read is a broken row, and answering "your password is wrong" would send somebody round
/// a loop that cannot succeed.
fn unavailable(error: CredentialError) -> DirectoryError {
    tracing::error!(%error, "a stored credential could not be processed");
    DirectoryError::Unavailable {
        detail: error.to_string(),
    }
}

impl AccountDirectory for InMemoryAccounts {
    fn authenticate<'a>(
        &'a self,
        email: &'a str,
        password: &'a str,
    ) -> DirectoryFuture<'a, Authentication> {
        Box::pin(async move { self.decide(Key::Email(email), password).await })
    }

    fn authenticate_user<'a>(
        &'a self,
        user: &'a UserId,
        password: &'a str,
    ) -> DirectoryFuture<'a, Authentication> {
        Box::pin(async move { self.decide(Key::User(user), password).await })
    }
}

impl AccountRegistry for InMemoryAccounts {
    fn create<'a>(
        &'a self,
        email: &'a str,
        password: &'a str,
        user: &'a UserId,
        at: Timestamp,
    ) -> DirectoryFuture<'a, Registration> {
        Box::pin(async move {
            // Hashed before the lock, so the check-and-write below is short. The cost of that
            // ordering is a hash computed for an address that turns out to be taken, which is
            // the cheap direction to be wrong in.
            let stored = self.credentials.hash(password).await.map_err(unavailable)?;
            // One critical section, as the port requires: a caller that read, saw nothing and
            // then wrote has a window in which a second registration for the same address
            // lands, and both would believe they own it.
            let mut held = self.held();
            // Against every address the table holds, a federated account's verified one
            // included: an address a provider vouched for is an address somebody owns.
            if held.addresses.contains_key(email) {
                return Ok(Registration::AlreadyExists);
            }
            if held.accounts.contains_key(user) {
                return Err(minted_id_taken(user));
            }
            held.addresses.insert(email.to_owned(), user.clone());
            held.accounts.insert(
                user.clone(),
                Account {
                    email: Some(email.to_owned()),
                    user_id: user.clone(),
                    stored: Some(stored),
                    display_name: None,
                    created_at: at,
                    failures: 0,
                    last_failure_at: None,
                },
            );
            tracing::info!(%user, "an account was created in the in-memory directory");
            Ok(Registration::Created(user.clone()))
        })
    }
}

/// A freshly minted id that an account already holds.
///
/// Unreachable with ids from `new_user_id` — a UUIDv7 collision — and an error rather than a
/// refusal for the reason the Postgres adapter's primary-key violation is one: it is not a fact
/// about the caller's address, so neither `AlreadyExists` nor `AddressTaken` would be true.
fn minted_id_taken(user: &UserId) -> DirectoryError {
    tracing::error!(%user, "a freshly minted account id is already held");
    DirectoryError::Unavailable {
        detail: "a freshly minted account id is already held".to_owned(),
    }
}

impl FederatedAccounts for InMemoryAccounts {
    fn resolve_or_create<'a>(
        &'a self,
        identity: &'a VerifiedIdentity,
        user: &'a UserId,
        at: Timestamp,
    ) -> DirectoryFuture<'a, FederatedLink> {
        Box::pin(async move {
            // One critical section, as the port requires: the lookup, the address check and the
            // write cannot be separated by a second callback for the same person.
            let mut held = self.held();
            let link = (identity.issuer.clone(), identity.subject.clone());
            if let Some(existing) = held.links.get(&link) {
                tracing::debug!(user = %existing, issuer = %identity.issuer, "a federated identity resolved to its account");
                return Ok(FederatedLink::Linked(existing.clone()));
            }
            // Verified addresses only, both ways: an unverified one neither blocks nor reserves.
            let address = reserved_address(identity).map(str::to_owned);
            if let Some(email) = &address
                && held.addresses.contains_key(email)
            {
                tracing::info!(issuer = %identity.issuer, "a federated sign-in asserted an address another account holds");
                return Ok(FederatedLink::AddressTaken);
            }
            if held.accounts.contains_key(user) {
                return Err(minted_id_taken(user));
            }
            if let Some(email) = &address {
                held.addresses.insert(email.clone(), user.clone());
            }
            held.links.insert(link, user.clone());
            held.accounts.insert(
                user.clone(),
                Account {
                    email: address,
                    user_id: user.clone(),
                    stored: None,
                    display_name: None,
                    created_at: at,
                    failures: 0,
                    last_failure_at: None,
                },
            );
            tracing::info!(%user, issuer = %identity.issuer, "created an account for a federated identity");
            Ok(FederatedLink::Created(user.clone()))
        })
    }
}

impl AccountProfiles for InMemoryAccounts {
    fn read<'a>(&'a self, user: &'a UserId) -> DirectoryFuture<'a, Option<ProfileRecord>> {
        Box::pin(async move { Ok(self.by_id(user).as_ref().map(Account::profile)) })
    }

    fn update<'a>(
        &'a self,
        user: &'a UserId,
        update: &'a ProfileUpdate,
    ) -> DirectoryFuture<'a, Option<ProfileRecord>> {
        Box::pin(async move {
            // One critical section, as the port requires: a read-modify-write a caller could
            // interleave is an edit from another device silently clobbered.
            let mut accounts = self.held();
            let Some(held) = accounts.accounts.get_mut(user) else {
                return Ok(None);
            };
            if let Some(display_name) = update.display_name.clone() {
                held.display_name = display_name;
            }
            Ok(Some(held.profile()))
        })
    }
}

impl PasswordChange for InMemoryAccounts {
    fn set_password<'a>(
        &'a self,
        user: &'a UserId,
        password: &'a str,
        _at: Timestamp,
    ) -> DirectoryFuture<'a, PasswordChanged> {
        Box::pin(async move {
            let stored = self.credentials.hash(password).await.map_err(unavailable)?;
            let mut accounts = self.held();
            // A federated account has no password to replace, and gaining one here would be a
            // password nobody chose at the provider — so it is no such account to this port.
            let Some(held) = accounts
                .accounts
                .get_mut(user)
                .filter(|held| held.stored.is_some())
            else {
                return Ok(PasswordChanged::NoSuchAccount);
            };
            held.stored = Some(stored);
            // The port requires it: a change is a successful credential presentation, and
            // leaving the lockout behind would bar somebody from an account they just proved
            // they own.
            held.failures = 0;
            held.last_failure_at = None;
            tracing::info!(%user, "an account's password was replaced");
            Ok(PasswordChanged::Yes)
        })
    }
}

#[cfg(test)]
mod tests;
