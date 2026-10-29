//! The Valkey adapters against a live server (#403).
//!
//! The same suites the in-memory doubles pass — [`conformance::run_all`] and
//! [`counters::run_all`] — driven against a `valkey/valkey` container, plus the two races a
//! single-threaded suite cannot stage: a finalize claim and a counter hit, each contested from
//! many tasks at once.
//!
//! # Gating
//!
//! `mise run test-rust` needs no podman, so every case here answers *skipped* — one line on
//! stderr, and a pass — unless one of:
//!
//! - `CAPSULE_TEST_VALKEY=1`: start a container through testcontainers (`DOCKER_HOST` pointing at
//!   the podman socket). `CAPSULE_TEST_VALKEY_TAG` overrides the image tag, which defaults to the
//!   one `capsule-server/compose.yaml` runs; `CAPSULE_TEST_CONTAINER_USERNS` (for instance
//!   `keep-id`) sets the container's user namespace mode, which a rootless podman may need.
//! - `CAPSULE_TEST_VALKEY_URL=redis://…`: run against a server already up. Every key is written
//!   under `capsule:`, and every case scopes and resets its own identifiers, so the suite can be
//!   re-run against a server that still holds the previous run.
//!
//! `.config/nextest.toml` puts this file in the one-thread `containers` group.
//!
//! # Time
//!
//! The harness advances a [`ManualClock`], exactly as the in-memory harness does. The adapters
//! decide expiry from the injected clock and use Valkey's own TTL only as the collector, which is
//! what lets an expiry case step one nanosecond either side of a boundary against a real server.

use std::sync::Arc;

use capsule_server::counter::valkey::ValkeyCounters;
use capsule_server::counter::{Budget, CounterKey, CounterStore, conformance as counters};
use capsule_server::store::conformance::{self, CohortHarness, Harness};
use capsule_server::store::memory::ManualClock;
use capsule_server::store::upload::{
    BlobRole, FinalizeClaim, UploadSessionRecord, UploadSessionStatus, UploadSessionStore,
};
use capsule_server::store::valkey::ValkeyStores;
use capsule_server::store::{
    AssetId, AuthStateStore, ChallengeStore, ChannelStore, Clock, CohortStore, EnrollmentStore,
    OidcAuthorizationStore, OidcDeviceStore, OwnerId, SessionId, SessionRecord, StoreError,
    StoreFuture, UploadId, UserId,
};
use jiff::{SignedDuration, Timestamp};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::valkey::{VALKEY_PORT, Valkey};

/// The image tag `capsule-server/compose.yaml` runs.
const DEFAULT_TAG: &str = "9.0.4";

/// A reachable server: its URL, and the container that must outlive the test if one was started.
struct Server {
    url: String,
    _container: Option<ContainerAsync<Valkey>>,
}

/// The server the suite runs against, or `None` — logged — when neither gate is set.
async fn server() -> Option<Server> {
    if let Some(url) = std::env::var("CAPSULE_TEST_VALKEY_URL")
        .ok()
        .filter(|url| !url.is_empty())
    {
        return Some(Server {
            url,
            _container: None,
        });
    }
    if std::env::var("CAPSULE_TEST_VALKEY").ok().as_deref() != Some("1") {
        eprintln!(
            "skipped: set CAPSULE_TEST_VALKEY=1 (a container via DOCKER_HOST) or \
             CAPSULE_TEST_VALKEY_URL=redis://… to run the Valkey suite"
        );
        return None;
    }
    let tag = std::env::var("CAPSULE_TEST_VALKEY_TAG").unwrap_or_else(|_| DEFAULT_TAG.to_owned());
    let mut request = Valkey::default().with_tag(tag);
    if let Some(userns) = std::env::var("CAPSULE_TEST_CONTAINER_USERNS")
        .ok()
        .filter(|mode| !mode.is_empty())
    {
        request = request.with_userns_mode(&userns);
    }
    let container = request.start().await.expect("the Valkey container starts");
    let host = container
        .get_host()
        .await
        .expect("the container has a host");
    let port = container
        .get_host_port_ipv4(VALKEY_PORT)
        .await
        .expect("the container maps its port");
    Some(Server {
        url: format!("redis://{host}:{port}"),
        _container: Some(container),
    })
}

