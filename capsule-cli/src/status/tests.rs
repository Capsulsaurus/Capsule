use capsule_sdk::auth::PersistedSession;
use secrecy::SecretString;

use super::*;

/// A store at a unique temp path, matching the convention in `session.rs`'s tests
/// (no extra dev-dependency for something the standard library covers).
fn temp_store(tag: &str) -> SessionStore {
    SessionStore::new(std::env::temp_dir().join(format!(
        "capsule-cli-status-{tag}-{}.json",
        nanoid::nanoid!()
    )))
}

fn session_expiring_at(unix: i64) -> PersistedSession {
    PersistedSession {
        access_token: SecretString::from("access"),
        refresh_token: SecretString::from("refresh"),
        access_expires_at_unix: unix,
    }
}

#[test]
fn auth_status_reports_logged_out_without_a_session() {
    let status = AuthStatus::check(&temp_store("logged-out")).expect("check");
    assert!(!status.signed_in);
    assert!(!status.access_token_fresh);
    assert!(status.token_expires_at.is_none());
}

/// The regression this change exists for: `auth status` used to read an environment
/// variable rather than the session store, so it reported "Not logged in" immediately
/// after `auth login` had succeeded.
#[test]
fn auth_status_reflects_a_session_that_login_persisted() {
    let store = temp_store("persisted");
    let future = Timestamp::now().as_second() + 900;
    store.save(&session_expiring_at(future)).expect("save");

    let status = AuthStatus::check(&store).expect("check");
    assert!(status.signed_in);
    assert!(status.access_token_fresh);
    assert_eq!(
        status.token_expires_at.map(jiff::Timestamp::as_second),
        Some(future)
    );
}

/// An expired *access* token is not a logged-out session — the refresh token still
/// works, and the next networked command renews it transparently. Reporting this as
/// logged out would send users to re-authenticate for no reason.
#[test]
fn an_expired_access_token_is_still_signed_in() {
    let store = temp_store("expired");
    let past = Timestamp::now().as_second() - 60;
    store.save(&session_expiring_at(past)).expect("save");

    let status = AuthStatus::check(&store).expect("check");
    assert!(status.signed_in, "a persisted session is still a session");
    assert!(!status.access_token_fresh);
}

/// An unreachable server must be reported as unreachable, not as a hardcoded
/// "Backend not implemented" — a user cannot tell that apart from a real outage.
#[tokio::test]
async fn server_status_reports_an_unreachable_endpoint() {
    // Port 1 on loopback refuses immediately; this asserts the failure path without
    // waiting out the probe timeout.
    let remote = RemoteConfig {
        auth_endpoint: "http://127.0.0.1:1/v1/auth".to_string(),
        sync_endpoint: "http://127.0.0.1:1".to_string(),
        upload_endpoint: "http://127.0.0.1:1/v1/upload".to_string(),
        albums_endpoint: "http://127.0.0.1:1/v1/albums".to_string(),
        protocol_version: capsule_core::crypto::primitives::PROTOCOL_VERSION.to_string(),
    };
    let status = ServerStatus::check(&remote).await.expect("check");
    assert!(
        matches!(
            status.connection_status,
            ConnectionStatus::Error(_) | ConnectionStatus::Disconnected
        ),
        "expected a failure status, got {:?}",
        status.connection_status
    );
    assert!(status.api_version.is_none());
    assert!(
        status.server_health.as_deref() != Some("Backend not implemented"),
        "the mocked health string must be gone"
    );
}
