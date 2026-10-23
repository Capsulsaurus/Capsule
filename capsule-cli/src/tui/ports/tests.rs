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

/// Answers every feature port with a fixed value echoing what it was asked.
struct Echo;

impl Ports for Echo {
    async fn session_summary(&self) -> Result<SessionSummary, String> {
        Ok(SessionSummary::SignedOut)
    }

    async fn login(&self, email: String, password: Secret) -> Result<LoginResult, String> {
        if password.expose() == "two-factor" {
            Ok(LoginResult::SecondFactor {
                mfa_token: Secret::new(email),
            })
        } else {
            Ok(LoginResult::SignedIn)
        }
    }

    async fn verify_second_factor(&self, mfa_token: Secret, code: Secret) -> Result<(), String> {
        Err(format!("{} {}", mfa_token.expose(), code.expose()))
    }

    async fn logout(&self) -> Result<bool, String> {
        Ok(true)
    }

    async fn sync(&self, from_start: bool) -> Result<SyncSummary, String> {
        Ok(SyncSummary {
            applied: usize::from(from_start),
            ..SyncSummary::default()
        })
    }

    async fn restore(&self, request: RestoreRequest) -> Result<RestoreSummary, String> {
        Ok(RestoreSummary {
            out: request.out.display().to_string(),
            ..RestoreSummary::default()
        })
    }

    async fn create_link(&self, request: CreateLinkRequest) -> Result<IssuedLink, String> {
        Err(request.library.display().to_string())
    }

    async fn revoke_link(&self, opaque_id: String) -> Result<String, String> {
        Ok(opaque_id)
    }
}

fn restore_request() -> RestoreRequest {
    RestoreRequest {
        library: PathBuf::from("/lib"),
        passphrase: Secret::new("pw"),
        out: PathBuf::from("/out"),
        scope: crate::restore::RestoreScope::Library,
    }
}

fn create_request() -> CreateLinkRequest {
    CreateLinkRequest {
        library: PathBuf::from("/lib"),
        passphrase: Secret::new("pw"),
        scope: capsule_core::sharing::ShareScope::Album(uuid::Uuid::nil()),
        expires_at: None,
        link_passphrase: None,
    }
}

#[tokio::test]
async fn each_feature_effect_comes_back_as_its_screens_action() {
    let cases = [
        (
            Effect::Auth(AuthEffect::Login {
                email: "me".to_owned(),
                password: Secret::new("pw"),
            }),
            Action::Auth(AuthAction::LoginFinished(Ok(LoginResult::SignedIn))),
        ),
        (
            Effect::Auth(AuthEffect::Login {
                email: "me".to_owned(),
                password: Secret::new("two-factor"),
            }),
            Action::Auth(AuthAction::LoginFinished(Ok(LoginResult::SecondFactor {
                mfa_token: Secret::new("me"),
            }))),
        ),
        (
            Effect::Auth(AuthEffect::VerifySecondFactor {
                mfa_token: Secret::new("t"),
                code: Secret::new("123"),
            }),
            Action::Auth(AuthAction::SecondFactorFinished(Err("t 123".to_owned()))),
        ),
        (
            Effect::Auth(AuthEffect::Logout),
            Action::Auth(AuthAction::SignOutFinished(Ok(true))),
        ),
        (
            Effect::Sync(SyncEffect::Run { from_start: true }),
            Action::Sync(SyncAction::Finished(Ok(SyncSummary {
                applied: 1,
                ..SyncSummary::default()
            }))),
        ),
        (
            Effect::Restore(RestoreEffect::Run(restore_request())),
            Action::Restore(RestoreAction::Finished(Ok(RestoreSummary {
                out: "/out".to_owned(),
                ..RestoreSummary::default()
            }))),
        ),
        (
            Effect::Share(ShareEffect::Create(create_request())),
            Action::Share(ShareAction::Created(Err("/lib".to_owned()))),
        ),
        (
            Effect::Share(ShareEffect::Revoke {
                opaque_id: "id".to_owned(),
            }),
            Action::Share(ShareAction::Revoked(Ok("id".to_owned()))),
        ),
    ];
    for (effect, action) in cases {
        assert_eq!(perform(&Echo, effect.clone()).await, action, "{effect:?}");
    }
}

#[tokio::test]
async fn a_port_left_out_answers_that_it_is_not_offered() {
    let ports = Answering(Ok(SessionSummary::SignedOut));
    let not_offered = || Err::<(), _>(NOT_OFFERED.to_owned());
    assert_eq!(
        perform(&ports, Effect::Auth(AuthEffect::Logout)).await,
        Action::Auth(AuthAction::SignOutFinished(Err(NOT_OFFERED.to_owned())))
    );
    assert_eq!(
        ports.login(String::new(), Secret::default()).await,
        Err(NOT_OFFERED.to_owned())
    );
    assert_eq!(
        ports
            .verify_second_factor(Secret::default(), Secret::default())
            .await,
        not_offered()
    );
    assert_eq!(ports.sync(false).await, Err(NOT_OFFERED.to_owned()));
    assert_eq!(
        ports.restore(restore_request()).await,
        Err(NOT_OFFERED.to_owned())
    );
    assert_eq!(
        ports.create_link(create_request()).await,
        Err(NOT_OFFERED.to_owned())
    );
    assert_eq!(
        ports.revoke_link(String::new()).await,
        Err(NOT_OFFERED.to_owned())
    );
}

/// The real ports with no config directory refuse every networked port with the same reason
/// the session read gives, before reaching for a server.
#[tokio::test]
async fn local_ports_without_a_config_directory_refuse_before_any_request() {
    let ports = LocalPorts::new(None);
    let reason = Err::<(), _>("could not resolve the config directory".to_owned());
    assert_eq!(ports.logout().await.map(|_| ()), reason);
    assert_eq!(ports.sync(false).await.map(|_| ()), reason);
    assert_eq!(ports.restore(restore_request()).await.map(|_| ()), reason);
    assert_eq!(
        ports.create_link(create_request()).await.map(|_| ()),
        reason
    );
    assert_eq!(ports.revoke_link(String::new()).await.map(|_| ()), reason);
    assert_eq!(
        ports
            .login(String::new(), Secret::default())
            .await
            .map(|_| ()),
        reason
    );
    assert_eq!(
        ports
            .verify_second_factor(Secret::default(), Secret::default())
            .await,
        reason
    );
}

/// A wrong library passphrase is a reason on the restore screen, not a panic, and nothing is
/// asked of the server: the library is opened before the session is resumed.
#[tokio::test]
async fn local_ports_restore_reports_a_library_that_does_not_open() {
    let scratch = Scratch::new();
    let ports = LocalPorts::new(Some(scratch.session_path()));
    let request = RestoreRequest {
        library: scratch.0.join("no-library-here"),
        ..restore_request()
    };
    let reason = ports.restore(request).await.expect_err("no library");
    assert!(
        reason.starts_with("Failed to open signed workspace"),
        "{reason}"
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
