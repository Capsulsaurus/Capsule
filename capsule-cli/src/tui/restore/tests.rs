//! Restore screen tests: the form's checks, the request it sends, and the frames it draws.

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::*;
use crate::restore::{AssetRestoreFailure, AssetRestoreOutcome, AssetRestoreStatus};
use crate::tui::driver::frame_text;
use crate::tui::state::{Action, AppState, Screen, SessionSummary};
use crate::tui::view;

const ALBUM_ID: &str = "0192f1a0-0000-7000-8000-000000000001";
const ASSET_ID: &str = "0192f1a0-0000-7000-8000-000000000002";

/// A screen editing its form with `values` set field by field.
fn with(values: &[&str]) -> RestoreScreen {
    let mut screen = RestoreScreen::default();
    screen.update(RestoreAction::Edit);
    for (index, value) in values.iter().enumerate() {
        screen.form.set(index, value);
    }
    screen
}

fn submitted(values: &[&str]) -> (RestoreScreen, Vec<Effect>) {
    let mut screen = with(values);
    let effects = screen.on_form_key(FormKey::Submit);
    (screen, effects)
}

fn request(scope: RestoreScope) -> Effect {
    Effect::Restore(RestoreEffect::Run(RestoreRequest {
        library: PathBuf::from("/photos/lib"),
        passphrase: Secret::new("pass phrase"),
        out: PathBuf::from("/tmp/out"),
        scope,
    }))
}

#[test]
fn a_whole_library_restore() {
    let (screen, effects) = submitted(&["/photos/lib", "/tmp/out", "pass phrase"]);
    assert_eq!(effects, vec![request(RestoreScope::Library)]);
    assert_eq!(screen.run(), Some(&Loadable::Loading));
    assert!(!screen.is_editing());
    assert!(screen.form().secret(PASSPHRASE).is_empty());
}

#[test]
fn an_album_or_an_asset_narrows_the_scope() {
    let album = ALBUM_ID.parse().expect("uuid");
    let asset = ASSET_ID.parse().expect("uuid");
    let (_, effects) = submitted(&["/photos/lib", "/tmp/out", "pass phrase", ALBUM_ID]);
    assert_eq!(effects, vec![request(RestoreScope::Album(album))]);
    let (_, effects) = submitted(&["/photos/lib", "/tmp/out", "pass phrase", "", ASSET_ID]);
    assert_eq!(effects, vec![request(RestoreScope::Asset(asset))]);
}

#[test]
fn the_form_is_refused_before_any_request() {
    let cases = [
        (
            vec!["", "/tmp/out", "pw"],
            "Enter the library and the output directory.",
        ),
        (
            vec!["/lib", "/tmp/out", ""],
            "Enter the library passphrase.",
        ),
        (
            vec!["/lib", "/tmp/out", "pw", "nope"],
            "The album id \"nope\" is not a UUID.",
        ),
        (
            vec!["/lib", "/tmp/out", "pw", ALBUM_ID, ASSET_ID],
            "Give an album id or an asset id, not both.",
        ),
    ];
    for (values, reason) in cases {
        let (screen, effects) = submitted(&values);
        assert!(effects.is_empty(), "{values:?}");
        assert_eq!(screen.invalid(), Some(reason));
        assert!(screen.is_editing(), "the form stays open to fix");
        assert_eq!(screen.run(), None);
    }
}

#[test]
fn edit_waits_while_a_restore_runs_and_cancel_stops_editing() {
    let (mut screen, _) = submitted(&["/lib", "/out", "pw"]);
    screen.update(RestoreAction::Edit);
    assert!(!screen.is_editing());
    screen.update(RestoreAction::Finished(Err("offline".to_owned())));
    screen.update(RestoreAction::Edit);
    assert!(screen.is_editing());
    screen.on_form_key(FormKey::Cancel);
    assert!(!screen.is_editing());
    // Keys reach nothing while no form takes text.
    assert!(screen.on_form_key(FormKey::Submit).is_empty());
}

