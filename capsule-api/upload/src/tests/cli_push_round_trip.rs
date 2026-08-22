//! Slice `S-D18` end-to-end: the real `capsule-cli` push path — `remote::auth_register` →
//! a signed offline import → `remote::held_blobs` → `remote::push` — driving the real auth,
//! upload, and sync services on **one origin**, exactly as
//! `capsule auth register && capsule import && capsule push` does.
//!
//! It hosts here (in the server crate, with the CLI as a dev-dep) so the leaf CLI keeps the
//! server stack out of its own dev-deps — the S-D1/S-D2/S-D5 test-placement precedent.
//!
//! # A blocked half, asserted rather than papered over
//!
//! The client's album id is **derived from the account master key** and is a UUID
//! ([Organization — The Default Album]). The server's album id column is
//! `character(21)` — a nanoid — across the whole schema (`albums.id`, `assets.album_id`,
//! `sync_entries.album_id`, …), and **no endpoint provisions an album at all**. So today a
//! client physically cannot name its own album to the server: inserting the client's album id
//! is a `value too long for type character(21)` error, and `POST /upload` therefore refuses
//! every real push at invariant 6 with `error.upload.album_access_denied`.
//!
//! That is a server-side gap, not something to route around client-side (weakening the album
//! binding would make invariant 6 unenforceable), so this test asserts **both** halves
//! honestly:
//!
//! - [`cli_push_is_refused_by_the_album_gate`] pins the wall: a well-formed, envelope-consistent
//!   bundle authored by the real CLI path reaches the server's invariant-6 gate and is refused
//!   there — nothing earlier in the stack is at fault.
//! - [`cli_push_round_trip_over_a_server_representable_album`] proves everything the gap is
//!   hiding: the same bundle, the same `capsule_sdk::push` request mapping, and the same
//!   upload client, against an album the server *can* represent — blob durable, feed entry
//!   minted, `capsule sync` + `capsule list` showing it, and a re-run that moves nothing.
//!
//! [Organization — The Default Album]: https://docs/design/organization/#the-default-album

use std::collections::HashSet;

use base64::Engine as _;
use capsule_cli::remote::{self, PushOptions, RemoteConfig};
use capsule_cli::session::SessionStore;
use capsule_core::crypto::primitives::Argon2Params;
use capsule_core::lifecycle::Workspace;
use capsule_sdk::push;
use capsule_sdk::staged::StagedScheduler;
use capsule_sdk::upload::{StaticToken, UploadClient, UploadOutcome, UploadTransport};
use nanoid::nanoid;
use salvo::Service;
use salvo::conn::tcp::TcpAcceptor;
use salvo::prelude::{Router, Server};
use sea_orm::{ActiveModelTrait, ColumnTrait, Database, EntityTrait, QueryFilter, Set};
use sea_orm_migration::MigratorTrait;

use super::{PROTOCOL, TestCtx, setup};
use crate::state::AppState;

/// The private half of the harness's Ed25519 keypair — the auth service signs access tokens
/// with it, the upload and sync services verify with the public half `setup()` installed.
const PRIV_B64: &str = "MC4CAQAwBQYDK2VwBCIEIN6eTvXEL7xMZWHY8rTk7VbQSGSuRkle5MVfiiYUStLF";
const PUB_B64: &str = "MCowBQYDK2VwAyEA66iVaMz1x2ogToGm5Hw34aITBLLqz0iEonbwjK57pWU=";
/// The server-only sync cursor MAC key.
const CURSOR_KEY: [u8; 32] = [0x5c; 32];
/// The CLI's max-known protocol — inside the server window, at/above the bundle's pin.
const CLIENT_MAX_PROTOCOL: &str = "2026-12-31";
/// The account password the CLI registers with.
const PASSWORD: &str = "password123";
/// The fast Argon2 cost; the production tier would dominate the suite's runtime.
const FAST: Argon2Params = Argon2Params {
    mem_kib: 64,
    t_cost: 1,
    p_cost: 1,
};

