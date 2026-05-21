use crate::common::{build_service, setup};
use salvo::http::StatusCode;
use salvo::http::header::RETRY_AFTER;
use salvo::test::TestClient;

// ── helpers ────────────────────────────────────────────────────────────────

async fn register(service: &salvo::Service, email: &str, username: &str) {
    TestClient::post("http://localhost/register")
        .json(&serde_json::json!({
            "username": username,
            "name": "Rate Limit Test User",
            "email": email,
            "password": "password123"
        }))
        .send(service)
        .await;
}

// ── login rate limiting (10 requests/min per IP) ───────────────────────────

/// The first 10 login requests from the same IP should not be rate-limited.
/// The 11th must return 429.
#[tokio::test]
async fn login_rate_limit_enforced() {
    let ctx = setup().await;
    let service = build_service(&ctx);

    register(&service, "rl_login@example.com", "rlluser").await;

    for i in 0..10 {
        let res = TestClient::post("http://localhost/login")
            .json(&serde_json::json!({
                "email": "rl_login@example.com",
                "password": "wrongpass"
            }))
            .send(&service)
            .await;
        assert_ne!(
            res.status_code,
            Some(StatusCode::TOO_MANY_REQUESTS),
            "attempt {} should not be rate-limited",
            i + 1
        );
    }

    let res = TestClient::post("http://localhost/login")
        .json(&serde_json::json!({
            "email": "rl_login@example.com",
            "password": "wrongpass"
        }))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::TOO_MANY_REQUESTS));
}

/// A 429 response from the login endpoint must carry a Retry-After header.
#[tokio::test]
async fn login_rate_limit_retry_after_header() {
    let ctx = setup().await;
    let service = build_service(&ctx);

    // Exhaust the 10-request window.
    for _ in 0..11 {
        TestClient::post("http://localhost/login")
            .json(&serde_json::json!({
                "email": "rl_hdr@example.com",
                "password": "wrongpass"
            }))
            .send(&service)
            .await;
    }

    // Now we're definitely rate-limited; confirm the header is present.
    let res = TestClient::post("http://localhost/login")
        .json(&serde_json::json!({
            "email": "rl_hdr@example.com",
            "password": "wrongpass"
        }))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::TOO_MANY_REQUESTS));
    assert!(
        res.headers().contains_key(RETRY_AFTER),
        "429 login response must include Retry-After header"
    );
}

// ── register rate limiting (10 requests/min per IP) ───────────────────────

/// The 11th registration attempt from the same IP must return 429.
/// Requests 1-10 are allowed through (the first succeeds; the rest return
/// 409 Conflict because they use the same email, but they still consume
/// the rate-limit budget since the check runs before deduplication).
#[tokio::test]
async fn register_rate_limit_enforced() {
    let ctx = setup().await;
    let service = build_service(&ctx);

    for i in 0..10 {
        let res = TestClient::post("http://localhost/register")
            .json(&serde_json::json!({
                "username": format!("rlreg{}", i),
                "name": "RL User",
                "email": "rl_reg@example.com",
                "password": "password123"
            }))
            .send(&service)
            .await;
        assert_ne!(
            res.status_code,
            Some(StatusCode::TOO_MANY_REQUESTS),
            "attempt {} should not be rate-limited",
            i + 1
        );
    }

    let res = TestClient::post("http://localhost/register")
        .json(&serde_json::json!({
            "username": "rlreg10",
            "name": "RL User",
            "email": "rl_reg@example.com",
            "password": "password123"
        }))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::TOO_MANY_REQUESTS));
}

// ── password-reset-request rate limiting (5 requests/min per email+IP) ────

/// After 5 password-reset-request calls for the same email the 6th must
/// return 429 (per-email rate limit bucket).
#[tokio::test]
async fn password_reset_email_rate_limit_enforced() {
    let ctx = setup().await;
    let service = build_service(&ctx);

    for i in 0..5 {
        let res = TestClient::post("http://localhost/password-reset-request")
            .json(&serde_json::json!({"email": "rl_reset@example.com"}))
            .send(&service)
            .await;
        assert_ne!(
            res.status_code,
            Some(StatusCode::TOO_MANY_REQUESTS),
            "attempt {} should not be rate-limited",
            i + 1
        );
    }

    let res = TestClient::post("http://localhost/password-reset-request")
        .json(&serde_json::json!({"email": "rl_reset@example.com"}))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::TOO_MANY_REQUESTS));
}

/// The per-IP bucket for password-reset-request is independent of the
/// per-email bucket.  After 5 calls using *different* emails from the same
/// IP, the 6th call (any email) must return 429.
#[tokio::test]
async fn password_reset_ip_rate_limit_enforced() {
    let ctx = setup().await;
    let service = build_service(&ctx);

    // All requests share the "unknown" IP because TestClient sends no
    // X-Forwarded-For header.  Use distinct emails so only the IP bucket
    // accumulates.
    for i in 0..5 {
        let res = TestClient::post("http://localhost/password-reset-request")
            .json(&serde_json::json!({"email": format!("rl_ip_{}@example.com", i)}))
            .send(&service)
            .await;
        assert_ne!(
            res.status_code,
            Some(StatusCode::TOO_MANY_REQUESTS),
            "attempt {} should not be rate-limited",
            i + 1
        );
    }

    let res = TestClient::post("http://localhost/password-reset-request")
        .json(&serde_json::json!({"email": "rl_ip_new@example.com"}))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::TOO_MANY_REQUESTS));
}
