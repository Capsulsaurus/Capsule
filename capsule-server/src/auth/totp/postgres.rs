//! [`PostgresTotp`] — the durable second-factor store (`S-C55`, #446).
//!
//! # One row per account, and every operation is one statement
//!
//! `totp_enrollments` holds an account's secret, whether a code has confirmed it, and the
//! highest step a code was accepted at. Each of the port's three check-and-writes is a single
//! statement whose row count decides the outcome, so none has a read-then-write window:
//!
//! - [`TotpStore::begin`] is an upsert whose `DO UPDATE` carries `WHERE state <> 'active'`. A
//!   conflicting active row is locked, left alone and reported as zero rows — `AlreadyActive` —
//!   and a pending one is replaced, which is the port's "without ceremony".
//! - [`TotpStore::activate`] is an `UPDATE … WHERE state = 'pending'`, so a second confirmation
//!   racing the first changes nothing and answers `NotPending`.
//! - [`TotpStore::consume`] is the compare-and-set RFC 6238 §5.2 needs: an `UPDATE … WHERE
//!   last_step IS NULL OR last_step < $step`, and in the same statement whether the account is
//!   enrolled at all, so `Replayed` and `NotEnrolled` are told apart without a second read. Two
//!   sign-ins racing on one code serialize on the row lock; the loser re-evaluates the `WHERE`
//!   against the winner's step and is a replay.
//!
//! # The secret never reaches a log line
//!
//! It is bound as a parameter and read back into a [`TotpSecret`], whose `Debug` prints nothing.
//! No statement here is logged with its values, and no error carries a row.

use jiff::Timestamp;
use sea_orm::{ConnectionTrait as _, DatabaseConnection, DbBackend, QueryResult, Statement, Value};

use super::{
    ActivateOutcome, BeginOutcome, ConsumeOutcome, EnrollmentState, TotpEnrollment, TotpSecret,
    TotpStore,
};
use crate::auth::directory::{DirectoryError, DirectoryFuture};
use crate::postgres::error::Port;
use crate::postgres::time::{from_micros, to_micros};
use crate::store::{StoreError, UserId};

/// Which port is speaking, for every error this adapter raises.
const PORT: Port = Port {
    store: "totp",
    record: "TotpEnrollment",
};

/// The durable second-factor store.
#[derive(Debug, Clone)]
pub struct PostgresTotp {
    connection: DatabaseConnection,
}

impl PostgresTotp {
    /// A store over `connection`.
    pub fn new(connection: DatabaseConnection) -> Self {
        Self { connection }
    }
}

/// A store fault is a directory fault.
///
/// [`DirectoryError`] has one variant by the port's own decision — whether the backend was down
/// or merely angry changes nothing about the response — so the [`StoreError`] distinction lives
/// in the log line the mapping already emitted, as it does for the account store.
fn unavailable(error: StoreError) -> DirectoryError {
    DirectoryError::Unavailable {
        detail: error.to_string(),
    }
}

/// The column spelling of a state.
fn state_token(state: EnrollmentState) -> &'static str {
    match state {
        EnrollmentState::Pending => "pending",
        EnrollmentState::Active => "active",
    }
}

/// A stored state, read back.
fn state_from(token: &str) -> Result<EnrollmentState, StoreError> {
    match token {
        "pending" => Ok(EnrollmentState::Pending),
        "active" => Ok(EnrollmentState::Active),
        other => Err(PORT.undecodable(format!("`{other}` is not an enrollment state"))),
    }
}

/// A step as the column holds it.
///
/// A step is seconds-since-epoch over thirty, so one past `i64::MAX` is a clock some two
/// hundred billion years out. Refused as a store fault rather than wrapped: a wrapped step would
/// compare *below* every real one and re-open the replay window.
fn step_to_column(step: u64) -> Result<i64, StoreError> {
    i64::try_from(step).map_err(|_| StoreError::Rejected {
        store: PORT.store,
        detail: format!("step {step} is past what a BIGINT column holds"),
    })
}

/// A stored step, read back.
fn step_from(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| PORT.undecodable(format!("{value} is not a TOTP step")))
}