/// Every Valkey store on one manual clock, with a one-minute uniform lifetime — the same shape
/// as `InMemoryStores::with_uniform_ttl`.
struct ValkeyHarness {
    clock: ManualClock,
    stores: ValkeyStores,
    auth: Arc<dyn AuthStateStore>,
    uploads: Arc<dyn UploadSessionStore>,
    challenges: Arc<dyn ChallengeStore>,
    enrollments: Arc<dyn EnrollmentStore>,
    channels: Arc<dyn ChannelStore>,
    cohorts: Arc<dyn CohortStore>,
    oidc_authorizations: Arc<dyn OidcAuthorizationStore>,
    oidc_devices: Arc<dyn OidcDeviceStore>,
}

impl ValkeyHarness {
    async fn connect(url: &str) -> Self {
        let clock = ManualClock::default();
        let shared: Arc<dyn Clock> = Arc::new(clock.clone());
        let stores =
            ValkeyStores::connect_with_uniform_ttl(url, shared, SignedDuration::from_mins(1))
                .await
                .expect("the Valkey server answers");
        Self {
            clock,
            auth: stores.auth(),
            uploads: stores.uploads(),
            challenges: stores.challenges(),
            enrollments: stores.enrollments(),
            channels: stores.channels(),
            cohorts: stores.cohorts(),
            oidc_authorizations: stores.oidc_authorizations(),
            oidc_devices: stores.oidc_devices(),
            stores,
        }
    }
}

// `Harness` was one trait when #403 wrote this file and is two since #402: `CohortHarness`
// carries the cohort map and the time seam, `Harness` extends it with the five volatile stores.
// The split exists so a Postgres-backed cohort harness need not implement five adapters it will
// never have. This harness has all six — the Valkey cohort hash is the interim adapter
// `PostgresCohorts` supersedes — so it implements both halves and `run_all` still drives every
// case in the module against one connection.
impl CohortHarness for ValkeyHarness {
    fn cohorts(&self) -> &dyn CohortStore {
        &*self.cohorts
    }

    fn advance(&self, by: SignedDuration) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.clock.advance(by);
            Ok::<(), StoreError>(())
        })
    }
}

impl Harness for ValkeyHarness {
    fn auth(&self) -> &dyn AuthStateStore {
        &*self.auth
    }

    fn uploads(&self) -> &dyn UploadSessionStore {
        &*self.uploads
    }

    fn challenges(&self) -> &dyn ChallengeStore {
        &*self.challenges
    }

    fn enrollments(&self) -> &dyn EnrollmentStore {
        &*self.enrollments
    }

    fn channels(&self) -> &dyn ChannelStore {
        &*self.channels
    }

    fn oidc_authorizations(&self) -> &dyn OidcAuthorizationStore {
        &*self.oidc_authorizations
    }

    fn oidc_devices(&self) -> &dyn OidcDeviceStore {
        &*self.oidc_devices
    }
}

/// The whole store suite, in one pass on one server.
#[tokio::test]
async fn the_store_suite_passes_against_valkey() {
    let Some(server) = server().await else {
        return;
    };
    let harness = ValkeyHarness::connect(&server.url).await;
    conformance::run_all(&harness).await;
}

