/// The suite, against a real Postgres.
mod postgres_conformance {
    use std::sync::Arc;

    use jiff::SignedDuration;
    use sea_orm::{ConnectionTrait, DbBackend, Statement, Value};

    use super::super::{PORT, PostgresAccounts};
    use crate::auth::accounts_memory::MAX_FAILED_ATTEMPTS;
    use crate::auth::conformance::{self, Harness};
    use crate::auth::credential::Credentials;
    use crate::auth::directory::AccountDirectory;
    use crate::auth::oidc::FederatedAccounts;
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

        fn federated(&self) -> &dyn FederatedAccounts {
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

        fn recorded_failures<'a>(&'a self, address: &'a str) -> StoreFuture<'a, Option<u32>> {
            Box::pin(async move {
                let row = self
                    .accounts
                    .connection
                    .query_one_raw(Statement::from_sql_and_values(
                        DbBackend::Postgres,
                        "SELECT failures FROM accounts WHERE email = $1",
                        [Value::from(address.to_owned())],
                    ))
                    .await
                    .map_err(PORT.failing("reading a failure count"))?;
                let Some(row) = row else {
                    return Ok(None);
                };
                let failures: i64 = row
                    .try_get("", "failures")
                    .map_err(PORT.failing("reading a failure count"))?;
                Ok(Some(
                    u32::try_from(failures).expect("a failure count is a small non-negative"),
                ))
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
                Credentials::with_reduced_cost().expect("the platform hashes"),
                clock.clone(),
                MAX_FAILED_ATTEMPTS,
                WINDOW,
            ),
            clock,
        };
        conformance::run_all(&harness).await;
    }
}
