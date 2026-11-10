/// The store suite, and the races only a real backend can contest, against a real Postgres.
mod postgres_conformance {
    use std::sync::Arc;

    use jiff::{SignedDuration, Timestamp};

    use super::super::PostgresStores;
    use crate::postgres::testing;
    use crate::store::conformance::{self, CohortHarness, Harness};
    use crate::store::memory::ManualClock;
    use crate::store::upload::LIFETIME_CAP;
    use crate::store::{
        AssetId, AuthStateStore, BlobRole, ChallengeStore, ChannelStore, Clock, CohortStore,
        CommittedRange, CommittedSet, DeviceCode, DevicePollSlot, EnrollmentStore, FinalizeClaim,
        OidcAuthorizationStore, OidcDeviceHandle, OidcDeviceStore, OidcNonce, OidcState, OwnerId,
        PendingAuthorization, PendingDeviceAuthorization, PkceVerifier, PostgresCohorts,
        StoreError, StoreFuture, UploadId, UploadSessionRecord, UploadSessionStatus,
        UploadSessionStore, UserId,
    };

    /// Every Postgres store on one manual clock, with a one-minute uniform lifetime — the same
    /// shape as `InMemoryStores::with_uniform_ttl`.
    struct PostgresHarness {
        clock: ManualClock,
        stores: PostgresStores,
        cohorts: PostgresCohorts,
    }

    impl PostgresHarness {
        fn over(database: &testing::TestDatabase) -> Self {
            Self::with_ttl(database, SignedDuration::from_mins(1))
        }

        fn with_ttl(database: &testing::TestDatabase, ttl: SignedDuration) -> Self {
            let clock = ManualClock::default();
            let shared: Arc<dyn Clock> = Arc::new(clock.clone());
            Self {
                stores: PostgresStores::with_uniform_ttl(
                    database.connection().clone(),
                    shared,
                    ttl,
                ),
                cohorts: PostgresCohorts::new(database.connection().clone()),
                clock,
            }
        }
    }

    impl CohortHarness for PostgresHarness {
        fn cohorts(&self) -> &dyn CohortStore {
            &self.cohorts
        }