/// A stored instant, read back.
fn instant_from(micros: i64) -> Result<Timestamp, StoreError> {
    from_micros(micros)
        .ok_or_else(|| PORT.undecodable(format!("{micros}µs is not a representable instant")))
}

/// Decode one enrollment row for `user`.
fn enrollment_from(user: &UserId, row: &QueryResult) -> Result<TotpEnrollment, StoreError> {
    let failed = PORT.failing("reading an enrollment");
    let secret: String = row.try_get("", "secret").map_err(&failed)?;
    let state: String = row.try_get("", "state").map_err(&failed)?;
    let last_step: Option<i64> = row.try_get("", "last_step").map_err(&failed)?;
    let enrolled_at: i64 = row.try_get("", "enrolled_at").map_err(&failed)?;
    let activated_at: Option<i64> = row.try_get("", "activated_at").map_err(&failed)?;
    Ok(TotpEnrollment {
        user_id: user.clone(),
        secret: TotpSecret::new(secret),
        state: state_from(&state)?,
        last_step: last_step.map(step_from).transpose()?,
        enrolled_at: instant_from(enrolled_at)?,
        activated_at: activated_at.map(instant_from).transpose()?,
    })
}

impl PostgresTotp {
    async fn begin_inner(&self, record: TotpEnrollment) -> Result<BeginOutcome, StoreError> {
        let last_step = record.last_step.map(step_to_column).transpose()?;
        let result = self
            .connection
            .execute_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO totp_enrollments \
                   (user_id, secret, state, last_step, enrolled_at, activated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6) \
                 ON CONFLICT (user_id) DO UPDATE SET \
                   secret = EXCLUDED.secret, \
                   state = EXCLUDED.state, \
                   last_step = EXCLUDED.last_step, \
                   enrolled_at = EXCLUDED.enrolled_at, \
                   activated_at = EXCLUDED.activated_at \
                 WHERE totp_enrollments.state <> 'active'",
                [
                    Value::from(record.user_id.as_str().to_owned()),
                    Value::from(record.secret.expose_base32().to_owned()),
                    Value::from(state_token(record.state).to_owned()),
                    Value::from(last_step),
                    Value::from(to_micros(record.enrolled_at)),
                    Value::from(record.activated_at.map(to_micros)),
                ],
            ))
            .await
            .map_err(PORT.failing("beginning an enrollment"))?;
        if result.rows_affected() == 0 {
            tracing::info!(
                user = %record.user_id,
                "an enrollment was refused: the account already has an active second factor"
            );
            return Ok(BeginOutcome::AlreadyActive);
        }
        tracing::info!(user = %record.user_id, "a second-factor enrollment was started");
        Ok(BeginOutcome::Started)
    }

    async fn read_inner(&self, user: &UserId) -> Result<Option<TotpEnrollment>, StoreError> {
        self.connection
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT secret, state, last_step, enrolled_at, activated_at \
                 FROM totp_enrollments WHERE user_id = $1",
                [Value::from(user.as_str().to_owned())],
            ))
            .await
            .map_err(PORT.failing("reading an enrollment"))?
            .map(|row| enrollment_from(user, &row))
            .transpose()
    }

    async fn activate_inner(
        &self,
        user: &UserId,
        step: u64,
        at: Timestamp,
    ) -> Result<ActivateOutcome, StoreError> {
        let step_column = step_to_column(step)?;
        let result = self
            .connection
            .execute_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE totp_enrollments \
                 SET state = 'active', activated_at = $3, last_step = $2 \
                 WHERE user_id = $1 AND state = 'pending'",
                [
                    Value::from(user.as_str().to_owned()),
                    Value::from(step_column),
                    Value::from(to_micros(at)),
                ],
            ))
            .await
            .map_err(PORT.failing("activating an enrollment"))?;
        if result.rows_affected() == 0 {
            tracing::debug!(%user, "an activation found no pending enrollment");
            return Ok(ActivateOutcome::NotPending);
        }
        tracing::info!(%user, step, "a second factor was activated");
        Ok(ActivateOutcome::Activated)
    }

    async fn consume_inner(&self, user: &UserId, step: u64) -> Result<ConsumeOutcome, StoreError> {
        let step_column = step_to_column(step)?;
        let doing = "consuming a TOTP step";
        let row = self
            .connection
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "WITH consumed AS ( \
                   UPDATE totp_enrollments SET last_step = $2 \
                   WHERE user_id = $1 AND (last_step IS NULL OR last_step < $2) \
                   RETURNING 1 \
                 ) \
                 SELECT EXISTS (SELECT 1 FROM consumed) AS fresh, \
                        EXISTS (SELECT 1 FROM totp_enrollments WHERE user_id = $1) AS enrolled",
                [
                    Value::from(user.as_str().to_owned()),
                    Value::from(step_column),
                ],
            ))
            .await
            .map_err(PORT.failing(doing))?
            .ok_or_else(|| PORT.undecodable("a compare-and-set returned no row".to_owned()))?;
        let failed = PORT.failing(doing);
        let fresh: bool = row.try_get("", "fresh").map_err(&failed)?;
        let enrolled: bool = row.try_get("", "enrolled").map_err(&failed)?;
        Ok(if fresh {
            tracing::debug!(%user, step, "a TOTP step was consumed");
            ConsumeOutcome::Fresh
        } else if enrolled {
            tracing::warn!(%user, step, "a TOTP code was refused as a replay");
            ConsumeOutcome::Replayed
        } else {
            ConsumeOutcome::NotEnrolled
        })
    }

    async fn disable_inner(&self, user: &UserId) -> Result<bool, StoreError> {
        let result = self
            .connection
            .execute_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "DELETE FROM totp_enrollments WHERE user_id = $1",
                [Value::from(user.as_str().to_owned())],
            ))
            .await
            .map_err(PORT.failing("disabling an enrollment"))?;
        let removed = result.rows_affected() > 0;
        if removed {
            tracing::info!(%user, "a second factor was disabled");
        }
        Ok(removed)
    }
}

