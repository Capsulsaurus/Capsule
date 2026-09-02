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
//! The cost of that is a read-modify-write gap in the failed-attempt counter, so two
//! simultaneous wrong passwords can be recorded as one. That is the right trade for a
//! development adapter and it is *not* the trade a Postgres adapter should make: there the
//! increment is one statement, which is why the port asks the adapter for the bookkeeping rather
//! than describing how to do it.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use jiff::Timestamp;

use super::credential::{CredentialError, Credentials};
use super::directory::{AccountDirectory, Authentication, DirectoryError, DirectoryFuture};
use super::profile::{
    AccountProfiles, PasswordChange, PasswordChanged, ProfileRecord, ProfileUpdate,
};
use super::registry::{AccountRegistry, Registration};
use crate::store::UserId;

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
    /// The address it signs in with, verbatim as it was registered.
    email: String,
    /// The id every session and every manifest names.
    user_id: UserId,
    /// The Argon2id PHC string. Never a password.
    stored: String,
    /// The name it chose to be shown as.
    display_name: Option<String>,
    /// When it was created.
    created_at: Timestamp,
    /// Consecutive failed credential presentations.
    failures: u32,
}

impl Account {
    /// Whether enough failures have accumulated to refuse a correct password.
    fn locked(&self) -> bool {
        self.failures >= MAX_FAILED_ATTEMPTS
    }

    /// The profile view of this account.
    fn profile(&self) -> ProfileRecord {
        ProfileRecord {
            user_id: self.user_id.clone(),
            email: self.email.clone(),
            display_name: self.display_name.clone(),
            created_at: self.created_at,
        }
    }
}

/// Accounts held in this process, keyed by the address they registered with.
///
/// Addresses are compared **verbatim**, exactly as the suite's double compares them. Case
/// folding would be a normalization policy this port does not describe, and a policy invented
/// here is a policy the Postgres adapter would have to guess at: `Foo@example.test` and
/// `foo@example.test` are two accounts until a slice says otherwise, and saying otherwise is a
/// decision about identity rather than about storage.
#[derive(Debug)]
pub struct InMemoryAccounts {
    credentials: Credentials,
    accounts: Mutex<BTreeMap<String, Account>>,
}

impl InMemoryAccounts {
    /// An empty directory over `credentials`.
    ///
    /// The verifier is passed in rather than constructed here because building one costs an
    /// Argon2id hash (the decoy), and a composition root that builds several adapters should pay
    /// that once.
    pub fn new(credentials: Credentials) -> Self {
        Self {
            credentials,
            accounts: Mutex::new(BTreeMap::new()),
        }
    }

    /// How many accounts are held, for a caller that logs the profile it came up on.
    pub fn len(&self) -> usize {
        self.accounts().len()
    }

    /// Whether no account has been registered yet.
    pub fn is_empty(&self) -> bool {
        self.accounts().is_empty()
    }

    /// Take the lock, recovering rather than propagating a poisoned one.
    ///
    /// The same choice [`crate::store::memory`] makes: a panic in one request must not turn
    /// every later account lookup into a second panic, and the invariant this map holds is a
    /// `BTreeMap`'s own rather than one a half-finished write could break.
    fn accounts(&self) -> MutexGuard<'_, BTreeMap<String, Account>> {
        self.accounts.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A snapshot of the account `email` names, if there is one.
    fn by_email(&self, email: &str) -> Option<Account> {
        self.accounts().get(email).cloned()
    }

    /// A snapshot of the account `user` names, if there is one.
    fn by_id(&self, user: &UserId) -> Option<Account> {
        self.accounts()
            .values()
            .find(|held| &held.user_id == user)
            .cloned()
    }

