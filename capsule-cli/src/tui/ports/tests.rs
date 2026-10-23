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

// ── Import and push ──────────────────────────────────────────────────────────

fn import_request(library: PathBuf) -> ImportRequest {
    ImportRequest {
        library,
        passphrase: Secret::new("pw"),
        kind: crate::tui::import::SourceKind::Files,
        sources: vec![PathBuf::from("/photos")],
    }
}

fn push_request(run: u64, library: PathBuf) -> PushRequest {
    PushRequest {
        run,
        library,
        passphrase: Secret::new("pw"),
        assets: None,
    }
}

/// Answers the import and push ports with values echoing what they were asked.
struct Pushing;

impl Ports for Pushing {
    async fn session_summary(&self) -> Result<SessionSummary, String> {
        Ok(SessionSummary::SignedOut)
    }

    async fn import(&self, request: ImportRequest) -> Result<ImportReport, String> {
        Ok(ImportReport {
            candidates: request.sources.len(),
            ..ImportReport::default()
        })
    }

    async fn push(&self, request: PushRequest) -> Result<PushSummary, String> {
        Ok(PushSummary {
            assets: request.assets.map_or(0, |assets| assets.len()),
            ..PushSummary::default()
        })
    }

    async fn push_progress(&self) -> Vec<PushProgress> {
        vec![PushProgress::Lagged(1)]
    }
}

#[tokio::test]
async fn import_and_push_effects_come_back_as_their_screens_actions() {
    let library = PathBuf::from("/lib");
    assert_eq!(
        perform(
            &Pushing,
            Effect::Import(ImportEffect::Run(import_request(library.clone())))
        )
        .await,
        Action::Import(ImportAction::Finished(Ok(ImportReport {
            candidates: 1,
            ..ImportReport::default()
        })))
    );
    let retry = PushRequest {
        assets: Some(vec![uuid::Uuid::nil()]),
        ..push_request(1, library)
    };
    assert_eq!(
        perform(&Pushing, Effect::Push(PushEffect::Run(retry))).await,
        Action::Push(PushAction::Finished(Ok(PushSummary {
            assets: 1,
            ..PushSummary::default()
        })))
    );
    assert_eq!(
        perform(&Pushing, Effect::Push(PushEffect::Watch)).await,
        Action::Push(PushAction::Progress(vec![PushProgress::Lagged(1)]))
    );
}

#[tokio::test]
async fn import_and_push_left_out_are_not_offered_and_tell_no_progress() {
    let ports = Answering(Ok(SessionSummary::SignedOut));
    let lib = PathBuf::from("/lib");
    assert_eq!(
        ports.import(import_request(lib.clone())).await,
        Err(NOT_OFFERED.to_owned())
    );
    assert_eq!(
        ports.push(push_request(1, lib)).await,
        Err(NOT_OFFERED.to_owned())
    );
    assert_eq!(
        ports.push_progress().await,
        vec![PushProgress::Closed { run: None }]
    );
}

/// A push that cannot start still closes its run's progress, so the screen's watch ends.
#[tokio::test]
async fn local_push_without_a_config_directory_refuses_and_closes_its_run() {
    let ports = LocalPorts::new(None);
    let refused = ports.push(push_request(7, PathBuf::from("/lib"))).await;
    assert_eq!(
        refused,
        Err("could not resolve the config directory".to_owned())
    );
    assert_eq!(
        ports.push_progress().await,
        vec![PushProgress::Closed { run: Some(7) }]
    );
}

/// A library that does not open is the push's reason, nothing is asked of the server, and the
/// run's progress is closed.
#[tokio::test]
async fn local_push_reports_a_library_that_does_not_open_and_closes_its_run() {
    let scratch = Scratch::new();
    let ports = LocalPorts::new(Some(scratch.session_path()));
    let reason = ports
        .push(push_request(3, scratch.0.join("no-library-here")))
        .await
        .expect_err("no library");
    assert!(
        reason.starts_with("Failed to open signed workspace"),
        "{reason}"
    );
    assert_eq!(
        ports.push_progress().await,
        vec![PushProgress::Closed { run: Some(3) }]
    );
}

#[tokio::test]
async fn local_import_reports_a_library_that_does_not_open() {
    let scratch = Scratch::new();
    let ports = LocalPorts::new(Some(scratch.session_path()));
    let reason = ports
        .import(import_request(scratch.0.join("no-library-here")))
        .await
        .expect_err("no library");
    assert!(
        reason.starts_with("Failed to open signed workspace"),
        "{reason}"
    );
}

