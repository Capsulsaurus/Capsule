//! Quota-service tests (slice `S-C6`) — one per Validation bullet of the Quota design doc.
//!
//! HTTP-surface tests drive the real upload router (testcontainer Postgres + Valkey);
//! accounting/ledger tests exercise `service::quota` directly against the same testcontainer
//! database (there is no HTTP metadata-growth or federated-cache surface yet — those are later
//! slices — so their enforcement is tested at the service boundary that owns it).

use capsule_i18n::error_codes;
use jiff::{SignedDuration, Timestamp};
use salvo::Service;
use salvo::http::StatusCode;
use salvo::test::{ResponseExt, TestClient};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, Set};
use serde_json::Value;
use service::quota::{
    self, BlobKind, ChargeOutcome, DEFAULT_PER_PEER_BUDGET_RATIO, QuotaLimits, QuotaState,
    ReleaseOutcome, WriteClass,
};

use super::{PROTOCOL, TestCtx, error_code, setup, valid_create_body};

/// A finite `QuotaLimits` for the direct-service tests.
fn limits(soft: u64, hard: u64) -> QuotaLimits {
    QuotaLimits {
        soft_limit: soft,
        hard_limit: hard,
        grace_window: SignedDuration::from_hours(24 * 14),
        per_peer_budget_ratio: DEFAULT_PER_PEER_BUDGET_RATIO,
    }
}

async fn post_create(ctx: &TestCtx, svc: &Service, body: &Value) -> (StatusCode, Value) {
    let mut res = TestClient::post("http://localhost/upload")
        .add_header("Authorization", format!("Bearer {}", ctx.token()), true)
        .add_header("X-Capsule-Protocol", PROTOCOL, true)
        .json(body)
        .send(svc)
        .await;
    let status = res.status_code.unwrap_or(StatusCode::OK);
    let json = res.take_json::<Value>().await.unwrap_or(Value::Null);
    (status, json)
}

/// **Hard-limit enforcement.** A session creation that would cross the hard limit is rejected
/// with `error.quota.exceeded`, and no pending asset row is written.
#[tokio::test]
async fn hard_limit_enforcement_rejects_and_writes_no_row() {
    let mut ctx = setup().await;
    ctx.set_quota_limits(800, 1000);
    let svc = ctx.service();

    // Existing usage: an 800-byte finalized asset.
    ctx.seed_asset(&ctx.user_id, &"a".repeat(64), 800, true, Timestamp::now())
        .await;

    // Declaring 500 more would reach 1300 > 1000 → refused.
    let over_hash = "b".repeat(64);
    let (status, body) = post_create(
        &ctx,
        &svc,
        &valid_create_body(&ctx.album_id, &over_hash, 500),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "over-quota create must be 403"
    );
    assert_eq!(error_code(&body), Some(error_codes::QUOTA_EXCEEDED));

    // No pending row for the rejected blob (the check runs before any insert).
    let rows = entity::asset::Entity::find()
        .filter(entity::asset::Column::FileHash.eq(&over_hash))
        .count(&ctx.db)
        .await
        .expect("count");
    assert_eq!(
        rows, 0,
        "a rejected over-quota session must write no pending row"
    );

    // A within-limit session (800 + 100 = 900 ≤ 1000) still succeeds — the gate is not over-eager.
    let ok_hash = "c".repeat(64);
    let (status, _) =
        post_create(&ctx, &svc, &valid_create_body(&ctx.album_id, &ok_hash, 100)).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "within-limit create must succeed"
    );
}

/// **Dedup attribution.** Two users hold the same content; only the first uploader's quota is
/// debited (global content-addressed dedup attribution).
#[tokio::test]
async fn dedup_attribution_charges_first_uploader_only() {
    let ctx = setup().await;
    let user2 = ctx.seed_user().await;

    let hash = "d".repeat(64);
    let t0 = Timestamp::now() - SignedDuration::from_hours(2);
    let t1 = Timestamp::now();
    // U1 uploads first, U2 (re)uploads the same content later.
    ctx.seed_asset(&ctx.user_id, &hash, 1000, true, t0).await;
    ctx.seed_asset(&user2, &hash, 1000, true, t1).await;

    let used_u1 = quota::Query::used(&ctx.db, &ctx.user_id)
        .await
        .expect("used u1");
    let used_u2 = quota::Query::used(&ctx.db, &user2).await.expect("used u2");
    assert_eq!(used_u1, 1000, "the first uploader is charged");
    assert_eq!(used_u2, 0, "the second uploader (merge) is not charged");
}

