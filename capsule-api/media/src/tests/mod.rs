//! In-crate integration tests for slice `S-C3` (storage-verification endpoint). These run
//! against a real Postgres (testcontainer, mirroring the S-C1/S-C2 pattern) plus a real
//! on-disk content-addressed blob tree, seeding the `indexed` fact through the shared
//! `service::sync` finalization writer exactly as the upload server does.
//!
//! Coverage map — the storage-verification doc's six **unsigned-verdict** Validation
//! bullets, each an explicit test:
//! - `durable_verdict` — Durable verdict (smoke): every blob stored ∧ indexed ∧ retrievable.
//! - `partial_missing_verdict` — Partial / missing verdict: a never-finalized metadata blob.
//! - `mid_gc_blob` / `quarantined_blob` — Mid-GC / quarantined blob → not retrievable.
//! - `wrong_hash_declaration` — a hash the server does not hold, surfaced not omitted.
//! - `verify_before_destroy_signal` — the server half of the verify-before-destroy gate:
//!   the endpoint flips non-durable → durable as the missing blob finalizes.
//! - `deep_scan_detects_bitrot` — structural `stored=true` but `deep=true` catches a
//!   corrupted blob's hash mismatch.
//!
//! Plus the S-C3 pricing/seam guarantees, proven deterministically (no sleeps):
//! - `deep_scan_coalesces_concurrent_rehashes` — concurrent deep scans of one blob share a
//!   single re-hash (injected gated hasher; invocation count == 1).
//! - `deep_scan_is_rate_limited_per_user` — the per-user deep budget, driven by an injected
//!   `MockClock` window.
//! - `durable_blob_survives_release_window_via_gc_grace` — the GC-grace seam S-C11 consumes.

//!
//! The `drops` module (slice `S-C5`) carries its own harness (`MediaTestCtx`,
//! `drop_setup`) below — invariants 26–32, the seal→stage→adopt happy path, and the
//! adoption-atomicity rollback smoke.

#![allow(clippy::unwrap_used)]

mod attestation;
mod blob;
mod drops;
mod moderation;
mod shares;
mod verify;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Once};

use auth::claims::Claims;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_core::crypto::CRYPTO_SUITE_ID;
use capsule_core::crypto::hash::{Hash32, hash_bytes as hash32_bytes};
use capsule_core::crypto::keys::{AmkVersion, HybridSigningKey};
use capsule_core::crypto::provenance::action::Action;
use capsule_core::crypto::provenance::manifest::{
    ASSET_MANIFEST_VERSION, KeyMode, ManifestCore, WrappedFileKey,
};
use capsule_core::drop::{PassphraseVerifier, SealedDrop, seal_drop};
use jiff::{SignedDuration, Timestamp};
use jsonwebtoken::{DecodingKey, EncodingKey};
use migration::Migrator;
use nanoid::nanoid;
use salvo::{Service, async_trait};
use sea_orm::{ActiveModelTrait, Database, DatabaseConnection, Set};
use sea_orm_migration::MigratorTrait;
use service::drop::{Mutation as DropMutation, NewLink};
use service::quota::QuotaLimits;
use service::sync::{ChangeKind, FeedBlobManifest, FeedBlobRef, FeedEntryInput};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};
use testcontainers_modules::postgres::Postgres;
use tokio::sync::Notify;

use crate::config::MediaServerConfig;
use crate::drop_state::DropState;
use crate::service::verify::{BlobHasher, Clock, VerificationService};

static TRACING: Once = Once::new();

/// The protocol version the seeded feed entries pin (inside the default window).
pub(crate) const PROTOCOL: &str = "2026-05-31";

/// A base64 Ed25519 pkcs8 keypair (mirrors the auth/upload/sync harness) so tests mint
/// access tokens the media server accepts.
const PRIV_B64: &str = "MC4CAQAwBQYDK2VwBCIEIN6eTvXEL7xMZWHY8rTk7VbQSGSuRkle5MVfiiYUStLF";
const PUB_B64: &str = "MCowBQYDK2VwAyEA66iVaMz1x2ogToGm5Hw34aITBLLqz0iEonbwjK57pWU=";

