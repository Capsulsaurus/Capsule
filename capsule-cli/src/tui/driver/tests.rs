//! Scripted key-driver tests: whole interactions, key by key, with the ports mocked.
//!
//! The responsiveness bound (no input waits more than one frame for a visible response) is
//! asserted structurally: the reducer is synchronous, the driver draws right after each press,
//! and these tests check that the frame drawn after a press already differs from the one before
//! it, while every effect the press asked for is still pending.

use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::tui::state::{Loadable, Screen, SessionSummary};

/// Answers every session read with `answer`, counting the reads.
struct Mock {
    answer: Result<SessionSummary, String>,
    reads: AtomicUsize,
}

impl Mock {
    fn new(answer: Result<SessionSummary, String>) -> Self {
        Self {
            answer,
            reads: AtomicUsize::new(0),
        }
    }
}

impl Ports for Mock {
    async fn session_summary(&self) -> Result<SessionSummary, String> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.answer.clone()
    }
}

fn driver(answer: Result<SessionSummary, String>) -> Driver<Mock> {
    Driver::new(Mock::new(answer), 80, 12)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[tokio::test]
async fn the_first_frame_shows_loading_and_settling_fills_it() {
    let mut d = driver(Ok(SessionSummary::SignedOut));
    assert_eq!(d.pending(), vec![Effect::LoadSession]);
    assert!(d.frame().contains("Reading the session…"), "{}", d.frame());
    d.settle().await;
    assert!(d.pending().is_empty());
    assert!(d.frame().contains("Not signed in."), "{}", d.frame());
    assert_eq!(d.ports.reads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn every_press_that_sends_an_action_changes_the_next_frame() {
    let mut d = driver(Ok(SessionSummary::SignedIn {
        access_token_fresh: true,
    }));
    d.settle().await;
    let script = [
        key(KeyCode::Char('?')),
        key(KeyCode::Esc),
        key(KeyCode::Char('r')),
    ];
    for press in script {
        let before = d.frame();
        let action = d.press(press);
        assert!(action.is_some(), "{press:?} is bound here");
        assert_ne!(d.frame(), before, "{press:?} drew nothing new");
    }
    // The reload's read is still pending: the press above was visible without it.
    assert_eq!(d.pending(), vec![Effect::LoadSession]);
}

#[tokio::test]
async fn reload_shows_loading_before_the_port_answers_and_the_answer_after() {
    let mut d = driver(Ok(SessionSummary::SignedOut));
    d.settle().await;
    d.press(key(KeyCode::Char('r')));
    assert_eq!(d.state().session(), &Loadable::Loading);
    assert!(d.frame().contains("Session: loading…"), "{}", d.frame());
    d.settle().await;
    assert_eq!(
        d.state().session(),
        &Loadable::Ready(SessionSummary::SignedOut)
    );
    assert_eq!(d.ports.reads.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn reload_mashed_while_loading_reads_once() {
    let mut d = driver(Ok(SessionSummary::SignedOut));
    d.settle().await;
    d.type_keys("rrr");
    d.settle().await;
    assert_eq!(d.ports.reads.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_failed_read_is_shown_with_its_reason() {
    let mut d = driver(Err("could not read the session: boom".to_owned()));
    d.settle().await;
    assert!(d.frame().contains("Session unavailable"), "{}", d.frame());
    assert!(d.frame().contains("boom"), "{}", d.frame());
}

#[tokio::test]
async fn help_then_back_then_quit() {
    let mut d = driver(Ok(SessionSummary::SignedOut));
    d.settle().await;
    d.type_keys("?");
    assert_eq!(d.state().screen(), Screen::Help);
    d.press(key(KeyCode::Esc));
    assert_eq!(d.state().screen(), Screen::Home);
    d.type_keys("q");
    assert!(!d.state().is_running());
}

#[tokio::test]
async fn an_effect_outcome_dispatched_while_on_help_lands_under_it() {
    let mut d = driver(Ok(SessionSummary::SignedOut));
    d.type_keys("?");
    d.dispatch(Action::SessionLoaded(Ok(SessionSummary::SignedIn {
        access_token_fresh: true,
    })));
    assert_eq!(d.state().screen(), Screen::Help);
    assert!(d.frame().contains("Signed in"), "{}", d.frame());
}

/// Signs in only with an authenticator code, and counts the sign-ins asked for.
struct TwoFactor {
    logins: AtomicUsize,
}

impl Ports for TwoFactor {
    async fn session_summary(&self) -> Result<SessionSummary, String> {
        Ok(if self.logins.load(Ordering::SeqCst) == 0 {
            SessionSummary::SignedOut
        } else {
            SessionSummary::SignedIn {
                access_token_fresh: true,
            }
        })
    }

    async fn login(
        &self,
        email: String,
        password: crate::tui::form::Secret,
    ) -> Result<crate::tui::auth::LoginResult, String> {
        assert_eq!(
            (email.as_str(), password.expose()),
            ("me@example.com", "pw")
        );
        Ok(crate::tui::auth::LoginResult::SecondFactor {
            mfa_token: crate::tui::form::Secret::new("challenge"),
        })
    }

    async fn verify_second_factor(
        &self,
        mfa_token: crate::tui::form::Secret,
        code: crate::tui::form::Secret,
    ) -> Result<(), String> {
        assert_eq!((mfa_token.expose(), code.expose()), ("challenge", "123456"));
        self.logins.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// The whole sign-in, key by key: open the account screen, type the credentials (a `q` in the
/// password is text, not quit), answer the second factor, and land signed in.
#[tokio::test]
async fn signing_in_with_a_second_factor_key_by_key() {
    let mut d = Driver::new(
        TwoFactor {
            logins: AtomicUsize::new(0),
        },
        80,
        16,
    );
    d.settle().await;
    d.type_keys("al");
    assert_eq!(d.state().screen(), Screen::Auth);
    assert!(d.state().is_editing());
    d.type_keys("me@example.com");
    d.press(key(KeyCode::Tab));
    d.type_keys("pq");
    d.press(key(KeyCode::Backspace));
    d.type_keys("w");
    assert!(d.state().is_running(), "q in a field is text");
    assert!(!d.frame().contains("pw"), "{}", d.frame());
    d.press(key(KeyCode::Enter));
    assert!(d.frame().contains("Signing in…"), "{}", d.frame());
    d.settle().await;
    assert!(d.frame().contains("Authenticator code"), "{}", d.frame());
    d.type_keys("123456");
    d.press(key(KeyCode::Enter));
    d.settle().await;
    assert_eq!(
        d.state().session(),
        &Loadable::Ready(SessionSummary::SignedIn {
            access_token_fresh: true
        })
    );
    assert!(d.frame().contains("Signed in."), "{}", d.frame());
    // Not typing any more: Esc goes back, q quits.
    d.press(key(KeyCode::Esc));
    assert_eq!(d.state().screen(), Screen::Home);
    d.type_keys("q");
    assert!(!d.state().is_running());
}

/// A sync run from the sync screen, with the default port answering that it is not offered:
/// the failure is shown, not waited on.
#[tokio::test]
async fn a_port_a_double_left_out_shows_as_a_failure() {
    let mut d = driver(Ok(SessionSummary::SignedOut));
    d.settle().await;
    d.type_keys("ss");
    assert!(d.frame().contains("Syncing…"), "{}", d.frame());
    d.settle().await;
    assert!(
        d.frame().contains(crate::tui::ports::NOT_OFFERED),
        "{}",
        d.frame()
    );
}

const FIRST: &str = "0192f1a0-0000-7000-8000-000000000001";
const SECOND: &str = "0192f1a0-0000-7000-8000-000000000002";

/// A library of two assets whose second fails its first push and lands on the retry. The
/// progress each run tells is scripted in the order the driver asks for it — the watch goes out
/// before its run, so a run's steps arrive before its summary and its close after — and every
/// request is recorded.
struct Library {
    imports: std::sync::Mutex<Vec<crate::tui::import::ImportRequest>>,
    pushes: std::sync::Mutex<Vec<crate::tui::push::PushRequest>>,
    progress: std::sync::Mutex<std::collections::VecDeque<Vec<crate::tui::push::PushProgress>>>,
}

impl Library {
    fn new() -> Self {
        use crate::tui::push::PushProgress;
        let first = vec![
            pushed(FIRST, None),
            pushed(SECOND, Some("connection reset")),
        ];
        let retry = vec![pushed(SECOND, None)];
        let progress = [
            told(&first),
            vec![PushProgress::Closed { run: Some(1) }],
            told(&retry),
            vec![PushProgress::Closed { run: Some(2) }],
        ];
        Self {
            imports: std::sync::Mutex::default(),
            pushes: std::sync::Mutex::default(),
            progress: std::sync::Mutex::new(progress.into()),
        }
    }
}

/// What a run tells for `outcomes`: each asset's start, one of its two blobs finalized, and its
/// outcome.
fn told(outcomes: &[crate::remote::AssetPushOutcome]) -> Vec<crate::tui::push::PushProgress> {
    use capsule_sdk::events::TransferEvent;

    use crate::tui::push::PushProgress;
    let mut told = Vec::new();
    for outcome in outcomes {
        let id = &outcome.asset_id;
        told.push(PushProgress::Started {
            asset_id: id.clone(),
            blobs: vec![format!("{id}-0"), format!("{id}-1")],
        });
        told.push(PushProgress::Transfer(TransferEvent::Finalize {
            blob: format!("{id}-0"),
            session_id: "s".to_owned(),
            size: 10,
        }));
        told.push(PushProgress::Finished(outcome.clone()));
    }
    told
}

fn pushed(asset: &str, failure: Option<&str>) -> crate::remote::AssetPushOutcome {
    use crate::remote::{AssetPushFailure, AssetPushOutcome, AssetPushStatus};
    AssetPushOutcome {
        asset_id: asset.to_owned(),
        status: if failure.is_some() {
            AssetPushStatus::Failed
        } else {
            AssetPushStatus::Verified
        },
        uploaded: 2,
        merged: 0,
        already_held: 0,
        deferred: 0,
        bytes: 20,
        receipts: if failure.is_some() { 0 } else { 2 },
        resealed: false,
        error: failure.map(|message| AssetPushFailure {
            kind: "upload",
            code: None,
            message: message.to_owned(),
        }),
    }
}

impl Ports for Library {
    async fn session_summary(&self) -> Result<SessionSummary, String> {
        Ok(SessionSummary::SignedIn {
            access_token_fresh: true,
        })
    }

    async fn import(
        &self,
        request: crate::tui::import::ImportRequest,
    ) -> Result<crate::import_report::ImportReport, String> {
        let imported = request.sources.len();
        self.imports.lock().unwrap().push(request);
        Ok(crate::import_report::ImportReport {
            imported,
            ..crate::import_report::ImportReport::default()
        })
    }

    async fn push(
        &self,
        request: crate::tui::push::PushRequest,
    ) -> Result<crate::remote::PushSummary, String> {
        let retry = request.assets.is_some();
        self.pushes.lock().unwrap().push(request);
        let outcomes = if retry {
            vec![pushed(SECOND, None)]
        } else {
            vec![
                pushed(FIRST, None),
                pushed(SECOND, Some("connection reset")),
            ]
        };
        let failed = outcomes
            .iter()
            .filter(|o| o.status == crate::remote::AssetPushStatus::Failed)
            .count();
        Ok(crate::remote::PushSummary {
            assets: outcomes.len(),
            verified_assets: outcomes.len() - failed,
            failed_assets: failed,
            outcomes,
            ..crate::remote::PushSummary::default()
        })
    }

    async fn push_progress(&self) -> Vec<crate::tui::push::PushProgress> {
        self.progress
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| vec![crate::tui::push::PushProgress::Closed { run: None }])
    }
}

/// Pick a source, import it, go on to push the library it landed in, see the asset that did
/// not reach custody, and retry just that one — key by key, the passphrase typed each time and
/// never drawn.
#[tokio::test]
async fn import_then_push_then_retry_the_failure_key_by_key() {
    let mut d = Driver::new(Library::new(), 100, 30);
    d.settle().await;

    // Import: pick a source, fill in the library, run.
    d.type_keys("i");
    assert_eq!(d.state().screen(), Screen::Import);
    d.type_keys("a");
    d.type_keys("/photos/2024");
    d.press(key(KeyCode::Enter));
    assert!(d.frame().contains("› /photos/2024"), "{}", d.frame());
    d.type_keys("e/lib");
    d.press(key(KeyCode::Tab));
    d.type_keys("secret");
    let before = d.frame();
    d.press(key(KeyCode::Enter));
    assert_ne!(
        d.frame(),
        before,
        "the press is visible before the import answers"
    );
    assert!(d.frame().contains("Importing…"), "{}", d.frame());
    d.settle().await;
    assert!(d.frame().contains("Done: 1 imported"), "{}", d.frame());
    let imports = d.ports.imports.lock().unwrap().clone();
    assert_eq!(imports.len(), 1);
    assert_eq!(
        imports[0].sources,
        vec![std::path::PathBuf::from("/photos/2024")]
    );
    assert_eq!(imports[0].passphrase.expose(), "secret");

    // On to the push screen, which already names the library.
    d.type_keys("p");
    assert_eq!(d.state().screen(), Screen::Push);
    assert!(d.frame().contains("/lib"), "{}", d.frame());
    d.type_keys("e");
    d.press(key(KeyCode::Tab));
    d.type_keys("secret");
    d.press(key(KeyCode::Enter));
    assert!(d.frame().contains("Pushing…"), "{}", d.frame());
    assert!(!d.frame().contains("secret"), "{}", d.frame());
    d.settle().await;
    assert!(!d.state().push().is_watching(), "the run's stream closed");
    assert_eq!(
        d.ports.progress.lock().unwrap().len(),
        2,
        "the run's steps and its close were each watched for"
    );
    let frame = d.frame();
    assert!(
        frame.contains("1 of 2 assets in custody, 1 failed."),
        "{frame}"
    );
    assert!(
        frame.contains(&format!("{SECOND}: connection reset")),
        "{frame}"
    );

    // Retry: only the failed asset is pushed again, and it lands.
    d.type_keys("r");
    assert!(d.state().is_editing());
    d.type_keys("secret");
    d.press(key(KeyCode::Enter));
    d.settle().await;
    let pushes = d.ports.pushes.lock().unwrap().clone();
    assert_eq!(pushes.len(), 2);
    assert_eq!(pushes[0].assets, None);
    assert_eq!(pushes[1].assets, Some(vec![SECOND.parse().unwrap()]));
    let frame = d.frame();
    assert!(
        frame.contains("2 of 2 assets in custody, 0 failed."),
        "{frame}"
    );
    assert!(!frame.contains("connection reset"), "{frame}");
    assert!(d.ports.progress.lock().unwrap().is_empty());
    assert!(!d.state().push().is_watching());
}

#[test]
fn frame_text_trims_each_row() {
    let buffer = Buffer::with_lines(["ab  ", "    "]);
    assert_eq!(frame_text(&buffer), "ab\n\n");
}
