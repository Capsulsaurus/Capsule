//! [`PostgresAccounts`] — the durable account store (`S-C53`, `S-C54`, #402).
//!
//! # Four ports, one table, one adapter
//!
//! [`AccountRegistry`], [`AccountDirectory`], [`AccountProfiles`] and [`PasswordChange`] are four
//! ports because they answer four questions with four disclosure contracts — registration has to
//! say whether an address is taken, authentication must not — and their module docs argue that at
//! length. None of that makes them four *stores*: one row holds every fact all four read, and
//! splitting it across four tables would put the lockout counter somewhere the password change
//! that must clear it cannot reach in the same statement.
//!
//! # Where this adapter is stricter than the in-memory one, and why
//!
//! [`accounts_memory`](super::accounts_memory) computes its hash outside its mutex and takes a
//! read-modify-write gap in the failed-attempt counter as a result, recording its own docs that
//! *"that is the right trade for a development adapter and it is not the trade a Postgres
//! adapter should make: there the increment is one statement"*. It is one statement here. The
//! decay, the reset-on-a-stale-run and the increment are a single `UPDATE … SET failures = CASE
//! …`, so two simultaneous wrong passwords are two counted failures rather than one.
//!
//! Argon2id still runs outside every statement. It costs tens of milliseconds, and a verification
//! held inside a transaction is a row lock held for the length of the slowest primitive in the
//! process.
//!
//! # The clock is injected, and never `now()`
//!
//! The lockout window is measured against [`Clock`], not against the database's `now()`. Two
//! reasons: the conformance suite has to be able to move time without sleeping for fifteen
//! minutes, and a server and its database disagreeing about the hour should not change who is
//! locked out.
//!
//! # Addresses are compared verbatim
//!
//! `email` carries a plain unique index and no `lower()`, no `citext`, no folded companion
//! column. Case folding is a normalization policy no port describes, and the conformance suite
//! asserts the consequence — `addresses_are_compared_verbatim` — precisely because a `citext`
//! column is the kind of thing that changes an identity decision without anybody making one.

use std::sync::Arc;

use jiff::{SignedDuration, Timestamp};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, Value};

use super::credential::{CredentialError, Credentials};
use super::directory::{AccountDirectory, Authentication, DirectoryError, DirectoryFuture};
use super::profile::{
    AccountProfiles, PasswordChange, PasswordChanged, ProfileRecord, ProfileUpdate,
};
use super::registry::{AccountRegistry, Registration};
use crate::postgres::error::Port;
use crate::postgres::time::{from_micros, to_micros};
use crate::store::{Clock, StoreError, UserId};

/// Which port is speaking, for every error this adapter raises.
const PORT: Port = Port {
    store: "accounts",
    record: "Account",
};

/// The durable account store.
#[derive(Clone)]
pub struct PostgresAccounts {
    connection: DatabaseConnection,
    credentials: Credentials,
    clock: Arc<dyn Clock>,
    lockout_attempts: u32,
    lockout_window: SignedDuration,
}

impl std::fmt::Debug for PostgresAccounts {
    /// Names the policy and never the state, for the reason [`Credentials`] does: the state is a
    /// pool holding a URL with a password in it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresAccounts")
            .field("lockout_attempts", &self.lockout_attempts)
            .field("lockout_window", &self.lockout_window)
            .finish_non_exhaustive()
    }
}

impl PostgresAccounts {
    /// An account store over `connection`, locking an account out for `lockout_window` after
    /// `lockout_attempts` consecutive failures.
    ///
    /// The verifier is passed in rather than constructed here because building one costs an
    /// Argon2id hash (the timing-equalized miss's decoy), and a composition root that builds
    /// several adapters should pay that once.
    pub fn new(
        connection: DatabaseConnection,
        credentials: Credentials,
        clock: Arc<dyn Clock>,
        lockout_attempts: u32,
        lockout_window: SignedDuration,
    ) -> Self {
        Self {
            connection,
            credentials,
            clock,
            lockout_attempts,
            lockout_window,
        }
    }

