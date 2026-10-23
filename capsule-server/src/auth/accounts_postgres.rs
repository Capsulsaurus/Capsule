//! [`PostgresAccounts`] — the durable account store (#402).
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
//! [`FederatedAccounts`] is the fifth port on the same row (#460). An account an OpenID Connect
//! sign-in created is a row with a **null credential**, the provider's verified address if it
//! asserted one, and the `(oidc_issuer, oidc_subject)` link — so the `409` for an address a
//! password account holds is the address index's answer, the profile is the same row's, and no
//! credential presentation can reach a password that does not exist: every statement that
//! verifies or replaces a credential is filtered on `credential IS NOT NULL`.
//!
//! # The attempt is reserved before it is verified
//!
//! Reading the failure count, verifying, and then recording the failure is a race: every guess
//! that reads the row before any of them writes it is admitted, so a burst of parallel guesses
//! walks straight past the ceiling (#510). Here the lockout check and the failure charge are one
//! conditional statement — `UPDATE … SET failures = CASE … WHERE <key> AND NOT <locked>
//! RETURNING credential` — and only an attempt that statement charged is verified. Postgres takes
//! the row lock for the `UPDATE` and, under `READ COMMITTED`, re-evaluates the `WHERE` against
//! the newest row version once a concurrent writer commits, so racing reservations serialize on
//! the row and the `failures` they see is exact: at most `lockout_attempts` of them inside a
//! window are ever charged and verified. A success then settles the charge by clearing the run.
//! The in-memory adapter does the same with one critical section.
//!
//! The charge is never refunded on a refusal or a dropped request: a refusal is what it already
//! says, and a request cancelled mid-verification must not be a free guess.
//!
//! Argon2id still runs outside every statement, and off the async runtime. It costs tens of
//! milliseconds, and a verification held inside a transaction is a row lock held for the length
//! of the slowest primitive in the process.
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
use super::oidc::{FederatedAccounts, FederatedLink, VerifiedIdentity, reserved_address};
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

    /// Reserve one credential presentation against the account `column` = `key` names.
    ///
    /// One conditional statement: the lockout check and the failure charge cannot be separated
    /// by a concurrent guess (see the module docs). The `CASE` is the decay — a failure whose
    /// predecessor is older than the window starts a fresh run at 1 rather than tipping a stale
    /// count over, so ten mistypes spread over a year never lock an account. The `NOT (…)` is the
    /// lockout, measured from the last **counted** failure: a locked row is not charged, because
    /// an attempt made while locked must not extend the window, or anybody who can reach the
    /// endpoint can keep somebody else's account locked forever.
    ///
    /// Recent failures are the operative ones, and the decay is not a courtesy: nothing in this
    /// server can clear a lockout otherwise. `login`, `reauthenticate` and `password` each ask
    /// the directory first and refuse on `Locked` before verifying anything, there is no unlock
    /// operation on any surface, and no operator command reaches this row.
    ///
    /// A row whose credential is null — a federated account — is matched by neither statement,
    /// so it is answered as no account at all: never charged, never `Locked`, and never verified,
    /// because there is nothing to verify (#460).
    ///
    /// When the statement charges nothing, a second read tells a locked account from a missing
    /// one. That read is not part of the decision's atomicity — a row that unlocks or vanishes
    /// in between is answered as it was a moment ago, and either answer refuses.
    ///
    /// `column` is a `&'static str` chosen from two literals below and never from a caller, so
    /// this is not a string-built query in the sense that matters — there is no input path to it.
    /// The alternative, two nearly identical statements, is the same statement twice with a
    /// different `WHERE`, and the second copy is where a lockout clause eventually gets left out.
    async fn reserve(&self, column: &'static str, key: &str) -> Result<Reservation, StoreError> {
        let now = self.clock.now();
        let reserved = self
            .connection
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!(
                    "UPDATE accounts SET \
                       failures = CASE \
                         WHEN last_failure_at IS NOT NULL AND $2 - last_failure_at >= $3 THEN 1 \
                         ELSE failures + 1 END, \
                       last_failure_at = $2 \
                     WHERE {column} = $1 \
                       AND credential IS NOT NULL \
                       AND NOT (failures >= $4 \
                                AND last_failure_at IS NOT NULL \
                                AND $2 - last_failure_at < $3) \
                     RETURNING user_id, credential, failures"
                ),
                [
                    Value::from(key.to_owned()),
                    Value::from(to_micros(now)),
                    Value::from(self.lockout_window.as_micros() as i64),
                    Value::from(i64::from(self.lockout_attempts)),
                ],
            ))
            .await
            .map_err(PORT.failing("reserving a credential presentation"))?;

        if let Some(row) = reserved {
            let failed = PORT.failing("reading a reserved account row");
            let user_id: String = row.try_get("", "user_id").map_err(&failed)?;
            let stored: String = row.try_get("", "credential").map_err(&failed)?;
            let failures: i64 = row.try_get("", "failures").map_err(&failed)?;
            let user_id = UserId::new(user_id);
            tracing::trace!(user = %user_id, failures, "a credential presentation was reserved");
            if failures == i64::from(self.lockout_attempts) {
                tracing::warn!(
                    user = %user_id,
                    failures,
                    window = %self.lockout_window,
                    "an account reached the failed-attempt ceiling; further attempts are locked \
                     out unless this one succeeds"
                );
            }
            return Ok(Reservation::Reserved { user_id, stored });
        }

        let exists = self
            .connection
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!(
                    "SELECT user_id FROM accounts WHERE {column} = $1 AND credential IS NOT NULL"
                ),
                [Value::from(key.to_owned())],
            ))
            .await
            .map_err(PORT.failing("looking an account up"))?;
        Ok(match exists {
            Some(row) => {
                let user_id: String = row
                    .try_get("", "user_id")
                    .map_err(PORT.failing("reading an account row"))?;
                tracing::debug!(user = %user_id, "a credential presentation met a lockout");
                Reservation::Locked
            }
            None => Reservation::NoSuchAccount,
        })
    }

    /// Settle a reservation whose password matched: the run of failures is over.
    async fn settle_success(&self, user: &UserId) -> Result<(), StoreError> {
        self.connection
            .execute_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE accounts SET failures = 0, last_failure_at = NULL WHERE user_id = $1",
                [Value::from(user.as_str().to_owned())],
            ))
            .await
            .map_err(PORT.failing("recording a successful credential presentation"))?;
        Ok(())
    }

    /// Reserve an attempt, verify `password` if one was reserved, and settle.
    ///
    /// The shared body of the two [`AccountDirectory`] methods: they differ only in how they find
    /// the account, and that is exactly the difference the port wants them to have.
    async fn decide(
        &self,
        column: &'static str,
        key: &str,
        password: &str,
    ) -> Result<Authentication, DirectoryError> {
        let reservation = self.reserve(column, key).await.map_err(store_unavailable)?;
        let (user_id, stored) = match reservation {
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
            Reservation::Reserved { user_id, stored } => (user_id, stored),
        };
        let granted = self
            .credentials
            .verify(password, &stored)
            .await
            .map_err(unavailable)?;
        if granted {
            self.settle_success(&user_id)
                .await
                .map_err(store_unavailable)?;
            Ok(Authentication::Granted(user_id))
        } else {
            Ok(Authentication::Refused)
        }
    }

    /// The profile of `user`, read through whatever connection the caller has.
    ///
    /// Every row has one: a federated account whose provider asserted no verified address reads
    /// back with [`ProfileRecord::email`] `None` (the OIDC accounts module docs), and the
    /// in-memory adapter answers the same.
    async fn profile(&self, user: &UserId) -> Result<Option<ProfileRecord>, StoreError> {
        let found = self
            .connection
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT user_id, email, display_name, created_at FROM accounts \
                 WHERE user_id = $1",
                [Value::from(user.as_str().to_owned())],
            ))
            .await
            .map_err(PORT.failing("reading a profile"))?;
        found.as_ref().map(profile_from).transpose()
    }

    /// The account `(issuer, subject)` is linked to, if any.
    async fn linked(&self, issuer: &str, subject: &str) -> Result<Option<UserId>, StoreError> {
        let found = self
            .connection
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT user_id FROM accounts WHERE oidc_issuer = $1 AND oidc_subject = $2",
                [
                    Value::from(issuer.to_owned()),
                    Value::from(subject.to_owned()),
                ],
            ))
            .await
            .map_err(PORT.failing("looking a federated identity up"))?;
        found
            .map(|row| {
                row.try_get::<String>("", "user_id")
                    .map(UserId::new)
                    .map_err(PORT.failing("reading a federated account row"))
            })
            .transpose()
    }

    /// Whether any account holds `email`.
    async fn address_held(&self, email: &str) -> Result<bool, StoreError> {
        let found = self
            .connection
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT 1 AS held FROM accounts WHERE email = $1",
                [Value::from(email.to_owned())],
            ))
            .await
            .map_err(PORT.failing("looking an address up"))?;
        Ok(found.is_some())
    }
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
        user_id: UserId,
        /// The Argon2id PHC string. Never a password, and never read above this adapter.
        stored: String,
    },
}