impl TotpStore for PostgresTotp {
    fn begin(&self, record: TotpEnrollment) -> DirectoryFuture<'_, BeginOutcome> {
        Box::pin(async move { self.begin_inner(record).await.map_err(unavailable) })
    }

    fn read<'a>(&'a self, user: &'a UserId) -> DirectoryFuture<'a, Option<TotpEnrollment>> {
        Box::pin(async move { self.read_inner(user).await.map_err(unavailable) })
    }

    fn activate<'a>(
        &'a self,
        user: &'a UserId,
        step: u64,
        at: Timestamp,
    ) -> DirectoryFuture<'a, ActivateOutcome> {
        Box::pin(async move {
            self.activate_inner(user, step, at)
                .await
                .map_err(unavailable)
        })
    }

    fn consume<'a>(&'a self, user: &'a UserId, step: u64) -> DirectoryFuture<'a, ConsumeOutcome> {
        Box::pin(async move { self.consume_inner(user, step).await.map_err(unavailable) })
    }

    fn disable<'a>(&'a self, user: &'a UserId) -> DirectoryFuture<'a, bool> {
        Box::pin(async move { self.disable_inner(user).await.map_err(unavailable) })
    }
}

#[cfg(test)]
mod tests {
    /// The suite, against a real Postgres.
    mod postgres_conformance {
        use super::super::PostgresTotp;
        use crate::auth::totp::TotpStore;
        use crate::auth::totp::conformance::{self, Harness};
        use crate::postgres::testing;

        /// A store over one container.
        #[derive(Debug)]
        struct PostgresHarness {
            enrollments: PostgresTotp,
        }

        impl Harness for PostgresHarness {
            fn enrollments(&self) -> &dyn TotpStore {
                &self.enrollments
            }
        }

        #[tokio::test]
        async fn the_postgres_totp_store_conforms() {
            let Some(database) = testing::start("the Postgres TOTP store").await else {
                return;
            };
            let harness = PostgresHarness {
                enrollments: PostgresTotp::new(database.connection().clone()),
            };
            conformance::run_all(&harness).await;
        }
    }
}