        fn advance(&self, by: SignedDuration) -> StoreFuture<'_, ()> {
            Box::pin(async move {
                self.clock.advance(by);
                Ok::<(), StoreError>(())
            })
        }
    }

    impl Harness for PostgresHarness {
        fn auth(&self) -> &dyn AuthStateStore {
            self.stores.auth_store()
        }

        fn uploads(&self) -> &dyn UploadSessionStore {
            self.stores.upload_store()
        }

        fn challenges(&self) -> &dyn ChallengeStore {
            self.stores.challenge_store()
        }

        fn enrollments(&self) -> &dyn EnrollmentStore {
            self.stores.enrollment_store()
        }

        fn channels(&self) -> &dyn ChannelStore {
            self.stores.channel_store()
        }

        fn oidc_authorizations(&self) -> &dyn OidcAuthorizationStore {
            self.stores.oidc_authorization_store()
        }

        fn oidc_devices(&self) -> &dyn OidcDeviceStore {
            self.stores.oidc_device_store()
        }

        fn now(&self) -> Timestamp {
            self.clock.now()
        }
    }

    /// A pending session for the races below.
    fn session(upload_id: &UploadId, created_at: Timestamp) -> UploadSessionRecord {
        UploadSessionRecord {
            upload_id: upload_id.clone(),
            asset_id: AssetId::new("postgres-race-asset"),
            owner_id: OwnerId::new("postgres-race-owner"),
            upload_user_id: UserId::new("postgres-race-uploader"),
            album_id: None,
            content_type: None,
            expected_hash: "c".repeat(64),
            crypto_suite_id: 1,
            protocol_version: "2026-08-29".to_owned(),
            blob_role: BlobRole::Original,
            intent_id: None,
            manifest_envelope: "{}".to_owned(),
            committed: CommittedSet::default(),
            in_flight_cap: 16,
            total_size: 1 << 20,
            status: UploadSessionStatus::Pending,
            failure_code: None,
            created_at,
            last_progress_at: created_at,
        }
    }

    /// The whole store suite, in one pass on one database.
    #[tokio::test]
    async fn the_store_suite_passes_against_postgres() {
        let Some(database) = testing::start("the Postgres state-store suite").await else {
            return;
        };
        conformance::run_all(&PostgresHarness::over(&database)).await;
    }

    /// Sixteen finalizers race for one session; exactly one wins.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn finalization_claims_race_to_one_winner() {
        let Some(database) = testing::start("the Postgres finalization race").await else {
            return;
        };
        let harness = PostgresHarness::with_ttl(&database, LIFETIME_CAP);
        let uploads = harness.stores.uploads();
        let upload_id = UploadId::new("postgres-race-claim");
        uploads
            .open(session(&upload_id, harness.clock.now()))
            .await
            .expect("the session opens");

        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..16 {
            let uploads = Arc::clone(&uploads);
            let upload_id = upload_id.clone();
            tasks.spawn(async move { uploads.claim_finalize(&upload_id).await.expect("answers") });
        }
        let (mut won, mut lost) = (0, 0);
        while let Some(claim) = tasks.join_next().await {
            match claim.expect("a claim task completes") {
                FinalizeClaim::Won(record) => {
                    assert_eq!(record.status, UploadSessionStatus::WaitingForProcessing);
                    won += 1;
                }
                FinalizeClaim::AlreadyClaimed => lost += 1,
                FinalizeClaim::NotFound => panic!("the session exists"),
            }
        }
        assert_eq!((won, lost), (1, 15), "exactly one finalizer wins");
    }

    /// Ranges recorded at once all land: the row lock serializes the read-merge-write.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_ranges_all_land() {
        let Some(database) = testing::start("the Postgres concurrent range record").await else {
            return;
        };
        let harness = PostgresHarness::with_ttl(&database, LIFETIME_CAP);
        let uploads = harness.stores.uploads();
        let upload_id = UploadId::new("postgres-race-ranges");
        uploads
            .open(session(&upload_id, harness.clock.now()))
            .await
            .expect("the session opens");

        const BLOCK: u64 = 4096;
        let blocks = (1u64 << 20) / BLOCK;
        let at = harness.clock.now();
        let mut tasks = tokio::task::JoinSet::new();
        for block in 0..blocks {
            let uploads = Arc::clone(&uploads);
            let upload_id = upload_id.clone();
            tasks.spawn(async move {
                uploads
                    .record_range(
                        &upload_id,
                        CommittedRange {
                            start: block * BLOCK,
                            end: (block + 1) * BLOCK,
                            accepted_at: at,
                        },
                    )
                    .await
                    .expect("answers")
                    .expect("the session is live")
            });
        }
        while let Some(recorded) = tasks.join_next().await {
            recorded.expect("a range task completes");
        }
        let record = uploads
            .read(&upload_id)
            .await
            .expect("answers")
            .expect("the session is live");
        assert!(
            record.is_full(),
            "every range landed: {:?}",
            record.committed
        );
        assert_eq!(record.status, UploadSessionStatus::Uploading);
    }

    /// Sixteen callbacks race one `state`; exactly one gets the pending authorization.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn oidc_callbacks_race_to_one_redemption() {
        let Some(database) = testing::start("the Postgres OIDC callback race").await else {
            return;
        };
        let harness = PostgresHarness::over(&database);
        let store = harness.stores.oidc_authorizations();
        let state = OidcState::new("postgres-race-oidc");
        store
            .begin(
                &state,
                PendingAuthorization {
                    nonce: OidcNonce::new("postgres-race-nonce"),
                    verifier: PkceVerifier::new("postgres-race-verifier"),
                    redirect_uri: "http://127.0.0.1:4242/cb".to_owned(),
                    binding_challenge: "postgres-race-binding".to_owned(),
                    issued_at: harness.clock.now(),
                },
            )
            .await
            .expect("the ceremony begins");

        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..16 {
            let store = Arc::clone(&store);
            let state = state.clone();
            tasks.spawn(async move { store.consume(&state).await.expect("answers") });
        }
        let (mut redeemed, mut refused) = (0, 0);
        while let Some(taken) = tasks.join_next().await {
            match taken.expect("a callback task completes") {
                Some(_) => redeemed += 1,
                None => refused += 1,
            }
        }
        assert_eq!((redeemed, refused), (1, 15), "exactly one callback redeems");
    }

    /// Sixteen polls race one due device handle; exactly one claims the slot.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn device_polls_race_to_one_slot() {
        let Some(database) = testing::start("the Postgres device poll race").await else {
            return;
        };
        let harness = PostgresHarness::over(&database);
        let store = harness.stores.oidc_devices();
        let handle = OidcDeviceHandle::new("postgres-race-device");
        store
            .begin(
                &handle,
                PendingDeviceAuthorization {
                    device_code: DeviceCode::new("postgres-race-device-code"),
                    binding_challenge: "postgres-race-binding".to_owned(),
                    interval: SignedDuration::from_secs(5),
                    issued_at: harness.clock.now(),
                },
            )
            .await
            .expect("the ceremony begins");
        harness.clock.advance(SignedDuration::from_secs(5));

        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..16 {
            let store = Arc::clone(&store);
            let handle = handle.clone();
            tasks.spawn(async move { store.claim(&handle).await.expect("answers") });
        }
        let (mut due, mut too_soon) = (0, 0);
        while let Some(slot) = tasks.join_next().await {
            match slot.expect("a poll task completes") {
                DevicePollSlot::Due(_) => due += 1,
                DevicePollSlot::TooSoon { .. } => too_soon += 1,
                DevicePollSlot::Unknown => panic!("the ceremony is live"),
            }
        }
        assert_eq!((due, too_soon), (1, 15), "exactly one poll claims the slot");
    }
}
