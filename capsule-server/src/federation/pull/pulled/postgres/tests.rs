use super::*;

#[test]
fn superseded_heads_round_trip_through_their_column() {
    let heads = vec![Hash32([1; 32]), Hash32([2; 32]), Hash32([3; 32])];
    let column = encode_heads(&heads);
    assert_eq!(column.len(), 96);
    assert_eq!(decode_heads(&column).expect("decodes"), heads);
    assert_eq!(decode_heads(&[]).expect("decodes"), Vec::<Hash32>::new());
}

#[test]
fn a_superseded_column_of_a_partial_hash_is_corrupt() {
    let error = decode_heads(&[0; 33]).expect_err("a partial hash");
    assert!(matches!(error, StoreError::Corrupt { .. }), "{error:?}");
    let error = hash_from_bytes(&[0; 31]).expect_err("a short head");
    assert!(matches!(error, StoreError::Corrupt { .. }), "{error:?}");
}

#[test]
fn changes_and_roles_read_back_and_nothing_else_does() {
    for change in [Change::Created, Change::Updated, Change::Deleted] {
        assert_eq!(change_from(change_name(change)).expect("a change"), change);
    }
    assert!(matches!(
        change_from("moved"),
        Err(StoreError::Corrupt { .. })
    ));
    for role in ["original", "metadata", "derivative", "provenance"] {
        assert_eq!(role_from(role).expect("a role"), role);
    }
    assert!(matches!(
        role_from("thumb"),
        Err(StoreError::Corrupt { .. })
    ));
}

#[test]
fn counts_past_a_bigint_are_refused_and_negative_columns_are_corrupt() {
    assert_eq!(to_column(7).expect("fits"), 7);
    assert!(matches!(
        to_column(u64::MAX),
        Err(StoreError::Rejected { .. })
    ));
    assert!(matches!(
        count_from::<u64>(-1, "count"),
        Err(StoreError::Corrupt { .. })
    ));
    assert!(matches!(
        count_from::<u32>(i64::from(u32::MAX) + 1, "epoch"),
        Err(StoreError::Corrupt { .. })
    ));
}

#[test]
fn a_hash_is_its_own_content_address() {
    let hash = Hash32([0xab; 32]);
    assert_eq!(
        address_of(&hash).expect("an address").as_str(),
        hash.to_hex()
    );
    assert_eq!(hash_from_hex(&hash.to_hex()).expect("a hash"), hash);
    assert!(matches!(
        hash_from_hex("not hex"),
        Err(StoreError::Corrupt { .. })
    ));
}