/// The relay passes the custody steps on in order, drops chunk traffic, and ends once the run
/// is done — though the shared channel stays open — with everything the run published passed
/// on first.
#[tokio::test]
async fn the_relay_passes_only_custody_steps_and_ends_with_the_run() {
    let events = TransferEvents::default_channel();
    let channel = ProgressChannel::new();
    let (run_done, done) = oneshot::channel();
    let relay = tokio::spawn(forward_custody_events(
        events.subscribe().expect("a channel handle"),
        channel.sender(),
        done,
    ));
    let finalize = TransferEvent::Finalize {
        blob: "aa".to_owned(),
        session_id: "s".to_owned(),
        size: 1,
    };
    let receipt = TransferEvent::Receipt {
        asset_id: "a".to_owned(),
        blob: "aa".to_owned(),
        receipt_seq: 0,
    };
    events.emit(|| TransferEvent::ChunkSent {
        blob: "aa".to_owned(),
        session_id: "s".to_owned(),
        offset: 0,
        len: 1,
    });
    events.emit(|| finalize.clone());
    events.emit(|| receipt.clone());
    run_done.send(()).expect("the relay is listening");
    // `events` is still held here: only the run ending stops the relay.
    relay.await.expect("the relay ends when the run is done");
    assert_eq!(
        channel.next_batch().await,
        vec![
            PushProgress::Transfer(finalize),
            PushProgress::Transfer(receipt)
        ]
    );
}

/// A run that ends without saying so (its port panicked) still ends the relay, and a channel
/// that closes ends it too.
#[tokio::test]
async fn the_relay_ends_when_the_run_is_gone_or_the_channel_closes() {
    let events = TransferEvents::default_channel();
    let channel = ProgressChannel::new();
    let (run_done, done) = oneshot::channel::<()>();
    let relay = tokio::spawn(forward_custody_events(
        events.subscribe().expect("a channel handle"),
        channel.sender(),
        done,
    ));
    drop(run_done);
    relay.await.expect("a dropped run ends the relay");

    let (_run_done, done) = oneshot::channel::<()>();
    let relay = tokio::spawn(forward_custody_events(
        events.subscribe().expect("a channel handle"),
        channel.sender(),
        done,
    ));
    drop(events);
    relay.await.expect("a closed channel ends the relay");
}

/// A subscriber that fell behind says how many events it missed, and goes on.
#[tokio::test]
async fn the_relay_says_how_many_events_it_missed() {
    let events = TransferEvents::channel(1);
    let subscriber = events.subscribe().expect("a channel handle");
    let verify = |durable| TransferEvent::Verify {
        asset_id: "a".to_owned(),
        durable,
        blobs: 1,
        not_durable: 0,
    };
    // Three published before the relay reads any: the capacity-1 channel keeps the last.
    events.emit(|| verify(false));
    events.emit(|| verify(false));
    events.emit(|| verify(true));
    let channel = ProgressChannel::new();
    let (run_done, done) = oneshot::channel();
    run_done.send(()).expect("the receiver is held");
    forward_custody_events(subscriber, channel.sender(), done).await;
    assert_eq!(
        channel.next_batch().await,
        vec![
            PushProgress::Lagged(2),
            PushProgress::Transfer(verify(true))
        ]
    );
}

#[tokio::test]
async fn a_batch_takes_what_is_queued_up_to_its_bound() {
    let channel = ProgressChannel::default();
    let tx = channel.sender();
    for missed in 0..(PROGRESS_BATCH as u64 + 3) {
        tx.send(PushProgress::Lagged(missed)).expect("send");
    }
    assert_eq!(channel.next_batch().await.len(), PROGRESS_BATCH);
    assert_eq!(channel.next_batch().await.len(), 3);
}

/// A panic unwinding through a port still closes its run's progress.
#[tokio::test]
async fn a_run_closes_its_progress_even_when_it_panics() {
    let channel = ProgressChannel::new();
    let closed = CloseOnDrop {
        tx: channel.sender(),
        run: 9,
    };
    let task = tokio::spawn(async move {
        let _closed = closed;
        panic!("a port panicked");
    });
    assert!(task.await.expect_err("it panicked").is_panic());
    assert_eq!(
        channel.next_batch().await,
        vec![PushProgress::Closed { run: Some(9) }]
    );
}
