/// The suite, against a real Postgres.
mod postgres_conformance {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use sea_orm::{
        ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait as _, Value,
    };

    use super::super::PostgresQuota;
    use crate::blob::ContentAddress;
    use crate::postgres::testing;
    use crate::quota::QuotaStore;
    use crate::quota::conformance::{self, Harness};
    use crate::quota::pulled_total::{self, Change};
    use crate::store::{StoreError, StoreFuture, UserId};

    /// A ledger over one container, with the connection its pulled holdings are written through.
    ///
    /// Its pulled-hold writes stand in for the subscription and pulled-album stores: each writes
    /// its row and moves the pulled total through [`pulled_total`], in one transaction, exactly as
    /// those stores do. `raw` turns the total's maintenance off, for the one case that writes
    /// rows under a schema older than the total.
    #[derive(Debug, Clone)]
    struct PostgresHarness {
        quotas: PostgresQuota,
        connection: DatabaseConnection,
        raw: Arc<AtomicBool>,
    }

    impl PostgresHarness {
        /// Run `sql` with `values`, as conformance setup.
        /// Run `sql` in a transaction that also moves the pulled total for the rows it
        /// returned, unless the harness is writing raw rows.
        async fn write<F, Fut>(
            &self,
            sql: &str,
            values: Vec<Value>,
            maintain: F,
        ) -> Result<(), StoreError>
        where
            F: FnOnce(sea_orm::DatabaseTransaction, Vec<sea_orm::QueryResult>) -> Fut,
            Fut: std::future::Future<Output = Result<sea_orm::DatabaseTransaction, StoreError>>,
        {
            let failed = |error: sea_orm::DbErr| StoreError::Rejected {
                store: "quota conformance",
                detail: error.to_string(),
            };
            let transaction = self.connection.begin().await.map_err(failed)?;
            let rows = transaction
                .query_all_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    sql,
                    values,
                ))
                .await
                .map_err(failed)?;
            let transaction = if self.raw.load(Ordering::SeqCst) {
                transaction
            } else {
                maintain(transaction, rows).await?
            };
            transaction.commit().await.map_err(failed)
        }

        async fn run(&self, sql: &str, values: Vec<Value>) -> Result<(), StoreError> {
            self.connection
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    sql,
                    values,
                ))
                .await
                .map(|_| ())
                .map_err(|error| StoreError::Rejected {
                    store: "quota conformance",
                    detail: error.to_string(),
                })
        }
    }

    impl Harness for PostgresHarness {
        fn quotas(&self) -> &dyn QuotaStore {
            &self.quotas
        }

        fn hold_pulled<'a>(
            &'a self,
            user: &'a UserId,
            peer: &'a str,
            album: &'a str,
            address: &'a ContentAddress,
            size: u64,
        ) -> StoreFuture<'a, ()> {
            Box::pin(async move {
                let job = || vec![Value::from(peer.to_owned()), Value::from(album.to_owned())];
                let mut subscription = vec![Value::from(user.as_str().to_owned())];
                subscription.extend(job());
                let subscribed = [(
                    user.as_str().to_owned(),
                    peer.to_owned(),
                    album.to_owned(),
                )];
                self.write(
                    "INSERT INTO federation_subscriptions (subscriber_id, peer_id, album_id, \
                     scope, jti, token, issued_at, expires_at, min_protocol_version, \
                     subscribed_at, updated_at) \
                     VALUES ($1, $2, $3, 'read', 'jti', 'token', 0, 0, '1.0', 0, 0) \
                     ON CONFLICT DO NOTHING RETURNING subscriber_id",
                    subscription,
                    |transaction, rows| async move {
                        if !rows.is_empty() {
                            pulled_total::subscriptions_changed(
                                &transaction,
                                &subscribed,
                                Change::Added,
                            )
                            .await?;
                        }
                        Ok(transaction)
                    },
                )
                .await?;
                self.run(
                    "INSERT INTO federation_pulled_albums (peer_id, album_id) VALUES ($1, $2) \
                     ON CONFLICT DO NOTHING",
                    job(),
                )
                .await?;
                let size = i64::try_from(size).expect("a test size fits a BIGINT");
                let mut blob = job();
                blob.extend([
                    Value::from(address.as_str().to_owned()),
                    Value::from(size),
                ]);
                let claimed = [(
                    peer.to_owned(),
                    album.to_owned(),
                    address.as_str().to_owned(),
                    size,
                )];
                self.write(
                    "INSERT INTO federation_pulled_metadata_blobs (peer_id, album_id, address, \
                     size) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING RETURNING address",
                    blob,
                    |transaction, rows| async move {
                        if !rows.is_empty() {
                            pulled_total::blobs_changed(&transaction, &claimed, Change::Added)
                                .await?;
                        }
                        Ok(transaction)
                    },
                )
                .await
            })
        }

        fn unsubscribe<'a>(
            &'a self,
            user: &'a UserId,
            peer: &'a str,
            album: &'a str,
        ) -> StoreFuture<'a, ()> {
            Box::pin(async move {
                let removed = [(
                    user.as_str().to_owned(),
                    peer.to_owned(),
                    album.to_owned(),
                )];
                self.write(
                    "DELETE FROM federation_subscriptions \
                     WHERE subscriber_id = $1 AND peer_id = $2 AND album_id = $3 \
                     RETURNING subscriber_id",
                    vec![
                        Value::from(user.as_str().to_owned()),
                        Value::from(peer.to_owned()),
                        Value::from(album.to_owned()),
                    ],
                    |transaction, rows| async move {
                        if !rows.is_empty() {
                            pulled_total::subscriptions_changed(
                                &transaction,
                                &removed,
                                Change::Removed,
                            )
                            .await?;
                        }
                        Ok(transaction)
                    },
                )
                .await
            })
        }
    }

    impl PostgresHarness {
        /// Forget a pulled job: its blob rows leave the pulled total, then the job row goes.
        ///
        /// What a job-forgetting write path owes the total. The foreign key's cascade would
        /// delete the blob rows on its own, but nothing would move the total for them.
        async fn forget_job(&self, peer: &str, album: &str) -> Result<(), StoreError> {
            let (peer, album) = (peer.to_owned(), album.to_owned());
            let (forgotten_peer, forgotten_album) = (peer.clone(), album.clone());
            self.write(
                "DELETE FROM federation_pulled_metadata_blobs \
                 WHERE peer_id = $1 AND album_id = $2 RETURNING address, size",
                vec![Value::from(peer.clone()), Value::from(album.clone())],
                |transaction, rows| async move {
                    let mut released = Vec::with_capacity(rows.len());
                    for row in &rows {
                        let address: String =
                            row.try_get("", "address").expect("a deleted address");
                        let size: i64 = row.try_get("", "size").expect("a deleted size");
                        released.push((
                            forgotten_peer.clone(),
                            forgotten_album.clone(),
                            address,
                            size,
                        ));
                    }
                    pulled_total::blobs_changed(&transaction, &released, Change::Removed)
                        .await?;
                    Ok(transaction)
                },
            )
            .await?;
            self.run(
                "DELETE FROM federation_pulled_albums WHERE peer_id = $1 AND album_id = $2",
                vec![Value::from(peer), Value::from(album)],
            )
            .await
        }
    }

    #[tokio::test]
    async fn the_postgres_ledger_conforms() {
        let Some(database) = testing::start("the Postgres quota ledger").await else {
            return;
        };
        let harness = PostgresHarness {
            quotas: PostgresQuota::new(database.connection().clone()),
            connection: database.connection().clone(),
            raw: Arc::new(AtomicBool::new(false)),
        };
        conformance::run_all(&harness).await;
        assert_eq!(
            drift(&harness).await,
            Vec::new(),
            "every conformance case leaves the stored pulled totals equal to the derived ones"
        );

        materialised::every_write_path_moves_the_stored_total(&harness).await;
        materialised::racing_writers_never_miss_each_other(&harness).await;
        materialised::a_rekey_of_a_counted_row_is_refused(&harness).await;
        materialised::the_reconciler_finds_and_rebuilds_drift(&harness).await;
        materialised::the_scrub_reports_drift_and_only_an_applied_rebuild_repairs_it(&harness)
            .await;
        materialised::the_migration_backfills_existing_holdings(&harness).await;

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

    /// What the reconciler would rebuild now.
    async fn drift(harness: &PostgresHarness) -> Vec<crate::quota::PulledDrift> {
        harness
            .quotas
            .pulled_drift()
            .await
            .expect("the drift audit reads")
    }

    /// The stored pulled total (#1171) against every write that moves one of its three inputs,
    /// each checked against the derivation it replaced.
    mod materialised {
        use std::sync::atomic::Ordering;

        use jiff::Timestamp;
        use sea_orm::Value;

        use super::{PostgresHarness, drift};
        use crate::blob::ContentAddress;
        use crate::quota::conformance::Harness as _;
        use crate::quota::{QuotaLimits, QuotaStore};
        use crate::store::UserId;

        const PEER: &str = "materialised-peer";

        /// `case`'s blob `n`.
        fn address(case: &str, n: u32) -> ContentAddress {
            let seed = format!("{case}/{n}");
            ContentAddress::parse(&capsule_core::crypto::hash::hash_bytes(seed.as_bytes()).to_hex())
                .expect("a digest is a content address")
        }

        /// What the ledger says `user` uses.
        async fn used(harness: &PostgresHarness, user: &UserId) -> u64 {
            harness
                .quotas
                .usage(user)
                .await
                .expect("the ledger answers")
                .used
        }

        /// Fail unless the stored state is the derived one.
        async fn assert_no_drift(harness: &PostgresHarness, after: &str) {
            assert_eq!(drift(harness).await, Vec::new(), "drift after {after}");
        }

        /// A subscription after the blobs, a charge and both releases of an address the account
        /// holds pulled, and the cascade that forgets a job.
        pub(super) async fn every_write_path_moves_the_stored_total(h: &PostgresHarness) {
            let (alice, bob) = (UserId::new("mat-alice"), UserId::new("mat-bob"));
            let (x, y) = (address("paths", 1), address("paths", 2));
            let limits = QuotaLimits::unlimited();
            let at = Timestamp::UNIX_EPOCH;

            h.hold_pulled(&alice, PEER, "paths", &x, 10)
                .await
                .expect("hold");
            h.hold_pulled(&alice, PEER, "paths", &y, 4)
                .await
                .expect("hold");
            // Bob subscribes to an album that already holds both: the subscription's trigger.
            h.hold_pulled(&bob, PEER, "paths", &x, 10)
                .await
                .expect("subscribe");
            assert_eq!(used(h, &alice).await, 14);
            assert_eq!(
                used(h, &bob).await,
                14,
                "a later subscriber holds what is there"
            );
            assert_no_drift(h, "the subscriptions and blobs").await;

            h.quotas()
                .charge(&alice, &x, 10, at, limits)
                .await
                .expect("charge");
            assert_eq!(
                used(h, &alice).await,
                14,
                "attributed and pulled is one copy"
            );
            assert_no_drift(h, "a charge of a pulled address").await;
            assert!(h.quotas().release(&alice, &x).await.expect("release"));
            assert_eq!(used(h, &alice).await, 14, "back to pulled, never credited");
            assert_no_drift(h, "a release").await;

            h.quotas()
                .reserve(&alice, &y, 4, at, at, limits)
                .await
                .expect("reserve");
            assert_no_drift(h, "a reservation of a pulled address").await;
            assert!(
                h.quotas()
                    .release_expired(&alice, &y, at)
                    .await
                    .expect("release an expired reservation")
            );
            assert_eq!(used(h, &alice).await, 14);
            assert_no_drift(h, "an expired reservation's release").await;

            h.forget_job(PEER, "paths").await.expect("forget the job");
            assert_eq!(used(h, &alice).await, 0, "forgetting the job released it");
            assert_eq!(used(h, &bob).await, 0);
            assert_no_drift(h, "the forgotten job").await;
        }

        /// Charges and pulled holds of the same addresses, and subscriptions and blobs of the
        /// same album, all at once: whichever of each pair locks second sees the first.
        pub(super) async fn racing_writers_never_miss_each_other(h: &PostgresHarness) {
            let owner = UserId::new("mat-race-owner");
            let limits = QuotaLimits::unlimited();
            let mut tasks = tokio::task::JoinSet::new();
            for n in 0..24 {
                let blob = address("race", n);
                let (charger, holder) = (h.clone(), h.clone());
                let (charging, holding) = (owner.clone(), owner.clone());
                let charged = blob.clone();
                tasks.spawn(async move {
                    charger
                        .quotas
                        .charge(&charging, &charged, 7, Timestamp::UNIX_EPOCH, limits)
                        .await
                        .map(|_| ())
                });
                tasks.spawn(
                    async move { holder.hold_pulled(&holding, PEER, "race", &blob, 7).await },
                );
            }
            for joined in tasks.join_all().await {
                joined.expect("a racing write");
            }
            assert_eq!(used(h, &owner).await, 24 * 7, "each address counted once");
            assert_no_drift(h, "racing charges and holds").await;

            let subscribers: Vec<UserId> = (0..12)
                .map(|n| UserId::new(format!("mat-race-subscriber-{n}")))
                .collect();
            let mut tasks = tokio::task::JoinSet::new();
            for (subscriber, n) in subscribers.iter().cloned().zip(0_u32..) {
                let blob = address("race-album", n);
                let holder = h.clone();
                tasks.spawn(async move {
                    holder
                        .hold_pulled(&subscriber, PEER, "race-album", &blob, 3)
                        .await
                });
            }
            for joined in tasks.join_all().await {
                joined.expect("a racing hold");
            }
            for subscriber in &subscribers {
                assert_eq!(
                    used(h, subscriber).await,
                    12 * 3,
                    "every subscriber holds every blob the album came to hold"
                );
            }
            assert_no_drift(h, "racing subscriptions and blobs").await;
        }

        /// A key column the total is computed from cannot be rewritten behind the triggers.
        pub(super) async fn a_rekey_of_a_counted_row_is_refused(h: &PostgresHarness) {
            let owner = UserId::new("mat-rekey");
            let blob = address("rekey", 1);
            h.quotas()
                .charge(
                    &owner,
                    &blob,
                    5,
                    Timestamp::UNIX_EPOCH,
                    QuotaLimits::unlimited(),
                )
                .await
                .expect("charge");
            h.hold_pulled(&owner, PEER, "rekey", &blob, 5)
                .await
                .expect("hold");
            for (sql, values) in [
                (
                    "UPDATE quota_attributions SET user_id = 'mat-rekey-thief' WHERE address = $1",
                    vec![Value::from(blob.as_str().to_owned())],
                ),
                (
                    "UPDATE federation_subscriptions SET subscriber_id = 'mat-rekey-thief' \
                     WHERE subscriber_id = $1",
                    vec![Value::from(owner.as_str().to_owned())],
                ),
                (
                    "UPDATE federation_pulled_metadata_blobs SET size = 1 WHERE address = $1",
                    vec![Value::from(blob.as_str().to_owned())],
                ),
            ] {
                assert!(h.run(sql, values).await.is_err(), "refused: {sql}");
            }
            assert_eq!(used(h, &owner).await, 5);
            assert_no_drift(h, "the refused rekeys").await;
        }

        /// Drift written behind the triggers is named, account by account, and rebuilt.
        pub(super) async fn the_reconciler_finds_and_rebuilds_drift(h: &PostgresHarness) {
            let (total, refs) = (
                UserId::new("mat-drift-total"),
                UserId::new("mat-drift-refs"),
            );
            let blob = address("drift", 1);
            for user in [&total, &refs] {
                h.hold_pulled(user, PEER, "drift", &blob, 9)
                    .await
                    .expect("hold");
            }
            h.run(
                "UPDATE quota_usage SET pulled = pulled + 5 WHERE user_id = $1",
                vec![Value::from(total.as_str().to_owned())],
            )
            .await
            .expect("skew a total");
            h.run(
                "UPDATE quota_pulled_holdings SET refs = refs + 1 WHERE user_id = $1",
                vec![Value::from(refs.as_str().to_owned())],
            )
            .await
            .expect("skew a reference count");

            let found = h.quotas.reconcile_pulled().await.expect("a reconcile");
            let named: Vec<(&str, u64, u64, bool)> = found
                .iter()
                .map(|d| (d.user.as_str(), d.stored, d.derived, d.holdings))
                .collect();
            assert_eq!(
                named,
                vec![
                    ("mat-drift-refs", 9, 9, true),
                    ("mat-drift-total", 14, 9, false)
                ],
            );
            assert_no_drift(h, "the rebuild").await;
            assert_eq!(used(h, &total).await, 9);
            h.unsubscribe(&refs, PEER, "drift")
                .await
                .expect("unsubscribe");
            assert_eq!(
                used(h, &refs).await,
                0,
                "the rebuilt count releases on time"
            );
            assert_no_drift(h, "the unsubscribe after a rebuild").await;
        }

        /// Drift written behind the triggers is a `capsule-server scrub` finding, a dry-run
        /// rebuild names it and writes nothing, and `rebuild --apply` rebuilds it (#1183).
        ///
        /// The scrub and the rebuild are assembled over empty in-memory stores beside this
        /// ledger, so every finding and every rebuilt account is the quota check's.
        pub(super) async fn the_scrub_reports_drift_and_only_an_applied_rebuild_repairs_it(
            h: &PostgresHarness,
        ) {
            use std::sync::Arc;

            use crate::blob::InMemoryBlobStore;
            use crate::gc::Mode;
            use crate::index::memory::InMemoryAssetIndex;
            use crate::ledger::InMemoryLedger;
            use crate::ledger::rebuild::{RebuildContext, rebuild};
            use crate::ledger::replay::LedgerKeys;
            use crate::quota::PulledDrift;
            use crate::scrub::{Depth, Finding, ScrubContext, scrub};
            use crate::share::InMemoryShares;
            use crate::store::memory::{InMemoryUploadSessions, ManualClock};

            let owner = UserId::new("mat-scrub");
            h.hold_pulled(&owner, PEER, "scrub", &address("scrub", 1), 8)
                .await
                .expect("hold");
            h.run(
                "UPDATE quota_usage SET pulled = pulled + 3 WHERE user_id = $1",
                vec![Value::from(owner.as_str().to_owned())],
            )
            .await
            .expect("skew a total behind the triggers");
            let drifted = PulledDrift {
                user: owner.clone(),
                stored: 11,
                derived: 8,
                holdings: false,
            };

            let quotas: Arc<dyn QuotaStore> = Arc::new(h.quotas.clone());
            let index = Arc::new(InMemoryAssetIndex::new());
            let blobs = Arc::new(InMemoryBlobStore::new());
            let clock = Arc::new(ManualClock::default());
            let scrubber = ScrubContext::new(
                index.clone(),
                blobs.clone(),
                Arc::new(InMemoryUploadSessions::new(
                    clock.clone(),
                    jiff::SignedDuration::from_hours(24),
                )),
                Arc::new(InMemoryShares::new()),
                clock.clone(),
            )
            .with_quotas(quotas.clone());
            let report = scrub(&scrubber, Depth::Structural)
                .await
                .expect("a scrub pass");
            assert_eq!(
                report.findings,
                vec![Finding::PulledQuotaDrift {
                    user: owner.clone(),
                    stored: 11,
                    derived: 8,
                    holdings: false,
                }],
            );
            assert_eq!(used(h, &owner).await, 11, "the scrub repaired nothing");

            let rebuilder = RebuildContext::new(
                index,
                blobs,
                Arc::new(InMemoryLedger::new()),
                quotas,
                LedgerKeys::default(),
                clock,
            );
            let dry = rebuild(&rebuilder, Mode::DryRun)
                .await
                .expect("a dry-run rebuild");
            assert_eq!(dry.pulled_drift, vec![drifted.clone()]);
            assert!(!dry.has_faults(), "drift is a repair, not a fault");
            assert_eq!(used(h, &owner).await, 11, "a dry run rebuilt nothing");

            let applied = rebuild(&rebuilder, Mode::Apply)
                .await
                .expect("an applied rebuild");
            assert_eq!(applied.pulled_drift, vec![drifted]);
            assert_eq!(used(h, &owner).await, 8, "the applied rebuild repaired it");
            assert_no_drift(h, "the applied rebuild").await;
            let again = rebuild(&rebuilder, Mode::Apply)
                .await
                .expect("a second rebuild");
            assert!(again.pulled_drift.is_empty(), "a rerun rebuilds nothing");
            assert!(
                scrub(&scrubber, Depth::Structural)
                    .await
                    .expect("a scrub pass")
                    .is_clean(),
                "the scrub after the repair is clean"
            );
        }

        /// Rows written before the migration are counted by it, and its `down` and `up` round
        /// trip.
        pub(super) async fn the_migration_backfills_existing_holdings(h: &PostgresHarness) {
            use server_migration::MigratorTrait as _;

            // Down to just before `000030`, however many ordinals come after it.
            let migrations = server_migration::Migrator::migrations();
            let ordinal = migrations
                .iter()
                .position(|migration| migration.name() == "m20260902_000030_quota_pulled_holdings")
                .expect("the materialised total's migration exists");
            let later = u32::try_from(migrations.len() - ordinal).expect("a handful of ordinals");
            server_migration::Migrator::down(&h.connection, Some(later))
                .await
                .expect("roll back the materialised total");
            // The schema has no pulled total to maintain until `000030` is back, so these rows
            // are written as an older server wrote them, and the backfill counts them.
            h.raw.store(true, Ordering::SeqCst);
            let (owner, blob, mine) = (
                UserId::new("mat-backfill"),
                address("backfill", 1),
                address("backfill", 2),
            );
            h.hold_pulled(&owner, PEER, "backfill-a", &blob, 20)
                .await
                .expect("hold");
            h.hold_pulled(&owner, PEER, "backfill-b", &blob, 20)
                .await
                .expect("hold through a second album");
            h.hold_pulled(&owner, PEER, "backfill-b", &mine, 6)
                .await
                .expect("hold");
            h.run(
                "INSERT INTO quota_attributions (address, user_id, size, charged_at) \
                 VALUES ($1, $2, 6, 0)",
                vec![
                    Value::from(mine.as_str().to_owned()),
                    Value::from(owner.as_str().to_owned()),
                ],
            )
            .await
            .expect("attribute");
            server_migration::Migrator::up(&h.connection, None)
                .await
                .expect("reapply the materialised total");
            h.raw.store(false, Ordering::SeqCst);

            assert_eq!(
                used(h, &owner).await,
                26,
                "the backfill counts each address once"
            );
            assert_no_drift(h, "the backfill").await;
            h.unsubscribe(&owner, PEER, "backfill-a")
                .await
                .expect("unsubscribe");
            assert_eq!(
                used(h, &owner).await,
                26,
                "still held through the other album"
            );
            h.unsubscribe(&owner, PEER, "backfill-b")
                .await
                .expect("unsubscribe");
            assert_eq!(used(h, &owner).await, 6, "the attribution is its own");
            assert_no_drift(h, "unsubscribing from backfilled holdings").await;
        }
    }
}
