//! Port tests: `perform` maps each effect to the action reporting it, and `LocalPorts` reads the
//! session file the way `capsule auth status` does.

use std::path::PathBuf;

use capsule_sdk::auth::PersistedSession;

use super::*;

/// A scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("capsule-tui-ports-{}", nanoid::nanoid!()));
        std::fs::create_dir_all(&path).expect("create scratch dir");
        Self(path)
    }

    fn session_path(&self) -> PathBuf {
        self.0.join("session.json")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Answering(Result<SessionSummary, String>);

impl Ports for Answering {
    async fn session_summary(&self) -> Result<SessionSummary, String> {
        self.0.clone()
    }
}

fn save(scratch: &Scratch, access_expires_at_unix: i64) {
    SessionStore::new(scratch.session_path())
        .save(&PersistedSession {
            access_token: "access".to_owned().into(),
            refresh_token: "refresh".to_owned().into(),
            access_expires_at_unix,
        })
        .expect("save session");
}

#[tokio::test]
async fn load_session_reports_the_ports_answer() {
    let ok = Answering(Ok(SessionSummary::SignedOut));
    assert_eq!(
        perform(&ok, Effect::LoadSession).await,
        Action::SessionLoaded(Ok(SessionSummary::SignedOut))
    );
    let failed = Answering(Err("nope".to_owned()));
    assert_eq!(
        perform(&failed, Effect::LoadSession).await,
        Action::SessionLoaded(Err("nope".to_owned()))
    );
}

#[tokio::test]
async fn no_session_file_is_signed_out() {
    let scratch = Scratch::new();
    let ports = LocalPorts::new(Some(scratch.session_path()));
    assert_eq!(ports.session_summary().await, Ok(SessionSummary::SignedOut));
}

#[tokio::test]
async fn a_session_with_a_future_expiry_is_signed_in_and_fresh() {
    let scratch = Scratch::new();
    save(&scratch, i64::from(i32::MAX));
    let ports = LocalPorts::new(Some(scratch.session_path()));
    assert_eq!(
        ports.session_summary().await,
        Ok(SessionSummary::SignedIn {
            access_token_fresh: true
        })
    );
}

#[tokio::test]
async fn a_session_with_a_past_expiry_is_signed_in_and_stale() {
    let scratch = Scratch::new();
    save(&scratch, 1);
    let ports = LocalPorts::new(Some(scratch.session_path()));
    assert_eq!(
        ports.session_summary().await,
        Ok(SessionSummary::SignedIn {
            access_token_fresh: false
        })
    );
}

#[tokio::test]
async fn a_malformed_session_file_is_a_reason_not_a_panic() {
    let scratch = Scratch::new();
    std::fs::write(scratch.session_path(), b"not json").expect("write");
    let ports = LocalPorts::new(Some(scratch.session_path()));
    let reason = ports.session_summary().await.expect_err("malformed");
    assert!(reason.starts_with("could not read the session"), "{reason}");
}

#[tokio::test]
async fn an_unresolved_config_directory_is_a_reason() {
    let ports = LocalPorts::new(None);
    assert_eq!(
        ports.session_summary().await,
        Err("could not resolve the config directory".to_owned())
    );
}
