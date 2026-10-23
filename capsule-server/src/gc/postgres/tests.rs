/// The suite, against a real Postgres.
mod postgres_conformance {
    use super::super::PostgresCollection;
    use crate::gc::CollectionStore;
    use crate::gc::conformance::{self, Harness};
    use crate::postgres::testing;

    /// A store over one container.
    #[derive(Debug)]
    struct PostgresHarness {
        marks: PostgresCollection,
    }

    impl Harness for PostgresHarness {
        fn marks(&self) -> &dyn CollectionStore {
            &self.marks
        }
    }

    #[tokio::test]
    async fn the_postgres_collection_store_conforms() {
        let Some(database) = testing::start("the Postgres collection store").await else {
            return;
        };
        let harness = PostgresHarness {
            marks: PostgresCollection::new(database.connection().clone()),
        };
        conformance::run_all(&harness).await;
    }
}
