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
async fn local_open_library_reports_a_library_that_does_not_open() {
    let scratch = Scratch::new();
    let ports = LocalPorts::new(Some(scratch.session_path()));
    let reason = ports
        .open_library(OpenRequest {
            library: scratch.0.join("no-library-here"),
            passphrase: Secret::new("pw"),
        })
        .await
        .expect_err("no library");
    assert!(
        reason.starts_with("Failed to open signed workspace"),
        "{reason}"
    );
}

fn preview_request(original: PathBuf, ext: &str) -> PreviewRequest {
    PreviewRequest {
        asset_id: uuid::Uuid::now_v7(),
        original,
        ext: ext.to_owned(),
    }
}

#[tokio::test]
async fn local_preview_decodes_nothing_for_a_terminal_without_images() {
    let scratch = Scratch::new();
    let ports = LocalPorts::new(Some(scratch.session_path()));
    assert!(ports.image_picker().is_none());
    // The original does not even exist: nothing is read.
    let reason = ports
        .preview(preview_request(scratch.0.join("missing.jpg"), "jpg"))
        .await
        .expect_err("no protocol");
    assert_eq!(reason, NO_IMAGE_PROTOCOL);
}

#[tokio::test]
async fn local_preview_decodes_an_original_through_the_media_module() {
    let scratch = Scratch::new();
    let ports =
        LocalPorts::new(Some(scratch.session_path())).with_picker(Some(Picker::halfblocks()));
    assert!(ports.image_picker().is_some());

    let original = scratch.0.join("photo.png");
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(2048, 1024, image::Rgba([1, 2, 3, 255]))
        .write_to(&mut png, image::ImageFormat::Png)
        .expect("encode a PNG");
    std::fs::write(&original, png.into_inner()).expect("write the original");
    let preview = ports
        .preview(preview_request(original, "png"))
        .await
        .expect("a PNG previews");
    assert_eq!(
        (preview.width, preview.height),
        (PREVIEW_EDGE, PREVIEW_EDGE / 2)
    );

    // A format with no still decoder is refused before its original is read.
    let reason = ports
        .preview(preview_request(scratch.0.join("missing.mp4"), "mp4"))
        .await
        .expect_err("not a still");
    assert_eq!(reason, "a .mp4 original is not a still this build decodes");
    // A missing original says so.
    let reason = ports
        .preview(preview_request(scratch.0.join("missing.jpg"), "jpg"))
        .await
        .expect_err("no file");
    assert!(
        reason.starts_with("could not read the original"),
        "{reason}"
    );
}

fn encode_request(rgba_len: usize) -> EncodeRequest {
    EncodeRequest {
        generation: 7,
        image: PreviewImage {
            width: 8,
            height: 8,
            rgba: vec![9; rgba_len].into(),
        },
        size: ratatui::layout::Size::new(10, 5),
    }
}

