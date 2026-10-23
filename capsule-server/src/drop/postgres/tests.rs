/// The suite, against a real Postgres.
mod postgres_conformance {
    use super::super::PostgresDrops;
    use crate::drop::DropStore;
    use crate::drop::conformance::{self, Harness};
    use crate::postgres::testing;

    /// A store over one container.
    #[derive(Debug)]
    struct PostgresHarness {
        drops: PostgresDrops,
    }

    impl Harness for PostgresHarness {
        fn drops(&self) -> &dyn DropStore {
            &self.drops
        }
    }

    #[tokio::test]
    async fn the_postgres_drop_store_conforms() {
        let Some(database) = testing::start("the Postgres drop store").await else {
            return;
        };
        let harness = PostgresHarness {
            drops: PostgresDrops::new(database.connection().clone()),
        };
        conformance::run_all(&harness).await;
    }
}