    /// The account `column` = `key` names, if there is one.
    ///
    /// `column` is a `&'static str` chosen from two literals below and never from a caller, so
    /// this is not a string-built query in the sense that matters — there is no input path to it.
    /// The alternative, two nearly identical statements, is the same query twice with a
    /// different `WHERE`, and the second copy is where a lockout column eventually gets left out.
    async fn lookup(&self, column: &'static str, key: &str) -> Result<Option<Held>, StoreError> {
        let found = self
            .connection
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!(
                    "SELECT user_id, credential, failures, last_failure_at \
                     FROM accounts WHERE {column} = $1"
                ),
                [Value::from(key.to_owned())],
            ))
            .await
            .map_err(PORT.failing("looking an account up"))?;
        let Some(found) = found else { return Ok(None) };

        let failed = PORT.failing("reading an account row");
        let user_id: String = found.try_get("", "user_id").map_err(&failed)?;
        let stored: String = found.try_get("", "credential").map_err(&failed)?;
        let failures: i64 = found.try_get("", "failures").map_err(&failed)?;
        let last_failure_at: Option<i64> = found.try_get("", "last_failure_at").map_err(&failed)?;
        Ok(Some(Held {
            user_id: UserId::new(user_id),
            stored,
            failures: u32::try_from(failures)
                .map_err(|_| PORT.undecodable(format!("{failures} is not a failure count")))?,
            last_failure_at: last_failure_at
                .map(|micros| {
                    from_micros(micros).ok_or_else(|| {
                        PORT.undecodable(format!("{micros}µs is not a representable instant"))
                    })
                })
                .transpose()?,
        }))
    }

    /// Whether enough *recent* failures have accumulated to refuse a correct password.
    ///
    /// Recent is the operative word, and the decay is not a courtesy: nothing in this server can
    /// clear a lockout otherwise. `login`, `reauthenticate` and `password` each ask the directory
    /// first and refuse on `Locked` before verifying anything, there is no unlock operation on
    /// any surface, and no operator command reaches this row — so a permanent lockout is a
    /// permanently lost account.
    fn locked(&self, held: &Held, now: Timestamp) -> bool {
        held.failures >= self.lockout_attempts
            && held
                .last_failure_at
                .is_some_and(|at| now.duration_since(at) < self.lockout_window)
    }

    /// Record the outcome of a credential presentation, in one statement.
    ///
    /// The `CASE` is the decay: a failure whose predecessor is older than the window starts a
    /// fresh run at 1 rather than tipping a stale count over, so ten mistypes spread over a year
    /// never lock an account. Done in SQL rather than by reading, deciding and writing, because
    /// the read-decide-write is exactly the gap the in-memory adapter documents as the price of
    /// being a development double.
    async fn record(&self, user: &UserId, granted: bool, now: Timestamp) -> Result<(), StoreError> {
        let statement = if granted {
            Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE accounts SET failures = 0, last_failure_at = NULL WHERE user_id = $1",
                [Value::from(user.as_str().to_owned())],
            )
        } else {
            Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE accounts SET \
                   failures = CASE \
                     WHEN last_failure_at IS NOT NULL AND $2 - last_failure_at >= $3 THEN 1 \
                     ELSE failures + 1 END, \
                   last_failure_at = $2 \
                 WHERE user_id = $1",
                [
                    Value::from(user.as_str().to_owned()),
                    Value::from(to_micros(now)),
                    Value::from(self.lockout_window.as_micros() as i64),
                ],
            )
        };
        self.connection
            .execute(statement)
            .await
            .map_err(PORT.failing("recording a credential presentation"))?;
        Ok(())
    }

    /// Decide `password` against a looked-up account, and record what happened.
    ///
    /// The shared body of the two [`AccountDirectory`] methods: they differ only in how they find
    /// the account, and that is exactly the difference the port wants them to have.
    async fn decide(
        &self,
        held: Option<Held>,
        password: &str,
    ) -> Result<Authentication, DirectoryError> {
        let now = self.clock.now();
        let Some(held) = held else {
            // The timing-equalized miss. Refusing here without doing the work would leak the
            // difference between an unknown address and a wrong password in the response time,
            // whatever the body said.
            self.credentials.absorb_miss(password);
            return Ok(Authentication::Refused);
        };
        if self.locked(&held, now) {
            // Still absorbed: a locked account that returned instantly would tell an attacker
            // which addresses they have already spent attempts on. Deliberately **not**
            // recorded — an attempt made while locked must not extend the window, or anybody
            // who can reach the endpoint can keep somebody else's account locked forever.
            self.credentials.absorb_miss(password);
            return Ok(Authentication::Locked);
        }
        let granted = self
            .credentials
            .verify(password, &held.stored)
            .map_err(unavailable)?;
        self.record(&held.user_id, granted, now)
            .await
            .map_err(store_unavailable)?;
        if granted {
            Ok(Authentication::Granted(held.user_id))
        } else {
            Ok(Authentication::Refused)
        }
    }

    /// The profile of `user`, read through whatever connection the caller has.
    async fn profile(&self, user: &UserId) -> Result<Option<ProfileRecord>, StoreError> {
        let found = self
            .connection
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT user_id, email, display_name, created_at FROM accounts \
                 WHERE user_id = $1",
                [Value::from(user.as_str().to_owned())],
            ))
            .await
            .map_err(PORT.failing("reading a profile"))?;
        found.as_ref().map(profile_from).transpose()
    }
}