    /// Record the outcome of a credential presentation against `email`.
    ///
    /// One place, so the reset-on-success half cannot be forgotten at one of the two call sites.
    fn record(&self, email: &str, granted: bool) {
        if let Some(held) = self.accounts().get_mut(email) {
            if granted {
                held.failures = 0;
            } else {
                held.failures = held.failures.saturating_add(1);
                if held.failures == MAX_FAILED_ATTEMPTS {
                    tracing::warn!(
                        user = %held.user_id,
                        failures = held.failures,
                        "an account reached the failed-attempt ceiling and is locked out"
                    );
                }
            }
        }
    }

    /// Decide `password` against a snapshot, and record what happened.
    ///
    /// The shared body of the two [`AccountDirectory`] methods: they differ only in how they
    /// find the account, and that is exactly the difference the port wants them to have.
    fn decide(
        &self,
        held: Option<Account>,
        password: &str,
    ) -> Result<Authentication, DirectoryError> {
        let Some(held) = held else {
            // The timing-equalized miss. Refusing here without doing the work would leak the
            // difference between an unknown address and a wrong password in the response time,
            // whatever the body said.
            self.credentials.absorb_miss(password);
            return Ok(Authentication::Refused);
        };
        if held.locked() {
            // Still absorbed: a locked account that returned instantly would tell an attacker
            // which addresses they have already spent attempts on.
            self.credentials.absorb_miss(password);
            return Ok(Authentication::Locked);
        }
        let granted = self
            .credentials
            .verify(password, &held.stored)
            .map_err(unavailable)?;
        self.record(&held.email, granted);
        if granted {
            Ok(Authentication::Granted(held.user_id))
        } else {
            Ok(Authentication::Refused)
        }
    }
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
        Box::pin(async move { self.decide(self.by_email(email), password) })
    }

    fn authenticate_user<'a>(
        &'a self,
        user: &'a UserId,
        password: &'a str,
    ) -> DirectoryFuture<'a, Authentication> {
        Box::pin(async move { self.decide(self.by_id(user), password) })
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
            let stored = self.credentials.hash(password).map_err(unavailable)?;
            // One critical section, as the port requires: a caller that read, saw nothing and
            // then wrote has a window in which a second registration for the same address
            // lands, and both would believe they own it.
            let mut accounts = self.accounts();
            if accounts.contains_key(email) {
                return Ok(Registration::AlreadyExists);
            }
            accounts.insert(
                email.to_owned(),
                Account {
                    email: email.to_owned(),
                    user_id: user.clone(),
                    stored,
                    display_name: None,
                    created_at: at,
                    failures: 0,
                },
            );
            tracing::info!(%user, "an account was created in the in-memory directory");
            Ok(Registration::Created(user.clone()))
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
            let mut accounts = self.accounts();
            let Some(held) = accounts.values_mut().find(|held| &held.user_id == user) else {
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
            let stored = self.credentials.hash(password).map_err(unavailable)?;
            let mut accounts = self.accounts();
            let Some(held) = accounts.values_mut().find(|held| &held.user_id == user) else {
                return Ok(PasswordChanged::NoSuchAccount);
            };
            held.stored = stored;
            // The port requires it: a change is a successful credential presentation, and
            // leaving the lockout behind would bar somebody from an account they just proved
            // they own.
            held.failures = 0;
            tracing::info!(%user, "an account's password was replaced");
            Ok(PasswordChanged::Yes)
        })
    }
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;

    use super::{Credentials, InMemoryAccounts, MAX_FAILED_ATTEMPTS};
    use crate::auth::directory::{AccountDirectory, Authentication};
    use crate::auth::profile::{AccountProfiles, PasswordChange, PasswordChanged, ProfileUpdate};
    use crate::auth::registry::{AccountRegistry, Registration};
    use crate::store::UserId;

    const EMAIL: &str = "somebody@example.test";
    const PASSWORD: &str = "correct horse battery staple";

    fn user() -> UserId {
        UserId::new("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e6f")
    }

    /// A directory with one registered account.
    async fn seeded() -> InMemoryAccounts {
        let accounts = InMemoryAccounts::new(Credentials::new().expect("the platform hashes"));
        assert_eq!(
            accounts
                .create(EMAIL, PASSWORD, &user(), Timestamp::UNIX_EPOCH)
                .await
                .expect("it writes"),
            Registration::Created(user())
        );
        accounts
    }

    #[tokio::test]
    async fn registering_then_signing_in_works() {
        // The whole reason this adapter exists rather than a fail-closed stub.
        let accounts = seeded().await;
        assert_eq!(
            accounts
                .authenticate(EMAIL, PASSWORD)
                .await
                .expect("it answers"),
            Authentication::Granted(user())
        );
    }

    #[tokio::test]
    async fn no_plaintext_password_is_retained() {
        // The property the port is built on: the credential never rises above the adapter, and
        // it is not sitting in the adapter either.
        let accounts = seeded().await;
        let held = accounts.by_email(EMAIL).expect("it is held");
        assert!(held.stored.starts_with("$argon2id$"), "{}", held.stored);
        assert!(!held.stored.contains(PASSWORD));
    }

    #[tokio::test]
    async fn a_taken_address_is_reported_and_nothing_is_written() {
        let accounts = seeded().await;
        let other = UserId::new("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e70");
        assert_eq!(
            accounts
                .create(
                    EMAIL,
                    "a different password entirely",
                    &other,
                    Timestamp::UNIX_EPOCH
                )
                .await
                .expect("it answers"),
            Registration::AlreadyExists
        );
        // The first account's credential still works, so nothing was overwritten.
        assert_eq!(
            accounts
                .authenticate(EMAIL, PASSWORD)
                .await
                .expect("it answers"),
            Authentication::Granted(user())
        );
    }

    #[tokio::test]
    async fn an_unknown_address_and_a_wrong_password_are_one_answer() {
        // The port collapses them into one value on purpose, so no caller *can* tell them apart.
        let accounts = seeded().await;
        assert_eq!(
            accounts
                .authenticate("nobody@example.test", PASSWORD)
                .await
                .expect("it answers"),
            Authentication::Refused
        );
        assert_eq!(
            accounts
                .authenticate(EMAIL, "the wrong password")
                .await
                .expect("it answers"),
            Authentication::Refused
        );
    }

    #[tokio::test]
    async fn enough_failures_lock_the_account_and_a_correct_password_is_told_so() {
        // `Locked` is the one refusal a *correct* password also receives, which is why it is a
        // separate value: a client showing "wrong password" here would send somebody round a
        // loop that cannot succeed.
        let accounts = seeded().await;
        for _ in 0..MAX_FAILED_ATTEMPTS {
            assert_eq!(
                accounts
                    .authenticate(EMAIL, "wrong")
                    .await
                    .expect("it answers"),
                Authentication::Refused
            );
        }
        assert_eq!(
            accounts
                .authenticate(EMAIL, PASSWORD)
                .await
                .expect("it answers"),
            Authentication::Locked
        );
    }

    #[tokio::test]
    async fn a_success_before_the_ceiling_clears_the_count() {
        let accounts = seeded().await;
        for _ in 0..MAX_FAILED_ATTEMPTS - 1 {
            let _ = accounts.authenticate(EMAIL, "wrong").await;
        }
        assert_eq!(
            accounts
                .authenticate(EMAIL, PASSWORD)
                .await
                .expect("it answers"),
            Authentication::Granted(user())
        );
        // Back to zero: the next wrong password does not tip an already-full counter over.
        let _ = accounts.authenticate(EMAIL, "wrong").await;
        assert_eq!(
            accounts
                .authenticate(EMAIL, PASSWORD)
                .await
                .expect("it answers"),
            Authentication::Granted(user())
        );
    }

    #[tokio::test]
    async fn a_password_change_clears_a_lockout() {
        // The port requires it: a change is a successful credential presentation.
        let accounts = seeded().await;
        for _ in 0..MAX_FAILED_ATTEMPTS {
            let _ = accounts.authenticate(EMAIL, "wrong").await;
        }
        assert_eq!(
            accounts
                .set_password(&user(), "a brand new password", Timestamp::UNIX_EPOCH)
                .await
                .expect("it writes"),
            PasswordChanged::Yes
        );
        assert_eq!(
            accounts
                .authenticate(EMAIL, "a brand new password")
                .await
                .expect("it answers"),
            Authentication::Granted(user())
        );
        assert_eq!(
            accounts
                .authenticate(EMAIL, PASSWORD)
                .await
                .expect("it answers"),
            Authentication::Refused
        );
    }

    #[tokio::test]
    async fn re_authentication_takes_the_account_from_the_credential_and_not_the_request() {
        let accounts = seeded().await;
        assert_eq!(
            accounts
                .authenticate_user(&user(), PASSWORD)
                .await
                .expect("it answers"),
            Authentication::Granted(user())
        );
        let stranger = UserId::new("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e71");
        assert_eq!(
            accounts
                .authenticate_user(&stranger, PASSWORD)
                .await
                .expect("it answers"),
            Authentication::Refused
        );
    }

    #[tokio::test]
    async fn changing_a_password_for_an_absent_account_writes_nothing() {
        let accounts = seeded().await;
        let stranger = UserId::new("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e72");
        assert_eq!(
            accounts
                .set_password(&stranger, "irrelevant", Timestamp::UNIX_EPOCH)
                .await
                .expect("it answers"),
            PasswordChanged::NoSuchAccount
        );
    }

    #[tokio::test]
    async fn a_profile_reads_back_what_registration_wrote_and_takes_an_edit() {
        let accounts = seeded().await;
        let profile = accounts
            .read(&user())
            .await
            .expect("it answers")
            .expect("the account exists");
        assert_eq!(profile.email, EMAIL);
        assert_eq!(profile.display_name, None);
        assert_eq!(profile.created_at, Timestamp::UNIX_EPOCH);

        let updated = accounts
            .update(
                &user(),
                &ProfileUpdate {
                    display_name: Some(Some("Ada Lovelace".to_owned())),
                },
            )
            .await
            .expect("it answers")
            .expect("the account exists");
        assert_eq!(updated.display_name.as_deref(), Some("Ada Lovelace"));

        // An absent field leaves the name alone; `Some(None)` clears it.
        let untouched = accounts
            .update(&user(), &ProfileUpdate::default())
            .await
            .expect("it answers")
            .expect("the account exists");
        assert_eq!(untouched.display_name.as_deref(), Some("Ada Lovelace"));

        let cleared = accounts
            .update(
                &user(),
                &ProfileUpdate {
                    display_name: Some(None),
                },
            )
            .await
            .expect("it answers")
            .expect("the account exists");
        assert_eq!(cleared.display_name, None);
    }

    #[tokio::test]
    async fn a_profile_for_an_absent_account_is_absent_and_not_an_error() {
        // Reachable with a perfectly valid credential: a session outlives the account row it
        // names if the account is deleted while a token is live.
        let accounts = seeded().await;
        let stranger = UserId::new("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e73");
        assert!(
            accounts
                .read(&stranger)
                .await
                .expect("it answers")
                .is_none()
        );
        assert!(
            accounts
                .update(&stranger, &ProfileUpdate::default())
                .await
                .expect("it answers")
                .is_none()
        );
    }

    #[tokio::test]
    async fn addresses_are_compared_verbatim() {
        // Recorded as a test rather than left implicit: case folding is a normalization policy
        // this port does not describe, and #402's adapter has to make the same choice.
        let accounts = seeded().await;
        assert_eq!(
            accounts
                .authenticate("Somebody@Example.test", PASSWORD)
                .await
                .expect("it answers"),
            Authentication::Refused
        );
    }
}
