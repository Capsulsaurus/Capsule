//! In-crate integration tests for slice `S-C1` (upload-server hardening). These run against
//! a real server (salvo `TestClient` over the actual router) backed by testcontainer
//! Postgres + Valkey, and — for the discard/scrub/CAS/replay-store internals that are not
//! reachable over HTTP — against the crate-internal services directly.
//!
//! Coverage map:
//! - `invariants` — a rejecting test for each server-side invariant 1–15, and one per row of
//!   the upload-protocol Strictness Table, asserting BOTH the HTTP status and the `error.*`
//!   code; plus the idempotency (replay + dedup) tests.
//! - `lifecycle` — the session-lifecycle smoke, the discard survival-floor test, the startup
//!   scrub, and the crash-injection recovery tests.

#![allow(clippy::unwrap_used)]

mod invariants;
mod lifecycle;
mod quota;
mod sdk_client;
mod sync_feed;

use std::path::PathBuf;
use std::sync::Once;

use auth::claims::Claims;
use environment::wrapper::SecretKeyWrapper;
use jiff::{SignedDuration, Timestamp};
use jsonwebtoken::{DecodingKey, EncodingKey};
use migration::Migrator;
use nanoid::nanoid;
use salvo::Service;
use sea_orm::{ActiveModelTrait, Database, DatabaseConnection, Set};
use sea_orm_migration::MigratorTrait;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};
use testcontainers_modules::postgres::Postgres;

use crate::config::{
    DEFAULT_CONTENT_TYPES, DEFAULT_DRIFT_DAYS, DEFAULT_PROTOCOL_MAX, DEFAULT_PROTOCOL_MIN,
    DEFAULT_QUOTA_GRACE_DAYS, DEFAULT_QUOTA_HARD_LIMIT, DEFAULT_QUOTA_SOFT_LIMIT,
    UploadServerConfig,
};
use crate::service::discard::DiscardService;
use crate::service::storage::StorageService;
use crate::service::upload::UploadService;
use crate::session::UploadSessionManager;
use crate::state::AppState;

static TRACING: Once = Once::new();

/// The protocol version the tests speak (inside the default `[min, max]` window).
pub(crate) const PROTOCOL: &str = "2026-05-31";

/// A base64 Ed25519 pkcs8 keypair (mirrors the auth test harness) so we can mint access
/// tokens the upload server accepts.
const PRIV_B64: &str = "MC4CAQAwBQYDK2VwBCIEIN6eTvXEL7xMZWHY8rTk7VbQSGSuRkle5MVfiiYUStLF";
const PUB_B64: &str = "MCowBQYDK2VwAyEA66iVaMz1x2ogToGm5Hw34aITBLLqz0iEonbwjK57pWU=";

pub(crate) struct TestCtx {
    _postgres: Option<ContainerAsync<Postgres>>,
    _valkey: Option<ContainerAsync<GenericImage>>,
    pub db: DatabaseConnection,
    pub config: UploadServerConfig,
    pub session_manager: UploadSessionManager,
    pub storage: StorageService,
    pub upload_service: UploadService,
    encoding_key: EncodingKey,
    /// The seeded uploader; also used as the owner-group id and album owner.
    pub user_id: String,
    pub album_id: String,
    pub upload_dir: PathBuf,
}

impl TestCtx {
    /// Build a salvo service over the real upload router (with the protocol gate hooped).
    pub(crate) fn service(&self) -> Service {
        let state = AppState::new(
            self.db.clone(),
            self.config.clone(),
            self.upload_service.clone(),
        );
        let inner = crate::routes::get_router(
            state,
            self.config.protocol_min.clone(),
            self.config.protocol_max.clone(),
        );
        // Mount under `/upload` to mirror the real app's `Router::with_path("upload")`.
        let router = salvo::Router::new().push(salvo::Router::with_path("upload").push(inner));
        Service::new(router)
    }

    pub(crate) fn discard(&self) -> DiscardService {
        DiscardService::new(
            self.session_manager.clone(),
            self.storage.clone(),
            self.db.clone(),
        )
    }

    /// A bearer access token for the seeded uploader.
    pub(crate) fn token(&self) -> String {
        Claims::new_access_token(self.user_id.clone(), None)
            .encode(&self.encoding_key)
            .expect("encode token")
    }

