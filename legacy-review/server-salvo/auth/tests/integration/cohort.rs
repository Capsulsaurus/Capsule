//! Session device-cohort storage + grouping (slice `S-C13`).
//!
//! Exercises the authentication doc's cohort Validation bullets against a real testcontainer
//! Postgres + Valkey:
//!
//! - **Cohort is advisory.** Session creation with an absent, garbage, over-long, or
//!   well-formed cohort value all succeed and authorize identically — the value changes only
//!   which (non-authoritative) grouping row is recorded, never whether login works or what a
//!   token can do. (The structural half — the authorization `Claims` carry no cohort field —
//!   is the `claims::tests::claims_carry_no_cohort_field_tripwire` unit tripwire.)
//! - **Cohort grouping.** Two sessions asserting one cohort group together in the listing
//!   (both sessions carry the hash; the cohort map holds a single entry). A second login with
//!   the same cohort but a fresh session models a reinstall grouping with "previously used".
//! - **Durable map outlives sessions.** After the session that created a cohort is revoked
//!   (logout), the `device_cohorts` row persists — the "seen before" fact survives expiry.

use auth::models::responses::{SessionListingResponse, TokenResponse};
use salvo::http::StatusCode;
use salvo::test::{ResponseExt, TestClient};
use secrecy::ExposeSecret;

use crate::common::{TestContext, build_service, setup};

/// A plausible well-formed cohort value (64-char hex, like a SHA-256 digest).
const VALID_COHORT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

async fn register(service: &salvo::Service, email: &str, username: &str) -> TokenResponse {
    let mut res = TestClient::post("http://localhost/register")
        .json(&serde_json::json!({
            "username": username,
            "name": "Cohort Test User",
            "email": email,
            "password": "password123",
        }))
        .send(service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::CREATED));
    res.take_json().await.expect("token response")
}

/// Login, optionally asserting a `cohort_hash`. Returns the parsed token response and the raw
/// status so callers can assert "succeeds regardless".
async fn login(
    service: &salvo::Service,
    email: &str,
    cohort: Option<&str>,
) -> (Option<StatusCode>, TokenResponse) {
    let mut body = serde_json::json!({ "email": email, "password": "password123" });
    if let Some(c) = cohort {
        body["cohort_hash"] = serde_json::Value::String(c.to_string());
    }
    let mut res = TestClient::post("http://localhost/login")
        .json(&body)
        .send(service)
        .await;
    let status = res.status_code;
    let tokens = res.take_json().await.expect("token response");
    (status, tokens)
}

async fn list_sessions(service: &salvo::Service, access: &str) -> SessionListingResponse {
    let mut res = TestClient::get("http://localhost/devices")
        .add_header("Authorization", format!("Bearer {access}"), true)
        .send(service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));
    res.take_json().await.expect("session listing")
}

async fn account_id(ctx: &TestContext, email: &str) -> String {
    service::user::Query::find_user_by_email(&ctx.db, email)
        .await
        .expect("db query")
        .expect("user exists")
        .id
}

/// Advisory: absent, garbage, over-long, and well-formed cohort values all let login succeed
/// and yield tokens that authorize identically. The only difference is which grouping row is
/// recorded (an over-long value records none — treated as absent).
#[tokio::test]
async fn cohort_is_advisory_absent_garbage_and_valid_behave_identically() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    register(&service, "advisory@example.com", "advisoryuser").await;

    // Absent cohort.
    let (status, absent) = login(&service, "advisory@example.com", None).await;
    assert_eq!(
        status,
        Some(StatusCode::OK),
        "login must succeed with no cohort"
    );

    // Garbage cohort (not a hash at all).
    let (status, garbage) = login(&service, "advisory@example.com", Some("!!not-a-hash!!")).await;
    assert_eq!(
        status,
        Some(StatusCode::OK),
        "login must succeed with garbage cohort"
    );

    // Over-long cohort (structurally treated as absent — no row recorded, still succeeds).
    let over_long = "f".repeat(500);
    let (status, oversized) = login(&service, "advisory@example.com", Some(&over_long)).await;
    assert_eq!(
        status,
        Some(StatusCode::OK),
        "login must succeed with over-long cohort"
    );

    // Well-formed cohort.
    let (status, valid) = login(&service, "advisory@example.com", Some(VALID_COHORT)).await;
    assert_eq!(
        status,
        Some(StatusCode::OK),
        "login must succeed with valid cohort"
    );

    // Every issued access token authorizes identically — the cohort never gates anything.
    for tokens in [&absent, &garbage, &oversized, &valid] {
        let access = tokens.access_token.expose_secret();
        let mut res = TestClient::get("http://localhost/devices")
            .add_header("Authorization", format!("Bearer {access}"), true)
            .send(&service)
            .await;
        assert_eq!(
            res.status_code,
            Some(StatusCode::OK),
            "access is unaffected by the cohort value"
        );
        let _ = res.take_bytes(None).await;
    }

    // The over-long value recorded no cohort (treated as absent); garbage and valid did.
    let uid = account_id(&ctx, "advisory@example.com").await;
    let map = service::cohort::Query::for_user(&ctx.db, &uid)
        .await
        .expect("cohort map");
    let hashes: Vec<&str> = map.iter().map(|c| c.cohort_hash.as_str()).collect();
    assert!(
        hashes.contains(&"!!not-a-hash!!"),
        "garbage cohort is stored verbatim"
    );
    assert!(hashes.contains(&VALID_COHORT), "valid cohort is stored");
    assert!(
        !hashes.iter().any(|h| h.len() > 128),
        "over-long cohort must not be stored (treated as absent): {hashes:?}"
    );
}