/// A record past its logical lifetime is reported dead and is **not** deleted by the reader.
///
/// The read gate only answers; `PEXPIRE` collects. A reader whose clock ran ahead would
/// otherwise delete, for every replica, state that is still live by the store's own lifetime.
#[tokio::test]
async fn a_logically_expired_record_is_dead_but_left_for_the_collector() {
    let Some(server) = server().await else {
        return;
    };
    let harness = ValkeyHarness::connect(&server.url).await;
    let session_id = SessionId::new("valkey-logical-expiry");
    let created_at = Timestamp::UNIX_EPOCH;
    harness
        .auth
        .open_session(SessionRecord {
            session_id: session_id.clone(),
            user_id: UserId::new("valkey-logical-expiry-user"),
            created_at,
            authenticated_at: created_at,
            last_active_at: created_at,
            user_agent: None,
            ip_address: None,
            cohort_hash: None,
            device_id: None,
        })
        .await
        .expect("the session opens");
    harness.advance(harness.auth.ttl()).await.expect("advances");

    assert_eq!(
        harness
            .auth
            .read_session(&session_id)
            .await
            .expect("answers"),
        None,
        "past its logical lifetime the record is absent to a reader"
    );

    let client = redis::Client::open(server.url.as_str()).expect("the URL opens");
    let mut connection = client
        .get_multiplexed_async_connection()
        .await
        .expect("a second connection");
    let exists: i64 = redis::cmd("EXISTS")
        .arg(format!("capsule:session:{session_id}"))
        .query_async(&mut connection)
        .await
        .expect("EXISTS answers");
    assert_eq!(
        exists, 1,
        "and the key is still there for PEXPIRE to collect"
    );
    let ttl: i64 = redis::cmd("PTTL")
        .arg(format!("capsule:session:{session_id}"))
        .query_async(&mut connection)
        .await
        .expect("PTTL answers");
    assert!(ttl > 0, "with a collector lifetime set: {ttl}");
    harness
        .auth
        .close_session(&session_id)
        .await
        .expect("cleans up");
}

/// The collector's sweep drops an expired session from the progress view (#519).
///
/// The sorted set has no lifetime of its own, so a member whose record expired stays until a
/// walk heals it; `gc --apply` is that walk.
#[tokio::test]
async fn the_progress_sweep_drops_an_expired_session_from_the_view() {
    use capsule_server::gc::ProgressSweep;

    let Some(server) = server().await else {
        return;
    };
    let harness = ValkeyHarness::connect(&server.url).await;
    let upload_id = UploadId::new("valkey-progress-sweep");
    harness
        .uploads
        .open(UploadSessionRecord {
            upload_id: upload_id.clone(),
            asset_id: AssetId::new("valkey-progress-sweep-asset"),
            owner_id: OwnerId::new("valkey-progress-sweep-owner"),
            upload_user_id: UserId::new("valkey-progress-sweep-uploader"),
            album_id: None,
            content_type: None,
            expected_hash: "d".repeat(64),
            crypto_suite_id: 1,
            protocol_version: "2026-08-29".to_owned(),
            blob_role: BlobRole::Original,
            intent_id: None,
            manifest_envelope: "{}".to_owned(),
            received_bytes: 0,
            total_size: 1,
            status: UploadSessionStatus::Pending,
            created_at: harness.clock.now(),
            last_progress_at: harness.clock.now(),
        })
        .await
        .expect("the session opens");

    let client = redis::Client::open(server.url.as_str()).expect("the URL opens");
    let mut connection = client
        .get_multiplexed_async_connection()
        .await
        .expect("a second connection");
    let mut score = redis::cmd("ZSCORE");
    score
        .arg("capsule:upload:progress")
        .arg(upload_id.to_string());
    let indexed = |score: Option<f64>| score.is_some();

    let before: Option<f64> = score.query_async(&mut connection).await.expect("answers");
    assert!(indexed(before), "an open session is in the view");

    harness
        .advance(harness.uploads.ttl())
        .await
        .expect("advances");
    let expired: Option<f64> = score.query_async(&mut connection).await.expect("answers");
    assert!(
        indexed(expired),
        "an expired session's member outlives its record until something walks over it"
    );

    harness
        .uploads
        .sweep_progress(harness.clock.now())
        .await
        .expect("the sweep answers");
    let after: Option<f64> = score.query_async(&mut connection).await.expect("answers");
    assert!(!indexed(after), "and the collector's sweep is that walk");
}

