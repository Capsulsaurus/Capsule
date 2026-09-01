//! Testcontainer-backed federation smokes + E2E case 4 (slice `S-E2`).
//!
//! These are the smoke/E2E halves of the federation doc's Validation bullets — the ones that must
//! run against a real Postgres feed:
//! - `federation_rate_budget_and_circuit_breaker_smoke` — bullets 4 & 5: exhaust a peer's
//!   events/hour budget against a real feed pull (assert `429`, resume after the window); trip the
//!   circuit breaker with malformed pulls and assert further pulls short-circuit until the back-off.
//! - `e2e_case_4_peer_a_pulls_from_peer_b` — **E2E case 4** + bullet 7 (cross-peer consistency):
//!   two service instances over two Postgres testcontainers; peer B (home) issues a capability for
//!   peer A, serves its sync feed under invariant-19/21 gates; peer A verifies, re-applies the full
//!   invariant battery (1–18 + 25) to every pulled manifest, and persists byte-identically. A
//!   tampered manifest is soft-failed (remembered in the rejected-hash table).
//!
//! Shape built: the two servers run **in one test process against two separate Postgres
//! containers** — the closest testcontainer-realizable form of "peer A pulls from peer B". B's serve
//! half is exercised through the real authorization gates (`authorize_pull`, `authorize_blob_fetch`)
//! and the real feed reader (`service::sync::Query::feed_page`); A's ingest half through
//! `federation::revalidate_pulled` writing into A's own feed store.

use base64::Engine as _;
use jiff::{SignedDuration, Timestamp};
use jsonwebtoken::{DecodingKey, EncodingKey};
use ring::signature::{Ed25519KeyPair, KeyPair};
use service::sync::{ChangeKind, FeedBlobManifest, FeedBlobRef, FeedEntryInput, Mutation, Query};

use crate::federation::FederationReject;
use crate::federation::capability::{
    CapabilityIssuer, FederationScope, IssueParams, VerifyContext,
};
use crate::federation::compartment::{PeerLimits, PeerRegistry, PullCost};
use crate::federation::pull::{
    PullValidationContext, PulledEnvelope, authorize_blob_fetch, authorize_pull, revalidate_pulled,
};
use crate::federation::rejected::RejectedHashTable;
use crate::federation::revocation::RevocationList;

const B_ID: &str = "home.b.tld";
const A_ID: &str = "peer.a.tld";
const PROTOCOL: &str = "2026-05-31";
const ALLOWED_TYPES: &[&str] = &["image/jpeg", "image/heic"];
const B_PKCS8: &str = "MC4CAQAwBQYDK2VwBCIEIN6eTvXEL7xMZWHY8rTk7VbQSGSuRkle5MVfiiYUStLF";

fn b_keypair() -> (EncodingKey, DecodingKey) {
    let doc = base64::engine::general_purpose::STANDARD
        .decode(B_PKCS8)
        .unwrap();
    let pair = Ed25519KeyPair::from_pkcs8_maybe_unchecked(&doc).unwrap();
    (
        EncodingKey::from_ed_der(&doc),
        DecodingKey::from_ed_der(pair.public_key().as_ref()),
    )
}

/// A well-formed envelope pinned to `PROTOCOL`, scoped to `album_id`.
fn envelope(album_id: &str, asset_id: &str) -> PulledEnvelope {
    PulledEnvelope {
        crypto_suite_id: capsule_core::crypto::CRYPTO_SUITE_ID,
        protocol_version: PROTOCOL.to_string(),
        album_id: Some(album_id.to_string()),
        file_id: asset_id.to_string(),
        amk_version: 1,
        ciphertext_hash: format!("{:064x}", 0xabc),
        plaintext_size: 4096,
        chunk_size: 65_520,
        key_mode: "derived".to_string(),
        metadata_blob_hash: None,
        created_by_user: "0192f000-0000-7000-8000-0000000000bb".to_string(),
        created_by_device: "0192f000-0000-7000-8000-0000000000cc".to_string(),
        client_version: "test/1".to_string(),
        timestamp: "2026-05-31T00:00:00Z".to_string(),
        action: "create".to_string(),
        prior_provenance_hash: None,
        retention_until: None,
    }
}

fn original_blobs() -> FeedBlobManifest {
    FeedBlobManifest {
        original: Some(FeedBlobRef {
            ciphertext_hash: format!("{:064x}", 0xabc),
            role: "original".to_string(),
            format: "image/jpeg".to_string(),
            size: 4096,
        }),
        derivatives: Vec::new(),
    }
}