fn decode_keys() -> (EncodingKey, DecodingKey) {
    let engine = base64::engine::general_purpose::STANDARD;
    let priv_bytes = engine.decode(PRIV_B64).expect("priv");
    let pub_bytes = engine.decode(PUB_B64).expect("pub");
    (
        EncodingKey::from_ed_der(&priv_bytes),
        DecodingKey::from_ed_der(&pub_bytes),
    )
}

pub(crate) struct TestCtx {
    _postgres: ContainerAsync<Postgres>,
    pub db: DatabaseConnection,
    pub upload_dir: PathBuf,
    pub album_id: String,
    pub user_id: String,
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
}

/// The shared test server identity + attestation seed. Matches the upload-crate test harness
/// so a receipt issued there verifies under this media server's keyring (one deployment).
pub(crate) const TEST_SERVER_ID: &str = "localhost";
pub(crate) const TEST_ATTESTATION_SEED: [u8; 64] = [7u8; 64];

impl TestCtx {
    /// The production verification service over this context's blob tree.
    pub(crate) fn service(&self) -> VerificationService {
        VerificationService::new(self.upload_dir.clone())
    }

    /// The shared attestation keyring for the test deployment (S-C15).
    pub(crate) fn attestation(&self) -> std::sync::Arc<service::attestation::AttestationKeyring> {
        std::sync::Arc::new(service::attestation::AttestationKeyring::new(
            TEST_SERVER_ID.to_string(),
            &TEST_ATTESTATION_SEED,
            Vec::new(),
        ))
    }

    /// The full media config for this context (S-C15 tests that need the attestation keyring).
    pub(crate) fn media_config(&self) -> MediaServerConfig {
        MediaServerConfig {
            server_id: TEST_SERVER_ID.to_string(),
            upload_dir: self.upload_dir.clone(),
            jwt_eddsa_decoding_key: self.decoding_key.clone(),
            valkey_url: String::new(),
            max_file_size: 8 * 1024 * 1024,
            protocol_min: "2026-01-01".to_string(),
            protocol_max: "2026-12-31".to_string(),
            allowed_content_types: vec!["image/jpeg".to_string()],
            timestamp_drift_days: 30,
            quota_limits: QuotaLimits::unlimited(),
            drop_rate_limit_max: 60,
            drop_rate_limit_window_secs: 60,
            attestation: self.attestation(),
        }
    }

    /// A salvo service over the S-C15 read surfaces mounted like the app: `/storage/verify`,
    /// `/assets/{id}/receipts`, and `/.well-known/capsule/attestation-keys`.
    pub(crate) fn s_c15_service(&self) -> Service {
        let state = crate::state::AppState::new(self.db.clone(), self.media_config());
        let router = salvo::Router::new()
            .push(
                salvo::Router::with_path("storage")
                    .push(crate::routes::get_storage_router(state.clone())),
            )
            .push(
                salvo::Router::with_path("assets")
                    .push(crate::routes::get_receipts_router(state.clone())),
            )
            .push(
                salvo::Router::with_path(".well-known").push(
                    salvo::Router::with_path("capsule")
                        .push(crate::routes::get_well_known_router(state)),
                ),
            );
        Service::new(router)
    }

    /// A salvo service over the real `/storage/verify` router (auth + AppState wired).
    pub(crate) fn http_service(&self) -> Service {
        let config = MediaServerConfig {
            server_id: TEST_SERVER_ID.to_string(),
            upload_dir: self.upload_dir.clone(),
            jwt_eddsa_decoding_key: self.decoding_key.clone(),
            // The verify router never opens drop sessions; these mirror drop_setup's
            // defaults so one config type serves both harnesses.
            valkey_url: String::new(),
            max_file_size: 8 * 1024 * 1024,
            protocol_min: "2026-01-01".to_string(),
            protocol_max: "2026-12-31".to_string(),
            allowed_content_types: vec!["image/jpeg".to_string()],
            timestamp_drift_days: 30,
            quota_limits: QuotaLimits::unlimited(),
            drop_rate_limit_max: 60,
            drop_rate_limit_window_secs: 60,
            attestation: self.attestation(),
        };
        let router =
            crate::routes::get_storage_router(crate::state::AppState::new(self.db.clone(), config));
        Service::new(router)
    }

