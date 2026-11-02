/// The suite, against a real Postgres.
mod postgres_conformance {
    use sea_orm::{ConnectionTrait as _, DatabaseConnection};

    use super::super::PostgresRejectedHashes;
    use crate::federation::rejected_hashes::RejectedHashes;
    use crate::federation::rejected_hashes::conformance::{self, BOUNDS, Harness};
    use crate::postgres::testing;
    use crate::store::{StoreError, StoreFuture};

    /// Empties the one table before handing it to a case.
    struct Emptied(DatabaseConnection);

    impl Harness for Emptied {
        fn empty(&self) -> StoreFuture<'_, Box<dyn RejectedHashes>> {
            Box::pin(async move {
                self.0
                    .execute_unprepared("DELETE FROM federation_rejected_hashes")
                    .await
                    .map_err(|error| StoreError::Unavailable {
                        store: "rejected hashes",
                        detail: error.to_string(),
                    })?;
                Ok(
                    Box::new(PostgresRejectedHashes::new(self.0.clone(), BOUNDS))
                        as Box<dyn RejectedHashes>,
                )
            })
        }
    }

    #[tokio::test]
    async fn the_postgres_rejected_hash_table_conforms() {
        let Some(database) = testing::start("the Postgres rejected-hash table").await else {
            return;
        };
        conformance::run_all(&Emptied(database.connection().clone())).await;
    }
}