/// The claimed view heals a member its record no longer backs.
///
/// `capsule:upload:claimed` has no lifetime of its own, and every script that writes a status
/// keeps it in step; what is left is a member whose record lapsed, or whose status something
/// else wrote. Boot recovery's listing is the walk that drops both, so a claim is never
/// re-driven for a session that is gone or already terminal.
#[tokio::test]
async fn the_claimed_view_heals_a_member_its_record_no_longer_backs() {
    let Some(server) = server().await else {
        return;
    };
    let harness = ValkeyHarness::connect(&server.url).await;
    let client = redis::Client::open(server.url.as_str()).expect("the URL opens");
    let mut connection = client
        .get_multiplexed_async_connection()
        .await
        .expect("a second connection");

    let mut claimed_ids = Vec::new();
    for tag in ["lapsed", "rewritten"] {
        let upload_id = UploadId::new(format!("valkey-claimed-{tag}"));
        harness
            .uploads
            .open(UploadSessionRecord {
                upload_id: upload_id.clone(),
                asset_id: AssetId::new(format!("valkey-claimed-{tag}-asset")),
                owner_id: OwnerId::new("valkey-claimed-owner"),
                upload_user_id: UserId::new("valkey-claimed-uploader"),
                album_id: None,
                content_type: None,
                expected_hash: "c".repeat(64),
                crypto_suite_id: 1,
                protocol_version: "2026-08-29".to_owned(),
                blob_role: BlobRole::Original,
                intent_id: None,
                manifest_envelope: "{}".to_owned(),
                received_bytes: 0,
                total_size: 1,
                status: UploadSessionStatus::Pending,
                created_at: harness.clock.now(),
                last_progress_at: harness.clock.now(),
            })
            .await
            .expect("the session opens");
        assert!(
            matches!(
                harness
                    .uploads
                    .claim_finalize(&upload_id)
                    .await
                    .expect("the claim answers"),
                FinalizeClaim::Won(_)
            ),
            "the first claim wins"
        );
        claimed_ids.push(upload_id);
    }
    let member = |id: &UploadId| {
        let mut command = redis::cmd("SISMEMBER");
        command.arg("capsule:upload:claimed").arg(id.to_string());
        command
    };
    for id in &claimed_ids {
        let indexed: i64 = member(id)
            .query_async(&mut connection)
            .await
            .expect("answers");
        assert_eq!(indexed, 1, "a claim enters the claimed view with the claim");
    }

    // One record gone out from under its member, one terminal by a write the set never saw.
    let _: i64 = redis::cmd("DEL")
        .arg(format!("capsule:upload:session:{}", claimed_ids[0]))
        .query_async(&mut connection)
        .await
        .expect("DEL answers");
    let _: i64 = redis::cmd("HSET")
        .arg(format!("capsule:upload:session:{}", claimed_ids[1]))
        .arg("status")
        .arg("completed")
        .query_async(&mut connection)
        .await
        .expect("HSET answers");

    let listed = harness
        .uploads
        .claimed()
        .await
        .expect("the listing answers");
    assert!(
        listed
            .iter()
            .all(|record| !claimed_ids.contains(&record.upload_id)),
        "neither is listed for recovery: {listed:?}"
    );
    for id in &claimed_ids {
        let indexed: i64 = member(id)
            .query_async(&mut connection)
            .await
            .expect("answers");
        assert_eq!(
            indexed, 0,
            "and the listing dropped the member it walked over"
        );
    }
}

