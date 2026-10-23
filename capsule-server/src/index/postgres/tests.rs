/// The suite, against a real Postgres.
///
/// One case, running the whole list in one pass, because nextest runs a process per test and
/// a container per case would be thirty containers rather than one. `index/conformance.rs`
/// said as much before this adapter existed: *"a Postgres-backed smoke test is one `run_all`
/// call"*.
mod postgres_conformance {
    use super::super::PostgresAssetIndex;
    use crate::index::conformance;
    use crate::postgres::testing;

    #[tokio::test]
    async fn the_postgres_index_conforms() {
        let Some(database) = testing::start("the Postgres asset index").await else {
            return;
        };
        let index = PostgresAssetIndex::new(database.connection().clone());
        conformance::run_all(&index).await;
        // The receipts the index writes land in the table the durable log serves.
        let receipts = crate::attestation::PostgresReceipts::new(database.connection().clone());
        conformance::run_custody(&index, &receipts, &conformance::suite_signer()).await;
    }

    /// Two assets, one blob: purging one and collecting leaves the other's bytes (#672).
    ///
    /// The collector's half of the shared-reference contract, over this index and the
    /// Postgres mark store on one database — the pair a deployment runs.
    #[tokio::test]
    async fn a_shared_blob_survives_purging_one_holder() {
        let Some(database) = testing::start("the Postgres shared-blob collection").await else {
            return;
        };
        crate::gc::conformance::a_shared_blob_survives_purging_one_holder(
            std::sync::Arc::new(PostgresAssetIndex::new(database.connection().clone())),
            std::sync::Arc::new(crate::gc::PostgresCollection::new(
                database.connection().clone(),
            )),
        )
        .await;
    }

    /// A receipt that cannot be written takes the index rows down with it (#693).
    ///
    /// The failure is planted below the port, where only the database can put it: a chain
    /// head that does not decode, so the receipt append inside the finalization transaction
    /// fails after the rows were written in it. Both must be gone — the blob unrecorded, no
    /// sequence number minted, and no receipt for the upload.
    #[tokio::test]
    async fn a_receipt_that_fails_rolls_the_index_rows_back() {
        use sea_orm::{ConnectionTrait, DbBackend, Statement, Value};

        use crate::attestation::{PostgresReceipts, ReceiptLog};
        use crate::index::{AssetIndex, Custody};
        use crate::store::{BlobRole, OwnerId};

        let Some(database) = testing::start("the Postgres custody rollback").await else {
            return;
        };
        let index = PostgresAssetIndex::new(database.connection().clone());
        let receipts = PostgresReceipts::new(database.connection().clone());
        let signer = conformance::suite_signer();
        let case = "custody-rollback";

        // The index tier minus its metadata blob, so the custody write below is the one that
        // would publish the asset and mint a number.
        let row = conformance::pending(case, 1);
        let asset = row.asset_id.clone();
        conformance::ok(index.reserve(row).await, "reserve");
        conformance::ok(
            index
                .record_blob(
                    &asset,
                    conformance::blob(BlobRole::Provenance, "rollback-p"),
                )
                .await,
            "record the manifest",
        );
        let owner = OwnerId::new(format!("{case}-owner"));
        let before = conformance::ok(index.head_seq(&owner).await, "read the head");

        database
            .connection()
            .execute_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO custody_receipts (receipt_seq, upload_id, asset_id, receipt) \
                     VALUES (1, 'planted', 'planted', $1)",
                [Value::from(b"not a receipt".to_vec())],
            ))
            .await
            .expect("plant an undecodable chain head");

        let metadata = conformance::blob(BlobRole::Metadata, "rollback-m");
        let draft = conformance::draft(case, 1, "u1", &metadata);
        let failed = index
            .record_blob_with_receipt(
                &asset,
                metadata,
                Custody {
                    draft: &draft,
                    signer: &signer,
                },
            )
            .await;
        assert!(
            failed.is_err(),
            "a receipt append over a corrupt chain must fail, and answered {failed:?}"
        );

        let row =
            conformance::ok(index.read(&asset).await, "read").expect("the reserved row survives");
        assert_eq!(
            row.address_for(BlobRole::Metadata),
            None,
            "the blob row committed without its receipt"
        );
        assert_eq!(
            row.sync_seq, None,
            "the publication committed without its receipt"
        );
        assert_eq!(
            conformance::ok(index.head_seq(&owner).await, "read the head"),
            before,
            "the number minted for it is rolled back with it"
        );
        assert_eq!(
            conformance::ok(receipts.for_upload(&draft.upload_id).await, "read the log"),
            None,
        );
    }

    /// A manifest hash recorded concurrently through a *sibling* asset is a replay, not a
    /// `23505` (#523).
    ///
    /// The asset-row lock serializes ops on one asset only, so this is the interleaving
    /// it leaves open: the hash is inserted by a transaction that has not committed, the op
    /// passes both idempotency look-ups, and its own insert waits on that row. The case
    /// forces exactly that ordering — it commits the planted row only once Postgres reports
    /// the op's insert waiting on a lock — rather than hoping a `join!` lands on it.
    #[tokio::test]
    async fn a_manifest_applied_concurrently_on_a_sibling_asset_is_a_replay() {
        use capsule_core::crypto::hash::Hash32;
        use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait, Value};

        use crate::index::{AssetIndex, OpAction, OpOutcome};
        use crate::store::OwnerId;

        let Some(database) = testing::start("the Postgres sibling-manifest race").await else {
            return;
        };
        let index = PostgresAssetIndex::new(database.connection().clone());
        let case = "op-sibling";
        let (asset, _) = conformance::publish(&index, case, 2).await;
        let created = conformance::head_of(&index, &asset).await;
        let owner = OwnerId::new(format!("{case}-owner"));
        let before = conformance::ok(index.head_seq(&owner).await, "read the head");
        let hash = Hash32([0x23; 32]);

        // The winner: the same hash, recorded and not yet committed.
        let winner = database
            .connection()
            .begin()
            .await
            .expect("open the winner's transaction");
        winner
            .execute_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO applied_manifests (manifest_hash, sync_seq) VALUES ($1, 999)",
                [Value::from(hash.as_bytes().to_vec())],
            ))
            .await
            .expect("plant the winner's row");

        let release = async {
            // Bounded: a few seconds at most, and a failure names what never happened.
            for _ in 0..500 {
                let waiting = database
                    .connection()
                    .query_one_raw(Statement::from_string(
                        DbBackend::Postgres,
                        "SELECT count(*) AS waiting FROM pg_stat_activity \
                             WHERE wait_event_type = 'Lock' \
                             AND query LIKE 'INSERT INTO applied_manifests%'",
                    ))
                    .await
                    .expect("read the lock waits")
                    .and_then(|row| row.try_get::<i64>("", "waiting").ok())
                    .unwrap_or(0);
                if waiting > 0 {
                    winner.commit().await.expect("commit the winner");
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            panic!("the op's insert never waited on the winner's row");
        };
        let (outcome, ()) = tokio::join!(
            index.apply_op(conformance::op(
                case,
                2,
                OpAction::MetadataUpdate,
                created,
                hash
            )),
            release,
        );

        assert_eq!(
            conformance::ok(outcome, "apply the losing op"),
            OpOutcome::Replayed { sync_seq: 999 },
            "a hash another transaction recorded first is a replay with that transaction's \
                 number, never a unique violation answered as a 503",
        );
        assert_eq!(
            conformance::ok(index.head_seq(&owner).await, "read the head"),
            before,
            "the number the losing op minted is rolled back with it",
        );
    }
}