    /// A bearer access token for the seeded uploader.
    pub(crate) fn token(&self) -> String {
        Claims::new_access_token(self.user_id.clone(), None)
            .encode(&self.encoding_key)
            .expect("encode token")
    }

    /// Compute the content address (lowercase hex SHA-256) of `bytes`.
    pub(crate) fn address(bytes: &[u8]) -> String {
        capsule_core::crypto::hash::hash_bytes(bytes).to_hex()
    }

    /// Write raw bytes to the content-addressed path for `hash` (bytes may or may not
    /// actually hash to `hash` — used to plant bit-rot).
    pub(crate) fn write_blob_bytes(&self, hash: &str, bytes: &[u8]) {
        let path = service::blob_store::blob_path(&self.upload_dir, hash);
        std::fs::create_dir_all(service::blob_store::blobs_dir(&self.upload_dir)).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    /// Record a committed feed entry referencing `hash` in `role` for `asset_id` — the
    /// `indexed` fact, minted exactly as the upload finalization transaction does.
    pub(crate) async fn index_blob(&self, asset_id: &str, role: &str, hash: &str, size: u64) {
        let blob_ref = FeedBlobRef {
            ciphertext_hash: hash.to_string(),
            role: role.to_string(),
            format: "image/jpeg".to_string(),
            size,
        };
        let (blobs, metadata_blob, original_held) = if role == "original" {
            (
                FeedBlobManifest {
                    original: Some(blob_ref),
                    derivatives: Vec::new(),
                },
                None,
                true,
            )
        } else {
            (
                FeedBlobManifest {
                    original: None,
                    derivatives: vec![blob_ref],
                },
                (role == "metadata").then(|| vec![0u8; size as usize]),
                false,
            )
        };
        let input = FeedEntryInput {
            album_id: self.album_id.clone(),
            protocol_version: PROTOCOL.to_string(),
            kind: ChangeKind::Created,
            asset_id: asset_id.to_string(),
            manifest_cbor: vec![0xa0],
            metadata_blob,
            blobs,
            original_held,
        };
        service::sync::Mutation::record_finalization(&self.db, input)
            .await
            .expect("record finalization");
    }

    /// Store `bytes` at their own content address **and** index them in `role`. Returns the
    /// content address. The common "fully durable blob" setup.
    pub(crate) async fn finalize_blob(&self, asset_id: &str, role: &str, bytes: &[u8]) -> String {
        let hash = Self::address(bytes);
        self.write_blob_bytes(&hash, bytes);
        self.index_blob(asset_id, role, &hash, bytes.len() as u64)
            .await;
        hash
    }

    /// A salvo service over the real `GET /blob/{hash}` router (slice `S-C10`; auth + AppState
    /// wired exactly as the app mounts it at `/blob`).
    pub(crate) fn blob_service(&self) -> Service {
        let state = crate::state::AppState::new(self.db.clone(), self.media_config());
        Service::new(crate::routes::get_blob_router(state))
    }

    /// Record a committed **original** feed reference with an explicit `original_held` — the
    /// awaiting-original (`false`) vs held (`true`) discriminator the serve endpoint reads. The
    /// blob bytes are written separately (or deliberately left absent for the staged/dangling
    /// cases).
    pub(crate) async fn index_original(
        &self,
        asset_id: &str,
        hash: &str,
        size: u64,
        original_held: bool,
    ) {
        let blob_ref = FeedBlobRef {
            ciphertext_hash: hash.to_string(),
            role: "original".to_string(),
            format: "image/jpeg".to_string(),
            size,
        };
        let input = FeedEntryInput {
            album_id: self.album_id.clone(),
            protocol_version: PROTOCOL.to_string(),
            kind: ChangeKind::Created,
            asset_id: asset_id.to_string(),
            manifest_cbor: vec![0xa0],
            metadata_blob: None,
            blobs: FeedBlobManifest {
                original: Some(blob_ref),
                derivatives: Vec::new(),
            },
            original_held,
        };
        service::sync::Mutation::record_finalization(&self.db, input)
            .await
            .expect("record finalization");
    }

    /// Seed the `assets` row for `asset_id` carrying the moderation `served` flag — the row
    /// the takedown gate (slice `S-C17`) reads on the content-addressed serve path. The
    /// key-free feed reference is seeded separately; this is the moderation state beside it.
    pub(crate) async fn seed_asset_row(&self, asset_id: &str, hash: &str, served: bool) {
        entity::asset::ActiveModel {
            id: Set(asset_id.to_string()),
            owner_id: Set(self.user_id.clone()),
            album_id: Set(Some(self.album_id.clone())),
            asset_type: Set(entity::asset::AssetType::Photo),
            file_size: Set(64),
            file_hash: Set(hash.to_string()),
            content_type: Set("image/jpeg".to_string()),
            is_stack_hidden: Set(false),
            uploaded: Set(true),
            upload_user_id: Set(self.user_id.clone()),
            uploaded_at: Set(entity::time::now_entity()),
            modified_at: Set(entity::time::now_entity().into()),
            deleted_at: Set(None),
            served: Set(served),
            ..Default::default()
        }
        .insert(&self.db)
        .await
        .expect("seed asset row");
    }

    /// Mark a blob's GC state — the seam the S-C11 GC worker owns and this endpoint reads.
    pub(crate) async fn mark_gc(
        &self,
        hash: &str,
        collectable_since: Option<Timestamp>,
        quarantined: bool,
    ) {
        entity::blob_gc::ActiveModel {
            content_hash: Set(hash.to_string()),
            collectable_since: Set(collectable_since.map(entity::time::ts_to_entity_tz)),
            quarantined: Set(quarantined),
        }
        .insert(&self.db)
        .await
        .expect("insert blob_gc row");
    }
}

pub(crate) async fn setup() -> TestCtx {
    TRACING.call_once(|| {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("info,sqlx=error,sea_orm=error")
            .with_test_writer()
            .try_init();
    });

    let container = Postgres::default()
        .with_tag("17")
        .start()
        .await
        .expect("start Postgres");
    let port = container.get_host_port_ipv4(5432).await.expect("pg port");
    let connection_string = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");

    let db = Database::connect(&connection_string)
        .await
        .expect("connect db");
    Migrator::refresh(&db).await.expect("migrate");

    let upload_dir = std::env::temp_dir().join(format!("capsule-media-test-{}", nanoid!()));
    std::fs::create_dir_all(&upload_dir).expect("mkdir upload dir");

    let (encoding_key, decoding_key) = decode_keys();

    // Seed user U, owner group id=U, member U∈U, album A owned by U — the rows the `assets`
    // foreign keys require, so a test can seed an asset row beside a feed reference (the
    // takedown gate's state, slice `S-C17`).
    let user_id = nanoid!();
    let album_id = nanoid!();
    let created = Timestamp::now() - SignedDuration::from_hours(24);
    entity::user::ActiveModel {
        id: Set(user_id.clone()),
        username: Set(format!("u{}", nanoid!(8))),
        name: Set(format!("Test {}", nanoid!(8))),
        email: Set(format!("{}@example.com", nanoid!(8))),
        account_verified: Set(true),
        needs_onboarding: Set(false),
        password_hash: Set(format!("hash-{}", nanoid!(12))),
        is_admin: Set(false),
        created_at: Set(entity::time::ts_to_entity(created)),
        modified_at: Set(entity::time::ts_to_entity(created)),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("insert user");
    entity::owner::ActiveModel {
        id: Set(user_id.clone()),
        created_at: Set(entity::time::ts_to_entity(created)),
    }
    .insert(&db)
    .await
    .expect("insert owner");
    entity::owner_member::ActiveModel {
        owner_id: Set(user_id.clone()),
        user_id: Set(user_id.clone()),
        created_at: Set(entity::time::ts_to_entity(created)),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("insert owner_member");
    entity::album::ActiveModel {
        id: Set(album_id.clone()),
        owner_id: Set(user_id.clone()),
        name: Set(format!("Album {}", nanoid!(6))),
        description: Set(String::new()),
        created_at: Set(entity::time::ts_to_entity(created)),
        modified_at: Set(entity::time::ts_to_entity(created)),
        deleted_at: Set(None),
    }
    .insert(&db)
    .await
    .expect("insert album");

    TestCtx {
        _postgres: container,
        db,
        upload_dir,
        album_id,
        user_id,
        encoding_key,
        decoding_key,
    }
}

// ─── Deterministic seams (mirrors the S-D7 Clock pattern) ────────────────────

/// A controllable clock: `now` advances only when the test says so.
#[derive(Clone)]
pub(crate) struct MockClock {
    now: Arc<std::sync::Mutex<Timestamp>>,
}

impl MockClock {
    pub(crate) fn new(start: Timestamp) -> Self {
        Self {
            now: Arc::new(std::sync::Mutex::new(start)),
        }
    }

    pub(crate) fn advance(&self, by: SignedDuration) {
        let mut guard = self.now.lock().unwrap();
        *guard = guard.checked_add(by).unwrap();
    }
}

impl Clock for MockClock {
    fn now(&self) -> Timestamp {
        *self.now.lock().unwrap()
    }
}

// The same controllable clock also drives the S-C4 share serve engine (rate windows + the
// fail-closed revocation-cache TTL), so both seams are proven without sleeps.
impl crate::service::share::Clock for MockClock {
    fn now(&self) -> Timestamp {
        *self.now.lock().unwrap()
    }
}

/// A [`BlobHasher`] that blocks inside the re-hash until released, counting invocations —
/// so concurrent-coalescing is provable without sleeps. It reports the blob as intact
/// (returns the content address embedded in the path's file stem).
#[derive(Clone)]
pub(crate) struct GatedHasher {
    count: Arc<AtomicUsize>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

impl GatedHasher {
    pub(crate) fn new() -> Self {
        Self {
            count: Arc::new(AtomicUsize::new(0)),
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        }
    }

    pub(crate) fn invocations(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }

    /// Wait until a re-hash has entered the hasher (is parked on the gate).
    pub(crate) async fn wait_entered(&self) {
        self.entered.notified().await;
    }

    /// Release all parked re-hashes.
    pub(crate) fn release(&self) {
        self.release.notify_waiters();
    }
}

#[async_trait]
impl BlobHasher for GatedHasher {
    async fn content_address(&self, path: &Path) -> std::io::Result<Option<String>> {
        self.count.fetch_add(1, Ordering::SeqCst);
        let notified = self.release.notified();
        self.entered.notify_waiters();
        notified.await;
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string);
        Ok(stem)
    }
}

// ── S-C5 drop-store harness ─────────────────────────────────────────────────

pub(crate) struct MediaTestCtx {
    _postgres: Option<ContainerAsync<Postgres>>,
    _valkey: Option<ContainerAsync<GenericImage>>,
    pub db: DatabaseConnection,
    pub config: MediaServerConfig,
    pub state: DropState,
    encoding_key: EncodingKey,
    /// The provisioning owner (the album owner; the quota-charged user).
    pub owner_id: String,
    pub album_id: String,
}

impl MediaTestCtx {
    /// Build a salvo service over the real drop routers, mounted like the app (`/u`, `/drops`).
    pub(crate) fn service(&self) -> Service {
        let link_router = crate::routes::get_drop_link_router(self.state.clone());
        let inbox_router = crate::routes::get_drops_router(self.state.clone());
        let router = salvo::Router::new()
            .push(salvo::Router::with_path("u").push(link_router))
            .push(salvo::Router::with_path("drops").push(inbox_router));
        Service::new(router)
    }

    /// A bearer access token for the provisioning owner.
    pub(crate) fn token(&self) -> String {
        Claims::new_access_token(self.owner_id.clone(), None)
            .encode(&self.encoding_key)
            .expect("encode token")
    }

    /// Register an upload link owned by the test owner, returning `(link_id, opaque_id)`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn create_link(
        &self,
        expires_at: Option<Timestamp>,
        max_total_bytes: Option<u64>,
        max_file_count: Option<u32>,
        max_file_size: Option<u64>,
        single_use: bool,
        passphrase_verifier: Option<serde_json::Value>,
    ) -> (String, String) {
        let opaque_id = hex_encode(&capsule_core::drop::generate_opaque_id());
        let link_id = DropMutation::create_link(
            &self.db,
            NewLink {
                owner_id: self.owner_id.clone(),
                opaque_id: opaque_id.clone(),
                album_hint: None,
                protocol_version: PROTOCOL.to_string(),
                crypto_suite_id: CRYPTO_SUITE_ID,
                expires_at,
                max_total_bytes,
                max_file_count,
                max_file_size,
                single_use,
                passphrase_verifier,
            },
        )
        .await
        .expect("create link");
        (link_id, opaque_id)
    }

    /// Revoke a link by id.
    pub(crate) async fn revoke_link(&self, link_id: &str) {
        DropMutation::revoke_link(&self.db, &self.owner_id, link_id)
            .await
            .expect("revoke link");
    }

    /// Seed an `assets` row attributed to the owner carrying `hash`/`size` (to pre-load quota).
    pub(crate) async fn seed_asset(&self, hash: &str, size: i64) {
        entity::asset::ActiveModel {
            id: Set(nanoid!()),
            owner_id: Set(self.owner_id.clone()),
            album_id: Set(Some(self.album_id.clone())),
            asset_type: Set(entity::asset::AssetType::Photo),
            file_size: Set(size),
            file_hash: Set(hash.to_string()),
            content_type: Set("image/jpeg".to_string()),
            is_stack_hidden: Set(false),
            uploaded: Set(true),
            upload_user_id: Set(self.owner_id.clone()),
            uploaded_at: Set(entity::time::now_entity()),
            modified_at: Set(entity::time::now_entity().into()),
            deleted_at: Set(None),
            ..Default::default()
        }
        .insert(&self.db)
        .await
        .expect("seed asset");
    }

    /// Set finite quota limits on the shared drop state's config for a quota test.
    pub(crate) async fn set_quota_limits(&mut self, soft: u64, hard: u64) {
        self.config.quota_limits = QuotaLimits {
            soft_limit: soft,
            hard_limit: hard,
            grace_window: SignedDuration::from_hours(24 * 14),
            per_peer_budget_ratio: 0.25,
        };
        self.rebuild_state().await;
    }

    /// Set a low drop-session rate limit for the invariant-31 test.
    pub(crate) async fn set_rate_limit(&mut self, max: u32) {
        self.config.drop_rate_limit_max = max;
        self.rebuild_state().await;
    }

    async fn rebuild_state(&mut self) {
        self.state = DropState::new(self.db.clone(), self.config.clone())
            .await
            .expect("rebuild drop state");
    }
}

pub(crate) async fn drop_setup() -> MediaTestCtx {
    TRACING.call_once(|| {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("info,sqlx=error,sea_orm=error")
            .with_test_writer()
            .try_init();
    });

    let (postgres_container, connection_string) =
        if let Ok(url) = std::env::var("TEST_DATABASE_URL") {
            (None, url)
        } else {
            let container = Postgres::default()
                .with_tag("17")
                .start()
                .await
                .expect("pg");
            let port = container.get_host_port_ipv4(5432).await.expect("pg port");
            (
                Some(container),
                format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres"),
            )
        };

    let db = Database::connect(&connection_string)
        .await
        .expect("connect");
    Migrator::refresh(&db).await.expect("migrate");

    let (valkey_container, valkey_url) = if let Ok(url) = std::env::var("TEST_VALKEY_URL") {
        (None, url)
    } else {
        let container = GenericImage::new("valkey/valkey", "8.0.1")
            .with_exposed_port(testcontainers::core::ContainerPort::Tcp(6379))
            .with_wait_for(testcontainers::core::WaitFor::message_on_stdout(
                "Ready to accept connections",
            ))
            .start()
            .await
            .expect("valkey");
        let port = container
            .get_host_port_ipv4(6379)
            .await
            .expect("valkey port");
        (Some(container), format!("redis://127.0.0.1:{port}"))
    };

    let (encoding_key, decoding_key) = decode_keys();

    let upload_dir = std::env::temp_dir().join(format!("capsule-drop-test-{}", nanoid!()));
    std::fs::create_dir_all(&upload_dir).expect("mkdir");

    let config = MediaServerConfig {
        server_id: "localhost".to_string(),
        upload_dir,
        jwt_eddsa_decoding_key: decoding_key,
        valkey_url,
        max_file_size: 8 * 1024 * 1024,
        protocol_min: "2026-01-01".to_string(),
        protocol_max: "2026-12-31".to_string(),
        allowed_content_types: vec![
            "image/jpeg".to_string(),
            "image/png".to_string(),
            "application/octet-stream".to_string(),
        ],
        timestamp_drift_days: 30,
        quota_limits: QuotaLimits::unlimited(),
        drop_rate_limit_max: 60,
        drop_rate_limit_window_secs: 60,
        attestation: std::sync::Arc::new(service::attestation::AttestationKeyring::new(
            "localhost".to_string(),
            &[7u8; 64],
            Vec::new(),
        )),
    };

    // Seed owner U, owner group id=U, member U∈U, album A owned by U.
    let owner_id = nanoid!();
    let created = Timestamp::now() - SignedDuration::from_hours(24);
    entity::user::ActiveModel {
        id: Set(owner_id.clone()),
        username: Set(format!("u{}", nanoid!(8))),
        name: Set(format!("Test {}", nanoid!(8))),
        email: Set(format!("{}@example.com", nanoid!(8))),
        account_verified: Set(true),
        needs_onboarding: Set(false),
        password_hash: Set(format!("hash-{}", nanoid!(12))),
        is_admin: Set(false),
        created_at: Set(entity::time::ts_to_entity(created)),
        modified_at: Set(entity::time::ts_to_entity(created)),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("insert user");
    entity::owner::ActiveModel {
        id: Set(owner_id.clone()),
        created_at: Set(entity::time::ts_to_entity(created)),
    }
    .insert(&db)
    .await
    .expect("insert owner");
    entity::owner_member::ActiveModel {
        owner_id: Set(owner_id.clone()),
        user_id: Set(owner_id.clone()),
        created_at: Set(entity::time::ts_to_entity(created)),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("insert owner_member");
    let album_id = nanoid!();
    entity::album::ActiveModel {
        id: Set(album_id.clone()),
        owner_id: Set(owner_id.clone()),
        name: Set(format!("Album {}", nanoid!(6))),
        description: Set(String::new()),
        created_at: Set(entity::time::ts_to_entity(created)),
        modified_at: Set(entity::time::ts_to_entity(created)),
        deleted_at: Set(None),
    }
    .insert(&db)
    .await
    .expect("insert album");

    let state = DropState::new(db.clone(), config.clone())
        .await
        .expect("drop state");

    MediaTestCtx {
        _postgres: postgres_container,
        _valkey: valkey_container,
        db,
        config,
        state,
        encoding_key,
        owner_id,
        album_id,
    }
}

/// Seal a drop under a fresh Drop Key and return the sealed bytes plus the JSON create body.
pub(crate) fn seal_and_body(
    content_type: &str,
    plaintext: &[u8],
) -> (SealedDrop, serde_json::Value) {
    let drop_key = capsule_core::crypto::keys::DekKeypair::generate();
    let sealed = seal_drop(plaintext, &drop_key.public_bytes(), content_type).unwrap();
    let body = create_body_from(&sealed);
    (sealed, body)
}

/// Build a valid `CreateDropRequest` JSON body from a sealed drop.
pub(crate) fn create_body_from(sealed: &SealedDrop) -> serde_json::Value {
    let d = &sealed.descriptor;
    serde_json::json!({
        "size": sealed.ciphertext.len(),
        "passphrase_proof": serde_json::Value::Null,
        "descriptor": {
            "content_type": d.content_type,
            "plaintext_size": d.plaintext_size,
            "chunk_size": d.chunk_size,
            "nonce_prefix": hex_encode(&d.nonce_prefix),
            "ciphertext_hash": d.ciphertext_hash.to_hex(),
            "kem_ct": BASE64.encode(&d.kem_ct),
            "suggested_filename": d.suggested_filename,
        }
    })
}

/// SHA-256 of `bytes` as bare lowercase hex — the wire checksum / content-hash format.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    capsule_core::utils::hash::hash_bytes(bytes)
}

/// The `error.*` code carried in a rejection's JSON body.
pub(crate) fn error_code(body: &serde_json::Value) -> Option<&str> {
    body.get("code").and_then(serde_json::Value::as_str)
}

/// Lowercase-hex encode.
pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Build an Argon2id passphrase verifier (fast test params) and its wire proof (hex).
pub(crate) fn passphrase_verifier(pw: &str) -> (serde_json::Value, String) {
    let params = capsule_core::crypto::primitives::Argon2Params {
        mem_kib: 64,
        t_cost: 1,
        p_cost: 1,
    };
    let v = PassphraseVerifier::derive(pw, params).unwrap();
    let proof = hex_encode(&v.verifier);
    (serde_json::to_value(&v).unwrap(), proof)
}

/// Build a base64 signed `create` manifest for adoption referencing `ciphertext_hash`, binding
/// `metadata_blob`. The server never verifies the signatures (no keys); it needs a structurally
/// valid, decodable, envelope-passing manifest.
pub(crate) fn adopt_manifest_cbor(
    _ctx: &MediaTestCtx,
    ciphertext_hash: &Hash32,
    metadata_blob: &[u8],
) -> String {
    let device = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
    let write = HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32]);
    let core = ManifestCore {
        version: ASSET_MANIFEST_VERSION.into(),
        crypto_suite_id: CRYPTO_SUITE_ID,
        protocol_version: PROTOCOL.into(),
        file_id: uuid::Uuid::now_v7(),
        album_id: uuid::Uuid::now_v7(),
        amk_version: AmkVersion(1),
        ciphertext_hash: *ciphertext_hash,
        plaintext_size: 64,
        chunk_size: 65536,
        nonce_prefix: [0u8; 7],
        key_mode: KeyMode::Wrapped,
        wrapped_file_key: Some(WrappedFileKey(vec![0u8; 48])),
        metadata_blob_hash: Some(hash32_bytes(metadata_blob)),
        created_by_user: uuid::Uuid::now_v7(),
        created_by_device: uuid::Uuid::now_v7(),
        client_version: "capsule-test/1.0".into(),
        timestamp: Timestamp::now().to_string(),
        action: Action::Create,
        prior_provenance_hash: None,
        retention_until: None,
    };
    let manifest = core.sign(&device, &write).unwrap();
    BASE64.encode(capsule_core::cbor::to_canonical_vec(&manifest).unwrap())
}