/// The suite and the charging hook, against a real Postgres.
mod postgres_conformance {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};

    use capsule_core::crypto::hash::hash_bytes;
    use sea_orm::DatabaseTransaction;

    use super::super::{HeldBlob, PeerBudgetCharge, PostgresPulledAlbums, PulledBlobCharge};
    use crate::blob::{BlobStore, ContentAddress, InMemoryBlobStore};
    use crate::federation::PeerId;
    use crate::federation::capability::Scope;
    use crate::federation::pull::JobKey;
    use crate::federation::pull::pulled::conformance::{self, Inspect as _};
    use crate::federation::pull::pulled::{
        AcceptedEntry, Change, Committed, OverBudget, PeerCachingBudget, PulledAlbum,
        PulledAlbumReads as _, PulledAlbums as _, ValidatedPage,
    };
    use crate::federation::subscriptions::postgres::PostgresSubscriptions;
    use crate::federation::subscriptions::{CapabilityToken, Subscription, SubscriptionStore as _};
    use crate::gc::HeldBlobs as _;
    use crate::postgres::testing;
    use crate::quota::QuotaStore as _;
    use crate::store::{AlbumId, StoreError, StoreFuture, UserId};

    /// Records every charge, and refuses them all once told to.
    #[derive(Debug, Default)]
    struct Recording {
        charged: Mutex<Vec<(JobKey, Vec<HeldBlob>)>>,
        refuse: Mutex<bool>,
    }

    impl Recording {
        fn charged(&self) -> Vec<(JobKey, Vec<HeldBlob>)> {
            self.charged
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }

        fn refuse(&self) {
            *self.refuse.lock().unwrap_or_else(PoisonError::into_inner) = true;
        }
    }

    impl PulledBlobCharge for Recording {
        fn charge<'a>(
            &'a self,
            _transaction: &'a DatabaseTransaction,
            job: &'a JobKey,
            held: &'a [HeldBlob],
        ) -> StoreFuture<'a, Option<OverBudget>> {
            Box::pin(async move {
                if *self.refuse.lock().unwrap_or_else(PoisonError::into_inner) {
                    return Ok(Some(refusal(job)));
                }
                self.charged
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push((job.clone(), held.to_vec()));
                Ok(None)
            })
        }
    }

    /// What [`Recording`] refuses with.
    fn refusal(job: &JobKey) -> OverBudget {
        OverBudget {
            subscriber: UserId::new("someone"),
            peer: job.peer.clone(),
            used: 2,
            budget: 1,
        }
    }

    fn page(high_water: u64, blobs: &[&[u8]]) -> ValidatedPage {
        ValidatedPage {
            entries: Vec::new(),
            metadata_blobs: blobs
                .iter()
                .map(|bytes| (hash_bytes(bytes), bytes.to_vec()))
                .collect::<BTreeMap<_, _>>(),
            state: PulledAlbum {
                cursor: Some(format!("c{high_water}")),
                high_water: Some(high_water),
                ..PulledAlbum::default()
            },
        }
    }

    /// Only the blobs a job holds for the first time are charged, and a refusal writes nothing.
    async fn charging(store: &PostgresPulledAlbums, blobs: &InMemoryBlobStore, hook: &Recording) {
        let job = JobKey::new(PeerId::new("home.charge.test"), AlbumId::new("charge-a"));
        let first: &[u8] = b"the first metadata blob";
        let second: &[u8] = b"the second metadata blob";
        for (from, next) in [
            (None, page(1, &[first])),
            (Some(1), page(2, &[first, second])),
            (Some(2), page(3, &[])),
        ] {
            assert_eq!(
                store.commit(&job, from, next).await.expect("commit"),
                Committed::Written
            );
        }
        assert_eq!(
            hook.charged(),
            vec![
                (
                    job.clone(),
                    vec![HeldBlob {
                        hash: hash_bytes(first),
                        size: 23,
                    }]
                ),
                (
                    job.clone(),
                    vec![HeldBlob {
                        hash: hash_bytes(second),
                        size: 24,
                    }]
                ),
            ],
            "a blob is charged once per job, and a page holding nothing new charges nothing"
        );

        hook.refuse();
        let third: &[u8] = b"a blob over the cap";
        let mut refused = page(4, &[third]);
        refused.state.assets.insert(
            "asset".to_owned(),
            crate::federation::pull::pulled::AssetHead {
                head: hash_bytes(b"head"),
                superseded: Vec::new(),
                deleted: false,
                amk_version: 1,
                metadata_blob: Some(hash_bytes(third)),
            },
        );
        assert_eq!(
            store
                .commit(&job, Some(3), refused)
                .await
                .expect("the charge refuses"),
            Committed::OverBudget(refusal(&job)),
            "the hook's refusal is the commit's answer"
        );
        let state = store.read(&job).await.expect("read");
        assert_eq!(state.high_water, Some(3), "the refused page moved nothing");
        assert!(state.assets.is_empty(), "the refused page wrote no head");
        assert_eq!(
            store
                .held_metadata_blob(&job, &hash_bytes(third))
                .await
                .expect("lookup"),
            None,
            "the refused page's blob is not held by the job"
        );
        // Its bytes reached the blob store before the refusal: an unreferenced orphan, which the
        // collector reclaims, and never a row naming bytes that are not there.
        let address =
            crate::blob::ContentAddress::parse(&hash_bytes(third).to_hex()).expect("an address");
        assert!(blobs.stat(&address).await.expect("stat").is_some());
    }

    #[tokio::test]
    async fn the_postgres_pulled_album_store_conforms() {
        let Some(database) = testing::start("the Postgres pulled-album store").await else {
            return;
        };
        let blobs = Arc::new(InMemoryBlobStore::new());
        let store = PostgresPulledAlbums::new(database.connection().clone(), blobs.clone());
        conformance::run_all(&store).await;
        crate::federation::breadcrumbs::conformance::run_all(&store).await;

        let hook = Arc::new(Recording::default());
        let charged = PostgresPulledAlbums::new(database.connection().clone(), blobs.clone())
            .with_charge(hook.clone());
        charging(&charged, &blobs, &hook).await;

        let subscriptions = PostgresSubscriptions::new(database.connection().clone());
        let budgeted = PostgresPulledAlbums::new(database.connection().clone(), blobs.clone())
            .with_charge(Arc::new(PeerBudgetCharge::new(PeerCachingBudget::bytes(
                conformance::BUDGET,
            ))));
        conformance::run_budget(&budgeted, &subscriptions).await;

        let quotas = crate::quota::PostgresQuota::new(database.connection().clone());
        charged_to_quota(&budgeted, &subscriptions, &quotas).await;

        held_blobs_outlive_the_durable_collector(&store, &blobs, database.connection()).await;
        a_claimed_blob_missing_from_the_blob_store_is_a_fault(&store, &blobs).await;

        a_sweep_inside_a_commit_waits_for_its_claim(database.connection()).await;
        a_commit_inside_a_sweep_puts_the_bytes_again(database.connection()).await;
    }

    /// A claim row whose bytes the blob store lost is this server's integrity fault: both reads
    /// through the claim answer [`StoreError::Corrupt`], never the absence a client would be
    /// told is a not-found.
    async fn a_claimed_blob_missing_from_the_blob_store_is_a_fault(
        store: &PostgresPulledAlbums,
        blobs: &InMemoryBlobStore,
    ) {
        let bytes: &[u8] = b"missing/a held metadata blob the blob store lost";
        let hash = hash_bytes(bytes);
        let job = JobKey::new(PeerId::new("missing.home"), AlbumId::new("missing"));
        let mut lost = page(1, &[bytes]);
        lost.entries.push(AcceptedEntry {
            asset_id: "asset".to_owned(),
            sync_seq: 1,
            change: Change::Updated,
            changed_at: jiff::Timestamp::UNIX_EPOCH,
            manifest: Some(b"asset@1".to_vec()),
            head: Some(hash_bytes(b"asset@1")),
            metadata_blob: Some(hash),
            blobs: Vec::new(),
            derivative_manifests: Vec::new(),
            original_held: false,
            purged: false,
        });
        assert!(matches!(
            store.commit(&job, None, lost).await.expect("a commit"),
            Committed::Written
        ));
        assert_eq!(
            store
                .asset_metadata(&job, "asset")
                .await
                .expect("the bytes are there"),
            Some(bytes.to_vec())
        );

        let address = ContentAddress::parse(&hash.to_hex()).expect("an address");
        assert!(blobs.remove(&address).await.expect("remove"));
        let error = store
            .asset_metadata(&job, "asset")
            .await
            .expect_err("the claim names bytes that are gone");
        assert!(matches!(error, StoreError::Corrupt { .. }), "{error:?}");
        let error = store
            .held_metadata_blob(&job, &hash)
            .await
            .expect_err("the same claim, read directly");
        assert!(matches!(error, StoreError::Corrupt { .. }), "{error:?}");
    }

    /// Parks a commit after its puts and claim rows, before its transaction commits.
    #[derive(Debug, Default)]
    struct Parked {
        reached: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }

    impl PulledBlobCharge for Parked {
        fn charge<'a>(
            &'a self,
            _transaction: &'a DatabaseTransaction,
            _job: &'a JobKey,
            _held: &'a [HeldBlob],
        ) -> StoreFuture<'a, Option<OverBudget>> {
            Box::pin(async move {
                self.reached.notify_one();
                self.release.notified().await;
                Ok(None)
            })
        }
    }

    /// Whether another transaction could take `address`'s lock right now. Takes and at once
    /// releases it when it could.
    async fn address_lock_is_free(
        connection: &sea_orm::DatabaseConnection,
        address: &ContentAddress,
    ) -> bool {
        use sea_orm::{ConnectionTrait as _, TransactionTrait as _};
        let probe = connection.begin().await.expect("a transaction");
        let row = probe
            .query_one_raw(sea_orm::Statement::from_sql_and_values(
                sea_orm::DbBackend::Postgres,
                "SELECT pg_try_advisory_xact_lock(\
                 hashtext('federation_pulled_metadata_blobs:' || $1)) AS free",
                vec![address.as_str().to_owned().into()],
            ))
            .await
            .expect("a probe")
            .expect("a row");
        let free: bool = row.try_get("", "free").expect("a flag");
        probe.rollback().await.expect("a rollback");
        free
    }

    fn address(bytes: &[u8]) -> ContentAddress {
        ContentAddress::parse(&hash_bytes(bytes).to_hex()).expect("an address")
    }

    /// The race D1 of #1141 names, collector second: an orphan a refused page left is put again
    /// by a retried commit, which then parks before its claim commits. The address is locked
    /// from that put, so a sweep started now waits, and sees the claim rather than removing
    /// the bytes it names.
    async fn a_sweep_inside_a_commit_waits_for_its_claim(connection: &sea_orm::DatabaseConnection) {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let parked = Arc::new(Parked::default());
        let store = Arc::new(
            PostgresPulledAlbums::new(connection.clone(), blobs.clone())
                .with_charge(parked.clone()),
        );
        let bytes: &'static [u8] = b"race/an orphan a retried page carries again";
        let address = address(bytes);
        blobs.put(&address, bytes).await.expect("the orphan");

        let job = JobKey::new(PeerId::new("home.race.test"), AlbumId::new("race-a"));
        let committing = tokio::spawn({
            let store = store.clone();
            async move { store.commit(&job, None, page(1, &[bytes])).await }
        });
        parked.reached.notified().await;
        assert!(
            !address_lock_is_free(connection, &address).await,
            "a commit holds the address from its put to its commit"
        );

        let removed = Arc::new(AtomicBool::new(false));
        let sweeping = tokio::spawn({
            let (store, blobs, address, removed) = (
                store.clone(),
                blobs.clone(),
                address.clone(),
                removed.clone(),
            );
            async move {
                let removal: StoreFuture<'_, ()> = Box::pin(async {
                    blobs.remove(&address).await.expect("a removal");
                    removed.store(true, Ordering::SeqCst);
                    Ok(())
                });
                store.sweep_unless_held(&address, removal).await
            }
        });
        parked.release.notify_one();
        assert_eq!(
            committing
                .await
                .expect("the commit task")
                .expect("a commit"),
            Committed::Written
        );
        assert!(
            sweeping.await.expect("the sweep task").expect("a sweep"),
            "the sweep waited out the commit and saw its claim"
        );
        assert!(!removed.load(Ordering::SeqCst), "nothing was removed");
        assert!(blobs.stat(&address).await.expect("stat").is_some());
        assert!(
            address_lock_is_free(connection, &address).await,
            "both released it"
        );
    }

    /// The same race, collector first: a commit started while a sweep is removing the bytes
    /// waits for the removal, then puts them again, so its claim names bytes that are there.
    async fn a_commit_inside_a_sweep_puts_the_bytes_again(
        connection: &sea_orm::DatabaseConnection,
    ) {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let store = Arc::new(PostgresPulledAlbums::new(connection.clone(), blobs.clone()));
        let bytes: &'static [u8] = b"race/an orphan swept as a page claims it";
        let address = address(bytes);
        blobs.put(&address, bytes).await.expect("the orphan");

        let removing = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let sweeping = tokio::spawn({
            let (store, blobs, address) = (store.clone(), blobs.clone(), address.clone());
            let (removing, release) = (removing.clone(), release.clone());
            async move {
                let removal: StoreFuture<'_, ()> = Box::pin(async {
                    removing.notify_one();
                    release.notified().await;
                    blobs.remove(&address).await.expect("a removal");
                    Ok(())
                });
                store.sweep_unless_held(&address, removal).await
            }
        });
        removing.notified().await;
        assert!(
            !address_lock_is_free(connection, &address).await,
            "a sweep holds the address across its question and its removal"
        );

        let job = JobKey::new(PeerId::new("home.race.test"), AlbumId::new("race-b"));
        let committing = tokio::spawn({
            let store = store.clone();
            async move { store.commit(&job, None, page(1, &[bytes])).await }
        });
        release.notify_one();
        assert!(
            !sweeping.await.expect("the sweep task").expect("a sweep"),
            "nothing held the blob when the sweep asked"
        );
        assert_eq!(
            committing
                .await
                .expect("the commit task")
                .expect("a commit"),
            Committed::Written
        );
        assert!(
            blobs.stat(&address).await.expect("stat").is_some(),
            "the commit put the bytes again after the removal"
        );
        assert!(store.holds(&address).await.expect("a lookup"));
    }

    /// The regression #1141 is, on the durable arm: a pulled page's metadata blob survives a
    /// collector over the same database and blob store past the grace window, and forgetting
    /// the job (its row deleted, cascading to its claims) leaves the blob collectable.
    async fn held_blobs_outlive_the_durable_collector(
        store: &PostgresPulledAlbums,
        blobs: &Arc<InMemoryBlobStore>,
        connection: &sea_orm::DatabaseConnection,
    ) {
        use sea_orm::ConnectionTrait as _;

        use crate::gc::{CollectionContext, DEFAULT_GRACE_WINDOW, Mode, collect};
        use crate::store::memory::ManualClock;

        let job = JobKey::new(PeerId::new("home.gc.test"), AlbumId::new("held-a"));
        let held: &[u8] = b"a pulled metadata blob the durable collector must keep";
        assert_eq!(
            store
                .commit(&job, None, page(1, &[held]))
                .await
                .expect("commit"),
            Committed::Written
        );
        let address =
            crate::blob::ContentAddress::parse(&hash_bytes(held).to_hex()).expect("an address");

        let clock = Arc::new(ManualClock::new(jiff::Timestamp::now()));
        let context = CollectionContext::new(
            Arc::new(crate::index::postgres::PostgresAssetIndex::new(
                connection.clone(),
            )),
            blobs.clone(),
            Arc::new(crate::gc::PostgresCollection::new(connection.clone())),
            Arc::new(crate::quota::PostgresQuota::new(connection.clone())),
            Arc::new(crate::share::postgres::PostgresShares::new(
                connection.clone(),
            )),
            clock.clone(),
            DEFAULT_GRACE_WINDOW,
        )
        .with_held_blobs(Arc::new(store.clone()));
        let three_days = jiff::SignedDuration::from_hours(72);

        let report = collect(&context, Mode::Apply).await.expect("a pass");
        assert!(!report.marked.contains(&address), "{report:?}");
        clock.advance(three_days);
        let report = collect(&context, Mode::Apply).await.expect("a pass");
        assert!(!report.swept.contains(&address), "{report:?}");
        assert!(
            blobs.stat(&address).await.expect("stat").is_some(),
            "a held pulled metadata blob must outlive the grace window"
        );

        connection
            .execute_raw(sea_orm::Statement::from_sql_and_values(
                sea_orm::DbBackend::Postgres,
                "DELETE FROM federation_pulled_albums WHERE peer_id = $1 AND album_id = $2",
                vec![
                    job.peer.as_str().to_owned().into(),
                    job.album.as_str().to_owned().into(),
                ],
            ))
            .await
            .expect("forgetting the job");
        let report = collect(&context, Mode::Apply).await.expect("a pass");
        assert!(report.marked.contains(&address), "released: {report:?}");
        clock.advance(three_days);
        let report = collect(&context, Mode::Apply).await.expect("a pass");
        assert!(report.swept.contains(&address), "{report:?}");
        assert!(blobs.stat(&address).await.expect("stat").is_none());
    }

    /// `user` subscribed to `job`, already lapsed.
    async fn subscribe(subscriptions: &PostgresSubscriptions, user: &UserId, job: &JobKey) {
        let at = jiff::Timestamp::UNIX_EPOCH;
        subscriptions
            .put(Subscription {
                subscriber: user.clone(),
                peer: job.peer.clone(),
                album: job.album.clone(),
                scope: Scope::Read,
                jti: format!("quota-{user}-{job}"),
                token: CapabilityToken::new(format!("token-quota-{user}-{job}")),
                issued_at: at,
                expires_at: at,
                min_protocol_version: "2026-06-01".to_owned(),
                subscribed_at: at,
                updated_at: at,
            })
            .await
            .expect("subscribing");
    }

    /// What `user`'s quota ledger says they use.
    async fn used(quotas: &crate::quota::PostgresQuota, user: &UserId) -> u64 {
        quotas.usage(user).await.expect("reading usage").used
    }

    /// A written page charges every subscriber's `quota_used` (#1162), lapsed ones too; a page
    /// the budget refuses charges nobody; an unsubscribe releases one subscriber only.
    async fn charged_to_quota(
        store: &PostgresPulledAlbums,
        subscriptions: &PostgresSubscriptions,
        quotas: &crate::quota::PostgresQuota,
    ) {
        let job = JobKey::new(PeerId::new("home.quota.test"), AlbumId::new("quota-a"));
        let (alice, bob) = (UserId::new("quota-alice"), UserId::new("quota-bob"));
        subscribe(subscriptions, &alice, &job).await;
        subscribe(subscriptions, &bob, &job).await;

        let held: &[u8] = b"a metadata blob the quota counts";
        assert_eq!(
            store
                .commit(&job, None, page(1, &[held]))
                .await
                .expect("commit"),
            Committed::Written
        );
        let size = u64::try_from(held.len()).expect("a small size");
        assert_eq!(
            used(quotas, &alice).await,
            size,
            "charged in full to each subscriber"
        );
        assert_eq!(used(quotas, &bob).await, size);

        let over = vec![7_u8; usize::try_from(conformance::BUDGET).expect("a small budget")];
        assert!(matches!(
            store
                .commit(&job, Some(1), page(2, &[held, &over]))
                .await
                .expect("commit"),
            Committed::OverBudget(_)
        ));
        assert_eq!(
            used(quotas, &alice).await,
            size,
            "a refused page is rolled back, and so is its charge"
        );

        assert!(
            subscriptions
                .remove(&alice, &job.peer, &job.album)
                .await
                .expect("unsubscribing")
        );
        assert_eq!(used(quotas, &alice).await, 0, "the unsubscribe released it");
        assert_eq!(used(quotas, &bob).await, size, "for that subscriber only");
    }
}
