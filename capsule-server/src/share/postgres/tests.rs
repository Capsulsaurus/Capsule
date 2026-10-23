/// The suite, against a real Postgres.
mod postgres_conformance {
    use super::super::PostgresShares;
    use crate::postgres::testing;
    use crate::share::ShareStore;
    use crate::share::conformance::{self, Harness};

    /// A store over one container.
    #[derive(Debug)]
    struct PostgresHarness {
        shares: PostgresShares,
    }

    impl Harness for PostgresHarness {
        fn shares(&self) -> &dyn ShareStore {
            &self.shares
        }
    }

    #[tokio::test]
    async fn the_postgres_share_store_conforms() {
        let Some(database) = testing::start("the Postgres share store").await else {
            return;
        };
        let harness = PostgresHarness {
            shares: PostgresShares::new(database.connection().clone()),
        };
        conformance::run_all(&harness).await;
    }
}
