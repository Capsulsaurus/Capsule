//! Slice `S-D5` end-to-end (E2E case 1): the real `capsule-cli` command functions
//! — `remote::auth_login` → `remote::sync` → `remote::list` — driving the real auth
//! and sync services over the wire, exactly as `capsule auth login && capsule sync
//! && capsule list` does.
//!
//! This is the client half's counterpart to `sdk_client.rs`: where that exercises
//! the SDK consumer against the served feed, this exercises the *CLI* built on the
//! SDK — the login persists a session, the sync drains the feed into the CLI's
//! SQLite through the anti-rewind `SyncState`, and the list queries what landed. It
//! hosts here (in the server crate, with the CLI as a dev-dep) so the leaf CLI
//! keeps the server stack out of its own dev-deps — the S-D1/S-D2 test-placement
//! precedent.

#![allow(clippy::unwrap_used)]

use base64::Engine as _;
use capsule_cli::remote::{self, RemoteConfig};
use capsule_cli::session::SessionStore;
use nanoid::nanoid;
use salvo::Service;
use salvo::conn::tcp::TcpAcceptor;
use salvo::prelude::{Router, Server};
use sea_orm::{ActiveModelTrait, Database, Set};
use sea_orm_migration::MigratorTrait;
use service::sync::{ChangeKind, FeedBlobManifest, FeedBlobRef, FeedEntryInput};
use testcontainers::core::{ContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{GenericImage, ImageExt};
use testcontainers_modules::postgres::Postgres;

use crate::config::{
    DEFAULT_PAGE_SIZE, DEFAULT_PROTOCOL_MAX, DEFAULT_PROTOCOL_MIN, MAX_PAGE_SIZE, SyncServerConfig,
};

/// The shared test Ed25519 keypair (base64 pkcs8) — the auth service signs access
/// tokens with the private half, the sync feed verifies with the public half.
const PRIV_B64: &str = "MC4CAQAwBQYDK2VwBCIEIN6eTvXEL7xMZWHY8rTk7VbQSGSuRkle5MVfiiYUStLF";
const PUB_B64: &str = "MCowBQYDK2VwAyEA66iVaMz1x2ogToGm5Hw34aITBLLqz0iEonbwjK57pWU=";
/// The server-only cursor MAC key.
const CURSOR_KEY: [u8; 32] = [0x5c; 32];
/// The protocol pin the seeded feed entries conform to (inside the server window).
const PROTOCOL: &str = "2026-05-31";
/// The CLI's max-known protocol — inside the server window, at/above the entries'.
const CLIENT_MAX_PROTOCOL: &str = "2026-12-31";
/// The password the user registers and logs in with.
const PASSWORD: &str = "password123";
/// How many feed entries to seed.
const SEEDED: usize = 3;

/// Serve a salvo router on an ephemeral TCP port and return its base URL.
async fn serve(router: Router) -> String {
    let service = Service::new(router);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let acceptor = TcpAcceptor::try_from(listener).unwrap();
    tokio::spawn(async move { Server::new(acceptor).serve(service).await });
    format!("http://{addr}")
}

/// `capsule auth login && capsule sync && capsule list` round-trips against the
/// real auth + sync services: login persists a session, sync drains the seeded
/// feed into the CLI store, list returns exactly what landed, a second sync is a
/// no-op (the persisted cursor + high-water resume), and logout clears the session.
#[tokio::test]
async fn cli_login_sync_list_round_trip() {
    // ── Postgres (shared by auth + sync) ────────────────────────────────────
    let postgres = Postgres::default().with_tag("17").start().await.unwrap();
    let pg_port = postgres.get_host_port_ipv4(5432).await.unwrap();
    let conn_str = format!("postgres://postgres:postgres@127.0.0.1:{pg_port}/postgres");
    let db = Database::connect(&conn_str).await.unwrap();
    migration::Migrator::refresh(&db).await.unwrap();

    // ── Valkey (auth session store) ─────────────────────────────────────────
    let valkey = GenericImage::new("valkey/valkey", "8.0.1")
        .with_exposed_port(ContainerPort::Tcp(6379))
        .with_wait_for(WaitFor::message_on_stdout("Ready to accept connections"))
        .start()
        .await
        .unwrap();
    let vk_port = valkey.get_host_port_ipv4(6379).await.unwrap();
    let valkey_url = format!("redis://127.0.0.1:{vk_port}");

    // ── Shared JWT keypair ──────────────────────────────────────────────────
    let engine = base64::engine::general_purpose::STANDARD;
    let enc_key = jsonwebtoken::EncodingKey::from_ed_der(&engine.decode(PRIV_B64).unwrap());
    let dec_key = jsonwebtoken::DecodingKey::from_ed_der(&engine.decode(PUB_B64).unwrap());

    // ── Real auth service over TCP ──────────────────────────────────────────
    let auth_config = auth::config::AuthConfig {
        host: "127.0.0.1".to_string(),
        port: 0,
        domain: "localhost".to_string(),
        jwt_eddsa_encoding_key: enc_key,
        jwt_eddsa_decoding_key: dec_key.clone(),
        jwt_refresh_token_duration_seconds: 3600,
        jwt_access_token_duration_seconds: 300,
        valkey_url,
        totp_issuer: "Capsule-Test".to_string(),
        allowed_origins: vec!["*".to_string()],
    };
    let auth_router = auth::get_router(db.clone(), auth_config).await.unwrap();
    let auth_url = serve(auth_router).await;

    // ── Real sync service over TCP ──────────────────────────────────────────
    let sync_config = SyncServerConfig {
        upload_dir: std::env::temp_dir(),
        jwt_eddsa_decoding_key: dec_key,
        protocol_min: DEFAULT_PROTOCOL_MIN.to_string(),
        protocol_max: DEFAULT_PROTOCOL_MAX.to_string(),
        cursor_mac_key: CURSOR_KEY,
        default_page_size: DEFAULT_PAGE_SIZE,
        max_page_size: MAX_PAGE_SIZE,
        allowed_origins: Vec::new(),
    };
    let sync_router = crate::get_router(db.clone(), sync_config).await.unwrap();
    let sync_url = serve(sync_router).await;

    // ── Register the user through the real auth service ─────────────────────
    let email = format!("{}@example.com", nanoid!(8)).to_lowercase();
    let username = format!("u{}", nanoid!(8)).to_lowercase();
    let http = reqwest::Client::new();
    let registered = http
        .post(format!("{auth_url}/register"))
        .json(&serde_json::json!({
            "username": username,
            "name": "CLI Tester",
            "email": email,
            "password": PASSWORD,
        }))
        .send()
        .await
        .unwrap();
    assert!(
        registered.status().is_success(),
        "registration failed: {}",
        registered.status()
    );

    // Seed the registered user's ownership + album + feed entries.
    let user = entity::user::Entity::find_by_email(&email)
        .one(&db)
        .await
        .unwrap()
        .expect("registered user row");
    let user_id = user.id.clone();
    let created = jiff::Timestamp::now() - jiff::SignedDuration::from_hours(24);
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
    for _ in 0..SEEDED {
        let input = FeedEntryInput {
            album_id: album_id.clone(),
            protocol_version: PROTOCOL.to_string(),
            kind: ChangeKind::Created,
            asset_id: nanoid!(),
            manifest_cbor: vec![0xa0],
            metadata_blob: None,
            blobs: FeedBlobManifest {
                original: Some(FeedBlobRef {
                    ciphertext_hash: format!("{:064x}", 0),
                    role: "original".to_string(),
                    format: "image/jpeg".to_string(),
                    size: 4096,
                }),
                derivatives: Vec::new(),
            },
            original_held: true,
        };
        service::sync::Mutation::record_finalization(&db, input)
            .await
            .unwrap();
    }

    // ── Drive the CLI command functions ─────────────────────────────────────
    let workdir = std::env::temp_dir().join(format!("capsule-cli-e2e-{}", nanoid!()));
    std::fs::create_dir_all(&workdir).unwrap();
    let store = SessionStore::new(workdir.join("session.json"));
    let cli_db = Database::connect(format!(
        "sqlite://{}?mode=rwc",
        workdir.join("capsule.sqlite").display()
    ))
    .await
    .unwrap();
    cli_migration::Migrator::up(&cli_db, None).await.unwrap();

    let remote = RemoteConfig {
        auth_endpoint: auth_url,
        upload_endpoint: format!("{sync_url}/v1/upload"),
        albums_endpoint: format!("{sync_url}/v1/albums"),
        sync_endpoint: sync_url,
        protocol_version: CLIENT_MAX_PROTOCOL.to_string(),
    };

    // `capsule auth login` — authenticates over the SDK and persists the session.
    assert!(store.load().unwrap().is_none(), "no session before login");
    remote::auth_login(&remote, &store, &email, PASSWORD)
        .await
        .expect("login");
    assert!(
        store.load().unwrap().is_some(),
        "login persisted the session"
    );

    // `capsule sync` — drains the feed into the CLI store (page size 2 → >1 page).
    let summary = remote::sync(&remote, &store, &cli_db, 2, false, false)
        .await
        .expect("sync");
    assert_eq!(summary.applied, SEEDED, "every seeded entry applied");
    assert_eq!(summary.albums, 1);
    assert!(summary.pages >= 2, "3 entries at page size 2 span >1 page");

    // `capsule list` — the client-side query over what synced.
    let rows = remote::list(&cli_db, false).await.expect("list");
    assert_eq!(rows.len(), SEEDED, "list shows exactly what synced");
    assert!(
        rows.iter().all(|row| row.album_id == album_id.as_bytes()),
        "every listed asset belongs to the seeded album"
    );

    // A second `capsule sync` is a no-op: the persisted cursor + high-water resume.
    let again = remote::sync(&remote, &store, &cli_db, 2, false, false)
        .await
        .expect("re-sync");
    assert_eq!(again.applied, 0, "second sync finds nothing new");

    // `capsule auth logout` — revokes server-side and clears the local session.
    assert!(
        remote::auth_logout(&remote, &store).await.expect("logout"),
        "logout reports a session was cleared"
    );
    assert!(
        store.load().unwrap().is_none(),
        "logout cleared the session"
    );

    let _ = std::fs::remove_dir_all(&workdir);
}
