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

#![allow(clippy::unwrap_used)]

mod verify;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Once};

use auth::claims::Claims;
use base64::Engine as _;
use jiff::{SignedDuration, Timestamp};
use jsonwebtoken::{DecodingKey, EncodingKey};
use migration::Migrator;
use nanoid::nanoid;
use salvo::{Service, async_trait};
use sea_orm::{ActiveModelTrait, Database, DatabaseConnection, Set};
use sea_orm_migration::MigratorTrait;
use service::sync::{ChangeKind, FeedBlobManifest, FeedBlobRef, FeedEntryInput};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;
use tokio::sync::Notify;

use crate::config::MediaServerConfig;
use crate::service::verify::{BlobHasher, Clock, VerificationService};

static TRACING: Once = Once::new();

/// The protocol version the seeded feed entries pin (inside the default window).
const PROTOCOL: &str = "2026-05-31";

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

impl TestCtx {
    /// The production verification service over this context's blob tree.
    pub(crate) fn service(&self) -> VerificationService {
        VerificationService::new(self.upload_dir.clone())
    }

    /// A salvo service over the real `/storage/verify` router (auth + AppState wired).
    pub(crate) fn http_service(&self) -> Service {
        let config = MediaServerConfig {
            upload_dir: self.upload_dir.clone(),
            jwt_eddsa_decoding_key: self.decoding_key.clone(),
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

    TestCtx {
        _postgres: container,
        db,
        upload_dir,
        album_id: nanoid!(),
        user_id: nanoid!(),
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
