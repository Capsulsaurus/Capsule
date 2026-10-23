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