/// The sweep pages by score cursor across many pages, ties and drops alike (#519).
///
/// Every session opens at one instant, so every member shares one score and each page resumes
/// by skipping the kept members already seen at it; every third record is deleted under the
/// walk, so pages also shrink as it heals. A cursor that skipped too few would answer a member
/// twice, and one that skipped too many would leave a dead member in the view.
#[tokio::test]
async fn the_progress_sweep_pages_by_score_cursor_across_ties_and_drops() {
    use capsule_server::gc::ProgressSweep;

    let Some(server) = server().await else {
        return;
    };
    let harness = ValkeyHarness::connect(&server.url).await;
    // Scoped to this run, so a server still holding a previous run's sessions changes nothing.
    let run = uuid::Uuid::now_v7();
    let ids: Vec<UploadId> = (0..100)
        .map(|n| UploadId::new(format!("valkey-progress-cursor-{run}-{n:03}")))
        .collect();
    for id in &ids {
        harness
            .uploads
            .open(UploadSessionRecord {
                upload_id: id.clone(),
                asset_id: AssetId::new(format!("{id}-asset")),
                owner_id: OwnerId::new("valkey-progress-cursor-owner"),
                upload_user_id: UserId::new("valkey-progress-cursor-uploader"),
                album_id: None,
                content_type: None,
                expected_hash: "e".repeat(64),
                crypto_suite_id: 1,
                protocol_version: "2026-08-29".to_owned(),
                blob_role: BlobRole::Original,
                intent_id: None,
                manifest_envelope: "{}".to_owned(),
                received_bytes: 0,
                total_size: 1,
                status: UploadSessionStatus::Pending,
                created_at: harness.clock.now(),
                last_progress_at: harness.clock.now(),
            })
            .await
            .expect("the session opens");
    }

    let client = redis::Client::open(server.url.as_str()).expect("the URL opens");
    let mut connection = client
        .get_multiplexed_async_connection()
        .await
        .expect("a second connection");
    let (dropped, kept): (Vec<_>, Vec<_>) = ids.iter().enumerate().partition(|(n, _)| n % 3 == 0);
    for (_, id) in &dropped {
        let _: i64 = redis::cmd("DEL")
            .arg(format!("capsule:upload:session:{id}"))
            .query_async(&mut connection)
            .await
            .expect("DEL answers");
    }

    // The walk lists only sessions that progressed strictly before its instant, so it runs a
    // second later — well inside the TTL, so every kept session is still live when it is read.
    harness
        .advance(SignedDuration::from_secs(1))
        .await
        .expect("advances");
    let swept = harness
        .uploads
        .least_recently_progressed(harness.clock.now(), usize::MAX)
        .await
        .expect("the walk answers");
    let mine: Vec<&UploadId> = swept.iter().filter(|id| ids.contains(id)).collect();
    let mut unique = mine.clone();
    unique.sort_by_key(ToString::to_string);
    unique.dedup();
    assert_eq!(unique.len(), mine.len(), "no member is answered twice");
    assert_eq!(mine.len(), kept.len(), "every live member is answered");
    for (_, id) in &kept {
        assert!(mine.contains(id), "{id} is answered");
    }

    let score = |id: &UploadId| {
        let mut command = redis::cmd("ZSCORE");
        command.arg("capsule:upload:progress").arg(id.to_string());
        command
    };
    for (_, id) in &dropped {
        let left: Option<f64> = score(id)
            .query_async(&mut connection)
            .await
            .expect("answers");
        assert!(
            left.is_none(),
            "{id}'s dead member is healed out of the view"
        );
    }
    harness
        .uploads
        .sweep_progress(harness.clock.now())
        .await
        .expect("the sweep answers");
    for (_, id) in &kept {
        let left: Option<f64> = score(id)
            .query_async(&mut connection)
            .await
            .expect("answers");
        assert!(left.is_some(), "{id}'s live member stays in the view");
    }
}

/// The whole counter suite, including the race, on one server.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_counter_suite_passes_against_valkey() {
    let Some(server) = server().await else {
        return;
    };
    let harness = ValkeyHarness::connect(&server.url).await;
    let store: Arc<dyn CounterStore> =
        Arc::new(ValkeyCounters::new(harness.stores.valkey().clone()));
    counters::run_all(store).await;
}

/// The pull worker's schedule, lease and circuit suite, against a real server (#964).
#[tokio::test]
async fn the_pull_job_suite_passes_against_valkey() {
    let Some(server) = server().await else {
        return;
    };
    let harness = ValkeyHarness::connect(&server.url).await;
    let store = capsule_server::federation::pull::valkey::ValkeyPullJobs::new(
        harness.stores.valkey().clone(),
    );
    capsule_server::federation::pull::conformance::run_all(&store).await;
}

/// Claims racing for one due job admit exactly one holder, decided on the server (#964).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pull_job_claims_race_to_one_holder() {
    use capsule_server::federation::PeerId;
    use capsule_server::federation::pull::valkey::ValkeyPullJobs;
    use capsule_server::federation::pull::{JobKey, PULL_LEASE, PullJobs};
    use capsule_server::store::AlbumId;

    let Some(server) = server().await else {
        return;
    };
    let harness = ValkeyHarness::connect(&server.url).await;
    let store = Arc::new(ValkeyPullJobs::new(harness.stores.valkey().clone()));
    let job = JobKey::new(
        PeerId::new("race.pull.test"),
        AlbumId::new(uuid::Uuid::now_v7().to_string()),
    );
    let now = Timestamp::now();
    let mut tasks = Vec::new();
    for replica in 0..24 {
        let store = store.clone();
        let job = job.clone();
        tasks.push(tokio::spawn(async move {
            store
                .claim(&job, &format!("replica-{replica}"), now, PULL_LEASE)
                .await
                .expect("the claim is answered")
                .is_some()
        }));
    }
    let mut admitted = 0;
    for task in tasks {
        if task.await.expect("the task finishes") {
            admitted += 1;
        }
    }
    assert_eq!(admitted, 1, "two replicas never pull one job");
}