/// Seed one finalized feed entry carrying a real (revalidatable) manifest envelope; returns the
/// stored `manifest_cbor` for byte-identity assertions.
async fn seed_manifest(
    ctx: &super::TestCtx,
    env: &PulledEnvelope,
    blobs: &FeedBlobManifest,
    asset_id: &str,
) -> Vec<u8> {
    let manifest_cbor = env.to_canonical_cbor().unwrap();
    Mutation::record_finalization(
        &ctx.db,
        FeedEntryInput {
            album_id: ctx.album_id.clone(),
            protocol_version: PROTOCOL.to_string(),
            kind: ChangeKind::Created,
            asset_id: asset_id.to_string(),
            manifest_cbor: manifest_cbor.clone(),
            metadata_blob: None,
            blobs: blobs.clone(),
            original_held: true,
        },
    )
    .await
    .unwrap();
    manifest_cbor
}

fn ingest_ctx(album_pin: &str) -> PullValidationContext<'_> {
    PullValidationContext {
        protocol_min: "2026-01-01",
        protocol_max: "2026-12-31",
        album_pin,
        device_added_at: "2026-05-30T00:00:00Z",
        server_clock: "2026-05-31T00:00:00Z",
        drift_days: 30,
        stored_chain_head: None,
        stored_amk_version: None,
        allowed_content_types: ALLOWED_TYPES,
        max_blob_size: 100_000_000,
    }
}

/// **E2E case 4 + cross-peer consistency (bullet 7).** Peer A pulls from peer B: B issues a
/// capability, serves its feed under the invariant-19/21 gates; A verifies, re-validates every
/// pulled manifest against invariants 1–18 + 25, persists byte-identically; a tampered manifest is
/// soft-failed.
#[tokio::test]
async fn e2e_case_4_peer_a_pulls_from_peer_b() {
    let server_b = super::setup().await; // home server: owns the album, serves the feed
    let server_a = super::setup().await; // pulling server: verifies + ingests

    // B produces a write in its album.
    let asset_id = "asset-e2e-1";
    let env = envelope(&server_b.album_id, asset_id);
    let b_bytes = seed_manifest(&server_b, &env, &original_blobs(), asset_id).await;

    // B mints a capability for A, and registers A's identity (so A is not a first-contact stranger).
    let (b_enc, b_dec) = b_keypair();
    service::federation::Peers::register(&server_b.db, A_ID, &[7u8; 32])
        .await
        .unwrap();
    let issuer = CapabilityIssuer::new(B_ID, b_enc);
    let now = Timestamp::now();
    let minted = issuer
        .issue(
            &IssueParams {
                peer: A_ID,
                album_id: &server_b.album_id,
                scope: FederationScope::Read,
                min_protocol_version: PROTOCOL,
                ttl: SignedDuration::from_hours(1),
            },
            now,
        )
        .unwrap();

    // ── Serve side (B): blocklist → capability verify (inv 19) → per-peer budget (inv 21). ──
    service::moderation::Blocklist::ensure_server_allowed(&server_b.db, A_ID)
        .await
        .unwrap();
    let active = service::federation::Revocations::active_jtis(&server_b.db, now)
        .await
        .unwrap();
    let revocation = RevocationList::owned(active);
    let registry = PeerRegistry::new(PeerLimits::default());
    let registered = service::federation::Peers::is_registered(&server_b.db, A_ID)
        .await
        .unwrap();
    let ctx = VerifyContext {
        expected_issuer: B_ID,
        album_id: &server_b.album_id,
        now,
    };
    let claims = authorize_pull(
        &minted.token,
        &b_dec,
        &ctx,
        &revocation,
        &registry,
        registered,
        PullCost {
            events: 1,
            bytes: 4096,
            cpu_ms: 1,
        },
    )
    .expect("B authorizes A's pull");
    assert_eq!(claims.sub, A_ID);

    // B reads the requested album's feed page (the same reader clients use).
    let rows = Query::feed_page(&server_b.db, &[server_b.album_id.clone()], 0, 100)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "B serves the one seeded entry");
    let row = &rows[0];
    assert_eq!(row.manifest_cbor, b_bytes);

    // ── Ingest side (A): scope-by-role → re-validate invariants 1–18 + 25 → persist. ──
    let blobs: FeedBlobManifest = serde_json::from_value(row.blobs.clone()).unwrap();
    if let Some(original) = &blobs.original {
        authorize_blob_fetch(claims.scope, &original.role).expect("read scope covers original");
    }
    let validated = revalidate_pulled(&row.manifest_cbor, &blobs, None, &ingest_ctx(PROTOCOL))
        .expect("A re-applies invariants 1-18 + 25 and accepts");
    assert_eq!(validated.file_id, asset_id);

    // A persists the pulled entry into its own feed store, byte-identically.
    Mutation::record_finalization(
        &server_a.db,
        FeedEntryInput {
            album_id: server_a.album_id.clone(),
            protocol_version: row.protocol_version.clone(),
            kind: ChangeKind::Created,
            asset_id: asset_id.to_string(),
            manifest_cbor: row.manifest_cbor.clone(),
            metadata_blob: row.metadata_blob.clone(),
            blobs: blobs.clone(),
            original_held: row.original_held,
        },
    )
    .await
    .unwrap();

    // Cross-peer consistency: A's stored manifest is byte-identical to B's.
    let a_rows = Query::feed_page(&server_a.db, &[server_a.album_id.clone()], 0, 100)
        .await
        .unwrap();
    assert_eq!(a_rows.len(), 1);
    assert_eq!(
        a_rows[0].manifest_cbor, b_bytes,
        "cross-peer state is byte-identical"
    );

    // ── Soft-fail: a tampered manifest is rejected locally and its hash remembered. ──
    let mut tampered = row.manifest_cbor.clone();
    tampered[0] ^= 0x01;
    let reject = revalidate_pulled(&tampered, &blobs, None, &ingest_ctx(PROTOCOL))
        .expect_err("a tampered manifest fails re-validation");
    let mut rejected_table = RejectedHashTable::with_defaults();
    let tampered_hash = capsule_core::crypto::hash::hash_bytes(&tampered);
    let hex: String = tampered_hash.0.iter().map(|b| format!("{b:02x}")).collect();
    rejected_table.remember(&hex, Timestamp::now());
    assert!(
        rejected_table.contains(&hex, Timestamp::now()),
        "soft-fail remembers the rejected hash (reject at invariant {})",
        reject.invariant
    );
}

