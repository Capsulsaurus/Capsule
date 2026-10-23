/// The suite, against a real Postgres.
mod postgres_conformance {
    use jiff::SignedDuration;

    use super::super::PostgresCohorts;
    use crate::postgres::testing;
    use crate::store::conformance::{self, CohortHarness};
    use crate::store::{CohortStore, StoreFuture};

    /// A harness over one container.
    ///
    /// `advance` is a **no-op**, and that is the honest implementation rather than a stub:
    /// the cohort map has no TTL at all, so there is no clock to move. The one case that
    /// calls it — `the_cohort_map_does_not_expire` — asserts a record survives a year, and a
    /// store that expires nothing passes it whatever the clock says.
    #[derive(Debug)]
    struct Harness {
        cohorts: PostgresCohorts,
    }

    impl CohortHarness for Harness {
        fn cohorts(&self) -> &dyn CohortStore {
            &self.cohorts
        }

        fn advance(&self, _by: SignedDuration) -> StoreFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn the_postgres_cohort_map_conforms() {
        let Some(database) = testing::start("the Postgres device-cohort map").await else {
            return;
        };
        let harness = Harness {
            cohorts: PostgresCohorts::new(database.connection().clone()),
        };
        conformance::run_all_cohorts(&harness).await;
    }
}
