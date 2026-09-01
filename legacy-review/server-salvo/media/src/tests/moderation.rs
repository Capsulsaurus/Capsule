//! Moderation hooks (slice `S-C8`) — the Moderation design doc's Validation bullets that live
//! at the service + serving layers (the sixth, suspension enforcement, is proven against the
//! real upload server in the upload crate's `moderation` tests):
//!
//! - `federated_report_transport` — Federated report transport (smoke): a signed report from a
//!   peer reaches the admin queue with structured metadata.
//! - `blocklist_refuses_federated_requests` — Blocklist enforcement (smoke): a blocklisted
//!   peer's federated requests (pull + report) are refused.
//! - `takedown_serves_410_and_records_provenance` — Takedown serving (smoke): a taken-down
//!   asset fetches `410` **on the content-addressed `GET /blob/{hash}` path** (slice `S-C17` —
//!   the real client/federation fetch path), its blob is preserved, and a moderation
//!   provenance record is visible in the user's audit log.
//! - `federated_report_authentication` — Federated-report authentication (unit): a valid-peer
//!   signature reaches the queue; unsigned / invalid-signature / unknown-peer reports are
//!   dropped; the per-`(reporting_server, reported_user)` rate limit applies backpressure
//!   (threat-model invariant 24).
//! - `per_user_block_is_scoped` — Block scoping (unit): a per-user block removes the blocked
//!   user from the blocker's shared albums, and does **not** appear as a server-level
//!   federation block against the blocked user's home server.

#![allow(clippy::unwrap_used)]

use capsule_core::cbor;
use ed25519_dalek::{Signer, SigningKey};
use jiff::{SignedDuration, Timestamp};
use migration::Migrator;
use nanoid::nanoid;
use salvo::Service;
use salvo::http::StatusCode;
use salvo::test::TestClient;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Database, DatabaseConnection, EntityTrait, PaginatorTrait,
    QueryFilter, Set,
};
use sea_orm_migration::MigratorTrait;
use service::moderation::report::{FEDERATED_REPORT_VERSION, ReportCore, SignedReport};
use service::moderation::{AuditLog, Blocklist, ModerationLimits, Report, Takedown};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;

use super::{PROTOCOL, TEST_SERVER_ID};

struct ModCtx {
    _postgres: ContainerAsync<Postgres>,
    db: DatabaseConnection,
    upload_dir: std::path::PathBuf,
    /// The seeded local user — owner + blocker + reported/takedown subject.
    user_id: String,
    album_id: String,
}

impl ModCtx {
    /// A salvo service over the real key-free blob router (`GET /blob/{hash}`) — the
    /// content-addressed path the takedown gate protects since slice `S-C17`.
    fn blob_service(&self) -> Service {
        let config = crate::config::MediaServerConfig {
            server_id: TEST_SERVER_ID.to_string(),
            upload_dir: self.upload_dir.clone(),
            jwt_eddsa_decoding_key: super::decode_keys().1,
            valkey_url: String::new(),
            max_file_size: 8 * 1024 * 1024,
            protocol_min: "2026-01-01".to_string(),
            protocol_max: "2026-12-31".to_string(),
            allowed_content_types: vec!["image/jpeg".to_string()],
            timestamp_drift_days: 30,
            quota_limits: service::quota::QuotaLimits::unlimited(),
            drop_rate_limit_max: 60,
            drop_rate_limit_window_secs: 60,
            attestation: std::sync::Arc::new(service::attestation::AttestationKeyring::new(
                TEST_SERVER_ID.to_string(),
                &[7u8; 64],
                Vec::new(),
            )),
            operational_public_key: None,
            deprecations: Vec::new(),
            deprecation_announcement_days: 90,
        };
        let state = crate::state::AppState::new(self.db.clone(), config);
        Service::new(crate::routes::get_blob_router(state))
    }