/// The columns an authentication decision reads.
#[derive(Debug)]
struct Held {
    user_id: UserId,
    /// The Argon2id PHC string. Never a password, and never read above this adapter.
    stored: String,
    failures: u32,
    last_failure_at: Option<Timestamp>,
}

/// Read one profile projection back.
fn profile_from(row: &sea_orm::QueryResult) -> Result<ProfileRecord, StoreError> {
    let failed = PORT.failing("reading a profile row");
    let user_id: String = row.try_get("", "user_id").map_err(&failed)?;
    let email: String = row.try_get("", "email").map_err(&failed)?;
    let display_name: Option<String> = row.try_get("", "display_name").map_err(&failed)?;
    let created_at: i64 = row.try_get("", "created_at").map_err(&failed)?;
    Ok(ProfileRecord {
        user_id: UserId::new(user_id),
        email,
        display_name,
        created_at: from_micros(created_at).ok_or_else(|| {
            PORT.undecodable(format!("{created_at}µs is not a representable instant"))
        })?,
    })
}

/// A credential fault is a directory fault: no decision was reached.
fn unavailable(error: CredentialError) -> DirectoryError {
    tracing::error!(%error, "a stored credential could not be processed");
    DirectoryError::Unavailable {
        detail: error.to_string(),
    }
}

/// A store fault is a directory fault, for the same reason.
///
/// [`DirectoryError`] has exactly one variant, and that is the port's decision rather than a
/// simplification here: *"whether the backend was down or merely angry changes nothing about the
/// response"*. The [`StoreError`] distinction is preserved in the log line the mapping already
/// emitted, which is where an operator reads it.
fn store_unavailable(error: StoreError) -> DirectoryError {
    DirectoryError::Unavailable {
        detail: error.to_string(),
    }
}

impl AccountDirectory for PostgresAccounts {
    fn authenticate<'a>(
        &'a self,
        email: &'a str,
        password: &'a str,
    ) -> DirectoryFuture<'a, Authentication> {
        Box::pin(async move {
            let held = self
                .lookup("email", email)
                .await
                .map_err(store_unavailable)?;
            self.decide(held, password).await
        })
    }

    fn authenticate_user<'a>(
        &'a self,
        user: &'a UserId,
        password: &'a str,
    ) -> DirectoryFuture<'a, Authentication> {
        Box::pin(async move {
            let held = self
                .lookup("user_id", user.as_str())
                .await
                .map_err(store_unavailable)?;
            self.decide(held, password).await
        })
    }
}

impl AccountRegistry for PostgresAccounts {
    fn create<'a>(
        &'a self,
        email: &'a str,
        password: &'a str,
        user: &'a UserId,
        at: Timestamp,
    ) -> DirectoryFuture<'a, Registration> {
        Box::pin(async move {
            // Hashed before the statement, so nothing holds a row while Argon2id runs. The cost
            // of that ordering is a hash computed for an address that turns out to be taken,
            // which is the cheap direction to be wrong in.
            let stored = self.credentials.hash(password).map_err(unavailable)?;
            // `ON CONFLICT DO NOTHING` on the unique index, so `AlreadyExists` is decided by the
            // index and never by a read followed by a write: two registrations racing on one
            // address must not both believe they own it.
            let inserted = self
                .connection
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "INSERT INTO accounts \
                       (user_id, email, display_name, credential, failures, created_at, updated_at) \
                     VALUES ($1, $2, NULL, $3, 0, $4, $4) \
                     ON CONFLICT (email) DO NOTHING",
                    [
                        Value::from(user.as_str().to_owned()),
                        Value::from(email.to_owned()),
                        Value::from(stored),
                        Value::from(to_micros(at)),
                    ],
                ))
                .await
                .map_err(PORT.failing("creating an account"))
                .map_err(store_unavailable)?;
            if inserted.rows_affected() == 0 {
                return Ok(Registration::AlreadyExists);
            }
            tracing::info!(%user, "an account was created");
            Ok(Registration::Created(user.clone()))
        })
    }
}

impl AccountProfiles for PostgresAccounts {
    fn read<'a>(&'a self, user: &'a UserId) -> DirectoryFuture<'a, Option<ProfileRecord>> {
        Box::pin(async move { self.profile(user).await.map_err(store_unavailable) })
    }

