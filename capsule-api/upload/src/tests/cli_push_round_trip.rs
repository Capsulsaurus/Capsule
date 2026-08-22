//! Slices `S-D18` + `S-C25` end to end: the real `capsule-cli` push path — `remote::auth_register`
//! → a signed offline import → `remote::held_blobs` → `remote::push` (which now provisions the
//! album first) — driving the real auth, album, upload, and sync services on **one origin**,
//! exactly as `capsule auth register && capsule import && capsule push` does.
//!
//! It hosts here (in the server crate, with the CLI as a dev-dep) so the leaf CLI keeps the
//! server stack out of its own dev-deps — the S-D1/S-D2/S-D5 test-placement precedent.
//!
//! # The wall S-D18 documented, and what removed it
//!
//! The client's album id is **derived from the account master key** and is a UUID
//! ([Organization — The Default Album]). Until `S-C25` the server's album id column was
//! `character(21)` — a nanoid — across the whole schema, and **no endpoint provisioned an
//! album at all**, so a client physically could not name its own album: inserting a UUID was
//! `value too long for type character(21)`, and `POST /upload` refused every real push at
//! invariant 6 with `error.upload.album_access_denied`. S-C25 widened the six album-id columns
//! to `varchar(64)` and added `POST /v1/albums`, so the push path is now whole. Nothing was
//! relaxed to get here: invariant 6 still demands a real album the caller can really write to
//! — provisioning is what makes that true, rather than what dodges it.
//!
//! What this suite pins:
//!
//! - [`cli_push_round_trip_puts_bytes_on_the_server`] — the whole flow through the *real*
//!   `remote::push` on the client's own derived UUID album: blobs durable, assets finalized,
//!   receipts issued, feed minted, `capsule sync` + `capsule list` showing the asset, and a
//!   re-run that moves nothing.
//! - [`provisioning_stores_no_client_supplied_text`] — the privacy constraint, read back out
//!   of Postgres: the plaintext `albums.name`/`albums.description` columns are empty.
//! - [`provisioning_is_idempotent`] — registering the same derived id again succeeds.
//! - [`provisioning_refuses_an_id_owned_by_another_account`] — and does so without saying
//!   whether that id exists.
//! - [`another_accounts_album_grants_no_write_capability`] — invariant 6 is intact: an album
//!   somebody else provisioned still confers nothing on the caller.
//!
//! [Organization — The Default Album]: https://docs/design/organization/#the-default-album

use base64::Engine as _;
use capsule_cli::remote::{self, PushOptions, RemoteConfig};
use capsule_cli::session::SessionStore;
use capsule_core::crypto::primitives::Argon2Params;
use capsule_core::lifecycle::Workspace;
use capsule_sdk::albums::{AlbumClient, AlbumTransport, StaticToken};
use nanoid::nanoid;
use salvo::Service;
use salvo::conn::tcp::TcpAcceptor;
use salvo::prelude::{Router, Server};
use sea_orm::{ActiveModelTrait, ColumnTrait, Database, EntityTrait, QueryFilter, Set};
use sea_orm_migration::MigratorTrait;
use uuid::Uuid;

use super::{TestCtx, setup};
use crate::state::{AppState, OpsState};

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

/// The whole server on ONE origin — `/v1/auth`, `/v1/upload`, `/v1/albums`, and the gRPC sync
/// feed at the root — mirroring `capsule_api::create_router`'s mounting, because that
/// single-origin shape is what `RemoteConfig` derives every surface from.
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

    // The `/albums` tree: provisioning (S-C25) plus the lifecycle-write surface (S-C16).
    let ops_state = OpsState::new(
        ctx.db.clone(),
        ctx.config.clone(),
        crate::service::ops::OpService::new(ctx.config.clone(), ctx.db.clone()),
    );
    let albums_router = crate::routes::get_ops_router(
        ops_state,
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
                .push(Router::with_path("upload").push(upload_router))
                .push(Router::with_path("albums").push(albums_router)),
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
        albums_endpoint: format!("{base}/v1/albums"),
        protocol_version: CLIENT_MAX_PROTOCOL.to_string(),
    }
}

/// One CLI-shaped library: `capsule library init` + `capsule import` of a single file, on the
/// signed lifecycle path with the CLI's own client identity. The album is the account's
/// **derived** default album — a UUID the server has never heard of.
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