/// Read one profile projection back.
fn profile_from(row: &sea_orm::QueryResult) -> Result<ProfileRecord, StoreError> {
    let failed = PORT.failing("reading a profile row");
    let user_id: String = row.try_get("", "user_id").map_err(&failed)?;
    let email: Option<String> = row.try_get("", "email").map_err(&failed)?;
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
        Box::pin(async move { self.decide("email", email, password).await })
    }

    fn authenticate_user<'a>(
        &'a self,
        user: &'a UserId,
        password: &'a str,
    ) -> DirectoryFuture<'a, Authentication> {
        Box::pin(async move { self.decide("user_id", user.as_str(), password).await })
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
            let stored = self.credentials.hash(password).await.map_err(unavailable)?;
            // `ON CONFLICT DO NOTHING` on the unique index, so `AlreadyExists` is decided by the
            // index and never by a read followed by a write: two registrations racing on one
            // address must not both believe they own it.
            let inserted = self
                .connection
                .execute_raw(Statement::from_sql_and_values(
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

impl FederatedAccounts for PostgresAccounts {
    fn resolve_or_create<'a>(
        &'a self,
        identity: &'a VerifiedIdentity,
        user: &'a UserId,
        at: Timestamp,
    ) -> DirectoryFuture<'a, FederatedLink> {
        Box::pin(async move {
            let address = reserved_address(identity).map(str::to_owned);
            // One statement: the lookup and the insert. The insert runs only when the lookup
            // found nothing, with a null credential and the link, and `ON CONFLICT DO NOTHING`
            // with no target defers to **both** unique indexes — the address and the link — so
            // neither "somebody else holds this verified address" nor "a second callback for the
            // same person got here first" can be decided by a read followed by a write.
            let row = self
                .connection
                .query_one_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "WITH existing AS ( \
                       SELECT user_id FROM accounts \
                       WHERE oidc_issuer = $1 AND oidc_subject = $2 \
                     ), inserted AS ( \
                       INSERT INTO accounts \
                         (user_id, email, display_name, credential, failures, created_at, \
                          updated_at, oidc_issuer, oidc_subject) \
                       SELECT $3, $4, NULL, NULL, 0, $5, $5, $1, $2 \
                       WHERE NOT EXISTS (SELECT 1 FROM existing) \
                       ON CONFLICT DO NOTHING \
                       RETURNING user_id \
                     ) \
                     SELECT 'linked' AS outcome, user_id FROM existing \
                     UNION ALL \
                     SELECT 'created' AS outcome, user_id FROM inserted",
                    [
                        Value::from(identity.issuer.clone()),
                        Value::from(identity.subject.clone()),
                        Value::from(user.as_str().to_owned()),
                        Value::String(address.clone()),
                        Value::from(to_micros(at)),
                    ],
                ))
                .await
                .map_err(PORT.failing("resolving a federated identity"))
                .map_err(store_unavailable)?;

            if let Some(row) = row {
                let failed = PORT.failing("reading a federated account row");
                let outcome: String = row
                    .try_get("", "outcome")
                    .map_err(&failed)
                    .map_err(store_unavailable)?;
                let user_id: String = row
                    .try_get("", "user_id")
                    .map_err(&failed)
                    .map_err(store_unavailable)?;
                let user_id = UserId::new(user_id);
                return Ok(if outcome == "created" {
                    tracing::info!(user = %user_id, issuer = %identity.issuer, "created an account for a federated identity");
                    FederatedLink::Created(user_id)
                } else {
                    tracing::debug!(user = %user_id, issuer = %identity.issuer, "a federated identity resolved to its account");
                    FederatedLink::Linked(user_id)
                });
            }

            // Nothing linked and nothing inserted: the insert met a unique index. Which one is
            // read back now, outside the decision — a link that appeared is a concurrent callback
            // for the same person, whose account this is; otherwise the verified address is held.
            if let Some(existing) = self
                .linked(&identity.issuer, &identity.subject)
                .await
                .map_err(store_unavailable)?
            {
                tracing::debug!(user = %existing, issuer = %identity.issuer, "a federated identity resolved to an account a concurrent sign-in created");
                return Ok(FederatedLink::Linked(existing));
            }
            if let Some(email) = &address
                && self.address_held(email).await.map_err(store_unavailable)?
            {
                tracing::info!(issuer = %identity.issuer, "a federated sign-in asserted an address another account holds");
                return Ok(FederatedLink::AddressTaken);
            }
            // The one index left is the primary key: a freshly minted id already held.
            tracing::error!(%user, "a freshly minted account id is already held");
            Err(DirectoryError::Unavailable {
                detail: "a freshly minted account id is already held".to_owned(),
            })
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
                .query_one_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "UPDATE accounts SET display_name = $2, updated_at = $3 \
                     WHERE user_id = $1 \
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
            let stored = self.credentials.hash(password).await.map_err(unavailable)?;
            // The lockout is cleared in the same statement, because the port requires it: a
            // change is a successful credential presentation, and leaving the failure count
            // behind would bar somebody from an account they just proved they own.
            //
            // `credential IS NOT NULL`: a federated account has no password to replace, and
            // gaining one here would be a password nobody chose at the provider (#460).
            let changed = self
                .connection
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "UPDATE accounts \
                     SET credential = $2, failures = 0, last_failure_at = NULL, updated_at = $3 \
                     WHERE user_id = $1 AND credential IS NOT NULL",
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
mod tests;
