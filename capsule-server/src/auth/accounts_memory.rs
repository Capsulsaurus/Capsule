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

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use jiff::{SignedDuration, Timestamp};

use super::credential::{CredentialError, Credentials};
use super::directory::{AccountDirectory, Authentication, DirectoryError, DirectoryFuture};
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
    clock: Arc<dyn Clock>,
    lockout_attempts: u32,
    lockout_window: SignedDuration,
    accounts: Mutex<BTreeMap<String, Account>>,
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

    /// A snapshot of the account `user` names, if there is one.
    fn by_id(&self, user: &UserId) -> Option<Account> {
        self.accounts()
            .values()
            .find(|held| &held.user_id == user)
            .cloned()
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
    fn reserve(&self, key: Key<'_>) -> Reservation {
        let now = self.clock.now();
        let window = self.lockout_window;
        let mut accounts = self.accounts();
        let held = match key {
            Key::Email(email) => accounts.get_mut(email),
            Key::User(user) => accounts.values_mut().find(|held| &held.user_id == user),
        };
        let Some(held) = held else {
            return Reservation::NoSuchAccount;
        };
        if held.locked(now, self.lockout_attempts, window) {
            tracing::debug!(user = %held.user_id, "a credential presentation met a lockout");
            return Reservation::Locked;
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
            email: held.email.clone(),
            user_id: held.user_id.clone(),
            stored: held.stored.clone(),
        }
    }

    /// Settle a reservation whose password matched: the run of failures is over.
    fn settle_success(&self, email: &str) {
        if let Some(held) = self.accounts().get_mut(email) {
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
        let (email, user_id, stored) = match reservation {
            Reservation::NoSuchAccount => {
                // The timing-equalized miss. Refusing here without doing the work would leak the
                // difference between an unknown address and a wrong password in the response
                // time, whatever the body said.
                self.credentials.absorb_miss(password).await;
                return Ok(Authentication::Refused);
            }
            Reservation::Locked => {
                // Still absorbed: a locked account that returned instantly would tell an
                // attacker which addresses they have already spent attempts on.
                self.credentials.absorb_miss(password).await;
                return Ok(Authentication::Locked);
            }
            Reservation::Reserved {
                email,
                user_id,
                stored,
            } => (email, user_id, stored),
        };
        let granted = self
            .credentials
            .verify(password, &stored)
            .await
            .map_err(unavailable)?;
        if granted {
            self.settle_success(&email);
            Ok(Authentication::Granted(user_id))
        } else {
            Ok(Authentication::Refused)
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
    /// No account answers to the key.
    NoSuchAccount,
    /// The account is refusing attempts; nothing was charged.
    Locked,
    /// One failure was charged, and this is what to verify against.
    Reserved {
        email: String,
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
                    last_failure_at: None,
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
            let stored = self.credentials.hash(password).await.map_err(unavailable)?;
            let mut accounts = self.accounts();
            let Some(held) = accounts.values_mut().find(|held| &held.user_id == user) else {
                return Ok(PasswordChanged::NoSuchAccount);
            };
            held.stored = stored;
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
mod tests {
    use std::sync::Arc;

    use jiff::{SignedDuration, Timestamp};

    use super::{Credentials, InMemoryAccounts, MAX_FAILED_ATTEMPTS};
    use crate::auth::directory::{AccountDirectory, Authentication};
    use crate::auth::profile::{AccountProfiles, PasswordChange, PasswordChanged, ProfileUpdate};
    use crate::auth::registry::{AccountRegistry, Registration};
    use crate::store::UserId;
    use crate::store::memory::ManualClock;

    const EMAIL: &str = "somebody@example.test";
    const PASSWORD: &str = "correct horse battery staple";

    fn user() -> UserId {
        UserId::new("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e6f")
    }

    /// A fifteen-minute lockout window, as a deployment gets by default.
    const WINDOW: SignedDuration = SignedDuration::from_mins(15);

    /// A directory with one registered account, over a clock the test drives.
    async fn seeded_on(clock: Arc<ManualClock>) -> InMemoryAccounts {
        let accounts = InMemoryAccounts::new(
            Credentials::new().expect("the platform hashes"),
            clock,
            MAX_FAILED_ATTEMPTS,
            WINDOW,
        );
        assert_eq!(
            accounts
                .create(EMAIL, PASSWORD, &user(), Timestamp::UNIX_EPOCH)
                .await
                .expect("it writes"),
            Registration::Created(user())
        );
        accounts
    }

    /// The same, for a case with nothing to say about time.
    async fn seeded() -> InMemoryAccounts {
        seeded_on(Arc::new(ManualClock::default())).await
    }

    /// Present a wrong password `times` times.
    async fn fail(accounts: &InMemoryAccounts, times: u32) {
        for _ in 0..times {
            let _ = accounts.authenticate(EMAIL, "wrong").await;
        }
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
        let held = accounts.accounts().get(EMAIL).cloned().expect("it is held");
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
    async fn a_lockout_decays_because_nothing_else_can_clear_it() {
        // There is no unlock operation on any surface, and `login`, `reauthenticate` and
        // `password` all refuse on `Locked` before they verify anything — so without a decay a
        // lockout is a permanently lost account rather than a throttle.
        let clock = Arc::new(ManualClock::default());
        let accounts = seeded_on(clock.clone()).await;
        fail(&accounts, MAX_FAILED_ATTEMPTS).await;
        assert_eq!(
            accounts
                .authenticate(EMAIL, PASSWORD)
                .await
                .expect("it answers"),
            Authentication::Locked
        );

        // One second short of the window: still locked. The boundary is asserted because an
        // off-by-one here is a lockout that never engages.
        clock.advance(WINDOW - SignedDuration::from_secs(1));
        assert_eq!(
            accounts
                .authenticate(EMAIL, PASSWORD)
                .await
                .expect("it answers"),
            Authentication::Locked
        );

        clock.advance(SignedDuration::from_secs(1));
        assert_eq!(
            accounts
                .authenticate(EMAIL, PASSWORD)
                .await
                .expect("it answers"),
            Authentication::Granted(user())
        );
    }

    #[tokio::test]
    async fn attempts_during_a_lockout_do_not_extend_it() {
        // Deliberate, and the direction is not obvious. Extending the window on every attempt
        // would keep a live guessing run permanently locked out — and would hand anybody who can
        // reach the endpoint a way to keep *somebody else's* account locked forever by hammering
        // it, which is a denial of service on an account rather than a defence of it. So the
        // window runs from the last **counted** failure, and an attempt made while locked is
        // refused without being counted.
        //
        // What that costs is bounded and small: a run gets `MAX_FAILED_ATTEMPTS` guesses per
        // window and no more, which is four a minute at the default. What bounds an attacker
        // across *many* accounts is a rate limiter, and the counter port that would carry one
        // has no trusted client address to key on (`registry`, the disclosure section).
        let clock = Arc::new(ManualClock::default());
        let accounts = seeded_on(clock.clone()).await;
        fail(&accounts, MAX_FAILED_ATTEMPTS).await;
        for _ in 0..3 {
            clock.advance(SignedDuration::from_mins(1));
            fail(&accounts, 1).await;
        }
        // Still inside the window measured from the tenth *counted* failure: locked.
        assert_eq!(
            accounts
                .authenticate(EMAIL, PASSWORD)
                .await
                .expect("it answers"),
            Authentication::Locked
        );
        // Past it: open, and the hammering did not move the deadline.
        clock.advance(WINDOW);
        assert_eq!(
            accounts
                .authenticate(EMAIL, PASSWORD)
                .await
                .expect("it answers"),
            Authentication::Granted(user())
        );
    }

    #[tokio::test]
    async fn failures_spread_wider_than_the_window_never_accumulate() {
        // Ten mistypes over a year is not a guessing run, and counting them as one would lock an
        // account on a tenth attempt made months after the ninth.
        let clock = Arc::new(ManualClock::default());
        let accounts = seeded_on(clock.clone()).await;
        for _ in 0..MAX_FAILED_ATTEMPTS * 2 {
            fail(&accounts, 1).await;
            clock.advance(WINDOW + SignedDuration::from_secs(1));
        }
        assert_eq!(
            accounts
                .authenticate(EMAIL, PASSWORD)
                .await
                .expect("it answers"),
            Authentication::Granted(user())
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
