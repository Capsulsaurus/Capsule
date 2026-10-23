/// The suite, against a real Postgres.
mod postgres_conformance {
    use super::super::PostgresEscrow;
    use crate::escrow::EscrowStore;
    use crate::escrow::conformance::{self, Harness};
    use crate::postgres::testing;

    /// A store over one container.
    #[derive(Debug)]
    struct PostgresHarness {
        escrows: PostgresEscrow,
    }

    impl Harness for PostgresHarness {
        fn escrows(&self) -> &dyn EscrowStore {
            &self.escrows
        }
    }

    #[tokio::test]
    async fn the_postgres_escrow_store_conforms() {
        let Some(database) = testing::start("the Postgres escrow store").await else {
            return;
        };
        let harness = PostgresHarness {
            escrows: PostgresEscrow::new(database.connection().clone()),
        };
        conformance::run_all(&harness).await;
    }
}