/// An album client for the account currently persisted in `store`.
async fn album_client(remote: &RemoteConfig, store: &SessionStore) -> AlbumClient {
    let session = capsule_sdk::auth::AuthClient::new(&remote.auth_endpoint)
        .unwrap()
        .resume(store.load().unwrap().expect("session persisted"))
        .unwrap();
    use secrecy::ExposeSecret as _;
    let token = session
        .export()
        .await
        .expect("session tokens")
        .access_token
        .expose_secret()
        .to_string();
    AlbumClient::new(AlbumTransport::with_static_token(
        reqwest::Client::new(),
        &remote.albums_endpoint,
        StaticToken(token),
    ))
}

/// A scratch directory that cleans itself up.
fn scratch() -> tempfile::TempDir {
    tempfile::TempDir::new().unwrap()
}

/// **The slice's acceptance test.** The real CLI push path — register, import, read server
/// truth off the feed, push — puts bytes on the server for an album the *client* named, and
/// re-running it moves nothing.
///
/// Every request here is the production one: `remote::push` provisions the derived album
/// through the SDK's album client and then drives the tier ladder through the SDK's upload
/// client. Nothing is re-pointed at a server-minted id, and invariant 6 is enforced in full —
/// the album exists because provisioning created it, and the caller can write to it because
/// provisioning bound it to their owner group.
#[tokio::test]
async fn cli_push_round_trip_puts_bytes_on_the_server() {
    let ctx = setup().await;
    let base = serve_one_origin(&ctx).await;
    let remote = remote_config(&base);
    let work = scratch();
    let store = SessionStore::new(work.path().join("session.json"));

    let (_email, user_id) = register(&ctx, &remote, &store).await;
    let ws = imported_library(work.path());
    let album_id = ws.default_album_id();
    assert_eq!(ws.asset_ids().len(), 1, "one asset imported offline");
    assert_eq!(
        album_id.hyphenated().to_string().len(),
        36,
        "the client's album id is a hyphenated UUID — the id space the schema now holds"
    );

    // Server truth: a fresh account holds nothing, so the whole ladder is outstanding.
    let session_client = capsule_sdk::auth::AuthClient::new(&remote.auth_endpoint).unwrap();
    let session = session_client
        .resume(store.load().unwrap().expect("session persisted"))
        .unwrap();
    let held = remote::held_blobs(&remote, session, 256)
        .await
        .expect("the feed pull is reachable on the single origin");
    assert!(held.is_empty(), "a fresh account holds no blobs");

    // A dry run plans the full ladder, opens nothing, and provisions nothing.
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
    assert_eq!(
        planned.provisioned_albums, 0,
        "a dry run reaches nothing, the album surface included"
    );
    assert!(
        entity::album::Entity::find_by_id(album_id.hyphenated().to_string())
            .one(&ctx.db)
            .await
            .unwrap()
            .is_none(),
        "and it wrote no album row"
    );

    // The real push: provision, then the ladder.
    let summary = remote::push(&remote, &store, &ws, PushOptions::default())
        .await
        .expect("the real push lands on the client's own derived album");
    assert_eq!(summary.provisioned_albums, 1, "one album, registered once");
    assert_eq!(summary.created_albums, 1, "and it was created by this run");
    assert_eq!(summary.uploaded_blobs, 2, "metadata index + original");
    assert_eq!(summary.pushed_assets, 1);
    assert!(!summary.is_no_op());

    // The album row exists, is owned by the pusher's owner group, and carries the client's id
    // verbatim — the widened column stores the UUID with no truncation or blank padding.
    let album = entity::album::Entity::find_by_id(album_id.hyphenated().to_string())
        .one(&ctx.db)
        .await
        .unwrap()
        .expect("provisioning created the album row");
    assert_eq!(
        album.id,
        album_id.hyphenated().to_string(),
        "the stored id is byte-identical to the derived one"
    );
    assert_eq!(album.owner_id, user_id, "bound to the caller's owner group");
    assert!(album.deleted_at.is_none());

    // Durable: each blob is committed into the content-addressed store, its asset row is
    // marked uploaded, and a custody receipt was issued in the same transaction.
    let assets = entity::asset::Entity::find()
        .filter(entity::asset::Column::OwnerId.eq(&user_id))
        .all(&ctx.db)
        .await
        .unwrap();
    assert_eq!(assets.len(), 2, "one asset row per blob session");
    assert!(assets.iter().all(|a| a.uploaded), "every row is finalized");
    assert!(
        assets
            .iter()
            .all(|a| a.album_id.as_deref() == Some(album.id.as_str())),
        "every row is filed in the client's own album"
    );
    for asset in &assets {
        let bytes = ctx
            .storage
            .read_committed_blob(&asset.file_hash)
            .await
            .unwrap_or_else(|e| panic!("blob {} must be durable: {e}", asset.file_hash));
        assert_eq!(
            capsule_core::utils::hash::hash_bytes(&bytes),
            asset.file_hash,
            "the stored blob content-addresses to what was declared"
        );
    }
    let receipts = entity::custody_receipt::Entity::find()
        .filter(entity::custody_receipt::Column::UploadedByUser.eq(&user_id))
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
    let synced = remote::sync(&remote, &store, &cli_db, 256, false, false)
        .await
        .expect("sync");
    assert_eq!(synced.applied, 2, "both finalizations reached the feed");
    let rows = remote::list(&cli_db, false).await.expect("list");
    assert_eq!(rows.len(), 2, "capsule list shows what push put there");
    assert!(
        rows.iter().any(|row| row.original_held),
        "the original blob's finalization flips original_held"
    );

    // **Re-running the push is a no-op.** The album is re-registered (idempotently, writing
    // nothing) and server truth says every blob is already held, so no session opens.
    let rerun = remote::push(&remote, &store, &ws, PushOptions::default())
        .await
        .expect("a second push against an unchanged library succeeds");
    assert!(rerun.is_no_op(), "nothing moved: {rerun:?}");
    assert_eq!(rerun.already_held_blobs, 2);
    assert_eq!(
        rerun.provisioned_albums, 1,
        "the album was re-registered — idempotently"
    );
    assert_eq!(
        rerun.created_albums, 0,
        "…and that registration created nothing the second time"
    );

    // The feed fold is stable across the re-run, and no second album row appeared. Counts are
    // scoped to this account: `setup()` seeds an unrelated user/album pair of its own.
    assert_eq!(
        entity::sync_entry::Entity::find()
            .filter(entity::sync_entry::Column::AlbumId.eq(&album.id))
            .all(&ctx.db)
            .await
            .unwrap()
            .len(),
        2,
        "a no-op re-push mints no new feed entries"
    );
    assert_eq!(
        entity::album::Entity::find()
            .filter(entity::album::Column::OwnerId.eq(&user_id))
            .all(&ctx.db)
            .await
            .unwrap()
            .len(),
        1,
        "one derived id is one album row, however many times it is registered"
    );
}