    /// Rebuild the config + upload service with finite quota limits (S-C6 tests). The router
    /// built by [`Self::service`] then enforces these limits at session creation.
    pub(crate) fn set_quota_limits(&mut self, soft_limit: u64, hard_limit: u64) {
        self.config.quota_soft_limit = soft_limit;
        self.config.quota_hard_limit = hard_limit;
        self.upload_service = UploadService::new(
            self.config.clone(),
            self.storage.clone(),
            self.session_manager.clone(),
            self.db.clone(),
        );
    }

    /// Seed a bare `users` row (no owner group) and return its id — a second uploader for the
    /// dedup-attribution test.
    pub(crate) async fn seed_user(&self) -> String {
        let id = nanoid!();
        let created = Timestamp::now() - SignedDuration::from_hours(24);
        entity::user::ActiveModel {
            id: Set(id.clone()),
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
        .insert(&self.db)
        .await
        .expect("insert second user");
        id
    }

    /// Seed one `assets` row owned by the seeded owner group, attributed to `uploader`,
    /// carrying content `hash` of `size` bytes. `uploaded_at` sets the first-uploader
    /// ordering for content-hash dedup attribution.
    pub(crate) async fn seed_asset(
        &self,
        uploader: &str,
        hash: &str,
        size: i64,
        uploaded: bool,
        uploaded_at: Timestamp,
    ) -> String {
        let id = nanoid!();
        entity::asset::ActiveModel {
            id: Set(id.clone()),
            owner_id: Set(self.user_id.clone()),
            album_id: Set(Some(self.album_id.clone())),
            width: Set(0),
            height: Set(0),
            asset_type: Set(entity::asset::AssetType::Photo),
            original_filename: Set(nanoid!()),
            file_size: Set(size),
            file_hash: Set(hash.to_string()),
            content_type: Set("image/jpeg".to_string()),
            is_favorite: Set(false),
            is_stack_hidden: Set(false),
            uploaded: Set(uploaded),
            upload_user_id: Set(uploader.to_string()),
            uploaded_at: Set(entity::time::ts_to_entity(uploaded_at)),
            modified_at: Set(entity::time::ts_to_entity_tz(uploaded_at)),
            deleted_at: Set(None),
            ..Default::default()
        }
        .insert(&self.db)
        .await
        .expect("insert asset");
        id
    }
}

fn decode_keys() -> (EncodingKey, DecodingKey) {
    use base64::Engine;
    let engine = base64::engine::general_purpose::STANDARD;
    let priv_bytes = engine.decode(PRIV_B64).expect("priv");
    let pub_bytes = engine.decode(PUB_B64).expect("pub");
    (
        EncodingKey::from_ed_der(&priv_bytes),
        DecodingKey::from_ed_der(&pub_bytes),
    )
}

/// Spin up fresh Postgres + Valkey containers, run migrations, seed a user/owner/album, and
/// build the upload services. Each test gets an isolated Valkey (the progress index is
/// global) and its own on-disk upload directory.
pub(crate) async fn setup() -> TestCtx {
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
                .expect("start Postgres");
            let port = container.get_host_port_ipv4(5432).await.expect("pg port");
            (
                Some(container),
                format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres"),
            )
        };

