/// The suite, against a real Postgres.
mod postgres_conformance {
    use super::super::PostgresMembership;
    use crate::membership::MembershipStore;
    use crate::membership::conformance::{self, Harness};
    use crate::postgres::testing;

    /// A store over one container.
    #[derive(Debug)]
    struct PostgresHarness {
        members: PostgresMembership,
    }

    impl Harness for PostgresHarness {
        fn members(&self) -> &dyn MembershipStore {
            &self.members
        }
    }

    #[tokio::test]
    async fn the_postgres_membership_store_conforms() {
        let Some(database) = testing::start("the Postgres membership store").await else {
            return;
        };
        let harness = PostgresHarness {
            members: PostgresMembership::new(database.connection().clone()),
        };
        conformance::run_all(&harness).await;
    }
}
