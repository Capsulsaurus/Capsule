/// The suite, against a real Postgres — plus the one property only the schema can show.
mod postgres_conformance {
    use sea_orm::{ConnectionTrait as _, DbBackend, Statement};

    use super::super::PostgresReceipts;
    use crate::attestation::ReceiptLog;
    use crate::attestation::conformance::{self, Harness};
    use crate::postgres::testing;

    /// A log over one container.
    #[derive(Debug)]
    struct PostgresHarness {
        receipts: PostgresReceipts,
    }

    impl Harness for PostgresHarness {
        fn receipts(&self) -> &dyn ReceiptLog {
            &self.receipts
        }
    }

    #[tokio::test]
    async fn the_postgres_receipt_log_conforms() {
        let Some(database) = testing::start("the Postgres receipt log").await else {
            return;
        };
        let harness = PostgresHarness {
            receipts: PostgresReceipts::new(database.connection().clone()),
        };
        conformance::run_all(&harness).await;

        // Invariant 33's structural half: the table refuses a rewrite or a removal from
        // anything that reaches it without the port, which has no such method.
        let connection = database.connection();
        for statement in [
            "UPDATE custody_receipts SET asset_id = 'rewritten'",
            "DELETE FROM custody_receipts",
            "TRUNCATE custody_receipts",
        ] {
            let refused = connection
                .execute_raw(Statement::from_string(DbBackend::Postgres, statement))
                .await;
            assert!(
                refused.is_err(),
                "the receipt log must refuse `{statement}`, and it was accepted"
            );
        }
        assert!(
            !harness
                .receipts
                .for_asset(&crate::store::AssetId::new("chain-asset"))
                .await
                .expect("the log answers")
                .is_empty(),
            "a refused rewrite changed nothing"
        );
    }
}