/// **The privacy constraint, read back out of Postgres.** Provisioning accepts no album name,
/// so the plaintext `albums.name` / `albums.description` columns — a residue of the
/// pre-key-free schema (slice `S-C26` retires them) — hold the empty string. The client's real
/// album title ("Imports" in the local library) never leaves the device.
#[tokio::test]
async fn provisioning_stores_no_client_supplied_text() {
    let ctx = setup().await;
    let base = serve_one_origin(&ctx).await;
    let remote = remote_config(&base);
    let work = scratch();
    let store = SessionStore::new(work.path().join("session.json"));

    register(&ctx, &remote, &store).await;
    let ws = imported_library(work.path());
    // The library names this album locally; the name lives in the encrypted sidecar.
    assert!(
        ws.albums().iter().any(|(_, name)| name == "Imports"),
        "the client does hold a name for this album"
    );

    remote::push(&remote, &store, &ws, PushOptions::default())
        .await
        .expect("push");

    let album = entity::album::Entity::find_by_id(ws.default_album_id().hyphenated().to_string())
        .one(&ctx.db)
        .await
        .unwrap()
        .expect("the album row");
    assert_eq!(album.name, "", "no client-supplied name reaches the server");
    assert_eq!(album.description, "", "nor any client-supplied description");
}

/// **Idempotency is the contract.** The same derived id arrives from every device the user
/// owns and again after a recovery, so registering it repeatedly must succeed. Only the first
/// call reports `created`.
#[tokio::test]
async fn provisioning_is_idempotent() {
    let ctx = setup().await;
    let base = serve_one_origin(&ctx).await;
    let remote = remote_config(&base);
    let work = scratch();
    let store = SessionStore::new(work.path().join("session.json"));

    let (_email, user_id) = register(&ctx, &remote, &store).await;
    let client = album_client(&remote, &store).await;
    let album_id = Uuid::now_v7();

    let first = client
        .provision(album_id)
        .await
        .expect("first registration");
    assert!(first.created, "the first call creates the row");
    for attempt in 0..3 {
        let again = client
            .provision(album_id)
            .await
            .unwrap_or_else(|e| panic!("re-registration {attempt} must succeed, got {e}"));
        assert_eq!(again.album_id, album_id);
        assert!(!again.created, "and writes nothing");
    }

    assert_eq!(
        entity::album::Entity::find()
            .filter(entity::album::Column::OwnerId.eq(&user_id))
            .all(&ctx.db)
            .await
            .unwrap()
            .len(),
        1,
        "four registrations of one id are one row"
    );
}

