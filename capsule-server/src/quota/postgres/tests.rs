/// The suite, against a real Postgres.
mod postgres_conformance {
    use super::super::PostgresQuota;
    use crate::postgres::testing;
    use crate::quota::QuotaStore;
    use crate::quota::conformance::{self, Harness};

    /// A ledger over one container.
    #[derive(Debug)]
    struct PostgresHarness {
        quotas: PostgresQuota,
    }

    impl Harness for PostgresHarness {
        fn quotas(&self) -> &dyn QuotaStore {
            &self.quotas
        }
    }

    #[tokio::test]
    async fn the_postgres_ledger_conforms() {
        let Some(database) = testing::start("the Postgres quota ledger").await else {
            return;
        };
        let harness = PostgresHarness {
            quotas: PostgresQuota::new(database.connection().clone()),
        };
        conformance::run_all(&harness).await;

        // A suspension survives a restart (#705): a fresh pool and a fresh adapter, sharing
        // nothing with the one that wrote it but the database, read it back.
        let user = crate::store::UserId::new("restart-user-1");
        let at = jiff::Timestamp::UNIX_EPOCH + jiff::SignedDuration::from_secs(86_400);
        assert!(
            harness
                .quotas
                .suspend(&user, at)
                .await
                .expect("the ledger suspends")
        );
        let reopened = PostgresQuota::new(
            sea_orm::Database::connect(database.url())
                .await
                .expect("a second pool connects"),
        );
        assert_eq!(
            reopened
                .usage(&user)
                .await
                .expect("the ledger answers")
                .suspended_since,
            Some(at),
            "a suspension is durable, not a property of the process that recorded it"
        );
    }
}
