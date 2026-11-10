//! The PostgreSQL adapters for the state ports (ADR-0015).
//!
//! # Expiry is a column, and a read decides it
//!
//! Every table here (migration `000031`) carries an `expires_at` of epoch microseconds, written
//! from the [`Clock`] the adapter was built with. Every read binds that clock's `now` and treats
//! a row with `expires_at <= now` as absent, exactly as the in-memory double's `is_live_at`
//! does, so the conformance suite's manual clock drives this adapter the same way it drives the
//! double. Nothing here deletes a row for being old except [`super::sweep`]; a dead row a read
//! skipped is garbage the sweeper collects, never state a caller can observe.
//!
//! This is not the generic TTL store ADR-0006 refused: each port keeps its own typed table and
//! its own operations, and no caller supplies a lifetime per call.
//!
//! # Atomicity
//!
//! Each port operation is one statement wherever one statement can state it (`DELETE …
//! RETURNING` for every consume-once read, `INSERT … ON CONFLICT` for every write that replaces).
//! The operations that must read before they write — merging a committed range, claiming a
//! device poll slot, appending to a relay mailbox — run in one transaction that locks the row
//! (`FOR UPDATE` or `FOR SHARE`) first, so two racing callers serialize on it rather than both
//! reading the same old value.

mod auth;
mod ceremony;
mod upload;

use std::sync::Arc;

use jiff::{SignedDuration, Timestamp};
use sea_orm::{DatabaseConnection, DbBackend, QueryResult, Statement, TryGetable, Value};

pub use self::auth::PostgresAuthState;
pub use self::ceremony::{
    PostgresChallenges, PostgresChannels, PostgresEnrollments, PostgresOidcAuthorizations,
    PostgresOidcDevices,
};
pub use self::upload::PostgresUploadSessions;
use super::auth::{DEFAULT_SESSION_HARD_CAP, DEFAULT_SESSION_TTL};
use super::ceremony::{
    CHALLENGE_TTL, ENROLLMENT_CODE_TTL, OIDC_AUTHORIZATION_TTL, OIDC_DEVICE_TTL, RELAY_CHANNEL_TTL,
};
use super::upload::LIFETIME_CAP;
use super::{Clock, StoreError};
use crate::postgres::error::Port;
use crate::postgres::time::from_micros;