/// The whole server on ONE origin — `/v1/auth`, `/v1/upload`, and the gRPC sync feed at the
/// root — mirroring `capsule_api::create_router`'s mounting, because that single-origin shape
/// is what `RemoteConfig` now derives every surface from.
async fn serve_one_origin(ctx: &TestCtx) -> String {
    let engine = base64::engine::general_purpose::STANDARD;
    let enc_key = jsonwebtoken::EncodingKey::from_ed_der(&engine.decode(PRIV_B64).unwrap());
    let dec_key = jsonwebtoken::DecodingKey::from_ed_der(&engine.decode(PUB_B64).unwrap());

    let auth_router = auth::get_router(
        ctx.db.clone(),
        auth::config::AuthConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
            domain: "localhost".to_string(),
            jwt_eddsa_encoding_key: enc_key,
            jwt_eddsa_decoding_key: dec_key.clone(),
            jwt_refresh_token_duration_seconds: 3600,
            jwt_access_token_duration_seconds: 300,
            valkey_url: ctx.config.valkey_url.clone(),
            totp_issuer: "Capsule-Test".to_string(),
            allowed_origins: vec!["*".to_string()],
        },
    )
    .await
    .unwrap();

    let upload_state = AppState::new(
        ctx.db.clone(),
        ctx.config.clone(),
        ctx.upload_service.clone(),
    );
    let upload_router = crate::routes::get_router(
        upload_state,
        ctx.config.protocol_min.clone(),
        ctx.config.protocol_max.clone(),
    );

    let sync_router = sync::get_router(
        ctx.db.clone(),
        sync::config::SyncServerConfig {
            upload_dir: ctx.upload_dir.clone(),
            jwt_eddsa_decoding_key: dec_key,
            protocol_min: ctx.config.protocol_min.clone(),
            protocol_max: ctx.config.protocol_max.clone(),
            cursor_mac_key: CURSOR_KEY,
            default_page_size: 256,
            max_page_size: 1024,
            allowed_origins: Vec::new(),
        },
    )
    .await
    .unwrap();

    let root = Router::new()
        .push(
            Router::with_path("v1")
                .push(Router::with_path("auth").push(auth_router))
                .push(Router::with_path("upload").push(upload_router)),
        )
        .push(sync_router);

    let service = Service::new(root);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let acceptor = TcpAcceptor::try_from(listener).unwrap();
    tokio::spawn(async move { Server::new(acceptor).serve(service).await });
    format!("http://{addr}")
}

/// The endpoints set explicitly (not from the environment): the harness serves a single origin,
/// so the test states it rather than depending on process-wide variables.
fn remote_config(base: &str) -> RemoteConfig {
    RemoteConfig {
        auth_endpoint: format!("{base}/v1/auth"),
        sync_endpoint: base.to_string(),
        upload_endpoint: format!("{base}/v1/upload"),
        protocol_version: CLIENT_MAX_PROTOCOL.to_string(),
    }
}

/// One CLI-shaped library: `capsule library init` + `capsule import` of a single file, on the
/// signed lifecycle path with the CLI's own client identity.
fn imported_library(dir: &std::path::Path) -> Workspace {
    let lib = dir.join("library");
    std::fs::create_dir_all(&lib).unwrap();
    let src = dir.join("photo.jpg");
    std::fs::write(&src, b"\xFF\xD8\xFF capsule push round-trip bytes").unwrap();

    let mut ws = Workspace::create_with_params(&lib, b"library-passphrase", FAST)
        .unwrap()
        .with_client_id(capsule_cli::CLIENT_ID, "0.1.0");
    let album = ws.default_album_id();
    ws.ensure_album(album, "Imports").unwrap();
    ws.import_asset(album, &src).unwrap();
    ws
}

/// Register through the real auth service the way `capsule auth register` does, back-date the
/// account so every import timestamp postdates it (invariant 7's device-authorization floor),
/// and return `(email, server user id)`.
async fn register(ctx: &TestCtx, remote: &RemoteConfig, store: &SessionStore) -> (String, String) {
    let email = format!("{}@example.com", nanoid!(8)).to_lowercase();
    let username = format!("u{}", nanoid!(8)).to_lowercase();
    remote::auth_register(remote, store, &username, "Push Tester", &email, PASSWORD)
        .await
        .expect("register");

    let user = entity::user::Entity::find()
        .filter(entity::user::Column::Email.eq(&email))
        .one(&ctx.db)
        .await
        .unwrap()
        .expect("registered user row");
    let created = jiff::Timestamp::now() - jiff::SignedDuration::from_hours(24);
    let mut active: entity::user::ActiveModel = user.clone().into();
    active.created_at = Set(entity::time::ts_to_entity(created));
    active.update(&ctx.db).await.unwrap();
    (email, user.id)
}

/// Give `user_id` an owner group and one album the **server** can represent (a nanoid), and
/// return the album id.
async fn seed_server_album(ctx: &TestCtx, user_id: &str) -> String {
    let created = jiff::Timestamp::now() - jiff::SignedDuration::from_hours(24);
    entity::owner::ActiveModel {
        id: Set(user_id.to_string()),
        created_at: Set(entity::time::ts_to_entity(created)),
    }
    .insert(&ctx.db)
    .await
    .unwrap();
    entity::owner_member::ActiveModel {
        owner_id: Set(user_id.to_string()),
        user_id: Set(user_id.to_string()),
        created_at: Set(entity::time::ts_to_entity(created)),
        ..Default::default()
    }
    .insert(&ctx.db)
    .await
    .unwrap();
    let album_id = nanoid!();
    entity::album::ActiveModel {
        id: Set(album_id.clone()),
        owner_id: Set(user_id.to_string()),
        name: Set("Imports".to_string()),
        description: Set(String::new()),
        created_at: Set(entity::time::ts_to_entity(created)),
        modified_at: Set(entity::time::ts_to_entity(created)),
        deleted_at: Set(None),
    }
    .insert(&ctx.db)
    .await
    .unwrap();
    album_id
}