#[tokio::test]
async fn local_encode_runs_off_the_loop_and_reports_its_failures() {
    let scratch = Scratch::new();
    let text = LocalPorts::new(Some(scratch.session_path()));
    assert_eq!(
        text.encode_preview(encode_request(8 * 8 * 4))
            .await
            .map(|_| ()),
        Err(NO_IMAGE_PROTOCOL.to_owned())
    );
    let ports = text.with_picker(Some(Picker::halfblocks()));
    assert!(
        ports
            .encode_preview(encode_request(8 * 8 * 4))
            .await
            .is_ok()
    );
    assert_eq!(
        ports.encode_preview(encode_request(3)).await.map(|_| ()),
        Err("the decoded preview has the wrong length".to_owned())
    );
    // Through `perform`, the answer names the preview and pane it was for.
    let action = perform(
        &ports,
        Effect::Library(LibraryEffect::Encode(encode_request(3))),
    )
    .await;
    assert!(
        matches!(
            action,
            Action::Library(LibraryAction::PreviewEncoded { generation: 7, size, result: Err(_) })
                if size == ratatui::layout::Size::new(10, 5)
        ),
        "{action:?}"
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

// ---- the identity-provider sign-in's two effects (issue #856) ----------------------------

fn prompt(code: &str) -> ProviderPrompt {
    ProviderPrompt::Device {
        verification_uri: "https://idp.test/device".to_owned(),
        user_code: Secret::new(code),
        verification_uri_complete: None,
        wait_secs: 600,
    }
}

/// Sets its flag when dropped: a sign-in task that was aborted drops what it held.
struct DropFlag(Arc<std::sync::atomic::AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// A sign-in run's future, boxed so a helper can name it.
type RunFuture = std::pin::Pin<Box<dyn Future<Output = Result<LoginResult, String>> + Send>>;

/// A run that shows `code`, then waits until `release` fires and answers what it carries,
/// setting `dropped` however it ends.
fn held_run(
    code: &'static str,
    release: oneshot::Receiver<Result<LoginResult, String>>,
    dropped: Arc<std::sync::atomic::AtomicBool>,
) -> impl FnOnce(oneshot::Sender<ProviderPrompt>) -> RunFuture {
    move |tx| {
        Box::pin(async move {
            let _flag = DropFlag(dropped);
            let _ = tx.send(prompt(code));
            release
                .await
                .unwrap_or_else(|_| Err("released without an answer".to_owned()))
        })
    }
}

fn flag() -> Arc<std::sync::atomic::AtomicBool> {
    Arc::new(std::sync::atomic::AtomicBool::new(false))
}

fn is_set(flag: &std::sync::atomic::AtomicBool) -> bool {
    flag.load(std::sync::atomic::Ordering::SeqCst)
}

/// Whether `flag` is set once the runtime has had a few turns to drop an aborted task.
async fn dropped_soon(flag: &std::sync::atomic::AtomicBool) -> bool {
    for _ in 0..16 {
        if is_set(flag) {
            return true;
        }
        tokio::task::yield_now().await;
    }
    is_set(flag)
}

#[tokio::test]
async fn a_begun_sign_in_answers_its_prompt_at_once_and_its_outcome_when_waited_on() {
    let slot = ProviderSlot::default();
    let (release, released) = oneshot::channel();
    let dropped = flag();
    assert_eq!(
        slot.begin(1, held_run("ABCD", released, Arc::clone(&dropped)))
            .await,
        Ok(prompt("ABCD"))
    );
    assert!(
        !is_set(&dropped),
        "the wait keeps running after the begin answered"
    );
    release
        .send(Ok(LoginResult::SignedIn))
        .expect("the run waits");
    assert_eq!(slot.wait(1).await, Ok(LoginResult::SignedIn));
    // Taken once: there is nothing left to wait on.
    assert_eq!(slot.wait(1).await, Err(NO_PROVIDER_WAIT.to_owned()));
}

#[tokio::test]
async fn a_sign_in_that_fails_before_it_shows_anything_answers_the_begin_with_why() {
    let slot = ProviderSlot::default();
    let failed = slot
        .begin(1, |_prompt| async {
            Err("refused by the server".to_owned())
        })
        .await;
    assert_eq!(failed, Err("refused by the server".to_owned()));
    assert_eq!(slot.wait(1).await, Err(NO_PROVIDER_WAIT.to_owned()));
    let silent = slot
        .begin(2, |_prompt| async { Ok(LoginResult::SignedIn) })
        .await;
    assert_eq!(
        silent,
        Err("the sign-in ended before it showed anything".to_owned())
    );
}

#[tokio::test]
async fn cancelling_aborts_the_wait_in_flight() {
    let slot = Arc::new(ProviderSlot::default());
    let (_release, released) = oneshot::channel();
    let dropped = flag();
    slot.begin(1, held_run("ABCD", released, Arc::clone(&dropped)))
        .await
        .expect("begun");
    let waiting = tokio::spawn({
        let slot = Arc::clone(&slot);
        async move { slot.wait(1).await }
    });
    // Let the wait take the task before it is cancelled.
    tokio::task::yield_now().await;
    slot.cancel(1);
    assert_eq!(
        waiting.await.expect("the wait returns"),
        Err(PROVIDER_CANCELLED.to_owned())
    );
    assert!(is_set(&dropped), "the aborted task dropped what it held");
}

#[tokio::test]
async fn cancelling_before_the_wait_leaves_nothing_to_wait_on() {
    let slot = ProviderSlot::default();
    let (_release, released) = oneshot::channel();
    let dropped = flag();
    slot.begin(1, held_run("ABCD", released, Arc::clone(&dropped)))
        .await
        .expect("begun");
    // Another attempt's cancel touches nothing.
    slot.cancel(7);
    assert!(!is_set(&dropped));
    slot.cancel(1);
    assert_eq!(slot.wait(1).await, Err(NO_PROVIDER_WAIT.to_owned()));
    assert!(dropped_soon(&dropped).await);
}

#[tokio::test]
async fn a_new_sign_in_replaces_and_stops_the_one_waiting() {
    let slot = ProviderSlot::default();
    let (_first_release, first_released) = oneshot::channel();
    let first_dropped = flag();
    slot.begin(
        1,
        held_run("FIRST", first_released, Arc::clone(&first_dropped)),
    )
    .await
    .expect("begun");
    let (second_release, second_released) = oneshot::channel();
    assert_eq!(
        slot.begin(2, held_run("SECOND", second_released, flag()))
            .await,
        Ok(prompt("SECOND"))
    );
    assert!(dropped_soon(&first_dropped).await);
    assert_eq!(slot.wait(1).await, Err(NO_PROVIDER_WAIT.to_owned()));
    second_release
        .send(Err("expired".to_owned()))
        .expect("the run waits");
    assert_eq!(slot.wait(2).await, Err("expired".to_owned()));
}

#[tokio::test]
async fn a_panicking_sign_in_answers_a_reason_instead_of_hanging() {
    let slot = ProviderSlot::default();
    let (release, released) = oneshot::channel::<()>();
    slot.begin(1, move |tx| async move {
        let _ = tx.send(prompt("ABCD"));
        let _ = released.await;
        panic!("a ceremony panicked");
    })
    .await
    .expect("begun");
    release.send(()).expect("the run waits");
    assert_eq!(
        slot.wait(1).await,
        Err("the sign-in task panicked; see the TUI log".to_owned())
    );
}

/// Answers the three identity-provider ports with what they were asked, recording cancels.
#[derive(Default)]
struct ProviderEcho {
    cancelled: std::sync::Mutex<Vec<u64>>,
}

impl Ports for ProviderEcho {
    async fn session_summary(&self) -> Result<SessionSummary, String> {
        Ok(SessionSummary::SignedOut)
    }

    async fn begin_provider(
        &self,
        attempt: u64,
        flow: ProviderFlow,
    ) -> Result<ProviderPrompt, String> {
        Ok(match flow {
            ProviderFlow::Device => prompt(&attempt.to_string()),
            ProviderFlow::Browser => ProviderPrompt::Browser {
                authorization_url: Secret::new(format!("https://idp.test/{attempt}")),
                opened: false,
                wait_secs: 300,
            },
        })
    }

    async fn await_provider(&self, attempt: u64) -> Result<LoginResult, String> {
        Err(format!("attempt {attempt}"))
    }

    async fn cancel_provider(&self, attempt: u64) {
        self.cancelled.lock().expect("unpoisoned").push(attempt);
    }
}

#[tokio::test]
async fn the_provider_effects_answer_with_their_own_attempt() {
    let ports = ProviderEcho::default();
    assert_eq!(
        perform(
            &ports,
            Effect::Auth(AuthEffect::BeginProvider {
                attempt: 4,
                flow: ProviderFlow::Device
            })
        )
        .await,
        Action::Auth(AuthAction::ProviderBegun {
            attempt: 4,
            result: Ok(prompt("4")),
        })
    );
    assert_eq!(
        perform(
            &ports,
            Effect::Auth(AuthEffect::AwaitProvider { attempt: 4 })
        )
        .await,
        Action::Auth(AuthAction::ProviderFinished {
            attempt: 4,
            result: Err("attempt 4".to_owned()),
        })
    );
    assert_eq!(
        perform(
            &ports,
            Effect::Auth(AuthEffect::CancelProvider { attempt: 4 })
        )
        .await,
        Action::Auth(AuthAction::ProviderCancelled { attempt: 4 })
    );
    assert_eq!(*ports.cancelled.lock().expect("unpoisoned"), vec![4]);
}

#[tokio::test]
async fn the_default_ports_do_not_offer_a_provider_sign_in() {
    let ports = Answering(Ok(SessionSummary::SignedOut));
    assert_eq!(
        ports.begin_provider(1, ProviderFlow::Device).await,
        Err(NOT_OFFERED.to_owned())
    );
    assert_eq!(ports.await_provider(1).await, Err(NOT_OFFERED.to_owned()));
}

/// A browser ceremony that completes with whatever code the redirect carried.
struct ScriptedBrowser {
    completed_with: std::sync::Mutex<Option<String>>,
}

const BROWSER_STATE: &str = "browser-state-4417";

impl crate::oidc::Ceremony for ScriptedBrowser {
    async fn begin(
        &self,
        redirect_uri: &str,
    ) -> Result<capsule_sdk::auth::OidcAuthorization, remote::RemoteError> {
        let now = u64::try_from(jiff::Timestamp::now().as_second()).unwrap_or(0);
        Ok(capsule_sdk::auth::OidcAuthorization {
            authorization_url: format!("https://idp.test/authorize?redirect_uri={redirect_uri}"),
            state: secrecy::SecretString::from(BROWSER_STATE),
            binding: secrecy::SecretString::from("binding"),
            expires_by: now + 600,
        })
    }

    async fn complete(
        &self,
        _: &capsule_sdk::auth::OidcAuthorization,
        _state: &str,
        code: &str,
    ) -> Result<remote::LoginStep, remote::RemoteError> {
        *self.completed_with.lock().expect("unpoisoned") = Some(code.to_owned());
        Ok(remote::LoginStep::SecondFactorRequired {
            mfa_token: secrecy::SecretString::from("browser-challenge"),
        })
    }
}

/// Follow the redirect the authorization URL names, as a browser would after the person signed
/// in, carrying `code`.
async fn follow_redirect(authorization_url: &str, code: &str) {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let url = url::Url::parse(authorization_url).expect("a URL");
    let redirect = url
        .query_pairs()
        .find(|(key, _)| key == "redirect_uri")
        .map(|(_, value)| value.into_owned())
        .expect("a redirect_uri");
    let authority = redirect
        .strip_prefix("http://")
        .and_then(|rest| rest.split('/').next())
        .expect("a loopback redirect URI")
        .to_owned();
    let mut stream = tokio::net::TcpStream::connect(&authority)
        .await
        .expect("the listener accepts");
    stream
        .write_all(
            format!(
                "GET /callback?state={BROWSER_STATE}&code={code} HTTP/1.1\r\nHost: {authority}\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .expect("the request is written");
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response).await;
}

#[tokio::test]
async fn a_browser_sign_in_shows_the_address_then_settles_on_the_redirect() {
    let ceremony = Arc::new(ScriptedBrowser {
        completed_with: std::sync::Mutex::new(None),
    });
    let slot = ProviderSlot::default();
    let opened_with = Arc::new(std::sync::Mutex::new(None::<String>));
    let begun = slot
        .begin(1, {
            let (ceremony, opened_with) = (Arc::clone(&ceremony), Arc::clone(&opened_with));
            move |prompt| async move {
                browser_sign_in(
                    ceremony.as_ref(),
                    prompt,
                    std::time::Duration::from_mins(5),
                    |url| {
                        *opened_with.lock().expect("unpoisoned") = Some(url.to_owned());
                        false
                    },
                )
                .await
            }
        })
        .await
        .expect("begun");
    let ProviderPrompt::Browser {
        authorization_url,
        opened,
        wait_secs,
    } = begun
    else {
        panic!("a browser prompt: {begun:?}");
    };
    assert!(!opened, "the opener said it could not open one");
    assert_eq!(wait_secs, 300);
    assert_eq!(
        opened_with.lock().expect("unpoisoned").as_deref(),
        Some(authorization_url.expose())
    );
    follow_redirect(authorization_url.expose(), "granted").await;
    assert_eq!(
        slot.wait(1).await,
        Ok(LoginResult::SecondFactor {
            mfa_token: Secret::new("browser-challenge"),
        })
    );
    assert_eq!(
        ceremony
            .completed_with
            .lock()
            .expect("unpoisoned")
            .as_deref(),
        Some("granted")
    );
}

#[tokio::test]
async fn a_refused_browser_ceremony_reads_as_the_cli_words_it() {
    struct Refusing;
    impl crate::oidc::Ceremony for Refusing {
        async fn begin(
            &self,
            _: &str,
        ) -> Result<capsule_sdk::auth::OidcAuthorization, remote::RemoteError> {
            Err(remote::RemoteError::Auth(
                capsule_sdk::auth::AuthError::OidcRedirectInvalid,
            ))
        }

        async fn complete(
            &self,
            _: &capsule_sdk::auth::OidcAuthorization,
            _: &str,
            _: &str,
        ) -> Result<remote::LoginStep, remote::RemoteError> {
            unreachable!("never begun")
        }
    }
    let (tx, rx) = oneshot::channel();
    let reason = browser_sign_in(&Refusing, tx, std::time::Duration::from_mins(5), |_| true)
        .await
        .expect_err("refused");
    assert!(
        reason.contains("OIDC_ALLOW_LOOPBACK_REDIRECT=true"),
        "{reason}"
    );
    assert!(rx.await.is_err(), "nothing was shown");
}