/// **Trash-retention accounting.** A soft-deleted asset still counts at full size until it is
/// hard-purged; the hard-purge releases the bytes.
#[tokio::test]
async fn trash_retained_counts_until_hard_purge() {
    let ctx = setup().await;
    let hash = "e".repeat(64);
    let asset_id = ctx
        .seed_asset(&ctx.user_id, &hash, 1000, true, Timestamp::now())
        .await;

    assert_eq!(
        quota::Query::used(&ctx.db, &ctx.user_id)
            .await
            .expect("used"),
        1000
    );

    // Soft-delete (trash): the row remains, so it still counts at full size.
    let mut am: entity::asset::ActiveModel = entity::asset::Entity::find_by_id(&asset_id)
        .one(&ctx.db)
        .await
        .expect("find")
        .expect("row")
        .into();
    am.deleted_at = Set(Some(entity::time::now_entity()));
    am.update(&ctx.db).await.expect("soft delete");
    assert_eq!(
        quota::Query::used(&ctx.db, &ctx.user_id)
            .await
            .expect("used"),
        1000,
        "a trash-retained asset still counts at full size"
    );

    // Hard-purge: the row is gone, so the bytes are released.
    entity::asset::Entity::delete_by_id(&asset_id)
        .exec(&ctx.db)
        .await
        .expect("hard purge");
    assert_eq!(
        quota::Query::used(&ctx.db, &ctx.user_id)
            .await
            .expect("used"),
        0,
        "hard-purge releases the bytes"
    );
}

/// **Federated-receive accounting.** A federated cache debits the receiver, is deduped by
/// content hash, and is bounded by the per-`(receiver, source_peer)` caching budget.
#[tokio::test]
async fn federated_receive_debits_receiver_deduped_and_budget_bounded() {
    let ctx = setup().await;
    let l = limits(u64::MAX, 1000); // budget = 25% of 1000 = 250
    let peer = "peer-alpha";
    let hash_a = "1".repeat(64);

    // A fresh federated cache debits the receiver.
    let out = quota::Mutation::charge_federated(
        &ctx.db,
        &ctx.user_id,
        &hash_a,
        100,
        BlobKind::Original,
        peer,
        &l,
    )
    .await
    .expect("charge a");
    assert_eq!(out, ChargeOutcome::Charged { byte_size: 100 });
    assert_eq!(
        quota::Query::used(&ctx.db, &ctx.user_id)
            .await
            .expect("used"),
        100
    );
    assert_eq!(
        quota::Query::used_from_peer(&ctx.db, &ctx.user_id, peer)
            .await
            .expect("peer"),
        100
    );

    // The same content again is a dedup merge — not double-counted, no budget consumed.
    let out = quota::Mutation::charge_federated(
        &ctx.db,
        &ctx.user_id,
        &hash_a,
        100,
        BlobKind::Original,
        peer,
        &l,
    )
    .await
    .expect("charge a dup");
    assert_eq!(out, ChargeOutcome::Merged { refcount: 2 });
    assert_eq!(
        quota::Query::used(&ctx.db, &ctx.user_id)
            .await
            .expect("used"),
        100,
        "a blob the server already holds is not double-counted"
    );

    // A new blob that would cross the per-peer budget (100 + 200 > 250) is refused.
    let hash_b = "2".repeat(64);
    let err = quota::Mutation::charge_federated(
        &ctx.db,
        &ctx.user_id,
        &hash_b,
        200,
        BlobKind::Original,
        peer,
        &l,
    )
    .await
    .expect_err("budget exceeded");
    match err {
        quota::QuotaError::PeerBudgetExceeded {
            budget,
            used,
            additional,
            ..
        } => {
            assert_eq!((budget, used, additional), (250, 100, 200));
        }
        other => panic!("expected PeerBudgetExceeded, got {other:?}"),
    }
    assert_eq!(err.code(), Some(error_codes::QUOTA_PEER_BUDGET_EXCEEDED));
}