/// A scratch directory that cleans itself up.
fn scratch() -> tempfile::TempDir {
    tempfile::TempDir::new().unwrap()
}

/// **The wall.** The real CLI push path — register, import, read server truth off the feed,
/// then push — reaches the server with a well-formed bundle and is refused at invariant 6,
/// because the client's master-key-derived (UUID) album id has no server representation and no
/// endpoint provisions one. Asserted, not worked around: weakening the album binding
/// client-side is exactly what would make invariant 6 unenforceable.
#[tokio::test]
async fn cli_push_is_refused_by_the_album_gate() {
    let ctx = setup().await;
    let base = serve_one_origin(&ctx).await;
    let remote = remote_config(&base);
    let work = scratch();
    let store = SessionStore::new(work.path().join("session.json"));

    register(&ctx, &remote, &store).await;
    let ws = imported_library(work.path());
    assert_eq!(ws.asset_ids().len(), 1, "one asset imported offline");

    // Server truth: a fresh account holds nothing, so the whole ladder is outstanding.
    let session_client = capsule_sdk::auth::AuthClient::new(&remote.auth_endpoint).unwrap();
    let session = session_client
        .resume(store.load().unwrap().expect("session persisted"))
        .unwrap();
    let held = remote::held_blobs(&remote, session, 256)
        .await
        .expect("the feed pull is reachable on the single origin");
    assert!(held.is_empty(), "a fresh account holds no blobs");

    // A dry run plans the full ladder and opens nothing.
    let planned = remote::push(
        &remote,
        &store,
        &ws,
        PushOptions {
            dry_run: true,
            ..Default::default()
        },
    )
    .await
    .expect("a dry run never touches the upload surface");
    assert_eq!(planned.uploaded_blobs, 2, "metadata index + original");
    assert!(planned.dry_run);

    // The real push reaches the server and is refused at the album gate.
    let error = remote::push(&remote, &store, &ws, PushOptions::default())
        .await
        .expect_err("no server album can represent the client's album id today");
    assert_eq!(
        error.error_code(),
        Some(capsule_i18n::error_codes::UPLOAD_ALBUM_ACCESS_DENIED),
        "the refusal is invariant 6, not a malformed request: {error}"
    );
}

