//! The `.well-known/capsule/*` registry (slice `S-C18`), against the real router + a real
//! Postgres testcontainer.
//!
//! Coverage map — one test per contract the registry owes:
//! - `server_info_round_trips_the_documented_facts` — the record deserializes back into the
//!   published shape and carries the API base URL, the auth + federation endpoints, the
//!   operational signing key, and the enforced `protocol_version` window.
//! - `server_info_never_carries_a_user_list` — the explicit privacy constraint: no
//!   user-identifying value from the database appears anywhere in the document, at any depth.
//! - `deprecation_announces_the_cutoff_and_min_protocol` — the announcement names the cutoff
//!   date and the minimum `protocol_version` that stays accepted; a cutoff already past is no
//!   longer an active window in either document.
//! - `revoked_jti_publishes_the_active_table` / `revoked_jti_is_bounded_to_a_24h_window` — the
//!   published list is the durable table's active rows, bounded to ≤ 24 h of revocations.
//! - `a_second_server_consumes_the_published_revocation_list` — the point of publishing:
//!   another server fetches the list and its **real** capability verifier
//!   (`sync::federation::verify_capability`) refuses a revoked token on it, and fails **closed**
//!   once the cached copy passes the 15-minute staleness bound.

use jiff::{SignedDuration, Timestamp};
use salvo::http::StatusCode;
use salvo::test::{ResponseExt, TestClient};
use sea_orm::EntityTrait;
use serde_json::Value;
use service::federation::Revocations;
use sync::federation::revocation::default_max_staleness;
use sync::federation::{
    CapabilityIssuer, CapabilityReject, FederationScope, IssueParams, RevocationList,
    RevocationVerdict, VerifyContext, verify_capability,
};

use super::{PROTOCOL, TEST_OPERATIONAL_PUBLIC_KEY, TEST_SERVER_ID, setup};
use crate::config::DeprecationAnnouncement;
use crate::service::well_known::{
    DeprecationDocument, OPERATIONAL_KEY_ALGORITHM, RevokedJtiDocument, ServerInfo,
};

/// `GET` a well-known path, returning the decoded JSON body (every record is public — no token).
async fn fetch(svc: &salvo::Service, path: &str) -> Value {
    let mut res = TestClient::get(format!("http://localhost/.well-known/capsule/{path}"))
        .send(svc)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK), "GET {path}");
    res.take_json::<Value>().await.expect("json body")
}

/// **`server-info` round-trips its documented shape.** The served document deserializes back
/// into [`ServerInfo`] and equals what the builder produces for the same config — so the wire
/// form and the type are one contract, not two — and every documented fact is present.
#[tokio::test]
async fn server_info_round_trips_the_documented_facts() {
    let ctx = setup().await;
    let config = ctx.media_config();
    let svc = ctx.well_known_service(config.clone());

    let body = fetch(&svc, "server-info").await;
    let info: ServerInfo = serde_json::from_value(body.clone()).expect("round-trips");
    assert_eq!(
        info,
        ServerInfo::build(&config, Timestamp::now()),
        "the served document is exactly the built record"
    );

    // The API base URL + the auth endpoints a client bootstraps through.
    assert_eq!(body["server_id"], TEST_SERVER_ID);
    assert_eq!(body["api_base_url"], "https://localhost/v1");
    assert_eq!(body["auth"]["login"], "https://localhost/v1/auth/login");
    assert_eq!(
        body["auth"]["register"],
        "https://localhost/v1/auth/register"
    );
    assert_eq!(body["auth"]["refresh"], "https://localhost/v1/auth/refresh");
    assert_eq!(body["auth"]["passkey"], "https://localhost/v1/auth/passkey");

    // The federation endpoints a peer pulls from — the same primitives a client uses.
    assert_eq!(
        body["federation"]["sync_feed"],
        "https://localhost/capsule.sync.v1.SyncService"
    );
    assert_eq!(body["federation"]["blob"], "https://localhost/v1/blob");

    // The server's operational signing key, verbatim.
    assert_eq!(body["signing_key"]["algorithm"], OPERATIONAL_KEY_ALGORITHM);
    use base64::Engine as _;
    assert_eq!(
        body["signing_key"]["public"].as_str(),
        Some(
            base64::engine::general_purpose::STANDARD
                .encode(TEST_OPERATIONAL_PUBLIC_KEY)
                .as_str()
        ),
        "the published key is the configured operational key"
    );

    // The `protocol_version` window this server actually enforces.
    assert_eq!(body["protocol_version"]["min"], config.protocol_min);
    assert_eq!(body["protocol_version"]["max"], config.protocol_max);
    assert!(
        config.protocol_min.as_str() <= PROTOCOL && PROTOCOL <= config.protocol_max.as_str(),
        "the fixture's pin sits inside the published window"
    );

    // Nothing is being deprecated in the default deployment, and that is stated, not implied.
    assert_eq!(body["deprecations"], Value::Array(vec![]));
}