/// **Durable revocation lifecycle**, tied to capability verification: revoke a `jti` in the
/// durable store, confirm the owned list reflects it (verify rejects with `403` revoked), then
/// prune once its `exp` has passed and confirm the list is bounded again.
#[tokio::test]
async fn federation_revocation_lifecycle_durable() {
    let server_b = super::setup().await;
    let (b_enc, b_dec) = b_keypair();
    let issuer = CapabilityIssuer::new(B_ID, b_enc);
    let now = Timestamp::now();
    let minted = issuer
        .issue(
            &IssueParams {
                peer: A_ID,
                album_id: &server_b.album_id,
                scope: FederationScope::Read,
                min_protocol_version: PROTOCOL,
                ttl: SignedDuration::from_hours(1),
            },
            now,
        )
        .unwrap();
    let exp: Timestamp = minted.claims.exp.parse().unwrap();
    let ctx = VerifyContext {
        expected_issuer: B_ID,
        album_id: &server_b.album_id,
        now,
    };

    // Before revocation the token verifies.
    let empty = RevocationList::owned(
        service::federation::Revocations::active_jtis(&server_b.db, now)
            .await
            .unwrap(),
    );
    crate::federation::verify_capability(&minted.token, &b_dec, &ctx, &empty).unwrap();

    // Revoke it durably; the owned list built from the store now rejects it (403 revoked).
    service::federation::Revocations::revoke(&server_b.db, &minted.claims.jti, exp)
        .await
        .unwrap();
    assert!(
        service::federation::Revocations::is_revoked(&server_b.db, &minted.claims.jti)
            .await
            .unwrap()
    );
    let revoked_list = RevocationList::owned(
        service::federation::Revocations::active_jtis(&server_b.db, now)
            .await
            .unwrap(),
    );
    let reject = crate::federation::verify_capability(&minted.token, &b_dec, &ctx, &revoked_list)
        .expect_err("a revoked token is refused");
    let fr = FederationReject::from(reject);
    assert_eq!(fr.http_status(), 403);
    assert_eq!(
        fr.code(),
        capsule_i18n::error_codes::FEDERATION_CAPABILITY_REVOKED
    );

    // After the token's exp passes, pruning drops the row — the published list stays bounded.
    let after_exp = exp.checked_add(SignedDuration::from_secs(1)).unwrap();
    let pruned = service::federation::Revocations::prune(&server_b.db, after_exp)
        .await
        .unwrap();
    assert_eq!(pruned, 1);
    assert!(
        !service::federation::Revocations::is_revoked(&server_b.db, &minted.claims.jti)
            .await
            .unwrap()
    );
    assert!(
        service::federation::Revocations::active_jtis(&server_b.db, after_exp)
            .await
            .unwrap()
            .is_empty()
    );
}

