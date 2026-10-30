/// The suite, against a real Postgres.
mod postgres_conformance {
    use super::super::PostgresSubscriptions;
    use crate::federation::subscriptions::conformance;
    use crate::postgres::testing;

    #[tokio::test]
    async fn the_postgres_subscription_store_conforms() {
        let Some(database) = testing::start("the Postgres subscription store").await else {
            return;
        };
        let store = PostgresSubscriptions::new(database.connection().clone());
        conformance::run_all(&store).await;
    }
}
