//! In-crate integration tests for slice `S-C2` (key-free sync feed). These run against a
//! real Postgres (testcontainer, mirroring the S-C1 pattern), seed feed entries through the
//! shared `service::sync` writer, and exercise the gRPC `SyncService` both directly and — for
//! the salvo↔tonic bridge — over a real gRPC client on the wire.
//!
//! Coverage map (download-sync Validation bullets, server-side):
//! - `sync_feed_forward_version_rejection` — the `x-capsule-protocol` handshake.
//! - `sync_feed_rewind_rejection` — forward-only, idempotent cursor pagination.
//! - `sync_cursor_authenticity` — a tampered/forged cursor is rejected (invariant 22).
//! - `sync_bridge_end_to_end` — a real tonic client through the salvo bridge.
//! (Sync-feed monotonicity — the per-album `sync_seq` mint in the finalization transaction —
//! is tested in `capsule-api-upload`'s `tests::sync_feed`, against the real finalization path.)

#![allow(clippy::unwrap_used)]

mod cli_round_trip;
mod federation;
mod sdk_client;

use std::sync::Once;

use auth::claims::Claims;
use base64::Engine as _;
use jsonwebtoken::{DecodingKey, EncodingKey};
use migration::Migrator;
use nanoid::nanoid;
use sea_orm::{ActiveModelTrait, Database, DatabaseConnection, Set};
use sea_orm_migration::MigratorTrait;
use service::sync::{ChangeKind, FeedBlobManifest, FeedBlobRef, FeedEntryInput};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;
use tonic::{Code, Request};

use crate::config::{
    DEFAULT_PAGE_SIZE, DEFAULT_PROTOCOL_MAX, DEFAULT_PROTOCOL_MIN, MAX_PAGE_SIZE, SyncServerConfig,
};
use crate::feed::SyncFeedService;
use crate::proto::capsule::sync::v1::SyncRequest;
use crate::proto::capsule::sync::v1::sync_service_server::SyncService;

static TRACING: Once = Once::new();

/// The protocol version the tests speak (inside the default `[min, max]` window).
const PROTOCOL: &str = "2026-05-31";

/// A base64 Ed25519 pkcs8 keypair (mirrors the auth/upload harness) so tests mint access
/// tokens the sync feed accepts.
const PRIV_B64: &str = "MC4CAQAwBQYDK2VwBCIEIN6eTvXEL7xMZWHY8rTk7VbQSGSuRkle5MVfiiYUStLF";
const PUB_B64: &str = "MCowBQYDK2VwAyEA66iVaMz1x2ogToGm5Hw34aITBLLqz0iEonbwjK57pWU=";

/// The server-only cursor MAC key the test config uses.
const CURSOR_KEY: [u8; 32] = [0x5c; 32];

pub(crate) struct TestCtx {
    _postgres: ContainerAsync<Postgres>,
    pub db: DatabaseConnection,
    pub config: SyncServerConfig,
    encoding_key: EncodingKey,
    pub user_id: String,
    pub album_id: String,
}

impl TestCtx {
    fn service(&self) -> SyncFeedService {
        SyncFeedService::new(self.db.clone(), &self.config)
    }

    /// A bearer access token for the seeded user.
    fn token(&self) -> String {
        Claims::new_access_token(self.user_id.clone(), None)
            .encode(&self.encoding_key)
            .expect("encode token")
    }

    /// A `Sync` request with a valid bearer + in-window protocol metadata.
    fn request(&self, cursor: Vec<u8>, page_size: u32) -> Request<SyncRequest> {
        let mut req = Request::new(SyncRequest { cursor, page_size });
        self.attach_metadata(req.metadata_mut(), PROTOCOL);
        req
    }

    fn attach_metadata(&self, md: &mut tonic::metadata::MetadataMap, protocol: &str) {
        md.insert(
            "authorization",
            format!("Bearer {}", self.token()).parse().unwrap(),
        );
        md.insert("x-capsule-protocol", protocol.parse().unwrap());
    }