    let db = Database::connect(&connection_string)
        .await
        .expect("connect db");
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
            .expect("start Valkey");
        let port = container
            .get_host_port_ipv4(6379)
            .await
            .expect("valkey port");
        (Some(container), format!("redis://127.0.0.1:{port}"))
    };

    let (encoding_key, decoding_key) = decode_keys();

    let upload_dir = std::env::temp_dir().join(format!("capsule-upload-test-{}", nanoid!()));
    std::fs::create_dir_all(&upload_dir).expect("mkdir upload dir");

    let config = UploadServerConfig {
        host: "127.0.0.1".to_string(),
        port: 0,
        domain: "localhost".to_string(),
        upload_dir: upload_dir.clone(),
        max_file_size: 8 * 1024 * 1024,
        max_cache_size: 128 * 1024 * 1024,
        valkey_url: valkey_url.clone(),
        jwt_eddsa_decoding_key: SecretKeyWrapper::from(decoding_key),
        allowed_origins: vec!["*".to_string()],
        protocol_min: DEFAULT_PROTOCOL_MIN.to_string(),
        protocol_max: DEFAULT_PROTOCOL_MAX.to_string(),
        allowed_content_types: DEFAULT_CONTENT_TYPES
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        timestamp_drift_days: DEFAULT_DRIFT_DAYS,
        quota_soft_limit: DEFAULT_QUOTA_SOFT_LIMIT,
        quota_hard_limit: DEFAULT_QUOTA_HARD_LIMIT,
        quota_grace_days: DEFAULT_QUOTA_GRACE_DAYS,
        quota_per_peer_budget_ratio: service::quota::DEFAULT_PER_PEER_BUDGET_RATIO,
    };

    let session_manager = UploadSessionManager::new(&valkey_url)
        .await
        .expect("session manager");
    let storage = StorageService::new(config.clone());
    let upload_service = UploadService::new(
        config.clone(),
        storage.clone(),
        session_manager.clone(),
        db.clone(),
    );

    // Seed user U, owner group id=U (owner_member U∈U), and album A owned by U. Keying the
    // owner group on the user id keeps get_album_access(U, A) and the asset.owner_id FK
    // consistent for a solo uploader.
    let user_id = nanoid!();
    let created = Timestamp::now() - SignedDuration::from_hours(24);
    let user = entity::user::ActiveModel {
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
    };
    user.insert(&db).await.expect("insert user");

    let owner = entity::owner::ActiveModel {
        id: Set(user_id.clone()),
        created_at: Set(entity::time::ts_to_entity(created)),
    };
    owner.insert(&db).await.expect("insert owner");

    let member = entity::owner_member::ActiveModel {
        owner_id: Set(user_id.clone()),
        user_id: Set(user_id.clone()),
        created_at: Set(entity::time::ts_to_entity(created)),
        ..Default::default()
    };
    member.insert(&db).await.expect("insert owner_member");

    let album_id = nanoid!();
    let album = entity::album::ActiveModel {
        id: Set(album_id.clone()),
        owner_id: Set(user_id.clone()),
        name: Set(format!("Album {}", nanoid!(6))),
        description: Set(String::new()),
        created_at: Set(entity::time::ts_to_entity(created)),
        modified_at: Set(entity::time::ts_to_entity(created)),
        deleted_at: Set(None),
    };
    album.insert(&db).await.expect("insert album");

    TestCtx {
        _postgres: postgres_container,
        _valkey: valkey_container,
        db,
        config,
        session_manager,
        storage,
        upload_service,
        encoding_key,
        user_id,
        album_id,
        upload_dir,
    }
}

/// SHA-256 of `bytes` as bare lowercase hex — the wire checksum / content-hash format.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    capsule_core::utils::hash::hash_bytes(bytes)
}

/// A canonical, envelope-consistent `POST /upload` JSON body for `album_id`, declaring a
/// blob of `size` bytes with content hash `hash`. Tests clone this and mutate one field to
/// drive a specific rejection.
pub(crate) fn valid_create_body(album_id: &str, hash: &str, size: u64) -> serde_json::Value {
    let timestamp = Timestamp::now().to_string();
    serde_json::json!({
        "size": size,
        "hash": hash,
        "content_type": "image/jpeg",
        "crypto_suite_id": 1,
        "protocol_version": PROTOCOL,
        "blob_role": "original",
        "album_id": album_id,
        "manifest_envelope": {
            "crypto_suite_id": 1,
            "protocol_version": PROTOCOL,
            "album_id": album_id,
            "file_id": nanoid!(),
            "amk_version": 1,
            "ciphertext_hash": hash,
            "plaintext_size": size,
            "chunk_size": 65536,
            "key_mode": "derived",
            "created_by_user": nanoid!(),
            "created_by_device": nanoid!(),
            "client_version": "capsule-test/1.0",
            "timestamp": timestamp,
            "action": "create",
        }
    })
}

/// The `error.*` code carried in a rejection's JSON body.
pub(crate) fn error_code(body: &serde_json::Value) -> Option<&str> {
    body.get("code").and_then(serde_json::Value::as_str)
}
