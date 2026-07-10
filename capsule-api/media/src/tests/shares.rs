//! Share-link serving tests for slice `S-C4` — the [Share Links design doc]'s six Validation
//! bullets, plus the deterministic rate-limit + fail-closed cache proofs.
//!
//! Coverage map (six bullets → tests):
//! - **Opaque-id entropy (unit)** — `opaque_id_is_128bit_csprng`.
//! - **Enumeration resistance (smoke)** — `random_opaque_ids_are_404` +
//!   `indistinguishable_404_is_byte_identical` (byte-identical body **and** headers across
//!   unknown / revoked / expired) + the deterministic `per_ip_and_per_link_rate_limits`.
//! - **Passphrase unwrap locality (unit)** — `wrapped_secret_served_opaquely`.
//! - **Revocation honored (smoke)** — `revoked_link_is_404` (DB) + `fail_closed_past_ttl` and
//!   `cache_serves_within_ttl_then_honors_revocation_past_ttl` (injected clock + resolver).
//! - **Privacy-strip on serve (unit)** — `metadata_is_stripped_on_serve_with_no_opt_out`.
//! - **Home-server-only (unit)** — `foreign_home_server_returns_pointer_not_content`.
//!
//! The HTTP smokes run against a real Postgres (the S-C3 `setup()` harness) over the real `/s`
//! router; the rate-limit and fail-closed proofs drive [`ShareServeService`] directly with a
//! `MockClock` + mock resolver (no sleeps, no torn-down DB), mirroring the S-C3 seam pattern.
//!
//! [Share Links design doc]: ../../../../capsule-docs/src/content/docs/design/share-links.md

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use capsule_core::crypto::hash::Hash32;
use capsule_core::crypto::primitives::Argon2Params;
use capsule_core::crypto::{CRYPTO_SUITE_ID, pwkdf};
use capsule_core::sharing::{WrappedScope, generate_opaque_id};
use capsule_core::sidecar::sidecar_v1::{CameraId, Gps, GpsSource, SIDECAR_SCHEMA_V1, SidecarV1};
use jiff::{SignedDuration, Timestamp};
use salvo::http::StatusCode;
use salvo::test::{ResponseExt, TestClient};
use salvo::{Service, async_trait};
use serde_json::Value;
use service::share::{
    Mutation as ShareMutation, PublishShare, ServeRecord, ShareAssetInput, ShareResolution,
    ShareScopeKind,
};
use uuid::Uuid;

use super::{TestCtx, hex_encode, setup};
use crate::service::share::{
    LinkResolver, ResolveError, ServeLimits, ServeOutcome, ShareServeService,
};
use crate::share_state::ShareState;

// ─────────────────────────────── Fixtures ────────────────────────────────────────

/// This deployment's home-server id (matches `TestCtx::media_config().server_id`).
const SELF_SERVER: &str = super::TEST_SERVER_ID;

/// A fixed instant for the deterministic (injected-clock) tests.
fn t0() -> Timestamp {
    "2026-07-10T00:00:00Z".parse().unwrap()
}

/// Tiny Argon2id params so the passphrase-wrap fixture is fast.
fn fast_argon() -> Argon2Params {
    Argon2Params {
        mem_kib: 64,
        t_cost: 1,
        p_cost: 1,
    }
}

/// A sidecar carrying every fingerprinting field (camera serial, device/session ids, precise
/// GPS) so the serve-path strip is observable (mirrors the `export_policy` fixture).
fn fingerprinted_sidecar() -> SidecarV1 {
    SidecarV1 {
        sidecar_schema: SIDECAR_SCHEMA_V1,
        crypto_suite_id: CRYPTO_SUITE_ID,
        uuid: Uuid::from_u128(1),
        hash: Hash32([0; 32]),
        capture_timestamp: "2026-05-31T10:00:00Z".into(),
        import_timestamp: "2026-05-31T11:00:00Z".into(),
        content_type: "image/jpeg".into(),
        dimensions: None,
        lqip: None,
        tags_user: Default::default(),
        tags_ai: Default::default(),
        caption: Default::default(),
        rating: Default::default(),
        stack_membership: Default::default(),
        cull: Default::default(),
        hidden: Default::default(),
        camera_id: Some(CameraId {
            model: "iPhone 15 Pro".into(),
            serial: "SECRET-SERIAL".into(),
        }),
        device_id: Uuid::from_u128(0xD1),
        session_id: Uuid::from_u128(0x5E),
        gps: Some(Gps {
            lat: 40.712812,
            lon: -74.006015,
            source: GpsSource::Exif,
        }),
        provenance_chain_hash: Some(Hash32([0; 32])),
        unknown: BTreeMap::new(),
        signature: None,
    }
}