    /// Seed one feed entry for the seeded album and return its minted `sync_seq`.
    async fn seed_entry(&self) -> i64 {
        let input = FeedEntryInput {
            album_id: self.album_id.clone(),
            protocol_version: PROTOCOL.to_string(),
            kind: ChangeKind::Created,
            asset_id: nanoid!(),
            manifest_cbor: vec![0xa0], // empty canonical-CBOR map (opaque here)
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
        service::sync::Mutation::record_finalization(&self.db, input)
            .await
            .expect("seed feed entry")
    }
}

fn decode_keys() -> (EncodingKey, DecodingKey) {
    let engine = base64::engine::general_purpose::STANDARD;
    let priv_bytes = engine.decode(PRIV_B64).expect("priv");
    let pub_bytes = engine.decode(PUB_B64).expect("pub");
    (
        EncodingKey::from_ed_der(&priv_bytes),
        DecodingKey::from_ed_der(&pub_bytes),
    )
}

/// Spin up a fresh Postgres, run migrations, seed a user/owner/album, and build the sync
/// service config.
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

    let (encoding_key, decoding_key) = decode_keys();

    let config = SyncServerConfig {
        upload_dir: std::env::temp_dir(),
        jwt_eddsa_decoding_key: decoding_key,
        protocol_min: DEFAULT_PROTOCOL_MIN.to_string(),
        protocol_max: DEFAULT_PROTOCOL_MAX.to_string(),
        cursor_mac_key: CURSOR_KEY,
        default_page_size: DEFAULT_PAGE_SIZE,
        max_page_size: MAX_PAGE_SIZE,
    };

    // Seed user U, owner group id=U (owner_member U∈U), and album A owned by U — matching the
    // upload harness so accessible_album_ids(U) resolves A.
    let user_id = nanoid!();
    let created = jiff::Timestamp::now() - jiff::SignedDuration::from_hours(24);
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
    .expect("insert album");

    TestCtx {
        _postgres: container,
        db,
        config,
        encoding_key,
        user_id,
        album_id,
    }
}

/// **Sync cursor authenticity (invariant 22).** A tampered cursor MAC is rejected at the
/// boundary with `INVALID_ARGUMENT` + `error.sync.cursor_invalid`; a valid cursor round-trips.
#[tokio::test]
async fn sync_cursor_authenticity() {
    let ctx = setup().await;
    ctx.seed_entry().await;
    let svc = ctx.service();

    // A valid empty (first-sync) cursor is accepted and yields a signed next_cursor.
    let ok = svc.sync(ctx.request(vec![], 10)).await.expect("valid sync");
    let next = ok.get_ref().next_cursor.clone();
    assert!(!next.is_empty(), "server returns a signed next_cursor");

    // Flip a bit in the returned cursor: the MAC no longer verifies.
    let mut forged = next.clone();
    let last = forged.len() - 1;
    forged[last] ^= 0x01;
    let err = svc.sync(ctx.request(forged, 10)).await.unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);
    assert_eq!(
        err.metadata().get("x-capsule-error-code").unwrap(),
        capsule_i18n::error_codes::SYNC_CURSOR_INVALID
    );

    // A re-issue of the untouched valid cursor is still accepted.
    svc.sync(ctx.request(next, 10)).await.expect("valid cursor");
}

/// **Sync feed forward-version rejection.** A request whose `x-capsule-protocol` is beyond the
/// accepted window is rejected via the handshake with `FAILED_PRECONDITION` +
/// `error.protocol.version_unsupported`, and the accepted range is advertised.
#[tokio::test]
async fn sync_feed_forward_version_rejection() {
    let ctx = setup().await;
    let svc = ctx.service();

    let mut req = Request::new(SyncRequest {
        cursor: vec![],
        page_size: 10,
    });
    ctx.attach_metadata(req.metadata_mut(), "2099-01-01"); // beyond protocol_max

    let err = svc.sync(req).await.unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);
    assert_eq!(
        err.metadata().get("x-capsule-error-code").unwrap(),
        capsule_i18n::error_codes::PROTOCOL_VERSION_UNSUPPORTED
    );
    assert!(
        err.metadata().get("x-capsule-protocol-max").is_some(),
        "rejection advertises the accepted range"
    );
}