    /// A bearer access token for the seeded local user (blob serving is session-authenticated).
    fn token(&self) -> String {
        auth::claims::Claims::new_access_token(self.user_id.clone(), None)
            .encode(&super::decode_keys().0)
            .expect("encode token")
    }

    /// Record the committed feed reference that makes `hash` a *served* content address for
    /// `asset_id` — the `indexed` fact, minted exactly as upload finalization does.
    async fn index_original(&self, asset_id: &str, hash: &str, size: u64) {
        service::sync::Mutation::record_finalization(
            &self.db,
            service::sync::FeedEntryInput {
                album_id: self.album_id.clone(),
                protocol_version: PROTOCOL.to_string(),
                kind: service::sync::ChangeKind::Created,
                asset_id: asset_id.to_string(),
                manifest_cbor: vec![0xa0],
                metadata_blob: None,
                blobs: service::sync::FeedBlobManifest {
                    original: Some(service::sync::FeedBlobRef {
                        ciphertext_hash: hash.to_string(),
                        role: "original".to_string(),
                        format: "image/jpeg".to_string(),
                        size,
                    }),
                    derivatives: Vec::new(),
                },
                original_held: true,
            },
        )
        .await
        .expect("record finalization");
    }

    /// Seed an `assets` row owned by the local user (`id` is a nanoid, matching the `char(21)`
    /// primary key). The takedown gate runs before the legacy serve path's own id/disk logic.
    async fn seed_asset(&self, id: &str, hash: &str, served: bool) {
        entity::asset::ActiveModel {
            id: Set(id.to_string()),
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
        .unwrap();
    }

    /// Seed a second local user and return its id (a share recipient / blocked user).
    async fn seed_user(&self) -> String {
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
        .unwrap();
        id
    }

    /// Share the seeded album with `user_id` (a view share). Returns the share id.
    async fn share_album_with(&self, user_id: &str) -> String {
        let share = entity::album_share::ActiveModel {
            album_id: Set(self.album_id.clone()),
            user_id: Set(user_id.to_string()),
            permission: Set(entity::album_share::SharePermission::View),
            created_at: Set(entity::time::now_entity()),
            ..Default::default()
        }
        .insert(&self.db)
        .await
        .unwrap();
        share.id
    }
}

async fn mod_setup() -> ModCtx {
    let container = Postgres::default().with_tag("17").start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let db = Database::connect(format!(
        "postgres://postgres:postgres@127.0.0.1:{port}/postgres"
    ))
    .await
    .unwrap();
    Migrator::refresh(&db).await.unwrap();

    let upload_dir = std::env::temp_dir().join(format!("capsule-moderation-test-{}", nanoid!()));
    std::fs::create_dir_all(&upload_dir).unwrap();

    // Seed user U, owner group id=U, member U∈U, album A owned by U.
    let user_id = nanoid!();
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
    .unwrap();
    entity::owner::ActiveModel {
        id: Set(user_id.clone()),
        created_at: Set(entity::time::ts_to_entity(created)),
    }
    .insert(&db)
    .await
    .unwrap();
    entity::owner_member::ActiveModel {
        owner_id: Set(user_id.clone()),
        user_id: Set(user_id.clone()),
        created_at: Set(entity::time::ts_to_entity(created)),
        ..Default::default()
    }
    .insert(&db)
    .await
    .unwrap();
    let album_id = nanoid!();
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
    .unwrap();

    ModCtx {
        _postgres: container,
        db,
        upload_dir,
        user_id,
        album_id,
    }
}

/// Build a signed report from `peer` (with signing key `key`) against `reported_user`.
fn signed_report(
    key: &SigningKey,
    peer: &str,
    reported_user: &str,
    content_hash: &str,
) -> SignedReport {
    let core = ReportCore {
        version: FEDERATED_REPORT_VERSION.to_string(),
        reporting_server: peer.to_string(),
        reported_user: reported_user.to_string(),
        content_hash: content_hash.to_string(),
        album_pointer: Some("album-42".to_string()),
        reason: Some("verified-csam".to_string()),
        issued_at: Timestamp::now().to_string(),
    };
    let bytes = cbor::to_canonical_vec(&core).unwrap();
    let signature = key.sign(&bytes).to_bytes().to_vec();
    SignedReport { core, signature }
}

// ─────────────────────────────────────────────────────────────────────────────
// Validation bullet 1 — Federated report transport (smoke).
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn federated_report_transport() {
    let ctx = mod_setup().await;
    let key = SigningKey::from_bytes(&[3u8; 32]);
    Report::register_peer(&ctx.db, "other.tld", &key.verifying_key().to_bytes())
        .await
        .unwrap();

    let hash = "a".repeat(64);
    let report = signed_report(&key, "other.tld", &ctx.user_id, &hash);
    let id = Report::intake(
        &ctx.db,
        &report,
        &ModerationLimits::default(),
        Timestamp::now(),
    )
    .await
    .expect("a valid signed report reaches the admin queue");

    // It reached the queue carrying structured metadata (content hash + album pointer, never
    // plaintext).
    let queue = Report::queue_for_user(&ctx.db, &ctx.user_id).await.unwrap();
    assert_eq!(queue.len(), 1, "the report must be in the admin queue");
    let row = &queue[0];
    assert_eq!(row.id, id);
    assert_eq!(row.reporting_server, "other.tld");
    assert_eq!(row.reported_user, ctx.user_id);
    assert_eq!(row.content_hash, hash);
    assert_eq!(row.album_pointer.as_deref(), Some("album-42"));
}

// ─────────────────────────────────────────────────────────────────────────────
// Validation bullet 2 — Blocklist enforcement (smoke).
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn blocklist_refuses_federated_requests() {
    let ctx = mod_setup().await;
    let key = SigningKey::from_bytes(&[4u8; 32]);
    Report::register_peer(&ctx.db, "evil.tld", &key.verifying_key().to_bytes())
        .await
        .unwrap();

    // Before the block, a federated pull-guard for the peer is allowed.
    Blocklist::ensure_server_allowed(&ctx.db, "evil.tld")
        .await
        .expect("un-blocked peer is allowed");

    // Blacklist the peer.
    Blocklist::block_server(&ctx.db, "evil.tld", Some("malicious"))
        .await
        .unwrap();
    assert!(
        Blocklist::is_server_blocked(&ctx.db, "evil.tld")
            .await
            .unwrap()
    );

    // A federation pull from that peer is now refused.
    let pull = Blocklist::ensure_server_allowed(&ctx.db, "evil.tld").await;
    assert!(
        matches!(
            pull,
            Err(service::moderation::ModerationError::ServerBlocked { .. })
        ),
        "a blocklisted peer's federated request must be refused"
    );

    // And a (validly signed) report from the blocked peer is refused before the queue.
    let report = signed_report(&key, "evil.tld", &ctx.user_id, &"b".repeat(64));
    let intake = Report::intake(
        &ctx.db,
        &report,
        &ModerationLimits::default(),
        Timestamp::now(),
    )
    .await;
    assert!(matches!(
        intake,
        Err(service::moderation::ModerationError::ServerBlocked { .. })
    ));
    assert_eq!(
        Report::queue_for_user(&ctx.db, &ctx.user_id)
            .await
            .unwrap()
            .len(),
        0,
        "a blocked peer's report must never reach the queue"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Validation bullet 3 — Takedown serving (smoke).
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn takedown_serves_410_and_records_provenance() {
    let ctx = mod_setup().await;
    let svc = ctx.blob_service();
    let token = ctx.token();

    let asset_id = nanoid!();
    let ciphertext = b"ciphertext-bytes";
    let hash = capsule_core::crypto::hash::hash_bytes(ciphertext).to_hex();
    ctx.seed_asset(&asset_id, &hash, true).await;
    ctx.index_original(&asset_id, &hash, ciphertext.len() as u64)
        .await;

    // Plant the content-addressed blob so we can prove takedown preserves it.
    let blob_path = service::blob_store::blob_path(&ctx.upload_dir, &hash);
    std::fs::create_dir_all(service::blob_store::blobs_dir(&ctx.upload_dir)).unwrap();
    std::fs::write(&blob_path, ciphertext).unwrap();

    // Before takedown the asset serves its ciphertext on the content-addressed path.
    let before = TestClient::get(format!("http://localhost/{hash}"))
        .add_header("Authorization", format!("Bearer {token}"), true)
        .send(&svc)
        .await;
    assert_eq!(
        before.status_code,
        Some(StatusCode::OK),
        "a servable asset serves its blob"
    );

    // Take it down.
    let event_id = Takedown::take_down(&ctx.db, &asset_id, Some("legal request"), false)
        .await
        .expect("takedown");

    // Subsequent fetches return 410 Gone — on the path clients and federated peers use.
    let after = TestClient::get(format!("http://localhost/{hash}"))
        .add_header("Authorization", format!("Bearer {token}"), true)
        .send(&svc)
        .await;
    assert_eq!(
        after.status_code,
        Some(StatusCode::GONE),
        "a taken-down asset must fetch 410 Gone"
    );

    // The underlying blob is preserved (takedown is a serving constraint, not destruction).
    assert!(
        blob_path.exists(),
        "takedown must not delete the underlying blob"
    );

    // A moderation provenance record is appended and visible in the user's audit log.
    let log = AuditLog::for_user(&ctx.db, &ctx.user_id).await.unwrap();
    assert_eq!(
        log.len(),
        1,
        "the takedown must append one audit-log record"
    );
    assert_eq!(log[0].id, event_id);
    assert_eq!(log[0].kind, "takedown");
    assert_eq!(log[0].asset_id.as_deref(), Some(asset_id.as_str()));
    assert_eq!(log[0].reason.as_deref(), Some("legal request"));

    // Lifting the takedown restores serving — the gate reads live state, it is not a tombstone.
    Takedown::lift(&ctx.db, &asset_id, Some("appeal granted"))
        .await
        .expect("lift");
    let lifted = TestClient::get(format!("http://localhost/{hash}"))
        .add_header("Authorization", format!("Bearer {token}"), true)
        .send(&svc)
        .await;
    assert_eq!(
        lifted.status_code,
        Some(StatusCode::OK),
        "a lifted takedown serves again"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Validation bullet 4 — Federated-report authentication (unit), invariant 24.
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn federated_report_authentication() {
    let ctx = mod_setup().await;
    let key = SigningKey::from_bytes(&[5u8; 32]);
    Report::register_peer(&ctx.db, "peer.tld", &key.verifying_key().to_bytes())
        .await
        .unwrap();
    let limits = ModerationLimits::default();

    // (a) A report signed by a valid peer key reaches the admin queue.
    let good = signed_report(&key, "peer.tld", &ctx.user_id, &"a".repeat(64));
    Report::intake(&ctx.db, &good, &limits, Timestamp::now())
        .await
        .expect("valid signature accepted");
    assert_eq!(
        Report::queue_for_user(&ctx.db, &ctx.user_id)
            .await
            .unwrap()
            .len(),
        1
    );

    // (b) An unsigned (zero-signature) report is dropped.
    let mut unsigned = signed_report(&key, "peer.tld", &ctx.user_id, &"b".repeat(64));
    unsigned.signature = vec![0u8; 64];
    assert!(matches!(
        Report::intake(&ctx.db, &unsigned, &limits, Timestamp::now()).await,
        Err(service::moderation::ModerationError::ReportUnsigned)
    ));

    // (c) An invalid-signature report (signed by the WRONG key) is dropped.
    let wrong_key = SigningKey::from_bytes(&[6u8; 32]);
    let core = signed_report(&key, "peer.tld", &ctx.user_id, &"c".repeat(64)).core;
    let sig = wrong_key
        .sign(&cbor::to_canonical_vec(&core).unwrap())
        .to_bytes()
        .to_vec();
    let tampered = SignedReport {
        core,
        signature: sig,
    };
    assert!(matches!(
        Report::intake(&ctx.db, &tampered, &limits, Timestamp::now()).await,
        Err(service::moderation::ModerationError::ReportUnsigned)
    ));

    // (d) A report from an unknown (unregistered) peer is dropped — unverifiable.
    let unknown_key = SigningKey::from_bytes(&[7u8; 32]);
    let unknown = signed_report(&unknown_key, "ghost.tld", &ctx.user_id, &"d".repeat(64));
    assert!(matches!(
        Report::intake(&ctx.db, &unknown, &limits, Timestamp::now()).await,
        Err(service::moderation::ModerationError::ReportUnsigned)
    ));

    // Only the one valid report ever reached the queue.
    assert_eq!(
        Report::queue_for_user(&ctx.db, &ctx.user_id)
            .await
            .unwrap()
            .len(),
        1,
        "only the valid report is queued; every rejected report wrote nothing"
    );

    // (e) Exceeding the per-(reporting_server, reported_user) rate budget applies backpressure.
    let tight = ModerationLimits {
        report_rate_max: 3,
        report_rate_window: SignedDuration::from_hours(1),
    };
    let victim = ctx.seed_user().await;
    // The queue already holds 0 for `victim`. Fill the budget with distinct valid reports.
    for i in 0..3u8 {
        let hash = format!("{:064x}", 0xf00 + u32::from(i));
        let r = signed_report(&key, "peer.tld", &victim, &hash);
        Report::intake(&ctx.db, &r, &tight, Timestamp::now())
            .await
            .expect("within budget");
    }
    // The 4th crosses the budget → backpressure (rate-limited), not amplified.
    let over = signed_report(&key, "peer.tld", &victim, &"a".repeat(64));
    assert!(matches!(
        Report::intake(&ctx.db, &over, &tight, Timestamp::now()).await,
        Err(service::moderation::ModerationError::ReportRateLimited { .. })
    ));
    assert_eq!(
        Report::queue_for_user(&ctx.db, &victim)
            .await
            .unwrap()
            .len(),
        3,
        "the over-budget report is dropped; the queue holds only the 3 within budget"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Validation bullet 5 — Block scoping (unit).
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn per_user_block_is_scoped() {
    let ctx = mod_setup().await;
    // V (the blocked user) is a share recipient on U's (the blocker's) album.
    let blocked = ctx.seed_user().await;
    ctx.share_album_with(&blocked).await;
    assert_eq!(
        entity::album_share::Entity::find()
            .filter(entity::album_share::Column::UserId.eq(&blocked))
            .count(&ctx.db)
            .await
            .unwrap(),
        1,
        "precondition: the blocked user shares the blocker's album"
    );

    // U blocks V.
    let revoked = Blocklist::block_user(&ctx.db, &ctx.user_id, &blocked)
        .await
        .unwrap();
    assert_eq!(revoked, 1, "the block must revoke the one shared album");

    // The blocked user is removed from the blocker's shared albums.
    assert_eq!(
        entity::album_share::Entity::find()
            .filter(entity::album_share::Column::UserId.eq(&blocked))
            .count(&ctx.db)
            .await
            .unwrap(),
        0,
        "a per-user block removes the blocked user from the blocker's shared albums"
    );
    assert!(
        Blocklist::is_user_blocked(&ctx.db, &ctx.user_id, &blocked)
            .await
            .unwrap()
    );

    // The per-user block does NOT appear as a server-level federation block against the blocked
    // user's home server (scoped to the user; never weaponizable to sever a peer).
    assert!(
        !Blocklist::is_server_blocked(&ctx.db, "other.tld")
            .await
            .unwrap(),
        "a per-user block must not become a server-level federation block"
    );
    assert_eq!(
        entity::server_blocklist::Entity::find()
            .count(&ctx.db)
            .await
            .unwrap(),
        0,
        "a per-user block writes no server_blocklist row"
    );
}
