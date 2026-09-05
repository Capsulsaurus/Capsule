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
use capsule_server::store::conformance::{self, Harness};
use capsule_server::store::memory::ManualClock;
use capsule_server::store::upload::{
    BlobRole, FinalizeClaim, UploadSessionRecord, UploadSessionStatus, UploadSessionStore,
};
use capsule_server::store::valkey::ValkeyStores;
use capsule_server::store::{
    AssetId, AuthStateStore, ChallengeStore, ChannelStore, Clock, CohortStore, EnrollmentStore,
    OwnerId, SessionId, SessionRecord, StoreError, StoreFuture, UploadId, UserId,
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
            stores,
        }
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

/// The durable boot arm reaches Valkey first, and then names the half that is not written.
#[tokio::test]
async fn the_durable_arm_reaches_valkey_and_then_names_the_postgres_half() {
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
    let config = Config::load(&environment, &Overrides::default(), Demands::Serve)
        .expect("it is well-formed");
    let error = assemble(&config)
        .await
        .expect_err("the Postgres half is not written");
    assert!(
        matches!(
            error,
            BootError::AdapterUnavailable {
                key: "DATABASE_URL",
                ..
            }
        ),
        "{error:?}"
    );
    assert!(format!("{error}").contains("#402"), "{error}");
}
