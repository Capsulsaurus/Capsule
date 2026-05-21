use crate::common::{build_service, setup};
use salvo::http::StatusCode;
use salvo::test::TestClient;

// ── helpers ────────────────────────────────────────────────────────────────

async fn register(service: &salvo::Service, email: &str, username: &str) {
    TestClient::post("http://localhost/register")
        .json(&serde_json::json!({
            "username": username,
            "name": "Lockout Test User",
            "email": email,
            "password": "password123"
        }))
        .send(service)
        .await;
}

/// Accumulate `n` failed login attempts directly in the DB, bypassing the
/// API rate limiter so the lockout logic can be tested in isolation.
async fn fail_login_n_times(ctx: &crate::common::TestContext, user_id: &str, n: u32) {
    for _ in 0..n {
        service::user::Mutation::track_login_failure(&ctx.db, user_id)
            .await
            .expect("track_login_failure failed");
    }
}

// ── tests ──────────────────────────────────────────────────────────────────

/// Exactly 10 failed attempts recorded in the DB triggers the lockout on
/// the next login attempt (wrong password → 423, not 401).
#[tokio::test]
async fn account_locked_after_ten_failed_attempts() {
    let ctx = setup().await;
    let service = build_service(&ctx);

    register(&service, "lockout@example.com", "lockoutuser").await;

    let user = service::user::Query::find_user_by_email(&ctx.db, "lockout@example.com")
        .await
        .expect("DB query failed")
        .expect("user should exist after registration");

    fail_login_n_times(&ctx, &user.id, 10).await;

    let res = TestClient::post("http://localhost/login")
        .json(&serde_json::json!({
            "email": "lockout@example.com",
            "password": "wrongpassword"
        }))
        .send(&service)
        .await;

    // 423 Locked — not 401 Unauthorized
    assert_eq!(res.status_code, Some(StatusCode::LOCKED));
}

/// After lockout the correct password is also rejected — the lockout check
/// runs before password verification.
#[tokio::test]
async fn locked_account_rejects_correct_password() {
    let ctx = setup().await;
    let service = build_service(&ctx);

    register(&service, "lockout2@example.com", "lockoutuser2").await;

    let user = service::user::Query::find_user_by_email(&ctx.db, "lockout2@example.com")
        .await
        .expect("DB query failed")
        .expect("user should exist");

    fail_login_n_times(&ctx, &user.id, 10).await;

    // "password123" is the correct password registered above.
    let res = TestClient::post("http://localhost/login")
        .json(&serde_json::json!({
            "email": "lockout2@example.com",
            "password": "password123"
        }))
        .send(&service)
        .await;

    assert_eq!(res.status_code, Some(StatusCode::LOCKED));
}

/// Fewer than 10 failed attempts must NOT trigger the lockout.
#[tokio::test]
async fn account_not_locked_below_threshold() {
    let ctx = setup().await;
    let service = build_service(&ctx);

    register(&service, "notlocked@example.com", "notlockeduser").await;

    let user = service::user::Query::find_user_by_email(&ctx.db, "notlocked@example.com")
        .await
        .expect("DB query failed")
        .expect("user should exist");

    // 9 failures — one below the threshold of 10.
    fail_login_n_times(&ctx, &user.id, 9).await;

    // Correct password should still work.
    let res = TestClient::post("http://localhost/login")
        .json(&serde_json::json!({
            "email": "notlocked@example.com",
            "password": "password123"
        }))
        .send(&service)
        .await;

    assert_eq!(res.status_code, Some(StatusCode::OK));
}

/// A successful login resets the failed-attempt counter, so previously
/// failed attempts do not persist toward a future lockout.
#[tokio::test]
async fn successful_login_resets_failure_counter() {
    let ctx = setup().await;
    let service = build_service(&ctx);

    register(&service, "resetcount@example.com", "resetcountuser").await;

    let user = service::user::Query::find_user_by_email(&ctx.db, "resetcount@example.com")
        .await
        .expect("DB query failed")
        .expect("user should exist");

    // 9 failures — one below threshold.
    fail_login_n_times(&ctx, &user.id, 9).await;

    // One successful login resets the counter.
    let res = TestClient::post("http://localhost/login")
        .json(&serde_json::json!({
            "email": "resetcount@example.com",
            "password": "password123"
        }))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));

    // The counter should now be 0.  Inject 9 more failures at the DB level;
    // the account must still accept the correct password (not locked yet).
    fail_login_n_times(&ctx, &user.id, 9).await;

    let res = TestClient::post("http://localhost/login")
        .json(&serde_json::json!({
            "email": "resetcount@example.com",
            "password": "password123"
        }))
        .send(&service)
        .await;
    assert_eq!(
        res.status_code,
        Some(StatusCode::OK),
        "9 failures after a counter reset should not lock the account"
    );
}