/// The partition ceiling, against a real server (#519).
#[tokio::test]
async fn a_full_counter_partition_refuses_a_new_key_against_valkey() {
    let Some(server) = server().await else {
        return;
    };
    let harness = ValkeyHarness::connect(&server.url).await;
    let store =
        ValkeyCounters::new(harness.stores.valkey().clone()).with_ceiling(counters::CEILING);
    counters::a_full_partition_refuses_only_a_new_key_in_that_partition(&store).await;
}

/// The registration budget, on the key the register route charges, against a real server (#1001).
///
/// The limit admits, the next hit waits out one window, and a second source keeps its own budget.
#[tokio::test]
async fn the_registration_source_budget_holds_against_valkey() {
    let Some(server) = server().await else {
        return;
    };
    let harness = ValkeyHarness::connect(&server.url).await;
    let store = ValkeyCounters::new(harness.stores.valkey().clone());
    let budget = capsule_server::counter::budgets::REGISTRATION_SOURCE;
    let source = CounterKey::RegistrationSource("valkey-registration-203.0.113.7".to_owned());
    let other = CounterKey::RegistrationSource("valkey-registration-203.0.113.8".to_owned());
    for key in [&source, &other] {
        store.reset(key).await.expect("resets");
    }
    let at = Timestamp::UNIX_EPOCH;
    for _ in 0..budget.limit {
        assert!(
            store
                .hit(&source, budget, at)
                .await
                .expect("answers")
                .admits()
        );
    }
    assert_eq!(
        store.hit(&source, budget, at).await.expect("answers"),
        capsule_server::counter::Verdict::Limited {
            retry_after: at.checked_add(budget.window).expect("in range"),
        }
    );
    assert!(
        store
            .hit(&other, budget, at)
            .await
            .expect("answers")
            .admits()
    );
    for key in [&source, &other] {
        store.reset(key).await.expect("cleans up");
    }
}

/// First hits racing into one partition admit exactly its ceiling of new keys.
///
/// The check and the open are one script, so a burst of first hits cannot all read the same
/// under-ceiling occupancy — the race a read-then-`ZADD` adapter loses.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn first_hits_race_to_the_partition_ceiling() {
    let Some(server) = server().await else {
        return;
    };
    let harness = ValkeyHarness::connect(&server.url).await;
    let store: Arc<dyn CounterStore> = Arc::new(
        ValkeyCounters::new(harness.stores.valkey().clone()).with_ceiling(counters::CEILING),
    );
    let keys: Vec<CounterKey> = (0..24)
        .map(|n| CounterKey::DropSource(format!("valkey-race-ceiling-{n}")))
        .collect();
    for key in &keys {
        store.reset(key).await.expect("resets");
    }
    // Far past every other case's instants, so nothing they left in the partition is live.
    let at = jiff::Timestamp::from_second(4_000_000_000).expect("a valid instant");
    let budget = Budget::new(5, SignedDuration::from_mins(1));
    let mut tasks = tokio::task::JoinSet::new();
    for key in keys.clone() {
        let store = Arc::clone(&store);
        tasks.spawn(async move { store.hit(&key, budget, at).await });
    }
    let (mut admitted, mut refused) = (0, 0);
    while let Some(outcome) = tasks.join_next().await {
        match outcome.expect("a hit task completes") {
            Ok(verdict) if verdict.admits() => admitted += 1,
            Ok(verdict) => panic!("a first hit is never rate-limited: {verdict:?}"),
            Err(error) => {
                assert!(capsule_server::counter::is_at_capacity(&error), "{error}");
                refused += 1;
            }
        }
    }
    assert_eq!(
        (admitted, refused),
        (counters::CEILING, 24 - counters::CEILING)
    );
    for key in &keys {
        store.reset(key).await.expect("cleans up");
    }
}