/// Grouping: two sessions asserting the same cohort both carry it in the listing, and the
/// durable map holds exactly one entry for that cohort. A second same-cohort login (a fresh
/// session, like a reinstall) groups with the first rather than forking a new cohort.
#[tokio::test]
async fn cohort_groups_sessions_and_reinstall_joins_previous() {
    let ctx = setup().await;
    let service = build_service(&ctx);

    // Session 1: registration with a cohort.
    let mut res = TestClient::post("http://localhost/register")
        .json(&serde_json::json!({
            "username": "groupuser",
            "name": "Cohort Test User",
            "email": "group@example.com",
            "password": "password123",
            "cohort_hash": VALID_COHORT,
        }))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::CREATED));
    let _: TokenResponse = res.take_json().await.expect("token response");

    // Session 2: a fresh login with the SAME cohort — models the same physical device after a
    // reinstall (new session, same cohort).
    let (_, s2) = login(&service, "group@example.com", Some(VALID_COHORT)).await;

    let listing = list_sessions(&service, s2.access_token.expose_secret()).await;

    // Both sessions are listed and both carry the same cohort — they group.
    assert_eq!(listing.devices.len(), 2, "both sessions listed");
    assert!(
        listing
            .devices
            .iter()
            .all(|d| d.cohort_hash.as_deref() == Some(VALID_COHORT)),
        "every session carries the asserted cohort"
    );

    // The durable map holds a single entry for the shared cohort (grouped, not forked).
    assert_eq!(listing.cohorts.len(), 1, "one grouped cohort");
    assert_eq!(listing.cohorts[0].cohort_hash, VALID_COHORT);
    assert!(
        listing.cohorts[0].last_seen >= listing.cohorts[0].first_seen,
        "last_seen advances on re-observation"
    );
}

/// A cohort under two different accounts yields two distinct durable rows — the `user_id` fold
/// keeps them unlinkable, so a colliding hash across users never merges accounts.
#[tokio::test]
async fn cohort_is_scoped_per_user() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    register(&service, "scope_a@example.com", "scopea").await;
    register(&service, "scope_b@example.com", "scopeb").await;

    login(&service, "scope_a@example.com", Some(VALID_COHORT)).await;
    login(&service, "scope_b@example.com", Some(VALID_COHORT)).await;

    let a =
        service::cohort::Query::for_user(&ctx.db, &account_id(&ctx, "scope_a@example.com").await)
            .await
            .expect("map a");
    let b =
        service::cohort::Query::for_user(&ctx.db, &account_id(&ctx, "scope_b@example.com").await)
            .await
            .expect("map b");
    assert_eq!(a.len(), 1);
    assert_eq!(b.len(), 1);
    // Same hash string, but two separate rows keyed by user — no cross-account linkage.
    assert_eq!(a[0].cohort_hash, VALID_COHORT);
    assert_eq!(b[0].cohort_hash, VALID_COHORT);
}

/// Durability: the `device_cohorts` row outlives the session that created it. After logout
/// revokes the session, the map still remembers the cohort — the "seen before" fact survives
/// session expiry, which is the whole reason the map exists separately from the session store.
#[tokio::test]
async fn cohort_durable_map_outlives_session() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let tokens = register(&service, "durable@example.com", "durableuser").await;

    let (_, session) = login(&service, "durable@example.com", Some(VALID_COHORT)).await;
    let uid = account_id(&ctx, "durable@example.com").await;

    // Cohort is recorded while the session lives.
    let before = service::cohort::Query::for_user(&ctx.db, &uid)
        .await
        .expect("cohort map");
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].cohort_hash, VALID_COHORT);

    // Revoke the cohort-bearing session (logout) — the session record is gone.
    let logout = TestClient::post("http://localhost/logout")
        .add_header(
            "Authorization",
            format!("Bearer {}", session.access_token.expose_secret()),
            true,
        )
        .send(&service)
        .await;
    assert_eq!(logout.status_code, Some(StatusCode::OK));

    // The durable map still remembers the cohort after the session is gone.
    let after = service::cohort::Query::for_user(&ctx.db, &uid)
        .await
        .expect("cohort map after logout");
    assert_eq!(
        after, before,
        "the device_cohorts row must outlive the session that created it"
    );

    // And it is still surfaced to a *different* live session's listing (the registration
    // session, which never carried a cohort of its own).
    let listing = list_sessions(&service, tokens.access_token.expose_secret()).await;
    assert!(
        listing
            .cohorts
            .iter()
            .any(|c| c.cohort_hash == VALID_COHORT),
        "the durable cohort is surfaced beyond the session that created it"
    );
}