/// A Postgres statement with positional `$n` binds.
fn statement(sql: &str, values: impl IntoIterator<Item = Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

/// Read column `name` of `row`, as the record `port` owns.
fn column<T: TryGetable>(port: Port, row: &QueryResult, name: &str) -> Result<T, StoreError> {
    row.try_get("", name)
        .map_err(|error| port.undecodable(format!("column `{name}`: {error}")))
}

/// Read the microsecond instant in column `name` of `row`.
fn instant(port: Port, row: &QueryResult, name: &str) -> Result<Timestamp, StoreError> {
    let micros: i64 = column(port, row, name)?;
    from_micros(micros).ok_or_else(|| {
        port.undecodable(format!(
            "`{name}` = {micros}µs is not a representable instant"
        ))
    })
}

/// A duration as whole microseconds, saturating at the column's range.
fn duration_micros(span: SignedDuration) -> i64 {
    i64::try_from(span.as_micros()).unwrap_or(if span.is_negative() {
        i64::MIN
    } else {
        i64::MAX
    })
}

/// Every Postgres state store on one connection pool and one clock.
///
/// What [`crate::boot`]'s durable arm binds, and what the conformance harness drives. The
/// lifetimes are the ports' production defaults unless built with
/// [`Self::with_uniform_ttl`].
#[derive(Debug, Clone)]
pub struct PostgresStores {
    auth: Arc<PostgresAuthState>,
    uploads: Arc<PostgresUploadSessions>,
    challenges: Arc<PostgresChallenges>,
    enrollments: Arc<PostgresEnrollments>,
    channels: Arc<PostgresChannels>,
    oidc_authorizations: Arc<PostgresOidcAuthorizations>,
    oidc_devices: Arc<PostgresOidcDevices>,
}

impl PostgresStores {
    /// Every store over `connection` on `clock`, with each port's default lifetime and the
    /// session lifetimes given.
    pub fn new(
        connection: DatabaseConnection,
        clock: Arc<dyn Clock>,
        session_ttl: SignedDuration,
        session_hard_cap: SignedDuration,
    ) -> Self {
        Self::build(
            connection,
            clock,
            |default| default,
            session_ttl,
            session_hard_cap,
        )
    }

    /// Every store over `connection` on `clock` with the production defaults.
    pub fn with_default_ttls(connection: DatabaseConnection, clock: Arc<dyn Clock>) -> Self {
        Self::new(
            connection,
            clock,
            DEFAULT_SESSION_TTL,
            DEFAULT_SESSION_HARD_CAP,
        )
    }

    /// Every store with one lifetime, `ttl`, for the conformance suite — the same shape as
    /// `InMemoryStores::with_uniform_ttl`. The session hard cap stays the default.
    pub fn with_uniform_ttl(
        connection: DatabaseConnection,
        clock: Arc<dyn Clock>,
        ttl: SignedDuration,
    ) -> Self {
        Self::build(connection, clock, |_| ttl, ttl, DEFAULT_SESSION_HARD_CAP)
    }

    fn build(
        connection: DatabaseConnection,
        clock: Arc<dyn Clock>,
        lifetime: impl Fn(SignedDuration) -> SignedDuration,
        session_ttl: SignedDuration,
        session_hard_cap: SignedDuration,
    ) -> Self {
        Self {
            auth: Arc::new(
                PostgresAuthState::new(connection.clone(), Arc::clone(&clock), session_ttl)
                    .with_hard_cap(session_hard_cap),
            ),
            uploads: Arc::new(PostgresUploadSessions::new(
                connection.clone(),
                Arc::clone(&clock),
                lifetime(LIFETIME_CAP),
            )),
            challenges: Arc::new(PostgresChallenges::new(
                connection.clone(),
                Arc::clone(&clock),
                lifetime(CHALLENGE_TTL),
            )),
            enrollments: Arc::new(PostgresEnrollments::new(
                connection.clone(),
                Arc::clone(&clock),
                lifetime(ENROLLMENT_CODE_TTL),
            )),
            channels: Arc::new(PostgresChannels::new(
                connection.clone(),
                Arc::clone(&clock),
                lifetime(RELAY_CHANNEL_TTL),
            )),
            oidc_authorizations: Arc::new(PostgresOidcAuthorizations::new(
                connection.clone(),
                Arc::clone(&clock),
                lifetime(OIDC_AUTHORIZATION_TTL),
            )),
            oidc_devices: Arc::new(PostgresOidcDevices::new(
                connection,
                clock,
                lifetime(OIDC_DEVICE_TTL),
            )),
        }
    }

    /// The session store.
    pub fn auth(&self) -> Arc<PostgresAuthState> {
        Arc::clone(&self.auth)
    }

    /// The upload-session store.
    pub fn uploads(&self) -> Arc<PostgresUploadSessions> {
        Arc::clone(&self.uploads)
    }

    /// The revoke-all challenge store.
    pub fn challenges(&self) -> Arc<PostgresChallenges> {
        Arc::clone(&self.challenges)
    }

    /// The device-enrollment store.
    pub fn enrollments(&self) -> Arc<PostgresEnrollments> {
        Arc::clone(&self.enrollments)
    }

    /// The enrollment relay-channel store.
    pub fn channels(&self) -> Arc<PostgresChannels> {
        Arc::clone(&self.channels)
    }

    /// The pending OIDC authorization store.
    pub fn oidc_authorizations(&self) -> Arc<PostgresOidcAuthorizations> {
        Arc::clone(&self.oidc_authorizations)
    }

    /// The pending OIDC device authorization store.
    pub fn oidc_devices(&self) -> Arc<PostgresOidcDevices> {
        Arc::clone(&self.oidc_devices)
    }
}

/// Borrowing accessors for the conformance harness, which hands out `&dyn` stores.
#[cfg(test)]
impl PostgresStores {
    fn auth_store(&self) -> &PostgresAuthState {
        &self.auth
    }

    fn upload_store(&self) -> &PostgresUploadSessions {
        &self.uploads
    }

    fn challenge_store(&self) -> &PostgresChallenges {
        &self.challenges
    }

    fn enrollment_store(&self) -> &PostgresEnrollments {
        &self.enrollments
    }

    fn channel_store(&self) -> &PostgresChannels {
        &self.channels
    }

    fn oidc_authorization_store(&self) -> &PostgresOidcAuthorizations {
        &self.oidc_authorizations
    }

    fn oidc_device_store(&self) -> &PostgresOidcDevices {
        &self.oidc_devices
    }
}

#[cfg(test)]
mod tests;