/// Sixteen finalizers race for one session; exactly one wins.
///
/// The property `finalization_is_claimed_exactly_once` asserts in sequence, contested for real:
/// the compare and the set are one script, so no two callers can both read `pending`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn finalization_claims_race_to_one_winner() {
    let Some(server) = server().await else {
        return;
    };
    let harness = ValkeyHarness::connect(&server.url).await;
    let uploads = Arc::clone(&harness.uploads);
    let upload_id = UploadId::new("valkey-race-claim");
    let created_at = Timestamp::UNIX_EPOCH;
    uploads
        .open(UploadSessionRecord {
            upload_id: upload_id.clone(),
            asset_id: AssetId::new("valkey-race-asset"),
            owner_id: OwnerId::new("valkey-race-owner"),
            upload_user_id: UserId::new("valkey-race-uploader"),
            album_id: None,
            content_type: None,
            expected_hash: "c".repeat(64),
            crypto_suite_id: 1,
            protocol_version: "2026-08-29".to_owned(),
            blob_role: BlobRole::Original,
            intent_id: None,
            manifest_envelope: "{}".to_owned(),
            received_bytes: 0,
            total_size: 1,
            status: UploadSessionStatus::Pending,
            created_at,
            last_progress_at: created_at,
        })
        .await
        .expect("the session opens");

    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..16 {
        let uploads = Arc::clone(&uploads);
        let upload_id = upload_id.clone();
        tasks.spawn(async move { uploads.claim_finalize(&upload_id).await.expect("answers") });
    }
    let mut won = 0;
    let mut lost = 0;
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
    uploads.discard(&upload_id).await.expect("cleans up");
}