/// **Never a user list.** `.well-known/` enumerating a server's users is forbidden outright
/// (abuse surface + privacy), so with real users and assets in the database, no
/// user-identifying value appears anywhere in the document — at any depth, in a key or a value.
#[tokio::test]
async fn server_info_never_carries_a_user_list() {
    let ctx = setup().await;

    // Real user + asset state to leak, if the record were ever built from the database.
    let asset_id = nanoid::nanoid!();
    let hash = ctx
        .finalize_blob(&asset_id, "original", b"a user's ciphertext")
        .await;
    ctx.seed_asset_row(&asset_id, &hash, true).await;
    let user = entity::user::Entity::find_by_id(ctx.user_id.clone())
        .one(&ctx.db)
        .await
        .expect("query user")
        .expect("the harness seeded a user");

    let svc = ctx.well_known_service(ctx.media_config());
    let body = fetch(&svc, "server-info").await;

    let mut strings = Vec::new();
    collect_strings(&body, &mut strings);
    for secret in [
        user.id.as_str(),
        user.username.as_str(),
        user.email.as_str(),
        user.name.as_str(),
        ctx.album_id.as_str(),
        asset_id.as_str(),
    ] {
        assert!(
            !strings.iter().any(|s| s.contains(secret)),
            "server-info leaked a user-identifying value ({secret})"
        );
    }
    for forbidden in ["users", "user", "accounts", "handles", "members", "assets"] {
        assert!(
            body.get(forbidden).is_none(),
            "server-info must not carry a `{forbidden}` field"
        );
    }
}

/// **`deprecation` names the cutoff and the surviving minimum.** An announcement whose cutoff
/// has already passed is not an *active* window and appears in neither document.
#[tokio::test]
async fn deprecation_announces_the_cutoff_and_min_protocol() {
    let ctx = setup().await;
    let mut config = ctx.media_config();
    let future = DeprecationAnnouncement {
        cutoff: "2099-01-01".to_string(),
        min_protocol_version: "2026-06-01".to_string(),
        announced_at: "2026-01-01".to_string(),
        min_client_build: Some("2.0.0".to_string()),
    };
    let past = DeprecationAnnouncement {
        cutoff: "2000-01-01".to_string(),
        min_protocol_version: "1999-06-01".to_string(),
        announced_at: "1999-01-01".to_string(),
        min_client_build: None,
    };
    config.deprecations = vec![past, future.clone()];
    let svc = ctx.well_known_service(config);

    let body = fetch(&svc, "deprecation").await;
    let doc: DeprecationDocument = serde_json::from_value(body.clone()).expect("round-trips");
    assert_eq!(doc.server_id, TEST_SERVER_ID);
    assert_eq!(
        doc.announcement_window_days, 90,
        "the policy's default notice"
    );
    assert_eq!(
        doc.announcements,
        vec![future.clone()],
        "only the cutoff still ahead is announced"
    );
    assert_eq!(body["announcements"][0]["cutoff"], "2099-01-01");
    assert_eq!(
        body["announcements"][0]["min_protocol_version"],
        "2026-06-01"
    );
    assert_eq!(body["announcements"][0]["min_client_build"], "2.0.0");

    // `server-info` carries the same active windows — one record set, two surfaces.
    let info: ServerInfo = serde_json::from_value(fetch(&svc, "server-info").await).expect("info");
    assert_eq!(info.deprecations, vec![future]);
}

/// **`revoked-jti` publishes the durable table's active rows.** An already-expired revocation is
/// not published (an expired token is rejected unconditionally anyway).
#[tokio::test]
async fn revoked_jti_publishes_the_active_table() {
    let ctx = setup().await;
    let now = Timestamp::now();
    let live = uuid::Uuid::now_v7().to_string();
    let expired = uuid::Uuid::now_v7().to_string();
    Revocations::revoke(&ctx.db, &live, now + SignedDuration::from_hours(1))
        .await
        .expect("revoke live");
    Revocations::revoke(&ctx.db, &expired, now - SignedDuration::from_hours(1))
        .await
        .expect("revoke expired");

    let svc = ctx.well_known_service(ctx.media_config());
    let body = fetch(&svc, "revoked-jti").await;
    let doc: RevokedJtiDocument = serde_json::from_value(body).expect("round-trips");

    assert_eq!(doc.server_id, TEST_SERVER_ID);
    assert_eq!(doc.window_hours, 24, "the ≤ 24 h publication bound");
    assert!(
        doc.issued_at.parse::<Timestamp>().is_ok(),
        "issued_at is RFC 3339"
    );
    assert!(
        doc.revoked_jti.contains(&live),
        "a live revocation publishes"
    );
    assert!(
        !doc.revoked_jti.contains(&expired),
        "an expired revocation is pruned from the published list"
    );
}