    fn update<'a>(
        &'a self,
        user: &'a UserId,
        update: &'a ProfileUpdate,
    ) -> DirectoryFuture<'a, Option<ProfileRecord>> {
        Box::pin(async move {
            // An empty update is a no-op the route answers with the current profile, and the
            // port says so — which is why it is a read here rather than an `UPDATE` that sets
            // every column to itself.
            let Some(display_name) = update.display_name.clone() else {
                return self.profile(user).await.map_err(store_unavailable);
            };
            // One statement, as the port requires: a caller that read, changed a field and wrote
            // the whole record back would clobber a concurrent edit from another device.
            let updated = self
                .connection
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "UPDATE accounts SET display_name = $2, updated_at = $3 WHERE user_id = $1 \
                     RETURNING user_id, email, display_name, created_at",
                    [
                        Value::from(user.as_str().to_owned()),
                        Value::from(display_name),
                        Value::from(to_micros(self.clock.now())),
                    ],
                ))
                .await
                .map_err(PORT.failing("updating a profile"))
                .map_err(store_unavailable)?;
            updated
                .as_ref()
                .map(profile_from)
                .transpose()
                .map_err(store_unavailable)
        })
    }
}

impl PasswordChange for PostgresAccounts {
    fn set_password<'a>(
        &'a self,
        user: &'a UserId,
        password: &'a str,
        at: Timestamp,
    ) -> DirectoryFuture<'a, PasswordChanged> {
        Box::pin(async move {
            let stored = self.credentials.hash(password).map_err(unavailable)?;
            // The lockout is cleared in the same statement, because the port requires it: a
            // change is a successful credential presentation, and leaving the failure count
            // behind would bar somebody from an account they just proved they own.
            let changed = self
                .connection
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "UPDATE accounts \
                     SET credential = $2, failures = 0, last_failure_at = NULL, updated_at = $3 \
                     WHERE user_id = $1",
                    [
                        Value::from(user.as_str().to_owned()),
                        Value::from(stored),
                        Value::from(to_micros(at)),
                    ],
                ))
                .await
                .map_err(PORT.failing("replacing a password"))
                .map_err(store_unavailable)?;
            if changed.rows_affected() == 0 {
                return Ok(PasswordChanged::NoSuchAccount);
            }
            tracing::info!(%user, "an account's password was replaced");
            Ok(PasswordChanged::Yes)
        })
    }
}

#[cfg(test)]
mod tests {
    /// The suite, against a real Postgres.
    mod postgres_conformance {
        use std::sync::Arc;

        use jiff::SignedDuration;

        use super::super::PostgresAccounts;
        use crate::auth::accounts_memory::MAX_FAILED_ATTEMPTS;
        use crate::auth::conformance::{self, Harness};
        use crate::auth::credential::Credentials;
        use crate::auth::directory::AccountDirectory;
        use crate::auth::profile::{AccountProfiles, PasswordChange};
        use crate::auth::registry::AccountRegistry;
        use crate::postgres::testing;
        use crate::store::StoreFuture;
        use crate::store::memory::ManualClock;

        /// A fifteen-minute lockout window, as a deployment gets by default.
        const WINDOW: SignedDuration = SignedDuration::from_mins(15);

        /// One adapter over one container, on a clock the suite drives.
        ///
        /// The clock is the whole reason the harness has an `advance`: the lockout cases assert a
        /// fifteen-minute decay from both sides of its boundary, and a container-backed suite
        /// that waited for it would take half an hour.
        #[derive(Debug)]
        struct PostgresHarness {
            accounts: PostgresAccounts,
            clock: Arc<ManualClock>,
        }

        impl Harness for PostgresHarness {
            fn registry(&self) -> &dyn AccountRegistry {
                &self.accounts
            }

            fn directory(&self) -> &dyn AccountDirectory {
                &self.accounts
            }

            fn profiles(&self) -> &dyn AccountProfiles {
                &self.accounts
            }

            fn passwords(&self) -> &dyn PasswordChange {
                &self.accounts
            }

            fn lockout_attempts(&self) -> u32 {
                MAX_FAILED_ATTEMPTS
            }

            fn lockout_window(&self) -> SignedDuration {
                WINDOW
            }

            fn advance(&self, by: SignedDuration) -> StoreFuture<'_, ()> {
                Box::pin(async move {
                    self.clock.advance(by);
                    Ok(())
                })
            }
        }

        #[tokio::test]
        async fn the_postgres_account_store_conforms() {
            let Some(database) = testing::start("the Postgres account store").await else {
                return;
            };
            let clock = Arc::new(ManualClock::default());
            let harness = PostgresHarness {
                accounts: PostgresAccounts::new(
                    database.connection().clone(),
                    Credentials::new().expect("the platform hashes"),
                    clock.clone(),
                    MAX_FAILED_ATTEMPTS,
                    WINDOW,
                ),
                clock,
            };
            conformance::run_all(&harness).await;
        }
    }
}