/// Sixteen callbacks race one `state`; exactly one gets the pending authorization (#460).
///
/// `an_oidc_authorization_is_single_use` asserts it in sequence. Contested for real, the read and
/// the removal are one script, so no two callbacks can both redeem one nonce and one verifier.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn oidc_callbacks_race_to_one_redemption() {
    use capsule_server::store::{OidcNonce, OidcState, PendingAuthorization, PkceVerifier};

    let Some(server) = server().await else {
        return;
    };
    let harness = ValkeyHarness::connect(&server.url).await;
    let store = Arc::clone(&harness.oidc_authorizations);
    let state = OidcState::new("valkey-race-oidc");
    store
        .begin(
            &state,
            PendingAuthorization {
                nonce: OidcNonce::new("valkey-race-nonce"),
                verifier: PkceVerifier::new("valkey-race-verifier"),
                redirect_uri: "http://127.0.0.1:4242/cb".to_owned(),
                binding_challenge: "valkey-race-binding".to_owned(),
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
    let mut redeemed = 0;
    let mut refused = 0;
    while let Some(taken) = tasks.join_next().await {
        match taken.expect("a callback task completes") {
            Some(_) => redeemed += 1,
            None => refused += 1,
        }
    }
    assert_eq!((redeemed, refused), (1, 15), "exactly one callback redeems");
}

/// Sixteen polls race one due device handle; exactly one claims the slot (#635).
///
/// `two_polls_racing_one_handle_claim_one_slot` asserts it for two. Contested for real, the read
/// of the schedule and its advance are one script, so one poll at most reaches the provider.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn device_polls_race_to_one_slot() {
    use capsule_server::store::{
        DeviceCode, DevicePollSlot, OidcDeviceHandle, PendingDeviceAuthorization,
    };

    let Some(server) = server().await else {
        return;
    };
    let harness = ValkeyHarness::connect(&server.url).await;
    let store = Arc::clone(&harness.oidc_devices);
    let handle = OidcDeviceHandle::new("valkey-race-device");
    store
        .begin(
            &handle,
            PendingDeviceAuthorization {
                device_code: DeviceCode::new("valkey-race-device-code"),
                binding_challenge: "valkey-race-binding".to_owned(),
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
    let mut due = 0;
    let mut too_soon = 0;
    while let Some(slot) = tasks.join_next().await {
        match slot.expect("a poll task completes") {
            DevicePollSlot::Due(_) => due += 1,
            DevicePollSlot::TooSoon { .. } => too_soon += 1,
            DevicePollSlot::Unknown => panic!("the ceremony is live"),
        }
    }
    assert_eq!((due, too_soon), (1, 15), "exactly one poll claims the slot");
}

/// A counter hit is charged and decided on the server: many tasks, one budget.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn counter_hits_race_to_the_budget() {
    let Some(server) = server().await else {
        return;
    };
    let harness = ValkeyHarness::connect(&server.url).await;
    let store: Arc<dyn CounterStore> =
        Arc::new(ValkeyCounters::new(harness.stores.valkey().clone()));
    let key = CounterKey::ShareLink("valkey-race-link".to_owned());
    store.reset(&key).await.expect("resets");
    let budget = Budget::new(5, SignedDuration::from_mins(1));
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..40 {
        let store = Arc::clone(&store);
        let key = key.clone();
        tasks.spawn(async move {
            store
                .hit(&key, budget, Timestamp::UNIX_EPOCH)
                .await
                .expect("answers")
                .admits()
        });
    }
    let mut admitted = 0;
    while let Some(admits) = tasks.join_next().await {
        if admits.expect("a hit task completes") {
            admitted += 1;
        }
    }
    assert_eq!(admitted, 5);
}

/// A durable boot with Valkey configured and no `DATABASE_URL` refuses, and names it.
///
/// **This case proved more before #402 and #403 met.** #403 wrote it to show the durable arm
/// reached Valkey *first* and then named the Postgres half as unwritten. Both halves exist now,
/// and the arm demands `DATABASE_URL`, opens the pool and checks the schema before it dials
/// Valkey at all — so with a live Valkey and no database this refuses on the configuration that
/// is absent, one step earlier than #403 wrote it for. Proving "Valkey was reached" now needs
/// *both* services up, which no single-container suite can arrange: `capsule_server::postgres`'s
/// container helper is `pub(crate)` and out of reach from an integration test. The Postgres-side
/// half of that proof is `boot::tests::postgres_conformance::
/// the_durable_arm_clears_postgres_and_refuses_on_an_unreachable_valkey`.
///
/// What it still proves is the property both lanes cared about most: a durable backend refuses
/// rather than silently falling back to the in-memory adapters.
#[tokio::test]
async fn a_durable_boot_without_a_database_url_refuses_and_names_it() {
    use std::collections::BTreeMap;

    use capsule_server::boot::{BootError, assemble};
    use capsule_server::config::{Config, Demands, Overrides};

    let Some(server) = server().await else {
        return;
    };
    let root = tempfile::tempdir().expect("a scratch directory");
    let environment: BTreeMap<String, String> = [
        ("BLOB_ROOT".to_owned(), root.path().display().to_string()),
        (
            "JWT_ED25519_DER".to_owned(),
            "MC4CAQAwBQYDK2VwBCIEIN6eTvXEL7xMZWHY8rTk7VbQSGSuRkle5MVfiiYUStLF".to_owned(),
        ),
        (
            "DATABASE_URL".to_owned(),
            "postgres://127.0.0.1:1/capsule".to_owned(),
        ),
        ("VALKEY_URL".to_owned(), server.url.clone()),
        (
            "ATTESTATION_KEY_SEED".to_owned(),
            base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                [9_u8; 64].as_slice(),
            ),
        ),
    ]
    .into_iter()
    .collect();
    let mut config = Config::load(&environment, &Overrides::default(), Demands::Serve)
        .expect("it is well-formed");
    // `Config::load` demands `DATABASE_URL` itself (#520); dropping it after loading reaches the
    // boot path's own backstop, which is what this case asserts.
    config.database_url = None;
    let error = assemble(&config)
        .await
        .expect_err("a durable backend with no database refuses");
    assert!(
        matches!(
            error,
            BootError::Missing {
                key: "DATABASE_URL"
            }
        ),
        "{error:?}"
    );
    assert!(format!("{error}").contains("DATABASE_URL"), "{error}");
    assert!(
        !format!("{error}").contains(&server.url),
        "a startup error must never carry a connection URL: {error}"
    );
}