/// **The list stays bounded to 24 h of revocations.** A row whose `exp` is further out than a
/// capability may ever live is not published — the bound is stated in the document and enforced
/// in the query.
#[tokio::test]
async fn revoked_jti_is_bounded_to_a_24h_window() {
    let ctx = setup().await;
    let now = Timestamp::now();
    let inside = uuid::Uuid::now_v7().to_string();
    let outside = uuid::Uuid::now_v7().to_string();
    Revocations::revoke(&ctx.db, &inside, now + SignedDuration::from_hours(23))
        .await
        .expect("revoke inside");
    Revocations::revoke(&ctx.db, &outside, now + SignedDuration::from_hours(48))
        .await
        .expect("revoke outside");

    let svc = ctx.well_known_service(ctx.media_config());
    let doc: RevokedJtiDocument =
        serde_json::from_value(fetch(&svc, "revoked-jti").await).expect("round-trips");
    assert!(doc.revoked_jti.contains(&inside));
    assert!(
        !doc.revoked_jti.contains(&outside),
        "a revocation beyond the 24 h window is not published"
    );
}

/// **A second server consumes the published list.** The whole point of publishing: peer B
/// fetches issuer A's document, caches it, and its real capability verifier refuses a revoked
/// token on it — then fails **closed** once the cached copy passes the 15-minute staleness bound
/// with no refresh. This is the federation doc's revocation-list acceptance, driven end to end
/// through the served document.
#[tokio::test]
async fn a_second_server_consumes_the_published_revocation_list() {
    let ctx = setup().await;
    let (encoding_key, decoding_key) = super::decode_keys();
    let now = Timestamp::now();

    // Issuer A mints two capabilities for peer B over the same album.
    let issuer = CapabilityIssuer::new(TEST_SERVER_ID, encoding_key);
    let params = IssueParams {
        peer: "peer.example",
        album_id: &ctx.album_id,
        scope: FederationScope::Read,
        min_protocol_version: PROTOCOL,
        ttl: SignedDuration::from_hours(6),
    };
    let doomed = issuer.issue(&params, now).expect("mint doomed capability");
    let healthy = issuer.issue(&params, now).expect("mint healthy capability");

    // A revokes one of them, and publishes.
    Revocations::revoke(
        &ctx.db,
        &doomed.claims.jti,
        now + SignedDuration::from_hours(6),
    )
    .await
    .expect("revoke");

    let svc = ctx.well_known_service(ctx.media_config());
    let published: RevokedJtiDocument =
        serde_json::from_value(fetch(&svc, "revoked-jti").await).expect("round-trips");

    // B caches what it fetched, under the doc's 15-minute staleness bound.
    let fetched_at = now;
    let cached = RevocationList::cached(
        published.revoked_jti.clone(),
        fetched_at,
        default_max_staleness(),
    );
    assert_eq!(
        cached.check(&doomed.claims.jti, now),
        RevocationVerdict::Revoked
    );
    assert_eq!(
        cached.check(&healthy.claims.jti, now),
        RevocationVerdict::NotRevoked
    );

    // B's real verifier refuses the revoked token and honors the other.
    let ctx_b = VerifyContext {
        expected_issuer: TEST_SERVER_ID,
        album_id: &ctx.album_id,
        now,
    };
    assert!(
        matches!(
            verify_capability(&doomed.token, &decoding_key, &ctx_b, &cached),
            Err(CapabilityReject::Revoked)
        ),
        "the peer refuses a token the published list revokes"
    );
    verify_capability(&healthy.token, &decoding_key, &ctx_b, &cached)
        .expect("an unrevoked capability still verifies");

    // Past the staleness bound with no refresh, every token becomes unconfirmable — fail closed.
    let stale_at = now + default_max_staleness() + SignedDuration::from_secs(1);
    assert!(
        matches!(
            verify_capability(
                &healthy.token,
                &decoding_key,
                &VerifyContext {
                    now: stale_at,
                    ..ctx_b
                },
                &cached
            ),
            Err(CapabilityReject::RevocationUnverifiable)
        ),
        "a stale cache fails closed rather than honoring the token"
    );

    // The issuer verifying its own tokens is never stale — it reads its own always-fresh list.
    let owned = RevocationList::owned(
        Revocations::published_jtis(&ctx.db, now)
            .await
            .expect("own list"),
    );
    assert_eq!(
        owned.check(&doomed.claims.jti, stale_at),
        RevocationVerdict::Revoked
    );
}

/// Every string in a JSON document, at any depth, keys included.
fn collect_strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => out.push(s.clone()),
        Value::Array(items) => items.iter().for_each(|v| collect_strings(v, out)),
        Value::Object(map) => {
            for (k, v) in map {
                out.push(k.clone());
                collect_strings(v, out);
            }
        }
        _ => {}
    }
}