/// **Everything the wall is hiding.** The same imported library, the same
/// `capsule_sdk::push` request mapping, and the same upload client — against an album the
/// server *can* represent — round-trips completely: both blobs land durably in the
/// content-addressed store, the assets are marked uploaded with custody receipts, the sync feed
/// reports `original_held`, `capsule sync` + `capsule list` show the asset, and a re-run of the
/// push moves nothing (`duplicate_blob` resolves as a merge).
#[tokio::test]
async fn cli_push_round_trip_over_a_server_representable_album() {
    let ctx = setup().await;
    let base = serve_one_origin(&ctx).await;
    let remote = remote_config(&base);
    let work = scratch();
    let store = SessionStore::new(work.path().join("session.json"));

    let (_email, user_id) = register(&ctx, &remote, &store).await;
    let server_album = seed_server_album(&ctx, &user_id).await;
    let ws = imported_library(work.path());
    let asset_id = ws.asset_ids()[0];
    let bundle = ws.upload_bundle(&asset_id).expect("upload bundle");
    assert_eq!(
        bundle.content_type, "image/jpeg",
        "the original declares a content type inside the server's closed enum"
    );

    let session = capsule_sdk::auth::AuthClient::new(&remote.auth_endpoint)
        .unwrap()
        .resume(store.load().unwrap().expect("session"))
        .unwrap();
    let token = {
        use secrecy::ExposeSecret as _;
        let persisted = session.export().await.expect("session tokens");
        persisted.access_token.expose_secret().to_string()
    };
    let client = UploadClient::new(UploadTransport::with_static_token(
        reqwest::Client::new(),
        &remote.upload_endpoint,
        PROTOCOL,
        StaticToken(token),
    ));

    // Drive the real ladder: `push::plan` chooses and orders the blobs, `push::create_request`
    // builds each `POST /upload` body. Only `album_id` is re-pointed at the server-representable
    // album — the one thing the id-space gap makes impossible for a real client.
    let scheduler = StagedScheduler::new(
        capsule_core::import::upload::UploadPolicy::Full,
        capsule_sdk::net::ConnectionClass::Unmetered,
    );
    let plan = push::plan(&scheduler, &bundle, &HashSet::<String>::new(), false);
    assert_eq!(
        plan.blobs.iter().map(|b| b.tier).collect::<Vec<_>>(),
        vec![
            capsule_core::import::upload::UploadTier::Index,
            capsule_core::import::upload::UploadTier::Original,
        ],
        "the ladder runs index-first"
    );

    let blobs = push::bundle_blobs(&bundle);
    let mut sent = Vec::new();
    for tier_blob in &plan.blobs {
        let (blob, hash) = blobs
            .iter()
            .find(|(_, hash)| *hash == tier_blob.hash)
            .expect("planned blob is part of the bundle");
        let mut request = push::create_request(&bundle, blob, hash);
        request.album_id = Some(server_album.clone());
        request.manifest_envelope.album_id = Some(server_album.clone());
        // The protocol pin the bundle carries must survive to the wire unchanged.
        assert_eq!(request.protocol_version, PROTOCOL);
        match client.upload(&request, blob.bytes).await.expect("upload") {
            UploadOutcome::Completed { .. } => sent.push(hash.clone()),
            other => panic!("expected a completed transfer, got {other:?}"),
        }
    }
    assert_eq!(sent.len(), 2, "both blobs transferred");

    // Durable: each blob is committed into the content-addressed store, its asset row is
    // marked uploaded, and a custody receipt was issued in the same transaction.
    for hash in &sent {
        let bytes = ctx
            .storage
            .read_committed_blob(hash)
            .await
            .unwrap_or_else(|e| panic!("blob {hash} must be durable: {e}"));
        assert_eq!(
            capsule_core::utils::hash::hash_bytes(&bytes),
            *hash,
            "the stored blob content-addresses to what was declared"
        );
    }
    let assets = entity::asset::Entity::find()
        .filter(entity::asset::Column::OwnerId.eq(&user_id))
        .all(&ctx.db)
        .await
        .unwrap();
    assert_eq!(assets.len(), 2, "one asset row per blob session");
    assert!(assets.iter().all(|a| a.uploaded), "every row is finalized");
    let receipts = entity::custody_receipt::Entity::find()
        .all(&ctx.db)
        .await
        .unwrap();
    assert_eq!(receipts.len(), 2, "a custody receipt per finalized blob");

    // `capsule sync` drains the feed into the CLI's store, and `capsule list` shows the asset.
    let cli_db = Database::connect(format!(
        "sqlite://{}?mode=rwc",
        work.path().join("capsule.sqlite").display()
    ))
    .await
    .unwrap();
    cli_migration::Migrator::up(&cli_db, None).await.unwrap();
    let summary = remote::sync(&remote, &store, &cli_db, 256, false, false)
        .await
        .expect("sync");
    assert_eq!(summary.applied, 2, "both finalizations reached the feed");
    let rows = remote::list(&cli_db, false).await.expect("list");
    assert_eq!(rows.len(), 2, "capsule list shows what push put there");
    assert!(
        rows.iter().any(|row| row.original_held),
        "the original blob's finalization flips original_held"
    );

    // Server truth now covers the whole bundle, so a re-run plans nothing at all.
    let session = capsule_sdk::auth::AuthClient::new(&remote.auth_endpoint)
        .unwrap()
        .resume(store.load().unwrap().expect("session"))
        .unwrap();
    let held = remote::held_blobs(&remote, session, 256)
        .await
        .expect("feed");
    for hash in &sent {
        assert!(held.contains(hash), "the feed reports blob {hash} as held");
    }
    let rerun = push::plan(&scheduler, &bundle, &held, false);
    assert!(
        rerun.blobs.is_empty(),
        "re-running push against an unchanged library is a no-op"
    );
    assert_eq!(rerun.already_held, 2);

    // …and forcing it re-drives the ladder into `duplicate_blob`, which is a merge, not a
    // failure: nothing is re-transferred and nothing errors.
    for tier_blob in &push::plan(&scheduler, &bundle, &held, true).blobs {
        let (blob, hash) = blobs
            .iter()
            .find(|(_, hash)| *hash == tier_blob.hash)
            .expect("planned blob");
        let mut request = push::create_request(&bundle, blob, hash);
        request.album_id = Some(server_album.clone());
        request.manifest_envelope.album_id = Some(server_album.clone());
        match client.upload(&request, blob.bytes).await.expect("re-push") {
            UploadOutcome::AlreadyStored { asset_ref } => {
                assert!(!asset_ref.is_empty(), "the merge names the existing asset");
            }
            other => panic!("expected AlreadyStored (merge), got {other:?}"),
        }
    }

    // The feed fold is stable across the re-run: still exactly the two blobs.
    let entry_count = entity::sync_entry::Entity::find()
        .all(&ctx.db)
        .await
        .unwrap()
        .len();
    assert_eq!(entry_count, 2, "a merged re-push mints no new feed entries");
}
