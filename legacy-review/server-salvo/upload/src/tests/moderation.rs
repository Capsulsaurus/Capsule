//! Moderation suspension enforcement (slice `S-C8`) against the real upload server.
//!
//! Covers the Moderation design doc's **Suspension enforcement** Validation bullet: a
//! suspended account's upload-session creation is rejected with the right structural code
//! (`error.moderation.account_suspended`, 403), distinct from quota and permission rejections.
//! The other five bullets are exercised at the service + serving layers in the media crate's
//! `moderation` tests.

use capsule_i18n::error_codes;
use salvo::Service;
use salvo::http::StatusCode;
use salvo::test::{ResponseExt, TestClient};
use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};
use serde_json::Value;
use service::moderation::Suspension;

use super::{PROTOCOL, TestCtx, error_code, setup, valid_create_body};

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

/// **Suspension enforcement.** A suspended account's upload-session creation is refused with
/// `403` + `error.moderation.account_suspended`, and no pending asset row is written. Lifting
/// the suspension restores the ability to upload.
#[tokio::test]
async fn suspended_account_upload_session_is_refused() {
    let ctx = setup().await;
    let svc = ctx.service();
    let hash = "c".repeat(64);

    // Baseline: an un-suspended account creates a session (201).
    let (status, _) = post_create(&ctx, &svc, &valid_create_body(&ctx.album_id, &hash, 64)).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "an un-suspended account must be able to create a session"
    );

    // Suspend the account (moderation action; also appends the audit-log record).
    Suspension::suspend(&ctx.db, &ctx.user_id, Some("policy violation"))
        .await
        .expect("suspend");

    // A fresh blob under the suspended account is refused with the structural code.
    let over_hash = "d".repeat(64);
    let (status, body) = post_create(
        &ctx,
        &svc,
        &valid_create_body(&ctx.album_id, &over_hash, 64),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a suspended account's create must be 403"
    );
    assert_eq!(
        error_code(&body),
        Some(error_codes::MODERATION_ACCOUNT_SUSPENDED),
        "the rejection must carry the moderation suspension code, not a quota/permission code"
    );

    // The refused create wrote no pending row (the gate runs before any write).
    let rows = entity::asset::Entity::find()
        .filter(entity::asset::Column::FileHash.eq(&over_hash))
        .count(&ctx.db)
        .await
        .expect("count");
    assert_eq!(
        rows, 0,
        "a refused suspended create must write no pending row"
    );

    // Lifting the suspension restores uploads.
    Suspension::unsuspend(&ctx.db, &ctx.user_id, None)
        .await
        .expect("unsuspend");
    let (status, _) = post_create(
        &ctx,
        &svc,
        &valid_create_body(&ctx.album_id, &over_hash, 64),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "lifting the suspension must restore session creation"
    );
}