/// An id already bound to a **different** account is refused with its own stable code — and
/// the refusal says nothing about whether that id exists, so the endpoint is not an existence
/// oracle over other accounts' derived album ids.
#[tokio::test]
async fn provisioning_refuses_an_id_owned_by_another_account() {
    let ctx = setup().await;
    let base = serve_one_origin(&ctx).await;
    let remote = remote_config(&base);
    let work = scratch();

    // Account A registers an album.
    let store_a = SessionStore::new(work.path().join("session-a.json"));
    register(&ctx, &remote, &store_a).await;
    let client_a = album_client(&remote, &store_a).await;
    let owned_by_a = Uuid::now_v7();
    client_a.provision(owned_by_a).await.expect("A registers");

    // Account B tries the same id, and separately an id nobody has ever registered that it
    // could not own either.
    let store_b = SessionStore::new(work.path().join("session-b.json"));
    let (_email_b, user_b) = register(&ctx, &remote, &store_b).await;
    let client_b = album_client(&remote, &store_b).await;

    let refused = client_b
        .provision(owned_by_a)
        .await
        .expect_err("B cannot take over A's album");
    assert_eq!(
        refused.error_code(),
        Some(capsule_i18n::error_codes::ALBUM_NOT_AVAILABLE),
        "the refusal is its own code, distinct from a malformed id: {refused}"
    );
    assert_ne!(
        refused.error_code(),
        Some(capsule_i18n::error_codes::UPLOAD_ALBUM_ACCESS_DENIED),
        "and distinct from the upload-time invariant-6 refusal"
    );

    // A's album is untouched, and B did not silently acquire one.
    let album = entity::album::Entity::find_by_id(owned_by_a.hyphenated().to_string())
        .one(&ctx.db)
        .await
        .unwrap()
        .expect("A's album survives");
    let a_owner = entity::owner_member::Entity::find()
        .filter(entity::owner_member::Column::OwnerId.eq(&album.owner_id))
        .all(&ctx.db)
        .await
        .unwrap();
    assert_eq!(a_owner.len(), 1, "still a solo owner group");
    assert_ne!(album.owner_id, user_b, "the album is still A's");
    assert!(
        entity::album::Entity::find()
            .filter(entity::album::Column::OwnerId.eq(&user_b))
            .all(&ctx.db)
            .await
            .unwrap()
            .is_empty(),
        "the refusal created nothing for B"
    );
}

/// A push into an album the caller does **not** own is still refused at invariant 6 — S-C25
/// gave clients a way to register an album, not a way around the album gate.
#[tokio::test]
async fn another_accounts_album_grants_no_write_capability() {
    let ctx = setup().await;
    let base = serve_one_origin(&ctx).await;
    let remote = remote_config(&base);
    let work = scratch();

    // A pushes a real library, so A owns a real, populated album.
    let store_a = SessionStore::new(work.path().join("session-a.json"));
    let (_email_a, user_a) = register(&ctx, &remote, &store_a).await;
    let ws = imported_library(&work.path().join("a"));
    let a_album = ws.default_album_id().hyphenated().to_string();
    remote::push(&remote, &store_a, &ws, PushOptions::default())
        .await
        .expect("A pushes");

    // B registers, and cannot take the album over.
    let store_b = SessionStore::new(work.path().join("session-b.json"));
    let (_email_b, user_b) = register(&ctx, &remote, &store_b).await;
    let refused = album_client(&remote, &store_b)
        .await
        .provision(ws.default_album_id())
        .await
        .expect_err("B cannot register A's album");
    assert_eq!(
        refused.error_code(),
        Some(capsule_i18n::error_codes::ALBUM_NOT_AVAILABLE)
    );

    // The album gate invariant 6 consults agrees: A can write, B holds nothing. Provisioning
    // did not weaken the check — it is the only thing that makes it passable for a real client.
    let a_access = service::album::Query::get_album_access(&ctx.db, &user_a, &a_album)
        .await
        .unwrap();
    assert!(
        a_access.is_some_and(|access| access.is_write()),
        "A can write to the album provisioning bound to A"
    );
    let b_access = service::album::Query::get_album_access(&ctx.db, &user_b, &a_album)
        .await
        .unwrap();
    assert!(
        b_access.is_none(),
        "B holds no capability on A's album, so invariant 6 refuses B's uploads"
    );
}