/// **Rate-budget (bullet 4) + circuit breaker (bullet 5) smoke, against a real Postgres feed.**
#[tokio::test]
async fn federation_rate_budget_and_circuit_breaker_smoke() {
    let server_b = super::setup().await;
    let asset_id = "asset-rate-1";
    let env = envelope(&server_b.album_id, asset_id);
    seed_manifest(&server_b, &env, &original_blobs(), asset_id).await;

    let (b_enc, b_dec) = b_keypair();
    let issuer = CapabilityIssuer::new(B_ID, b_enc);
    let now = Timestamp::now();
    let minted = issuer
        .issue(
            &IssueParams {
                peer: A_ID,
                album_id: &server_b.album_id,
                scope: FederationScope::Read,
                min_protocol_version: PROTOCOL,
                ttl: SignedDuration::from_hours(1),
            },
            now,
        )
        .unwrap();

    let revocation = RevocationList::owned(Vec::<String>::new());
    let ctx = VerifyContext {
        expected_issuer: B_ID,
        album_id: &server_b.album_id,
        now,
    };

    // ── Rate budget: a tiny events/hour budget. The pull that breaks it is 429. ──
    let limits = PeerLimits {
        events_per_hour: 3,
        ..PeerLimits::default()
    };
    let registry = PeerRegistry::new(limits);
    for _ in 0..3 {
        // The feed read backs each pull against the real Postgres.
        let rows = Query::feed_page(&server_b.db, &[server_b.album_id.clone()], 0, 100)
            .await
            .unwrap();
        authorize_pull(
            &minted.token,
            &b_dec,
            &ctx,
            &revocation,
            &registry,
            true,
            PullCost {
                events: rows.len() as u64,
                ..Default::default()
            },
        )
        .expect("within budget");
    }
    let over = authorize_pull(
        &minted.token,
        &b_dec,
        &ctx,
        &revocation,
        &registry,
        true,
        PullCost {
            events: 1,
            ..Default::default()
        },
    )
    .expect_err("the budget-breaking pull is refused");
    assert_eq!(over.http_status(), 429);
    assert_eq!(
        over.code(),
        capsule_i18n::error_codes::FEDERATION_RATE_BUDGET_EXCEEDED
    );

    // Pulls resume after the 1-hour window rolls.
    let next_window = now.checked_add(SignedDuration::from_hours(1)).unwrap();
    let ctx_next = VerifyContext {
        expected_issuer: B_ID,
        album_id: &server_b.album_id,
        now: next_window,
    };
    // The capability is still valid at next_window (24h ceiling), so re-mint one anchored there to
    // keep the temporal check independent of the budget assertion.
    let minted2 = issuer
        .issue(
            &IssueParams {
                peer: A_ID,
                album_id: &server_b.album_id,
                scope: FederationScope::Read,
                min_protocol_version: PROTOCOL,
                ttl: SignedDuration::from_hours(1),
            },
            next_window,
        )
        .unwrap();
    authorize_pull(
        &minted2.token,
        &b_dec,
        &ctx_next,
        &revocation,
        &registry,
        true,
        PullCost {
            events: 1,
            ..Default::default()
        },
    )
    .expect("pulls resume after the window rolls");

    // ── Circuit breaker: malformed pulls spend the error budget, then the breaker opens. ──
    let breaker_registry = PeerRegistry::new(PeerLimits {
        error_budget: 2,
        ..PeerLimits::default()
    });
    // Each malformed manifest fails re-validation; the server records the error against the peer.
    let malformed = b"\xff\xff not a manifest";
    let blobs = original_blobs();
    for _ in 0..3 {
        let err = revalidate_pulled(malformed, &blobs, None, &ingest_ctx(PROTOCOL));
        assert!(err.is_err(), "malformed manifest is rejected");
        breaker_registry.record_error(A_ID, now);
    }
    assert!(breaker_registry.is_circuit_open(A_ID, now));

    // While the breaker is open, an otherwise-valid pull is short-circuited with 429.
    let short = authorize_pull(
        &minted.token,
        &b_dec,
        &ctx,
        &revocation,
        &breaker_registry,
        true,
        PullCost {
            events: 1,
            ..Default::default()
        },
    )
    .expect_err("breaker short-circuits the pull");
    assert_eq!(short.http_status(), 429);
    assert_eq!(
        short.code(),
        capsule_i18n::error_codes::FEDERATION_CIRCUIT_OPEN
    );
    assert!(matches!(short, FederationReject::Compartment(_)));
}