/// **Derivative reclaim on purge.** A hard-purge drops the derivative + metadata references;
/// a zero-reference blob is GC'd and its bytes credited back — no orphan left counting.
#[tokio::test]
async fn derivative_and_metadata_reclaimed_on_purge() {
    let ctx = setup().await;
    let meta = "3".repeat(64);
    let deriv = "4".repeat(64);

    quota::Mutation::charge_aux(&ctx.db, &ctx.user_id, &meta, 50, BlobKind::Metadata)
        .await
        .expect("charge meta");
    quota::Mutation::charge_aux(&ctx.db, &ctx.user_id, &deriv, 150, BlobKind::Derivative)
        .await
        .expect("charge deriv");
    assert_eq!(
        quota::Query::used(&ctx.db, &ctx.user_id)
            .await
            .expect("used"),
        200
    );

    // A second reference to the derivative (e.g. shared across assets) merges the refcount.
    let out = quota::Mutation::charge_aux(&ctx.db, &ctx.user_id, &deriv, 150, BlobKind::Derivative)
        .await
        .expect("charge deriv again");
    assert_eq!(out, ChargeOutcome::Merged { refcount: 2 });

    // Releasing one reference retains the blob (refcount still > 0) — bytes still counted.
    assert_eq!(
        quota::Mutation::release_hash(&ctx.db, &deriv)
            .await
            .expect("release deriv 1"),
        ReleaseOutcome::Retained { refcount: 1 }
    );
    assert_eq!(
        quota::Query::used(&ctx.db, &ctx.user_id)
            .await
            .expect("used"),
        200
    );

    // The last reference drops → GC + credit.
    match quota::Mutation::release_hash(&ctx.db, &deriv)
        .await
        .expect("release deriv 2")
    {
        ReleaseOutcome::GarbageCollected {
            freed_bytes,
            attributed_user_id,
        } => {
            assert_eq!(freed_bytes, 150);
            assert_eq!(attributed_user_id, ctx.user_id);
        }
        other => panic!("expected GarbageCollected, got {other:?}"),
    }
    quota::Mutation::release_hash(&ctx.db, &meta)
        .await
        .expect("release meta");

    assert_eq!(
        quota::Query::used(&ctx.db, &ctx.user_id)
            .await
            .expect("used"),
        0,
        "no orphaned derivative/metadata left counting after purge"
    );
    // Releasing an absent hash is a no-op.
    assert_eq!(
        quota::Mutation::release_hash(&ctx.db, &meta)
            .await
            .expect("release absent"),
        ReleaseOutcome::Absent
    );
}

/// **Grace expiry (smoke).** With the grace window mocked into the past, the account is
/// Grace-expired: metadata-growth writes are refused while lifecycle (delete) writes and
/// reads are still admitted.
#[tokio::test]
async fn grace_expired_enters_read_only_mode() {
    let ctx = setup().await;
    let l = limits(800, 1000);

    // Usage over the hard limit.
    ctx.seed_asset(&ctx.user_id, &"5".repeat(64), 1200, true, Timestamp::now())
        .await;

    // Mock the grace window past: hard-exceeded since 15 days ago (> 14-day window).
    let since = Timestamp::now() - SignedDuration::from_hours(24 * 15);
    entity::user_quota::ActiveModel {
        user_id: Set(ctx.user_id.clone()),
        hard_exceeded_since: Set(Some(entity::time::ts_to_entity(since))),
        suspended: Set(false),
        updated_at: Set(entity::time::now_entity()),
    }
    .insert(&ctx.db)
    .await
    .expect("seed grace marker");

    // Reported state is Grace-expired.
    let status = quota::Query::current_status(&ctx.db, &ctx.user_id, &l)
        .await
        .expect("status");
    assert_eq!(status.state, QuotaState::GraceExpired);

    // Metadata-growth writes are refused (read-only) …
    let err = quota::Mutation::check(&ctx.db, &ctx.user_id, 10, WriteClass::MetadataGrowth, &l)
        .await
        .expect_err("grace-locked");
    assert!(matches!(err, quota::QuotaError::GraceLocked { .. }));
    assert_eq!(err.code(), Some(error_codes::QUOTA_GRACE_LOCKED));

    // … but lifecycle (delete-your-way-out) writes are always admitted …
    quota::Mutation::check(&ctx.db, &ctx.user_id, 0, WriteClass::Lifecycle, &l)
        .await
        .expect("lifecycle admitted in grace-expired");

    // … and new upload sessions remain refused (still over the hard limit).
    let err = quota::Mutation::check(&ctx.db, &ctx.user_id, 10, WriteClass::UploadSession, &l)
        .await
        .expect_err("still over hard");
    assert!(matches!(err, quota::QuotaError::Exceeded { .. }));
}

/// **Quota status reporting.** `GET /quota` returns accurate `used` + `state` for a fixture
/// user.
#[tokio::test]
async fn quota_status_reporting_over_http() {
    let mut ctx = setup().await;
    ctx.set_quota_limits(800, 1000);
    let svc = ctx.service();

    // 850 bytes used → SoftWarning (soft 800 ≤ used < hard 1000).
    ctx.seed_asset(&ctx.user_id, &"6".repeat(64), 850, true, Timestamp::now())
        .await;

    let mut res = TestClient::get("http://localhost/upload/quota")
        .add_header("Authorization", format!("Bearer {}", ctx.token()), true)
        .send(&svc)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));
    let body = res.take_json::<Value>().await.expect("quota body");
    assert_eq!(body["used"].as_u64(), Some(850));
    assert_eq!(body["soft_limit"].as_u64(), Some(800));
    assert_eq!(body["hard_limit"].as_u64(), Some(1000));
    assert_eq!(body["state"].as_str(), Some("soft_warning"));
}

/// `GET /quota` requires authentication.
#[tokio::test]
async fn quota_status_requires_auth() {
    let ctx = setup().await;
    let svc = ctx.service();
    let res = TestClient::get("http://localhost/upload/quota")
        .send(&svc)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
}