/// A `LinkOnly` (no-passphrase) wrapped scope for the opaque store/serve path.
fn link_only_scope() -> WrappedScope {
    WrappedScope::LinkOnly {
        blob: b"opaque-encapsulated-scope-material".to_vec(),
    }
}

/// A passphrase-protected wrapped scope (an Argon2id layer wraps the material).
fn passphrase_scope() -> WrappedScope {
    WrappedScope::Passphrase {
        wrapped: pwkdf::wrap_with(&[0u8; 16], b"correct horse", fast_argon()).unwrap(),
    }
}

/// One covered asset with the given content hash and sidecar.
fn asset(content_hash: &str, sidecar: SidecarV1) -> ShareAssetInput {
    ShareAssetInput {
        asset_id: Uuid::now_v7().to_string(),
        content_hash: content_hash.to_string(),
        content_type: "image/jpeg".to_string(),
        size: 12,
        sidecar,
    }
}

/// Publish a share link through the service publish surface (mirrors the drop-store provision
/// step). Returns `(link_id, opaque_id)`.
async fn publish(
    ctx: &TestCtx,
    home_server: &str,
    wrapped_scope: WrappedScope,
    expires_at: Option<Timestamp>,
    assets: Vec<ShareAssetInput>,
) -> (String, String) {
    let opaque_id = hex_encode(&generate_opaque_id());
    let link_id = ShareMutation::publish_share(
        &ctx.db,
        PublishShare {
            owner_id: ctx.user_id.clone(),
            opaque_id: opaque_id.clone(),
            home_server: home_server.to_string(),
            scope_kind: ShareScopeKind::Album,
            scope_id: ctx.album_id.clone(),
            wrapped_scope,
            assets,
            expires_at,
        },
    )
    .await
    .expect("publish share");
    (link_id, opaque_id)
}

/// A salvo service over the real share serve router, mounted like the app at `/s`.
fn share_service(ctx: &TestCtx) -> Service {
    let state = ShareState::new(ctx.db.clone(), ctx.media_config());
    let router = salvo::Router::new()
        .push(salvo::Router::with_path("s").push(crate::routes::get_share_router(state)));
    Service::new(router)
}

/// A `GET /s/{path}` request; returns `(status, sorted headers, body bytes)` — the raw material
/// for the byte-identity assertion.
async fn get_raw(svc: &Service, path: &str) -> (StatusCode, Vec<(String, Vec<u8>)>, Vec<u8>) {
    let mut res = TestClient::get(format!("http://localhost/s/{path}"))
        .send(svc)
        .await;
    let status = res.status_code.unwrap_or(StatusCode::OK);
    let mut headers: Vec<(String, Vec<u8>)> = res
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.as_bytes().to_vec()))
        .collect();
    headers.sort();
    let body = res.take_bytes(None).await.unwrap_or_default().to_vec();
    (status, headers, body)
}

/// A `GET /s/{path}` request returning `(status, json)`.
async fn get_json(svc: &Service, path: &str) -> (StatusCode, Value) {
    let mut res = TestClient::get(format!("http://localhost/s/{path}"))
        .send(svc)
        .await;
    let status = res.status_code.unwrap_or(StatusCode::OK);
    let json = res.take_json::<Value>().await.unwrap_or(Value::Null);
    (status, json)
}

// ─────────────────── Bullet 1: opaque-id entropy (unit) ───────────────────────────