#[test]
fn a_failed_effect_reports_the_run_failing() {
    let request = RestoreRequest {
        library: PathBuf::from("/lib"),
        passphrase: Secret::new("pw"),
        out: PathBuf::from("/out"),
        scope: RestoreScope::Library,
    };
    assert_eq!(
        RestoreEffect::Run(request).failed("panicked".to_owned()),
        RestoreAction::Finished(Err("panicked".to_owned()))
    );
}

#[test]
fn the_passphrase_never_reaches_debug() {
    let mut screen = with(&["/lib", "/out", "hunter2"]);
    let printed_form = format!("{screen:?}");
    let effects = screen.on_form_key(FormKey::Submit);
    let printed = format!("{printed_form} {effects:?}");
    assert!(!printed.contains("hunter2"), "{printed}");
}

fn outcome(n: usize, status: AssetRestoreStatus) -> AssetRestoreOutcome {
    AssetRestoreOutcome {
        asset_id: format!("asset-{n}"),
        album_id: "album".to_owned(),
        status,
        path: None,
        fetched_bytes: 0,
        resumed_bytes: 0,
        error: (status == AssetRestoreStatus::Failed).then(|| AssetRestoreFailure {
            kind: "digest_mismatch",
            code: None,
            message: "the original does not match its manifest".to_owned(),
        }),
    }
}

fn summary(restored: usize, failed: usize) -> RestoreSummary {
    let mut outcomes: Vec<_> = (0..restored)
        .map(|n| outcome(n, AssetRestoreStatus::Restored))
        .collect();
    outcomes.extend((0..failed).map(|n| outcome(100 + n, AssetRestoreStatus::Failed)));
    RestoreSummary {
        assets: restored + failed,
        restored,
        failed,
        out: "/tmp/out".to_owned(),
        outcomes,
        ..RestoreSummary::default()
    }
}

#[test]
fn summary_lines_list_the_first_failures_and_count_the_rest() {
    let lines = summary_lines(&summary(1, FAILURES_SHOWN + 2));
    assert_eq!(lines.len(), 1 + FAILURES_SHOWN + 1);
    let last: String = lines[lines.len() - 1]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert_eq!(
        last,
        "…and 2 more; `capsule restore --json` lists every one."
    );
}

fn draw(state: &AppState) -> String {
    let mut terminal = Terminal::new(TestBackend::new(80, 18)).expect("test terminal");
    terminal
        .draw(|frame| view::render(frame, state))
        .expect("draw on a test backend");
    frame_text(terminal.backend().buffer())
}

fn on_restore() -> AppState {
    let (mut state, _) = AppState::init();
    state.update(Action::SessionLoaded(Ok(SessionSummary::SignedIn {
        access_token_fresh: true,
    })));
    state.update(Action::Open(Screen::Restore));
    state
}

#[test]
fn restore_before_editing() {
    insta::assert_snapshot!(draw(&on_restore()));
}

#[test]
fn restore_editing_with_a_refusal() {
    let mut state = on_restore();
    state.update(Action::Restore(RestoreAction::Edit));
    for c in "/photos/lib".chars() {
        state.update(Action::Form(FormKey::Char(c)));
    }
    state.update(Action::Form(FormKey::Submit));
    insta::assert_snapshot!(draw(&state));
}

#[test]
fn restore_finished_with_failures() {
    let mut state = on_restore();
    state.update(Action::Restore(RestoreAction::Finished(Ok(summary(2, 1)))));
    insta::assert_snapshot!(draw(&state));
}

#[test]
fn restore_that_could_not_run() {
    let mut state = on_restore();
    state.update(Action::Restore(RestoreAction::Finished(Err(
        "Failed to open signed workspace: wrong passphrase".to_owned(),
    ))));
    insta::assert_snapshot!(draw(&state));
}