/// **Sync feed rewind rejection.** The cursor is strictly forward-only and idempotent: paging
/// with a cursor returns only entries after it (never a repeat), re-issuing the same cursor
/// returns the identical page, and the per-album `sync_seq` never regresses within a page.
#[tokio::test]
async fn sync_feed_rewind_rejection() {
    let ctx = setup().await;
    let mut seqs = Vec::new();
    for _ in 0..5 {
        seqs.push(ctx.seed_entry().await);
    }
    assert_eq!(seqs, vec![1, 2, 3, 4, 5], "per-album sync_seq is monotonic");

    let svc = ctx.service();

    // First page of 2 from the start.
    let page1 = svc.sync(ctx.request(vec![], 2)).await.unwrap();
    let p1 = page1.get_ref();
    assert_eq!(p1.entries.len(), 2);
    assert_eq!(seqs_of(p1), vec![1, 2]);

    // Idempotency: re-issuing the SAME (empty) cursor returns the identical first page.
    let page1_again = svc.sync(ctx.request(vec![], 2)).await.unwrap();
    assert_eq!(seqs_of(page1_again.get_ref()), vec![1, 2]);
    assert_eq!(
        page1_again.get_ref().next_cursor,
        p1.next_cursor,
        "same cursor ⇒ same next_cursor (deterministic page)"
    );

    // Forward-only: paging with next_cursor never re-returns a seen entry.
    let page2 = svc
        .sync(ctx.request(p1.next_cursor.clone(), 2))
        .await
        .unwrap();
    assert_eq!(
        seqs_of(page2.get_ref()),
        vec![3, 4],
        "no rewind, no repeats"
    );

    let page3 = svc
        .sync(ctx.request(page2.get_ref().next_cursor.clone(), 2))
        .await
        .unwrap();
    assert_eq!(seqs_of(page3.get_ref()), vec![5]);

    // Draining past the tail yields an empty page and a stable cursor.
    let tail = svc
        .sync(ctx.request(page3.get_ref().next_cursor.clone(), 2))
        .await
        .unwrap();
    assert!(
        tail.get_ref().entries.is_empty(),
        "tail cursor drains cleanly"
    );

    // sync_seq is strictly increasing across the whole feed (the client's anti-rewind basis).
    let all = svc.sync(ctx.request(vec![], 100)).await.unwrap();
    let observed = seqs_of(all.get_ref());
    assert!(
        observed.windows(2).all(|w| w[0] < w[1]),
        "sync_seq never regresses: {observed:?}"
    );
}

/// **Salvo↔tonic bridge, end-to-end.** A real gRPC client talks to the served endpoint through
/// the salvo bridge and reads the feed with negotiation + auth metadata on the wire.
#[tokio::test]
async fn sync_bridge_end_to_end() {
    use salvo::Service;
    use salvo::conn::tcp::TcpAcceptor;
    use salvo::prelude::Server;

    use crate::proto::capsule::sync::v1::sync_service_client::SyncServiceClient;

    let ctx = setup().await;
    ctx.seed_entry().await;
    ctx.seed_entry().await;

    let router = crate::get_router(ctx.db.clone(), ctx.config.clone())
        .await
        .expect("router");
    let service = Service::new(router);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let acceptor = TcpAcceptor::try_from(listener).unwrap();
    let server = tokio::spawn(async move { Server::new(acceptor).serve(service).await });

    let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .expect("connect gRPC");
    let mut client = SyncServiceClient::new(channel);

    let mut request = Request::new(SyncRequest {
        cursor: vec![],
        page_size: 100,
    });
    ctx.attach_metadata(request.metadata_mut(), PROTOCOL);
    let response = client.sync(request).await.expect("gRPC sync over bridge");
    assert_eq!(
        response.get_ref().entries.len(),
        2,
        "the bridge delivered the feed page end-to-end"
    );
    assert!(!response.get_ref().next_cursor.is_empty());

    // A forged cursor is rejected across the wire too.
    let mut bad = Request::new(SyncRequest {
        cursor: vec![9u8; 41],
        page_size: 10,
    });
    ctx.attach_metadata(bad.metadata_mut(), PROTOCOL);
    let status = client.sync(bad).await.unwrap_err();
    assert_eq!(status.code(), Code::InvalidArgument);

    server.abort();
}

fn seqs_of(resp: &crate::proto::capsule::sync::v1::SyncResponse) -> Vec<u64> {
    resp.entries.iter().map(|e| e.sync_seq).collect()
}