/// The serve path is addressed by the full 128-bit CSPRNG opaque id (never a structured/
/// sequential id whose timestamp would cut entropy) — the structural enumeration defense.
#[tokio::test]
async fn opaque_id_is_128bit_csprng() {
    let ids: Vec<[u8; 16]> = (0..256).map(|_| generate_opaque_id()).collect();
    assert_eq!(ids[0].len(), 16, "opaque id must be a full 128 bits");
    // No collisions across a batch (CSPRNG, not sequential).
    for i in 0..ids.len() {
        for j in (i + 1)..ids.len() {
            assert_ne!(ids[i], ids[j], "two CSPRNG opaque ids must never collide");
        }
    }
    // Not time-ordered: the leading 48 bits (a UUIDv7 timestamp field) are not monotonic.
    let high48 = |id: &[u8; 16]| id[..6].iter().fold(0u64, |a, &b| (a << 8) | b as u64);
    assert!(
        ids.windows(2).any(|w| high48(&w[0]) >= high48(&w[1])),
        "opaque ids must not be time-ordered like a UUIDv7"
    );
}

// ─────────────────── Bullet 2: enumeration resistance (smoke) ─────────────────────

/// Probing the serve endpoint with random ids reveals nothing: every unknown id is a `404`.
#[tokio::test]
async fn random_opaque_ids_are_404() {
    let ctx = setup().await;
    let svc = share_service(&ctx);
    for _ in 0..16 {
        let (status, _, _) = get_raw(&svc, &hex_encode(&generate_opaque_id())).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}

/// A not-found, a revoked, and an expired link all return a **byte-identical** `404` — same
/// status, same headers, same (empty) body — so a probe cannot distinguish them (never a `410`).
#[tokio::test]
async fn indistinguishable_404_is_byte_identical() {
    let ctx = setup().await;
    let svc = share_service(&ctx);

    // Unknown: a random id that was never published.
    let unknown = hex_encode(&generate_opaque_id());

    // Revoked: publish a live link on this home server, then revoke it.
    let (revoked_link, revoked) = publish(&ctx, SELF_SERVER, link_only_scope(), None, vec![]).await;
    ShareMutation::revoke_share(&ctx.db, &ctx.user_id, &revoked_link)
        .await
        .expect("revoke");

    // Expired: publish a link whose expiry is already in the past.
    let past = Timestamp::now() - SignedDuration::from_hours(1);
    let (_expired_link, expired) =
        publish(&ctx, SELF_SERVER, link_only_scope(), Some(past), vec![]).await;

    let a = get_raw(&svc, &unknown).await;
    let b = get_raw(&svc, &revoked).await;
    let c = get_raw(&svc, &expired).await;

    assert_eq!(a.0, StatusCode::NOT_FOUND);
    // Byte-identical status, headers, and body across all three.
    assert_eq!(a.0, b.0, "unknown vs revoked status differ");
    assert_eq!(a.0, c.0, "unknown vs expired status differ");
    assert_eq!(
        a.1, b.1,
        "unknown vs revoked headers differ: {:?} {:?}",
        a.1, b.1
    );
    assert_eq!(
        a.1, c.1,
        "unknown vs expired headers differ: {:?} {:?}",
        a.1, c.1
    );
    assert_eq!(a.2, b.2, "unknown vs revoked body differ");
    assert_eq!(a.2, c.2, "unknown vs expired body differ");
}

// ─────────────────── Bullet 3: passphrase unwrap locality (unit) ──────────────────

/// The wrapped-secret endpoint serves **only** the opaque wrapped material; the passphrase never
/// crosses the wire (the server never receives it), and the metadata flags the passphrase layer.
#[tokio::test]
async fn wrapped_secret_served_opaquely() {
    let ctx = setup().await;
    let svc = share_service(&ctx);
    let scope = passphrase_scope();
    let expected_b64 = B64.encode(capsule_core::cbor::to_canonical_vec(&scope).unwrap());
    let (_id, opaque) = publish(&ctx, SELF_SERVER, scope, None, vec![]).await;

    // The metadata flags the passphrase layer so the client knows to prompt + unwrap locally.
    let (mstatus, meta) = get_json(&svc, &opaque).await;
    assert_eq!(mstatus, StatusCode::OK);
    assert_eq!(meta["passphrase_protected"], Value::Bool(true));

    // The wrapped-secret endpoint returns the exact opaque material, and only that.
    let (wstatus, wrapped) = get_json(&svc, &format!("{opaque}/wrapped-secret")).await;
    assert_eq!(wstatus, StatusCode::OK);
    assert_eq!(wrapped["passphrase_protected"], Value::Bool(true));
    assert_eq!(wrapped["wrapped_scope"], Value::String(expected_b64));
    // No passphrase field is ever present in the served payload.
    assert!(wrapped.get("passphrase").is_none());
}

// ─────────────────── Bullet 4: revocation honored (smoke + injected) ──────────────

/// A revoked link is refused (indistinguishable `404`) on the serve path.
#[tokio::test]
async fn revoked_link_is_404() {
    let ctx = setup().await;
    let svc = share_service(&ctx);
    let (link_id, opaque) = publish(&ctx, SELF_SERVER, link_only_scope(), None, vec![]).await;

    // Live before revocation.
    let (before, _) = get_json(&svc, &opaque).await;
    assert_eq!(before, StatusCode::OK);

    ShareMutation::revoke_share(&ctx.db, &ctx.user_id, &link_id)
        .await
        .expect("revoke");

    // A fresh serve engine (no warm cache) honors the revocation immediately.
    let fresh = share_service(&ctx);
    let (after, _, _) = get_raw(&fresh, &opaque).await;
    assert_eq!(after, StatusCode::NOT_FOUND);
}

/// Past the revocation-cache TTL, a serve process that cannot confirm liveness **refuses** rather
/// than serving on stale-allowed state (fail-closed; deterministic via injected clock + resolver).
#[tokio::test]
async fn fail_closed_past_ttl() {
    let clock = super::MockClock::new(t0());
    let resolver = Arc::new(MockResolver::live("opq"));
    let svc = ShareServeService::with_seams(
        resolver.clone(),
        Arc::new(clock.clone()),
        ServeLimits {
            max_per_window: 1000,
            window: SignedDuration::from_secs(60),
            revocation_ttl: SignedDuration::from_secs(60),
        },
    );

    // First serve (resolver reachable): live → served + cached at T0.
    assert!(matches!(
        svc.resolve_serve("opq", "1.2.3.4").await,
        ServeOutcome::Serve(_)
    ));

    // Revocation state goes unreachable; still within the TTL the cache is trusted (serves).
    resolver.set_unreachable(true);
    assert!(matches!(
        svc.resolve_serve("opq", "1.2.3.4").await,
        ServeOutcome::Serve(_)
    ));

    // Past the TTL, the cache is stale and the read fails → fail closed (refuse, never serve).
    clock.advance(SignedDuration::from_secs(61));
    assert!(matches!(
        svc.resolve_serve("opq", "1.2.3.4").await,
        ServeOutcome::NotFound
    ));
}

/// Within the TTL a cached-live result is trusted; past the TTL an authoritative re-read honors a
/// revocation that happened after caching. Bounds intra-server staleness to the TTL.
#[tokio::test]
async fn cache_serves_within_ttl_then_honors_revocation_past_ttl() {
    let clock = super::MockClock::new(t0());
    let resolver = Arc::new(MockResolver::live("opq"));
    let svc = ShareServeService::with_seams(
        resolver.clone(),
        Arc::new(clock.clone()),
        ServeLimits {
            max_per_window: 1000,
            window: SignedDuration::from_secs(60),
            revocation_ttl: SignedDuration::from_secs(60),
        },
    );

    // Serve + cache at T0.
    assert!(matches!(
        svc.resolve_serve("opq", "ip").await,
        ServeOutcome::Serve(_)
    ));
    // The link is revoked authoritatively, but within the TTL the cache still serves it.
    resolver.set_resolution(ShareResolution::Gone);
    clock.advance(SignedDuration::from_secs(30));
    assert!(matches!(
        svc.resolve_serve("opq", "ip").await,
        ServeOutcome::Serve(_)
    ));
    // Past the TTL, the re-read honors the revocation.
    clock.advance(SignedDuration::from_secs(31));
    assert!(matches!(
        svc.resolve_serve("opq", "ip").await,
        ServeOutcome::NotFound
    ));
}

// ─────────────────── Bullet 2 (cont.): rate limits (injected clock) ───────────────

/// Two independent fixed-window limiters — per source IP and per `{opaque-id}` — both charged on
/// every serve, deterministically driven by an injected clock.
#[tokio::test]
async fn per_ip_and_per_link_rate_limits() {
    let clock = super::MockClock::new(t0());
    let resolver = Arc::new(MockResolver::live("x"));
    let svc = ShareServeService::with_seams(
        resolver,
        Arc::new(clock.clone()),
        ServeLimits {
            max_per_window: 3,
            window: SignedDuration::from_secs(60),
            revocation_ttl: SignedDuration::from_secs(60),
        },
    );

    // Per-`{opaque-id}`: 3 hits on one id from one IP are served, the 4th is throttled.
    for _ in 0..3 {
        assert!(matches!(
            svc.resolve_serve("link-a", "10.0.0.1").await,
            ServeOutcome::Serve(_)
        ));
    }
    assert!(matches!(
        svc.resolve_serve("link-a", "10.0.0.1").await,
        ServeOutcome::RateLimited
    ));

    // Per-IP is a *separate* budget: a different id from a *fresh* IP is served (its own opaque
    // bucket) — proving the two limiters are independent.
    assert!(matches!(
        svc.resolve_serve("link-b", "10.0.0.2").await,
        ServeOutcome::Serve(_)
    ));

    // The per-IP budget throttles enumeration across *different* ids from one source. IP
    // 10.0.0.3 hits 3 distinct ids (each its own opaque bucket, all within budget) then a 4th —
    // the opaque bucket is fresh but the IP bucket is exhausted.
    for i in 0..3 {
        assert!(matches!(
            svc.resolve_serve(&format!("scan-{i}"), "10.0.0.3").await,
            ServeOutcome::Serve(_)
        ));
    }
    assert!(matches!(
        svc.resolve_serve("scan-3", "10.0.0.3").await,
        ServeOutcome::RateLimited
    ));

    // Advancing past the window resets both buckets.
    clock.advance(SignedDuration::from_secs(61));
    assert!(matches!(
        svc.resolve_serve("link-a", "10.0.0.1").await,
        ServeOutcome::Serve(_)
    ));
}

// ─────────────────── Bullet 5: privacy strip on serve (unit) ──────────────────────

/// The serve path **always** strips the boundary-crossing fingerprinting fields from the served
/// metadata blob — camera serial, device/session ids, precise GPS — with **no opt-out**: even a
/// request that *asks* to retain them (a hypothetical query flag) is served stripped.
#[tokio::test]
async fn metadata_is_stripped_on_serve_with_no_opt_out() {
    let ctx = setup().await;
    let svc = share_service(&ctx);
    let hash = "a".repeat(64);
    let (_id, opaque) = publish(
        &ctx,
        SELF_SERVER,
        link_only_scope(),
        None,
        vec![asset(&hash, fingerprinted_sidecar())],
    )
    .await;

    // Even with a would-be opt-out query flag, the strip is unconditional.
    let (status, meta) = get_json(
        &svc,
        &format!("{opaque}?retain_camera_serial=true&retain_full_gps=true"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let blob_b64 = meta["assets"][0]["metadata_blob"]
        .as_str()
        .expect("metadata_blob");
    let served =
        SidecarV1::from_canonical_slice(&B64.decode(blob_b64).unwrap(), SIDECAR_SCHEMA_V1).unwrap();

    // Fingerprinting fields are stripped; the non-identifying camera model is retained.
    assert_eq!(served.camera_id.as_ref().unwrap().serial, "");
    assert_eq!(served.camera_id.as_ref().unwrap().model, "iPhone 15 Pro");
    assert_eq!(served.device_id, Uuid::nil());
    assert_eq!(served.session_id, Uuid::nil());
    let gps = served.gps.unwrap();
    assert_eq!(gps.lat, 40.71, "GPS truncated to city level");
    assert_eq!(gps.lon, -74.01);
}

// ─────────────────── Bullet 6: home-server-only (unit) ────────────────────────────

/// A share this server does not host is not served: the endpoint returns a structured
/// `{ home_server }` pointer (never content, never an HTTP redirect). A share this server *does*
/// host serves normally.
#[tokio::test]
async fn foreign_home_server_returns_pointer_not_content() {
    let ctx = setup().await;
    let svc = share_service(&ctx);

    // A share whose home server is a *peer* (not this server).
    let (_id, foreign) = publish(&ctx, "peer.example.net", link_only_scope(), None, vec![]).await;
    let (status, body) = get_json(&svc, &foreign).await;
    assert_eq!(
        status,
        StatusCode::MISDIRECTED_REQUEST,
        "a peer never serves content"
    );
    assert_eq!(
        body["home_server"],
        Value::String("peer.example.net".into())
    );
    assert!(
        body.get("assets").is_none(),
        "the pointer carries no content"
    );

    // A share hosted here serves normally.
    let (_id2, local) = publish(&ctx, SELF_SERVER, link_only_scope(), None, vec![]).await;
    let (lstatus, lbody) = get_json(&svc, &local).await;
    assert_eq!(lstatus, StatusCode::OK);
    assert_eq!(lbody["home_server"], Value::String(SELF_SERVER.into()));
}

// ─────────────────── Blob serving ────────────────────────────────────────────────

/// A ciphertext blob the share covers is served verbatim; a hash the share does not cover is an
/// indistinguishable `404` (no arbitrary blob oracle).
#[tokio::test]
async fn blob_serves_only_covered_hashes() {
    let ctx = setup().await;
    let svc = share_service(&ctx);
    let ciphertext = b"opaque-ciphertext-bytes";
    let hash = TestCtx::address(ciphertext);
    ctx.write_blob_bytes(&hash, ciphertext);
    let (_id, opaque) = publish(
        &ctx,
        SELF_SERVER,
        link_only_scope(),
        None,
        vec![asset(&hash, fingerprinted_sidecar())],
    )
    .await;

    // The covered hash is served verbatim as opaque octets.
    let (status, _, body) = get_raw(&svc, &format!("{opaque}/blob/{hash}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, ciphertext);

    // A hash the share does not cover is a 404 (no blob oracle).
    let (miss, _, _) = get_raw(&svc, &format!("{opaque}/blob/{}", "b".repeat(64))).await;
    assert_eq!(miss, StatusCode::NOT_FOUND);
}

// ─────────────────── Mock resolver (injected fail-closed / rate seams) ────────────

/// A controllable authoritative resolver: it returns a canned resolution, or `Err` once flipped
/// to "unreachable" — so the fail-closed posture is provable without a torn-down database.
struct MockResolver {
    resolution: std::sync::Mutex<ShareResolution>,
    unreachable: AtomicBool,
}

impl MockResolver {
    fn live(opaque: &str) -> Self {
        Self {
            resolution: std::sync::Mutex::new(ShareResolution::Serve(serve_record(opaque))),
            unreachable: AtomicBool::new(false),
        }
    }

    fn set_unreachable(&self, v: bool) {
        self.unreachable.store(v, Ordering::SeqCst);
    }

    fn set_resolution(&self, r: ShareResolution) {
        *self.resolution.lock().unwrap() = r;
    }
}

#[async_trait]
impl LinkResolver for MockResolver {
    async fn resolve(
        &self,
        _opaque_id: &str,
        _now: Timestamp,
    ) -> Result<ShareResolution, ResolveError> {
        if self.unreachable.load(Ordering::SeqCst) {
            return Err(ResolveError);
        }
        Ok(self.resolution.lock().unwrap().clone())
    }
}

/// A minimal live servable record for the mock resolver.
fn serve_record(opaque: &str) -> ServeRecord {
    ServeRecord {
        opaque_id: opaque.to_string(),
        scope_kind: ShareScopeKind::Album,
        scope_id: "album-x".to_string(),
        home_server: SELF_SERVER.to_string(),
        wrapped_scope_b64: "d3JhcHBlZA==".to_string(),
        passphrase_protected: false,
        expires_at: None,
        assets: Vec::new(),
    }
}
